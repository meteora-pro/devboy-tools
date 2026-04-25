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

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use devboy_format_pipeline::adaptive_config::{
    AdaptiveConfig, DataProfile, SessionStats,
};
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
        "from-claude-logs" => match cmd_from_claude_logs(&args[2..]) {
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
        Aggregate pipeline telemetry and emit a tuned pipeline_config.toml.

    devboy-tune from-claude-logs [--input-dir <PATH>] [--project <NAME>]
                                 [--output <PATH>] [--dry-run]
        Mine Claude Code JSONL logs (~/.claude/projects/...), classify the
        user's tool/data/agent profile, and emit a tuned pipeline_config.toml.
        Use this when no telemetry has been collected yet — the logs already
        capture model_id, tool usage, and endpoint distribution.

    devboy-tune show [--config <PATH>]
        Pretty-print the current config.

    devboy-tune help
        Show this message.

Defaults:
    --input-dir          ~/.config/devboy/telemetry/events
    --input-dir (logs)   ~/.claude/projects
    --output             ~/.config/devboy/pipeline_config.toml
    --config             ~/.config/devboy/pipeline_config.toml
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
    /// Tokens saved by L0 dedup hits only (baseline − final, summed
    /// across events whose `layer_used == L0`).
    dedup_saved_tokens: u64,
    /// Tokens saved by L1 / L2 encoders (baseline − final on the L0-miss
    /// branch only).
    encoder_saved_tokens: u64,
}

