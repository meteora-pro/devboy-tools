//! `devboy-tune` — offline adaptive-config tuner.
//!
//! Reads telemetry JSONL emitted by the `LayeredPipeline`, aggregates
//! per-endpoint fingerprints, applies rules R1-R5 from
//! `docs/research/paper-2-mckp-format-adaptive.md` §Adaptive Configuration,
//! and writes an updated `pipeline_config.toml`.
//!
//! # Usage
//!
//! ```shell
//! devboy-tune analyze \
//!   --input-dir ~/.config/devboy/telemetry/events \
//!   --output    ~/.config/devboy/pipeline_config.toml
//!
//! devboy-tune show --config ~/.config/devboy/pipeline_config.toml
//! ```
//!
//! When `--input-dir` is missing, any sessions already recorded are
//! analyzed. When absent, the tuner emits a default config and exits OK
//! (so first-time setup succeeds without prior telemetry).

use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use devboy_format_pipeline::adaptive_config::AdaptiveConfig;
use devboy_format_pipeline::telemetry::PipelineEvent;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }
    match args[1].as_str() {
        "analyze" => match cmd_analyze(&args[2..]) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        "show" => match cmd_show(&args[2..]) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        },
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown subcommand: {other}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
devboy-tune — offline tuner for the layered pipeline

Usage:
    devboy-tune analyze [--input-dir <PATH>] [--output <PATH>]
        Aggregate telemetry and emit a tuned pipeline_config.toml.

    devboy-tune show [--config <PATH>]
        Pretty-print the current config.

    devboy-tune help
        Show this message.

Defaults:
    --input-dir  ~/.config/devboy/telemetry/events
    --output     ~/.config/devboy/pipeline_config.toml
    --config     ~/.config/devboy/pipeline_config.toml
";

// ─── ENDPOINT STATISTICS ────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
struct EndpointStats {
    call_count: u64,
    total_chars: u64,
    dup_hits: u64,
    shape_counts: BTreeMap<String, u64>,
    layer_counts: BTreeMap<String, u64>,
    total_baseline_tokens: u64,
    total_final_tokens: u64,
}

impl EndpointStats {
    fn update(&mut self, ev: &PipelineEvent) {
        self.call_count += 1;
        self.total_chars += ev.response_chars;
        if ev.is_dedup_hit {
            self.dup_hits += 1;
        }
        *self
            .shape_counts
            .entry(format!("{:?}", ev.shape))
            .or_insert(0) += 1;
        *self
            .layer_counts
            .entry(format!("{:?}", ev.layer_used))
            .or_insert(0) += 1;
        self.total_baseline_tokens += ev.tokens_baseline as u64;
        self.total_final_tokens += ev.tokens_final as u64;
    }

    fn dup_rate(&self) -> f32 {
        if self.call_count == 0 {
            0.0
        } else {
            self.dup_hits as f32 / self.call_count as f32
        }
    }

    fn avg_chars(&self) -> f32 {
        if self.call_count == 0 {
            0.0
        } else {
            self.total_chars as f32 / self.call_count as f32
        }
    }

    fn dominant_shape(&self) -> Option<&str> {
        self.shape_counts
            .iter()
            .max_by_key(|(_, c)| **c)
            .map(|(s, _)| s.as_str())
    }

    fn savings_pct(&self) -> f32 {
        if self.total_baseline_tokens == 0 {
            0.0
        } else {
            1.0 - (self.total_final_tokens as f32 / self.total_baseline_tokens as f32)
        }
    }
}

// ─── CORPUS AGGREGATE ───────────────────────────────────────────────────

#[derive(Debug, Default)]
struct CorpusStats {
    total_events: u64,
    total_sessions: u64,
    total_baseline_tokens: u64,
    total_final_tokens: u64,
    compaction_events: u64,
    per_endpoint: BTreeMap<String, EndpointStats>,
    sessions_seen: BTreeMap<String, u64>,
}