impl EndpointStats {
    fn update(&mut self, ev: &PipelineEvent) {
        use devboy_format_pipeline::telemetry::Layer;
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
        let saved = (ev.tokens_baseline as i64 - ev.tokens_final as i64).max(0) as u64;
        match ev.layer_used {
            Layer::L0 => self.dedup_saved_tokens += saved,
            Layer::L1 | Layer::L2 => self.encoder_saved_tokens += saved,
            Layer::L3 => {} // passthrough — no savings
        }
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

    /// Fraction of total baseline tokens reclaimed by L0 dedup hints.
    fn dedup_savings_pct(&self) -> f32 {
        if self.total_baseline_tokens == 0 {
            0.0
        } else {
            self.dedup_saved_tokens as f32 / self.total_baseline_tokens as f32
        }
    }

    /// Fraction of total baseline tokens reclaimed by L1 / L2 encoders.
    fn encoder_savings_pct(&self) -> f32 {
        if self.total_baseline_tokens == 0 {
            0.0
        } else {
            self.encoder_saved_tokens as f32 / self.total_baseline_tokens as f32
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
            // Heuristic: high dup rate on a nested object ⇒ poller ⇒ pipeline_deep_mckp.
            "NestedObject" if s.dup_rate() >= 0.20 => {
                cfg.templates
                    .endpoint_overrides
                    .insert(endpoint.clone(), "pipeline_deep_mckp".into());
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

// ─── CLAUDE-LOG SCANNER ──────────────────────────────────────────────────

/// Aggregate observed from Claude Code session JSONL logs.
///
/// We never store raw text — only counts, model ids, and tool / endpoint names.
/// Used by the `from-claude-logs` subcommand to seed `pipeline_config.toml`
/// when no `[crate::telemetry]` events have been collected yet.
#[derive(Debug, Default)]
struct ClaudeLogStats {
    total_events: u64,
    sessions: BTreeSet<String>,
    /// model_id (`message.model`) → event count.
    model_counts: BTreeMap<String, u64>,
    /// tool name (`message.content[].name`) → invocation count.
    tool_counts: BTreeMap<String, u64>,
    /// Endpoint-pattern hits — used to auto-enable matching data profiles.
    /// Key is the full tool name (e.g. `mcp__gitlab__get_issues`).
    endpoints: BTreeMap<String, u64>,
    /// Read-class tools (Read, Glob, Grep, NotebookRead, …).
    read_invocations: u64,
    /// Total tool invocations (read + write + mcp + bash + …).
    total_invocations: u64,
    /// Number of `/compact` events seen (heuristic — looks for the explicit token).
    compactions: u64,
}

impl ClaudeLogStats {
    fn dominant_model(&self) -> Option<&str> {
        self.model_counts
            .iter()
            .max_by_key(|(_, c)| **c)
            .map(|(s, _)| s.as_str())
    }

    fn read_share(&self) -> f32 {
        if self.total_invocations == 0 {
            0.0
        } else {
            self.read_invocations as f32 / self.total_invocations as f32
        }
    }

    fn to_session_stats(&self) -> SessionStats {
        let avg = if self.sessions.is_empty() {
            self.total_events as usize
        } else {
            (self.total_events as usize) / self.sessions.len().max(1)
        };
        SessionStats {
            event_count: avg,
            compaction_count: if self.sessions.is_empty() {
                self.compactions as usize
            } else {
                (self.compactions as usize) / self.sessions.len().max(1)
            },
            read_share: self.read_share(),
        }
    }
}

const READ_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "NotebookRead",
    "WebFetch",
    "WebSearch",
];

/// Update `stats` with one parsed JSONL line. Returns `true` if the line
/// contributed something (so the caller can count "read" events).
fn ingest_claude_line(line: &str, stats: &mut ClaudeLogStats) -> bool {
    let val: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let Some(obj) = val.as_object() else {
        return false;
    };

    if let Some(sid) = obj.get("sessionId").and_then(|v| v.as_str()) {
        stats.sessions.insert(sid.to_string());
    } else if let Some(sid) = obj.get("session_id").and_then(|v| v.as_str()) {
        stats.sessions.insert(sid.to_string());
    }
    stats.total_events += 1;

    // model id from assistant messages
    if let Some(model) = val
        .pointer("/message/model")
        .and_then(|v| v.as_str())
        .or_else(|| val.pointer("/model").and_then(|v| v.as_str()))
    {
        *stats
            .model_counts
            .entry(model.to_string())
            .or_insert(0) += 1;
    }

    // tool uses inside content blocks
    let content = val
        .pointer("/message/content")
        .and_then(|v| v.as_array())
        .or_else(|| val.pointer("/content").and_then(|v| v.as_array()));
    if let Some(arr) = content {
        for blk in arr {
            let Some(t) = blk.get("type").and_then(|v| v.as_str()) else {
                continue;
            };
            if t == "tool_use" {
                if let Some(name) = blk.get("name").and_then(|v| v.as_str()) {
                    *stats
                        .tool_counts
                        .entry(name.to_string())
                        .or_insert(0) += 1;
                    stats.total_invocations += 1;
                    if READ_TOOLS.contains(&name) {
                        stats.read_invocations += 1;
                    }
                    if name.starts_with("mcp__") {
                        *stats
                            .endpoints
                            .entry(name.to_string())
                            .or_insert(0) += 1;
                    }
                }
            }
            // /compact heuristic: a user message containing the token
            if t == "text"
                && blk
                    .get("text")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| s.starts_with("/compact"))
            {
                stats.compactions += 1;
            }
        }
    }
    true
}

fn scan_claude_jsonl_dir(dir: &Path, stats: &mut ClaudeLogStats) -> Result<usize, String> {
    let mut read = 0usize;
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("read_dir({:?}): {e}", dir))?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            // Recurse one level — ~/.claude/projects/<project>/*.jsonl
            read += scan_claude_jsonl_dir(&p, stats).unwrap_or(0);
            continue;
        }
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
            if ingest_claude_line(trimmed, stats) {
                read += 1;
            }
        }
    }
    Ok(read)
}