impl CorpusStats {
    fn update(&mut self, ev: &PipelineEvent) {
        self.total_events += 1;
        self.total_baseline_tokens += ev.tokens_baseline as u64;
        self.total_final_tokens += ev.tokens_final as u64;
        if ev.context_partition > 0 {
            self.compaction_events += 1;
        }
        *self
            .sessions_seen
            .entry(ev.session_hash.clone())
            .or_insert(0) += 1;
        self.per_endpoint
            .entry(ev.endpoint_class.clone())
            .or_default()
            .update(ev);
    }

    fn finalize(&mut self) {
        self.total_sessions = self.sessions_seen.len() as u64;
    }

    fn savings_pct(&self) -> f32 {
        if self.total_baseline_tokens == 0 {
            0.0
        } else {
            1.0 - (self.total_final_tokens as f32 / self.total_baseline_tokens as f32)
        }
    }
}

// ─── I/O ────────────────────────────────────────────────────────────────

fn default_input_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/devboy/telemetry/events")
}

fn default_output() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/devboy/pipeline_config.toml")
}

fn parse_flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    for w in args.windows(2) {
        if w[0] == name {
            return Some(&w[1]);
        }
    }
    None
}

fn scan_jsonl_dir(dir: &Path, out: &mut CorpusStats) -> Result<usize, String> {
    let mut read = 0usize;
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir({:?}): {e}", dir))?;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let f = match File::open(&p) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let br = BufReader::new(f);
        for line in br.lines().map_while(|r| r.ok()) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Parse once as a generic JSON value so we can robustly distinguish
            // PipelineEvent records from session_summary wrappers — even if
            // future schema additions introduce a `type` field on
            // PipelineEvent itself.
            let value: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(obj) = value.as_object() else {
                continue;
            };
            // Explicit session_summary wrapper: `{"type":"session_summary","data":{…}}`.
            if obj.get("data").is_some()
                && obj
                    .get("type")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| t == "session_summary")
            {
                continue;
            }
            if let Ok(ev) = serde_json::from_value::<PipelineEvent>(value) {
                out.update(&ev);
                read += 1;
            }
        }
    }
    out.finalize();
    Ok(read)
}

// ─── RULES R1-R5 ────────────────────────────────────────────────────────

fn apply_tuning_rules(cfg: &mut AdaptiveConfig, stats: &CorpusStats) {
    // R1: per-endpoint dedup enablement based on dup_rate.
    for (endpoint, s) in &stats.per_endpoint {
        if s.call_count < 20 {
            continue; // too little data; skip
        }
        let rate = s.dup_rate();
        if rate >= 0.30 {
            cfg.dedup
                .enabled_per_endpoint
                .insert(endpoint.clone(), true);
            let ovr = cfg.endpoint_overrides.entry(endpoint.clone()).or_default();
            ovr.dedup_enabled = Some(true);
            ovr.lru_size = Some((1 + (rate * 20.0).round() as usize).min(10));
        } else if rate <= 0.05 {
            cfg.dedup
                .enabled_per_endpoint
                .insert(endpoint.clone(), false);
            let ovr = cfg.endpoint_overrides.entry(endpoint.clone()).or_default();
            ovr.dedup_enabled = Some(false);
        }
    }

    // R2: per-endpoint template selection based on dominant shape.
    for (endpoint, s) in &stats.per_endpoint {
        if s.call_count < 20 {
            continue;
        }
        let Some(shape) = s.dominant_shape() else {
            continue;
        };
        match shape {
            "MarkdownTable" => {
                cfg.templates
                    .endpoint_overrides
                    .insert(endpoint.clone(), "csv_from_md".into());
            }
            "NestedObject" => {
                // Heuristic: if dedup rate high, it's a poller — use pipeline_deep_mckp.
                if s.dup_rate() >= 0.20 {
                    cfg.templates
                        .endpoint_overrides
                        .insert(endpoint.clone(), "pipeline_deep_mckp".into());
                }
            }
            _ => {}
        }
    }

    // R3: global LRU sizing based on compaction frequency.
    if stats.total_events > 0 {
        let compaction_rate = stats.compaction_events as f32 / stats.total_events as f32;
        if compaction_rate > 0.05 {
            cfg.dedup.lru_size = 10;
        } else if stats.total_sessions > 0
            && (stats.total_events / stats.total_sessions.max(1)) < 20
        {
            cfg.dedup.lru_size = 3;
        }
        // else leave default (5)
    }

    // R4: MCKP recursion depth based on observed nesting.
    // (We don't yet collect depth_max in telemetry; fall back to default 5.)
    let _ = &cfg.mckp.recursion_depth;

    // R5: min_body_chars based on p25 (approx via mean / 4).
    if stats.total_events > 0 {
        let mean_chars = stats.total_baseline_tokens as f32 * 4.0 / stats.total_events as f32;
        let suggested = (mean_chars * 0.25) as usize;
        cfg.dedup.min_body_chars = suggested.clamp(100, 500);
    }
}