/// Apply v2 profile rules to `cfg` based on `claude_stats`.
///
/// Rules:
/// - **P1** (LLM): if a single model dominates ≥80% of assistant events,
///   set `profiles.llm.active` to that model id (when it matches a known
///   variant; otherwise leave at `"auto"`).
/// - **P2** (Agent): set `profiles.agent.active` from the auto classifier
///   so subsequent loads pin the choice (no surprise on different sessions).
/// - **P3** (Data): for every observed `mcp__*` endpoint, ensure a matching
///   data-profile variant exists; if absent, register a placeholder so the
///   user can edit it.
fn apply_profile_rules(cfg: &mut AdaptiveConfig, claude_stats: &ClaudeLogStats) {
    // P1: dominant model
    if let Some(model) = claude_stats.dominant_model() {
        let total: u64 = claude_stats.model_counts.values().sum();
        let count = claude_stats
            .model_counts
            .get(model)
            .copied()
            .unwrap_or(0);
        if total > 0 && (count * 100 / total) >= 80
            && cfg.profiles.llm.variants.contains_key(model)
        {
            cfg.profiles.llm.active = model.to_string();
        }
    }

    // P2: pin agent variant
    let session = claude_stats.to_session_stats();
    let picked = cfg.profiles.agent.resolve(&session).clone();
    // Find which key resolves to this profile. Match by reference is awkward;
    // re-classify with the same heuristic and look up the name.
    let pinned: &str = if session.event_count >= 500 && session.compaction_count >= 3 {
        "marathon_refactor"
    } else if session.event_count <= 200 && session.read_share >= 0.5 {
        "file_search_heavy"
    } else {
        "default"
    };
    if cfg.profiles.agent.variants.contains_key(pinned) {
        cfg.profiles.agent.active = pinned.to_string();
    }
    let _ = picked; // documentational

    // P3: register unknown endpoints as placeholder data profiles
    let known_patterns: BTreeSet<String> = cfg
        .profiles
        .data
        .variants
        .values()
        .map(|v| v.endpoint_pattern.clone())
        .collect();
    for endpoint in claude_stats.endpoints.keys() {
        let already = known_patterns.iter().any(|p| {
            endpoint == p || endpoint.starts_with(p)
        });
        if already {
            continue;
        }
        // Generate a key from the last segment of the mcp tool name.
        let key = endpoint
            .rsplit("__")
            .next()
            .unwrap_or(endpoint)
            .to_string();
        cfg.profiles.data.variants.entry(key).or_insert_with(|| {
            DataProfile {
                endpoint_pattern: endpoint.clone(),
                preferred_format: None, // user picks
                hint_set: Vec::new(),
            }
        });
    }
}

fn default_claude_logs_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude/projects")
}