// ─── SUBCOMMANDS ────────────────────────────────────────────────────────

fn cmd_analyze(args: &[String]) -> Result<(), String> {
    let input_dir = parse_flag(args, "--input-dir")
        .map(PathBuf::from)
        .unwrap_or_else(default_input_dir);
    let output = parse_flag(args, "--output")
        .map(PathBuf::from)
        .unwrap_or_else(default_output);

    eprintln!("# input:  {}", input_dir.display());
    eprintln!("# output: {}", output.display());

    let mut corpus = CorpusStats::default();
    let read = if input_dir.exists() {
        scan_jsonl_dir(&input_dir, &mut corpus)?
    } else {
        eprintln!("# input dir missing — writing default config");
        0
    };

    eprintln!(
        "# events: {} | sessions: {} | endpoints: {} | savings: {:.1}%",
        read,
        corpus.total_sessions,
        corpus.per_endpoint.len(),
        corpus.savings_pct() * 100.0,
    );

    let mut cfg = AdaptiveConfig::load_or_default(&output)
        .map_err(|e| format!("load existing config: {e}"))?;
    apply_tuning_rules(&mut cfg, &corpus);
    cfg.save(&output)
        .map_err(|e| format!("write config: {e}"))?;

    eprintln!("# tuned config → {}", output.display());
    print_top_endpoints(&corpus, 10);
    Ok(())
}

fn cmd_show(args: &[String]) -> Result<(), String> {
    let cfg_path = parse_flag(args, "--config")
        .map(PathBuf::from)
        .unwrap_or_else(default_output);
    let cfg = AdaptiveConfig::load(&cfg_path).map_err(|e| format!("load config: {e}"))?;
    println!(
        "{}",
        toml::to_string_pretty(&cfg).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn print_top_endpoints(corpus: &CorpusStats, n: usize) {
    let mut endpoints: Vec<_> = corpus.per_endpoint.iter().collect();
    endpoints.sort_by(|a, b| b.1.call_count.cmp(&a.1.call_count));
    eprintln!();
    eprintln!("# top endpoints by call count:");
    eprintln!(
        "#   {:<40} {:>8} {:>8} {:>8} {:>10}",
        "endpoint", "calls", "dup_rate", "avg_chars", "savings"
    );
    for (name, s) in endpoints.iter().take(n) {
        eprintln!(
            "#   {:<40} {:>8} {:>8.1}% {:>8.0} {:>9.1}%",
            truncate(name, 40),
            s.call_count,
            s.dup_rate() * 100.0,
            s.avg_chars(),
            s.savings_pct() * 100.0,
        );
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let prefix: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{prefix}…")
    }
}

// ─── TESTS ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_format_pipeline::telemetry::{Layer, Shape};

    fn ev(endpoint: &str, dup: bool, shape: Shape, base: u32, finalt: u32) -> PipelineEvent {
        // PipelineEvent is #[non_exhaustive]; build via Default + mutation.
        let mut e = PipelineEvent::default();
        e.session_hash = "s1".into();
        e.tool_call_id_hash = "tc".into();
        e.tool_name_anon = endpoint.into();
        e.endpoint_class = endpoint.into();
        e.response_chars = (base as u64) * 4;
        e.shape = shape;
        e.is_dedup_hit = dup;
        e.layer_used = if dup { Layer::L0 } else { Layer::L3 };
        e.tokens_baseline = base;
        e.tokens_final = finalt;
        e.sample_rate_applied = 1.0;
        e
    }

    #[test]
    fn corpus_aggregates_and_rule_r1_enables_dedup_on_high_rate() {
        let mut corpus = CorpusStats::default();
        // 30 calls, 15 dedup hits → 50% dup rate
        for i in 0..30 {
            corpus.update(&ev("ep1", i < 15, Shape::Prose, 100, 100));
        }
        corpus.finalize();
        let mut cfg = AdaptiveConfig::default();
        apply_tuning_rules(&mut cfg, &corpus);
        assert_eq!(cfg.dedup.enabled_per_endpoint.get("ep1"), Some(&true));
        assert!(cfg.endpoint_overrides["ep1"].lru_size.unwrap() > 5);
    }

    #[test]
    fn rule_r1_disables_dedup_on_low_rate() {
        let mut corpus = CorpusStats::default();
        for i in 0..40 {
            corpus.update(&ev("ep_unique", i == 0, Shape::Prose, 100, 100));
        }
        corpus.finalize();
        let mut cfg = AdaptiveConfig::default();
        apply_tuning_rules(&mut cfg, &corpus);
        assert_eq!(
            cfg.dedup.enabled_per_endpoint.get("ep_unique"),
            Some(&false)
        );
    }

    #[test]
    fn rule_r2_picks_csv_from_md_for_markdown_tables() {
        let mut corpus = CorpusStats::default();
        for _ in 0..25 {
            corpus.update(&ev("md_endpoint", false, Shape::MarkdownTable, 100, 50));
        }
        corpus.finalize();
        let mut cfg = AdaptiveConfig::default();
        apply_tuning_rules(&mut cfg, &corpus);
        assert_eq!(
            cfg.templates.endpoint_overrides.get("md_endpoint"),
            Some(&"csv_from_md".to_string())
        );
    }

    #[test]
    fn rule_r2_picks_pipeline_deep_mckp_for_high_dup_nested() {
        let mut corpus = CorpusStats::default();
        for i in 0..30 {
            corpus.update(&ev(
                "pipeline_endpoint",
                i < 10, // 33% dup rate
                Shape::NestedObject,
                100,
                if i < 10 { 10 } else { 100 },
            ));
        }
        corpus.finalize();
        let mut cfg = AdaptiveConfig::default();
        apply_tuning_rules(&mut cfg, &corpus);
        assert_eq!(
            cfg.templates.endpoint_overrides.get("pipeline_endpoint"),
            Some(&"pipeline_deep_mckp".to_string())
        );
    }

    #[test]
    fn low_call_count_endpoints_skipped() {
        let mut corpus = CorpusStats::default();
        // Only 5 calls — below 20-sample minimum.
        for i in 0..5 {
            corpus.update(&ev("rare", i < 4, Shape::Prose, 100, 100));
        }
        corpus.finalize();
        let mut cfg = AdaptiveConfig::default();
        apply_tuning_rules(&mut cfg, &corpus);
        assert!(!cfg.dedup.enabled_per_endpoint.contains_key("rare"));
    }

    #[test]
    fn endpoint_stats_computes_fields() {
        let mut s = EndpointStats::default();
        s.update(&ev("x", false, Shape::Prose, 100, 100));
        s.update(&ev("x", true, Shape::Prose, 100, 10));
        assert_eq!(s.call_count, 2);
        assert!((s.dup_rate() - 0.5).abs() < 1e-6);
        assert!((s.savings_pct() - 0.45).abs() < 1e-6);
    }
}