fn print_claude_summary(stats: &ClaudeLogStats) {
    eprintln!();
    eprintln!("# claude logs scanned:");
    eprintln!("#   events:   {}", stats.total_events);
    eprintln!("#   sessions: {}", stats.sessions.len());
    eprintln!(
        "#   tools:    {} invocations ({} read-class, {:.1}% read-share)",
        stats.total_invocations,
        stats.read_invocations,
        stats.read_share() * 100.0
    );
    eprintln!("#   compactions: {}", stats.compactions);
    eprintln!();
    eprintln!("#   model distribution:");
    let mut models: Vec<_> = stats.model_counts.iter().collect();
    models.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (m, c) in models.iter().take(8) {
        eprintln!("#     {:<35} {:>8}", truncate(m, 35), c);
    }
    if !stats.endpoints.is_empty() {
        eprintln!();
        eprintln!("#   top mcp endpoints:");
        let mut eps: Vec<_> = stats.endpoints.iter().collect();
        eps.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        for (e, c) in eps.iter().take(10) {
            eprintln!("#     {:<45} {:>8}", truncate(e, 45), c);
        }
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

    let agg_dedup_saved: u64 = corpus
        .per_endpoint
        .values()
        .map(|s| s.dedup_saved_tokens)
        .sum();
    let agg_encoder_saved: u64 = corpus
        .per_endpoint
        .values()
        .map(|s| s.encoder_saved_tokens)
        .sum();
    let dedup_pct = if corpus.total_baseline_tokens == 0 {
        0.0
    } else {
        agg_dedup_saved as f32 / corpus.total_baseline_tokens as f32 * 100.0
    };
    let encoder_pct = if corpus.total_baseline_tokens == 0 {
        0.0
    } else {
        agg_encoder_saved as f32 / corpus.total_baseline_tokens as f32 * 100.0
    };
    eprintln!(
        "# events: {} | sessions: {} | endpoints: {} | dedup: {:.1}% | encoder: {:.1}% | total: {:.1}%",
        read,
        corpus.total_sessions,
        corpus.per_endpoint.len(),
        dedup_pct,
        encoder_pct,
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

fn cmd_from_claude_logs(args: &[String]) -> Result<(), String> {
    let input_dir = parse_flag(args, "--input-dir")
        .map(PathBuf::from)
        .unwrap_or_else(default_claude_logs_dir);
    let output = parse_flag(args, "--output")
        .map(PathBuf::from)
        .unwrap_or_else(default_output);
    let project = parse_flag(args, "--project").map(String::from);
    let dry_run = args.iter().any(|a| a == "--dry-run");

    eprintln!("# input:   {}", input_dir.display());
    if let Some(p) = &project {
        eprintln!("# project: {p}");
    }
    eprintln!("# output:  {}{}", output.display(), if dry_run { " (dry-run)" } else { "" });

    if !input_dir.exists() {
        return Err(format!(
            "claude logs directory not found: {}",
            input_dir.display()
        ));
    }

    let scan_root = match &project {
        Some(p) => input_dir.join(p),
        None => input_dir.clone(),
    };
    let mut stats = ClaudeLogStats::default();
    let read = scan_claude_jsonl_dir(&scan_root, &mut stats)?;
    eprintln!("# read {read} jsonl lines");

    if read == 0 {
        return Err("no jsonl events parsed — check the path".into());
    }

    print_claude_summary(&stats);

    let mut cfg = AdaptiveConfig::load_or_default(&output)
        .map_err(|e| format!("load existing config: {e}"))?;
    apply_profile_rules(&mut cfg, &stats);

    eprintln!();
    eprintln!("# proposed profile pins:");
    eprintln!("#   profiles.llm.active   = {:?}", cfg.profiles.llm.active);
    eprintln!("#   profiles.agent.active = {:?}", cfg.profiles.agent.active);
    let new_data: Vec<&str> = cfg
        .profiles
        .data
        .variants
        .keys()
        .map(String::as_str)
        .collect();
    eprintln!("#   profiles.data.variants = {} ({:?})", new_data.len(), new_data);

    if dry_run {
        eprintln!();
        eprintln!("# dry-run: not writing config. Re-run without --dry-run to apply.");
        return Ok(());
    }

    cfg.save(&output)
        .map_err(|e| format!("write config: {e}"))?;
    eprintln!("# wrote → {}", output.display());
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
    endpoints.sort_by_key(|e| std::cmp::Reverse(e.1.call_count));
    eprintln!();
    eprintln!("# top endpoints by call count:");
    eprintln!(
        "#   {:<35} {:>7} {:>8} {:>8} {:>9} {:>9} {:>9}",
        "endpoint", "calls", "dup_rate", "avg_chars",
        "dedup%", "encoder%", "total%"
    );
    for (name, s) in endpoints.iter().take(n) {
        eprintln!(
            "#   {:<35} {:>7} {:>7.1}% {:>8.0} {:>8.1}% {:>8.1}% {:>8.1}%",
            truncate(name, 35),
            s.call_count,
            s.dup_rate() * 100.0,
            s.avg_chars(),
            s.dedup_savings_pct() * 100.0,
            s.encoder_savings_pct() * 100.0,
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

    #[test]
    fn endpoint_stats_empty_corpus_returns_zero_rates() {
        let s = EndpointStats::default();
        assert_eq!(s.dup_rate(), 0.0);
        assert_eq!(s.avg_chars(), 0.0);
        assert_eq!(s.savings_pct(), 0.0);
        assert!(s.dominant_shape().is_none());
    }

    #[test]
    fn endpoint_stats_splits_dedup_vs_encoder_savings() {
        // L0 hit on event 1: baseline=100, final=10 -> dedup_saved=90
        // L2 encode on event 2: baseline=100, final=70 -> encoder_saved=30
        // L3 passthrough on event 3: baseline=100, final=100 -> 0 saved
        let mut s = EndpointStats::default();
        let mut e1 = ev("ep", true, Shape::Prose, 100, 10);
        e1.layer_used = Layer::L0;
        s.update(&e1);
        let mut e2 = ev("ep", false, Shape::ArrayOfObjects, 100, 70);
        e2.layer_used = Layer::L2;
        s.update(&e2);
        let mut e3 = ev("ep", false, Shape::Prose, 100, 100);
        e3.layer_used = Layer::L3;
        s.update(&e3);

        assert_eq!(s.dedup_saved_tokens, 90);
        assert_eq!(s.encoder_saved_tokens, 30);
        assert_eq!(s.total_baseline_tokens, 300);
        // 90/300 = 30%
        assert!((s.dedup_savings_pct() - 0.30).abs() < 1e-6);
        // 30/300 = 10%
        assert!((s.encoder_savings_pct() - 0.10).abs() < 1e-6);
        // total = 1 - 180/300 = 40%
        assert!((s.savings_pct() - 0.40).abs() < 1e-6);
        // Decomposition holds: dedup + encoder = total
        assert!(
            (s.dedup_savings_pct() + s.encoder_savings_pct() - s.savings_pct())
                .abs() < 1e-6
        );
    }

    #[test]
    fn endpoint_stats_dominant_shape_picks_max() {
        let mut s = EndpointStats::default();
        for _ in 0..3 {
            s.update(&ev("x", false, Shape::Prose, 10, 10));
        }
        for _ in 0..7 {
            s.update(&ev("x", false, Shape::MarkdownTable, 10, 10));
        }
        assert_eq!(s.dominant_shape(), Some("MarkdownTable"));
    }

    #[test]
    fn corpus_stats_tracks_sessions_and_compactions() {
        let mut corpus = CorpusStats::default();
        for _ in 0..3 {
            let mut e = ev("x", false, Shape::Prose, 10, 10);
            e.context_partition = 2;
            corpus.update(&e);
        }
        for _ in 0..2 {
            let mut e = ev("x", false, Shape::Prose, 10, 10);
            e.session_hash = "s2".into();
            e.context_partition = 0;
            corpus.update(&e);
        }
        corpus.finalize();
        assert_eq!(corpus.total_sessions, 2);
        assert_eq!(corpus.compaction_events, 3);
        assert_eq!(corpus.total_events, 5);
    }

    #[test]
    fn corpus_savings_pct_zero_when_no_tokens() {
        let corpus = CorpusStats::default();
        assert_eq!(corpus.savings_pct(), 0.0);
    }

    #[test]
    fn rule_r3_raises_lru_under_frequent_compactions() {
        let mut corpus = CorpusStats::default();
        for _ in 0..100 {
            let mut e = ev("x", false, Shape::Prose, 10, 10);
            e.context_partition = 3; // Every event is post-compaction.
            corpus.update(&e);
        }
        corpus.finalize();
        let mut cfg = AdaptiveConfig::default();
        apply_tuning_rules(&mut cfg, &corpus);
        assert_eq!(cfg.dedup.lru_size, 10);
    }

    #[test]
    fn rule_r3_shrinks_lru_on_tiny_sessions() {
        let mut corpus = CorpusStats::default();
        // 5 sessions × 3 events = 15 events, 3 events/session avg — tiny.
        for s in 0..5 {
            for _ in 0..3 {
                let mut e = ev("x", false, Shape::Prose, 10, 10);
                e.session_hash = format!("s{s}");
                corpus.update(&e);
            }
        }
        corpus.finalize();
        let mut cfg = AdaptiveConfig::default();
        apply_tuning_rules(&mut cfg, &corpus);
        assert_eq!(cfg.dedup.lru_size, 3);
    }

    #[test]
    fn rule_r5_clamps_min_body_chars() {
        let mut corpus = CorpusStats::default();
        for _ in 0..30 {
            corpus.update(&ev("x", false, Shape::Prose, 2000, 2000));
        }
        corpus.finalize();
        let mut cfg = AdaptiveConfig::default();
        apply_tuning_rules(&mut cfg, &corpus);
        assert!((100..=500).contains(&cfg.dedup.min_body_chars));
    }

    #[test]
    fn parse_flag_handles_missing_and_present_keys() {
        let args: Vec<String> = vec!["--foo".into(), "bar".into(), "--baz".into(), "qux".into()];
        assert_eq!(parse_flag(&args, "--foo"), Some("bar"));
        assert_eq!(parse_flag(&args, "--baz"), Some("qux"));
        assert_eq!(parse_flag(&args, "--missing"), None);
    }

    #[test]
    fn truncate_returns_short_inputs_unchanged() {
        assert_eq!(truncate("hi", 10), "hi");
        assert_eq!(truncate("", 10), "");
    }

    #[test]
    fn truncate_ellipsises_long_inputs() {
        let out = truncate("abcdefghij", 5);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 5);
    }

    #[test]
    fn default_paths_point_under_home_config_devboy() {
        let inp = default_input_dir();
        let out = default_output();
        // We don't assert the HOME is set; just verify the shape — the path
        // ends with the documented suffix.
        assert!(inp.ends_with(".config/devboy/telemetry/events"));
        assert!(out.ends_with(".config/devboy/pipeline_config.toml"));
    }

    #[test]
    fn print_top_endpoints_does_not_panic_on_empty() {
        // Smoke — prints to stderr; just ensures no division-by-zero or overflow.
        let c = CorpusStats::default();
        print_top_endpoints(&c, 10);
    }

    #[test]
    fn print_top_endpoints_handles_long_names() {
        let mut c = CorpusStats::default();
        let name = "a".repeat(60); // forces truncate() to engage
        let mut e = ev(&name, false, Shape::Prose, 100, 100);
        e.endpoint_class = name.clone();
        for _ in 0..3 {
            c.update(&e);
        }
        c.finalize();
        print_top_endpoints(&c, 5);
    }

    // ─── Integration: exercises the on-disk JSONL scanner ─────────────

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let pid = std::process::id();
            let p = std::env::temp_dir().join(format!("devboy_tune_test_{pid}_{tag}"));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_jsonl(path: &std::path::Path, lines: &[String]) {
        let mut contents = String::new();
        for l in lines {
            contents.push_str(l);
            contents.push('\n');
        }
        std::fs::write(path, contents).unwrap();
    }

    fn event_line(endpoint: &str, dup: bool, tok_final: u32) -> String {
        let mut e = ev(endpoint, dup, Shape::Prose, 100, tok_final);
        e.session_hash = "sess".into();
        serde_json::to_string(&e).unwrap()
    }

    #[test]
    fn scan_jsonl_aggregates_real_file() {
        let dir = TempDir::new("real");
        let lines: Vec<String> = (0..25).map(|i| event_line("ep", i < 10, 50)).collect();
        write_jsonl(&dir.path().join("s1.jsonl"), &lines);

        let mut corpus = CorpusStats::default();
        let read = scan_jsonl_dir(dir.path(), &mut corpus).unwrap();
        assert_eq!(read, 25);
        assert_eq!(corpus.total_events, 25);
        let s = corpus.per_endpoint.get("ep").unwrap();
        assert_eq!(s.call_count, 25);
        assert!((s.dup_rate() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn scan_jsonl_skips_session_summary_wrappers() {
        let dir = TempDir::new("skip_summary");
        let ev_line = event_line("ep", false, 100);
        let summary = r#"{"type":"session_summary","data":{"session_hash":"x","total_events":5}}"#;
        let malformed = r#"{"not_json at all"#;
        let lines = vec![
            ev_line.clone(),
            summary.to_string(),
            malformed.to_string(),
            "".to_string(),
            ev_line.clone(),
        ];
        write_jsonl(&dir.path().join("mixed.jsonl"), &lines);

        let mut corpus = CorpusStats::default();
        let read = scan_jsonl_dir(dir.path(), &mut corpus).unwrap();
        assert_eq!(read, 2);
    }

    #[test]
    fn scan_jsonl_ignores_non_jsonl_files() {
        let dir = TempDir::new("non_jsonl");
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        std::fs::write(dir.path().join("backup.json"), "{}").unwrap();
        write_jsonl(
            &dir.path().join("events.jsonl"),
            &[event_line("a", false, 50)],
        );

        let mut corpus = CorpusStats::default();
        let n = scan_jsonl_dir(dir.path(), &mut corpus).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn scan_jsonl_missing_dir_errors() {
        let mut corpus = CorpusStats::default();
        let res = scan_jsonl_dir(std::path::Path::new("/no/such/path/really"), &mut corpus);
        assert!(res.is_err());
    }

    // ─── Claude-log scanner tests ─────────────────────────────────────

    #[test]
    fn ingest_claude_line_extracts_model_and_tools() {
        let line = r#"{
            "sessionId": "sess1",
            "message": {
                "model": "claude-sonnet-4-6",
                "content": [
                    {"type": "tool_use", "name": "Read", "input": {"file_path": "/x"}},
                    {"type": "tool_use", "name": "mcp__gitlab__get_issues", "input": {}}
                ]
            }
        }"#;
        let mut s = ClaudeLogStats::default();
        assert!(ingest_claude_line(line, &mut s));
        assert_eq!(s.total_events, 1);
        assert_eq!(s.sessions.len(), 1);
        assert_eq!(s.model_counts.get("claude-sonnet-4-6"), Some(&1));
        assert_eq!(s.tool_counts.get("Read"), Some(&1));
        assert_eq!(s.tool_counts.get("mcp__gitlab__get_issues"), Some(&1));
        assert_eq!(s.read_invocations, 1);
        assert_eq!(s.total_invocations, 2);
        assert_eq!(s.endpoints.get("mcp__gitlab__get_issues"), Some(&1));
    }

    #[test]
    fn ingest_claude_line_detects_compact_command() {
        let line = r#"{
            "sessionId": "s1",
            "message": {
                "content": [{"type": "text", "text": "/compact"}]
            }
        }"#;
        let mut s = ClaudeLogStats::default();
        ingest_claude_line(line, &mut s);
        assert_eq!(s.compactions, 1);
    }

    #[test]
    fn ingest_claude_line_handles_malformed_json() {
        let mut s = ClaudeLogStats::default();
        assert!(!ingest_claude_line("not json", &mut s));
        assert_eq!(s.total_events, 0);
    }

    #[test]
    fn claude_log_stats_dominant_model() {
        let mut s = ClaudeLogStats::default();
        for _ in 0..7 {
            *s.model_counts.entry("glm-5.1".into()).or_insert(0) += 1;
        }
        for _ in 0..2 {
            *s.model_counts.entry("gpt-oss:20b".into()).or_insert(0) += 1;
        }
        assert_eq!(s.dominant_model(), Some("glm-5.1"));
    }

    #[test]
    fn apply_profile_rules_pins_dominant_model_when_known() {
        let mut s = ClaudeLogStats::default();
        // 90% glm-5.1, 10% other → above 80% threshold
        for _ in 0..90 {
            *s.model_counts.entry("glm-5.1".into()).or_insert(0) += 1;
        }
        for _ in 0..10 {
            *s.model_counts.entry("other-model".into()).or_insert(0) += 1;
        }
        let mut cfg = AdaptiveConfig::default();
        apply_profile_rules(&mut cfg, &s);
        assert_eq!(cfg.profiles.llm.active, "glm-5.1");
    }

    #[test]
    fn apply_profile_rules_keeps_auto_for_unknown_dominant_model() {
        let mut s = ClaudeLogStats::default();
        for _ in 0..100 {
            *s.model_counts.entry("totally-unknown".into()).or_insert(0) += 1;
        }
        let mut cfg = AdaptiveConfig::default();
        apply_profile_rules(&mut cfg, &s);
        assert_eq!(cfg.profiles.llm.active, "auto");
    }

    #[test]
    fn apply_profile_rules_registers_new_endpoints() {
        let mut s = ClaudeLogStats::default();
        s.endpoints.insert("mcp__brand_new__do_thing".into(), 5);
        let mut cfg = AdaptiveConfig::default();
        let n_before = cfg.profiles.data.variants.len();
        apply_profile_rules(&mut cfg, &s);
        assert!(cfg.profiles.data.variants.len() > n_before);
        // Variant key is the last "__" segment.
        let k = "do_thing";
        assert!(cfg.profiles.data.variants.contains_key(k));
    }

    #[test]
    fn apply_profile_rules_classifies_marathon_session() {
        let mut s = ClaudeLogStats::default();
        s.total_events = 1500;
        s.sessions.insert("s1".into());
        s.compactions = 5;
        s.read_invocations = 100;
        s.total_invocations = 500;
        let mut cfg = AdaptiveConfig::default();
        apply_profile_rules(&mut cfg, &s);
        assert_eq!(cfg.profiles.agent.active, "marathon_refactor");
    }

    #[test]
    fn scan_claude_jsonl_dir_recurses_into_project_subdir() {
        let dir = TempDir::new("claude_recurse");
        let project = dir.path().join("proj-foo");
        std::fs::create_dir_all(&project).unwrap();
        let line = r#"{"sessionId":"s","message":{"model":"glm-5.1","content":[{"type":"tool_use","name":"Read","input":{}}]}}"#;
        std::fs::write(project.join("session1.jsonl"), format!("{line}\n")).unwrap();
        let mut stats = ClaudeLogStats::default();
        let n = scan_claude_jsonl_dir(dir.path(), &mut stats).unwrap();
        assert_eq!(n, 1);
        assert_eq!(stats.read_invocations, 1);
    }

    #[test]
    fn scan_jsonl_tolerates_type_field_without_data_wrapper() {
        // If PipelineEvent later gains a `type` field it must still parse.
        let dir = TempDir::new("future_type");
        let mut e = ev("ep", false, Shape::Prose, 100, 100);
        e.session_hash = "sess".into();
        let mut val = serde_json::to_value(&e).unwrap();
        if let Some(obj) = val.as_object_mut() {
            obj.insert("type".into(), serde_json::json!("pipeline_event"));
        }
        let line = serde_json::to_string(&val).unwrap();
        write_jsonl(&dir.path().join("future.jsonl"), &[line]);

        let mut corpus = CorpusStats::default();
        let n = scan_jsonl_dir(dir.path(), &mut corpus).unwrap();
        assert_eq!(n, 1);
    }
}
