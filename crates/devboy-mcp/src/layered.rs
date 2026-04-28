//! Per-session layered-pipeline state for the MCP server.
//!
//! Wraps a [`devboy_format_pipeline::LayeredPipeline`] in
//! `Arc<Mutex<…>>` so it can sit in `McpServer` (which takes `&self` in
//! handlers) and still be advanced through the L0 dedup cache. The
//! pipeline is created once per server process and persists across all
//! `tools/call` requests on that connection.
//!
//! Wiring contract:
//!
//! - On every successful `tools/call`, the server invokes
//!   [`SessionPipeline::process`] with the raw response text. A hint is
//!   returned when the L0 cache fires, otherwise the unmodified body
//!   passes through (L1/L2 encoders are typed-domain and live in
//!   `devboy-format-pipeline::Pipeline`; this hot path covers
//!   *cross-turn* dedup only).
//! - Mutating tools (`Edit` / `Write` / `MultiEdit` / `NotebookEdit`)
//!   call [`SessionPipeline::invalidate_file`] before the cache is
//!   consulted on the next `Read`, ensuring the agent sees fresh
//!   contents after an edit.
//! - On `/compact` (host-side compaction), the host calls
//!   [`SessionPipeline::on_compaction_boundary`] to advance the
//!   partition counter and drop entries that would otherwise outlive
//!   the cache window.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use devboy_format_pipeline::adaptive_config::AdaptiveConfig;
use devboy_format_pipeline::layered_pipeline::{LayeredPipeline, ToolResponseInput};
use devboy_format_pipeline::telemetry::{EnrichmentEffectiveness, JsonlSink, Layer, TelemetrySink};

use crate::protocol::{ToolCallParams, ToolCallResult, ToolResultContent};

/// Maximum number of recent tool names retained for the Paper 3
/// planner's `follow_up` lookup. 16 covers a "find → fix → verify"
/// loop comfortably; older calls fall out FIFO.
const RECENT_TOOLS_WINDOW: usize = 16;

/// Bytes below which a response counts as "empty" for fail-fast
/// streak tracking. Picked at 8 to absorb pure whitespace / a single
/// `[]` or `{}` envelope without arming the circuit on real-but-tiny
/// answers.
const FAIL_FAST_EMPTY_THRESHOLD_BYTES: usize = 8;

/// Per-session pipeline handle. Cloneable; holds an `Arc` to the inner
/// `LayeredPipeline` plus Paper 3 enricher state (recent-tools window,
/// effectiveness counters, fail-fast circuit).
#[derive(Clone)]
pub struct SessionPipeline {
    inner: Arc<Mutex<LayeredPipeline>>,
    config: Arc<AdaptiveConfig>,
    /// FIFO buffer of tool names invoked on this session — feeds the
    /// Paper 3 planner's `follow_up` lookup. Anonymisation is not
    /// applied (see `ToolValueModel` "Naming contract").
    recent_tools: Arc<Mutex<VecDeque<String>>>,
    /// Live aggregate of planner effectiveness for this session.
    enrichment: Arc<Mutex<EnrichmentEffectiveness>>,
    /// Per-tool count of consecutive empty responses, drives
    /// `fail_fast_after_n` in `[tools.<name>]`. Reset on the first
    /// non-empty response.
    fail_fast_streak: Arc<Mutex<BTreeMap<String, u32>>>,
}

impl SessionPipeline {
    /// Create a new pipeline for the current MCP server process. The
    /// session id is derived from the process id so multiple concurrent
    /// `devboy mcp` instances do not collide in shared telemetry.
    ///
    /// When `config.telemetry.enabled` is `true`, a [`JsonlSink`] is
    /// opened at `<config.telemetry.path | ~/.devboy/telemetry>/<session>.jsonl`
    /// and attached to the pipeline. Failures to open the sink (missing
    /// permissions, etc.) are logged at WARN level and degrade to a
    /// no-op telemetry — they never fail the server start-up.
    pub fn new(config: AdaptiveConfig) -> Self {
        let session_id = format!("mcp_{}", std::process::id());
        let mut pipeline = LayeredPipeline::new(session_id.clone(), config.clone());

        if config.telemetry.enabled
            && let Some(path) = resolve_telemetry_path(&config, &session_id)
        {
            match JsonlSink::open(&path) {
                Ok(sink) => {
                    let arc: Arc<dyn TelemetrySink> = Arc::new(sink);
                    pipeline = pipeline.with_telemetry(arc);
                    tracing::info!(target: "devboy_mcp::telemetry", "telemetry sink opened at {}", path.display());
                }
                Err(e) => {
                    tracing::warn!(
                        target: "devboy_mcp::telemetry",
                        "telemetry sink at {} failed to open: {e} — running without telemetry",
                        path.display()
                    );
                }
            }
        }

        Self {
            inner: Arc::new(Mutex::new(pipeline)),
            config: Arc::new(config),
            recent_tools: Arc::new(Mutex::new(VecDeque::with_capacity(RECENT_TOOLS_WINDOW))),
            enrichment: Arc::new(Mutex::new(EnrichmentEffectiveness::default())),
            fail_fast_streak: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Snapshot of the Paper 3 enrichment counters so far in this
    /// session. Cheap (clone of `EnrichmentEffectiveness`); intended
    /// for `tools/list` debug output, end-of-session summary, or live
    /// status reporting.
    pub fn enrichment_snapshot(&self) -> EnrichmentEffectiveness {
        self.enrichment
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Snapshot of recent tool names (oldest first). Used by the host
    /// when it builds a `TurnContext` for `EnrichmentPlanner::build_plan`.
    pub fn recent_tools_snapshot(&self) -> Vec<String> {
        self.recent_tools
            .lock()
            .map(|g| g.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Returns `true` when the planner's fail-fast circuit is armed for
    /// `tool_name` — the host should refuse to dispatch the call and
    /// emit a short hint instead. Armed iff:
    /// 1. `[tools.<tool_name>].fail_fast_after_n = Some(n)`, and
    /// 2. the last `n` consecutive responses for that tool were "empty"
    ///    (≤ `FAIL_FAST_EMPTY_THRESHOLD_BYTES`).
    ///
    /// `EnrichmentEffectiveness` is **not** updated here — the host is
    /// expected to call [`Self::record_fail_fast_skip`] once it has
    /// actually skipped the dispatch, so the saved-call counters stay
    /// honest if the host opts to override the recommendation.
    pub fn should_skip(&self, tool_name: &str) -> bool {
        let Some(model) = self.config.effective_tool_value_model(tool_name) else {
            return false;
        };
        let Some(threshold) = model.fail_fast_after_n else {
            return false;
        };
        let streak = self
            .fail_fast_streak
            .lock()
            .ok()
            .and_then(|g| g.get(tool_name).copied())
            .unwrap_or(0);
        streak >= threshold
    }

    /// Notify the aggregator that the host actually short-circuited a
    /// call this turn (the host saw `should_skip` return `true` and
    /// honoured it). `predicted_cost_tokens` should come from the
    /// tool's `cost_model.typical_kb` so the saved-token count stays
    /// proportional to the call we avoided.
    pub fn record_fail_fast_skip(&self, predicted_cost_tokens: u32) {
        if let Ok(mut e) = self.enrichment.lock() {
            e.record_fail_fast_skip(predicted_cost_tokens);
        }
    }

    /// Notify the pipeline that the host compacted its context. Drops
    /// dedup entries from prior partitions on the next eviction sweep.
    pub fn on_compaction_boundary(&self) {
        if let Ok(mut p) = self.inner.lock() {
            p.on_compaction_boundary();
        }
    }

    /// Invalidate all cache entries pointing at `file_path`. Called by
    /// the server before a mutating tool (`Edit`/`Write`/...) is
    /// dispatched so that a subsequent `Read` of the same file does
    /// not return a stale `> [ref: …]` hint.
    pub fn invalidate_file(&self, file_path: &str) {
        if let Ok(mut p) = self.inner.lock() {
            p.invalidate_file(file_path);
        }
    }

    /// Process a single tool-call response through L0 dedup. When the
    /// L0 layer emits a reference hint (`> [ref: tc_42, byte-identical]`
    /// or its terse / verbose variant), the input `ToolCallResult` is
    /// rewritten to carry the hint instead of the original body. Other
    /// layer outcomes pass the original result through unchanged —
    /// L1/L2 encoders for typed-domain responses live in `Pipeline`.
    pub fn process(
        &self,
        request_id: &str,
        params: &ToolCallParams,
        result: ToolCallResult,
        ts_ms: i64,
    ) -> ToolCallResult {
        // Errors must never be deduped — a stale hint instead of a real
        // error message would silently break the agent's recovery loop.
        if result.is_error == Some(true) {
            return result;
        }

        let file_path = extract_file_path(params.arguments.as_ref());

        let mut new_content: Vec<ToolResultContent> = Vec::with_capacity(result.content.len());
        let mut p = match self.inner.lock() {
            Ok(g) => g,
            // A poisoned mutex means an earlier panic — best-effort fall
            // back to passing the response through unmodified.
            Err(_) => return result,
        };

        // Track per-call totals so we can update Paper 3 counters once,
        // not per content piece.
        let mut total_dedup_hits: u32 = 0;
        let mut total_dedup_tokens_saved: u64 = 0;
        let mut max_original_chars: usize = 0;

        for c in result.content {
            match c {
                ToolResultContent::Text { text } => {
                    max_original_chars = max_original_chars.max(text.len());
                    let input = ToolResponseInput {
                        tool_call_id: request_id,
                        tool_name: &params.name,
                        file_path: file_path.as_deref(),
                        content: &text,
                        is_sidechain: false,
                        ts_ms,
                    };
                    let out = p.process(input);
                    if matches!(out.layer, Layer::L0) {
                        total_dedup_hits = total_dedup_hits.saturating_add(1);
                        // `tokens_saved` is `tokens_baseline - tokens_final`
                        // — the body the LLM never had to spend context on.
                        if out.tokens_saved > 0 {
                            total_dedup_tokens_saved =
                                total_dedup_tokens_saved.saturating_add(out.tokens_saved as u64);
                        }
                    }
                    // Only rewrite when L0 fired — other layers do not
                    // operate on opaque text content from arbitrary
                    // upstream tools (the typed-domain L1/L2 path goes
                    // through `Pipeline::transform_*`).
                    let body = if matches!(out.layer, Layer::L0) {
                        out.output
                    } else {
                        text
                    };
                    new_content.push(ToolResultContent::Text { text: body });
                }
            }
        }

        // Drop the pipeline mutex before grabbing the Paper 3 mutexes —
        // we never hold both at once, which keeps deadlock impossible
        // even if a future caller decides to lock them in any order.
        drop(p);

        // Paper 3: update enrichment counters + recent-tools window +
        // fail-fast streak. All non-fatal — a poisoned mutex skips the
        // update but never breaks the response.
        if total_dedup_hits > 0
            && let Ok(mut e) = self.enrichment.lock()
        {
            e.inference_calls_saved_dedup = e
                .inference_calls_saved_dedup
                .saturating_add(total_dedup_hits);
            e.inference_tokens_saved = e
                .inference_tokens_saved
                .saturating_add(total_dedup_tokens_saved);
        }

        if let Ok(mut streak) = self.fail_fast_streak.lock() {
            let entry = streak.entry(params.name.clone()).or_insert(0);
            if max_original_chars <= FAIL_FAST_EMPTY_THRESHOLD_BYTES {
                *entry = entry.saturating_add(1);
            } else {
                *entry = 0;
            }
        }

        if let Ok(mut recent) = self.recent_tools.lock() {
            if recent.len() >= RECENT_TOOLS_WINDOW {
                recent.pop_front();
            }
            recent.push_back(params.name.clone());
        }

        ToolCallResult {
            content: new_content,
            is_error: result.is_error,
        }
    }
}

/// Pull `file_path` / `path` / `notebook_path` out of a tool call's
/// arguments. Tools not in the file-operating family produce `None`.
pub fn extract_file_path(args: Option<&serde_json::Value>) -> Option<String> {
    let obj = args?.as_object()?;
    for k in ["file_path", "path", "notebook_path"] {
        if let Some(v) = obj.get(k).and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
    }
    None
}

/// True iff `name` is a mutating file-operating tool. Server uses this
/// to fire a cache invalidation before the tool is dispatched.
pub fn is_mutating_tool(name: &str) -> bool {
    matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit")
}

/// Resolve the JSONL sink target for a session. Honours
/// `telemetry.path`, then `$DEVBOY_TELEMETRY_DIR`, then
/// `$HOME/.devboy/telemetry/`, then `$TMPDIR/.devboy-telemetry/`.
fn resolve_telemetry_path(config: &AdaptiveConfig, session_id: &str) -> Option<PathBuf> {
    let dir: PathBuf = if let Some(p) = config.telemetry.path.as_deref() {
        Path::new(p).to_path_buf()
    } else if let Ok(env_dir) = std::env::var("DEVBOY_TELEMETRY_DIR") {
        PathBuf::from(env_dir)
    } else if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        home.join(".devboy").join("telemetry")
    } else {
        std::env::temp_dir().join(".devboy-telemetry")
    };
    Some(dir.join(format!("{session_id}.jsonl")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ToolCallParams, ToolCallResult, ToolResultContent};
    use serde_json::json;

    fn read_params(path: &str) -> ToolCallParams {
        ToolCallParams {
            name: "Read".to_string(),
            arguments: Some(json!({"file_path": path})),
        }
    }

    fn long_text(seed: &str) -> String {
        // Body must clear the 200-byte min_body_chars default to be
        // eligible for dedup.
        format!("{}{}", seed, "x".repeat(400))
    }

    #[test]
    fn second_identical_read_emits_reference_hint() {
        let pipeline = SessionPipeline::new(AdaptiveConfig::default());
        let body = long_text("file-A:");
        let r1 = pipeline.process(
            "req_1",
            &read_params("/tmp/a.rs"),
            ToolCallResult::text(body.clone()),
            0,
        );
        let r2 = pipeline.process(
            "req_2",
            &read_params("/tmp/a.rs"),
            ToolCallResult::text(body.clone()),
            10,
        );
        // First call returns the body unchanged.
        let ToolResultContent::Text { text: t1 } = &r1.content[0];
        assert_eq!(t1, &body);
        // Second call returns a hint (much shorter, contains `[ref:`).
        let ToolResultContent::Text { text: t2 } = &r2.content[0];
        assert!(t2.len() < body.len() / 2, "expected hint, got `{t2}`");
        assert!(
            t2.contains("[ref:") || t2.contains("[ref "),
            "expected reference hint, got `{t2}`"
        );
    }

    #[test]
    fn edit_invalidation_busts_cache() {
        let pipeline = SessionPipeline::new(AdaptiveConfig::default());
        let body = long_text("file-B:");
        let _ = pipeline.process(
            "req_1",
            &read_params("/tmp/b.rs"),
            ToolCallResult::text(body.clone()),
            0,
        );
        // Mutating tool fires its invalidation hook.
        pipeline.invalidate_file("/tmp/b.rs");
        // A subsequent identical read must come back fresh, not as a hint.
        let r3 = pipeline.process(
            "req_3",
            &read_params("/tmp/b.rs"),
            ToolCallResult::text(body.clone()),
            10,
        );
        let ToolResultContent::Text { text: t3 } = &r3.content[0];
        assert_eq!(t3, &body, "expected fresh body after invalidation");
    }

    #[test]
    fn errors_are_never_deduped() {
        let pipeline = SessionPipeline::new(AdaptiveConfig::default());
        let body = long_text("err:");
        let _ = pipeline.process(
            "req_1",
            &read_params("/tmp/c.rs"),
            ToolCallResult::text(body.clone()),
            0,
        );
        let mut err = ToolCallResult::text(body.clone());
        err.is_error = Some(true);
        let r2 = pipeline.process("req_2", &read_params("/tmp/c.rs"), err, 10);
        let ToolResultContent::Text { text: t2 } = &r2.content[0];
        assert_eq!(t2, &body, "errors must pass through untouched");
    }

    #[test]
    fn telemetry_disabled_by_default_writes_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = AdaptiveConfig::default();
        cfg.telemetry.path = Some(tmp.path().to_string_lossy().into_owned());
        // enabled stays false (the default)
        let pipeline = SessionPipeline::new(cfg);
        let body = long_text("file-T:");
        let _ = pipeline.process(
            "req_1",
            &read_params("/tmp/t.rs"),
            ToolCallResult::text(body),
            0,
        );
        // Default is `enabled = false` → directory must remain empty.
        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "telemetry must be silent until explicitly enabled, found {entries:?}"
        );
    }

    #[test]
    fn telemetry_enabled_creates_jsonl_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = AdaptiveConfig::default();
        cfg.telemetry.enabled = true;
        cfg.telemetry.path = Some(tmp.path().to_string_lossy().into_owned());
        // Flush after every event so the file is non-empty when we read it.
        cfg.telemetry.flush_every_n = 1;
        let pipeline = SessionPipeline::new(cfg);
        let body = long_text("file-U:");
        let _ = pipeline.process(
            "req_1",
            &read_params("/tmp/u.rs"),
            ToolCallResult::text(body),
            0,
        );
        let mut found = false;
        for entry in std::fs::read_dir(tmp.path()).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().and_then(|s| s.to_str()) == Some("jsonl") {
                let contents = std::fs::read_to_string(entry.path()).unwrap();
                assert!(
                    contents.contains("\"endpoint_class\":\"Read\""),
                    "expected Read event in JSONL, got {contents}"
                );
                found = true;
                break;
            }
        }
        assert!(
            found,
            "expected at least one .jsonl file in {:?}",
            tmp.path()
        );
    }

    // ─── Paper 3 enrichment wiring ────────────────────────────────────

    fn pipeline_with_fail_fast_on(tool: &str, threshold: u32) -> SessionPipeline {
        let mut cfg = AdaptiveConfig::default();
        let model = devboy_core::ToolValueModel {
            fail_fast_after_n: Some(threshold),
            ..devboy_core::ToolValueModel::default()
        };
        cfg.tools.insert(tool.to_string(), model);
        SessionPipeline::new(cfg)
    }

    fn empty_params(name: &str) -> ToolCallParams {
        ToolCallParams {
            name: name.to_string(),
            arguments: None,
        }
    }

    #[test]
    fn dedup_hit_increments_inference_calls_saved_dedup() {
        let pipeline = SessionPipeline::new(AdaptiveConfig::default());
        let body = long_text("file-D:");
        let _ = pipeline.process(
            "req_1",
            &read_params("/tmp/d.rs"),
            ToolCallResult::text(body.clone()),
            0,
        );
        let pre = pipeline.enrichment_snapshot();
        assert_eq!(pre.inference_calls_saved_dedup, 0);

        // Second identical Read fires L0 → counter must move.
        let _ = pipeline.process(
            "req_2",
            &read_params("/tmp/d.rs"),
            ToolCallResult::text(body),
            10,
        );
        let post = pipeline.enrichment_snapshot();
        assert_eq!(post.inference_calls_saved_dedup, 1);
        assert!(
            post.inference_tokens_saved > 0,
            "tokens_saved must be > 0 after a real L0 dedup, got {}",
            post.inference_tokens_saved
        );
        assert_eq!(post.total_calls_saved(), 1);
    }

    #[test]
    fn recent_tools_window_records_calls_in_order() {
        let pipeline = SessionPipeline::new(AdaptiveConfig::default());
        for (i, name) in ["Glob", "Grep", "Read"].iter().enumerate() {
            let _ = pipeline.process(
                &format!("req_{i}"),
                &ToolCallParams {
                    name: (*name).to_string(),
                    arguments: None,
                },
                ToolCallResult::text(format!("body-{i}")),
                i as i64,
            );
        }
        assert_eq!(
            pipeline.recent_tools_snapshot(),
            vec!["Glob".to_string(), "Grep".into(), "Read".into()]
        );
    }

    #[test]
    fn fail_fast_arms_after_n_consecutive_empty_responses() {
        // Tool with fail_fast_after_n = 2: arms on the 2nd empty response.
        let pipeline = pipeline_with_fail_fast_on("ToolSearch", 2);
        assert!(!pipeline.should_skip("ToolSearch"), "fresh streak");

        // 1st empty — streak = 1, not yet armed.
        let _ = pipeline.process(
            "req_1",
            &empty_params("ToolSearch"),
            ToolCallResult::text(String::new()),
            0,
        );
        assert!(!pipeline.should_skip("ToolSearch"));

        // 2nd empty — streak = 2, threshold met.
        let _ = pipeline.process(
            "req_2",
            &empty_params("ToolSearch"),
            ToolCallResult::text(String::new()),
            10,
        );
        assert!(pipeline.should_skip("ToolSearch"));

        // Tool without fail_fast_after_n must never arm, however many
        // empty responses it produces.
        for i in 0..5 {
            let _ = pipeline.process(
                &format!("rd_{i}"),
                &empty_params("Read"),
                ToolCallResult::text(String::new()),
                100 + i,
            );
        }
        assert!(!pipeline.should_skip("Read"));
    }

    #[test]
    fn fail_fast_streak_resets_on_non_empty_response() {
        let pipeline = pipeline_with_fail_fast_on("ToolSearch", 2);
        let _ = pipeline.process(
            "req_1",
            &empty_params("ToolSearch"),
            ToolCallResult::text(String::new()),
            0,
        );
        // Non-empty response must clear the streak.
        let _ = pipeline.process(
            "req_2",
            &empty_params("ToolSearch"),
            ToolCallResult::text("a real result".to_string()),
            10,
        );
        let _ = pipeline.process(
            "req_3",
            &empty_params("ToolSearch"),
            ToolCallResult::text(String::new()),
            20,
        );
        // Streak is now 1 (not 3) — circuit must NOT be armed.
        assert!(!pipeline.should_skip("ToolSearch"));
    }

    #[test]
    fn record_fail_fast_skip_updates_aggregator() {
        let pipeline = pipeline_with_fail_fast_on("ToolSearch", 2);
        pipeline.record_fail_fast_skip(40);
        pipeline.record_fail_fast_skip(40);
        let s = pipeline.enrichment_snapshot();
        assert_eq!(s.inference_calls_saved_fail_fast, 2);
        assert_eq!(s.inference_tokens_saved, 80);
    }

    #[test]
    fn extract_file_path_handles_three_argument_names() {
        assert_eq!(
            extract_file_path(Some(&json!({"file_path": "/x"}))),
            Some("/x".into())
        );
        assert_eq!(
            extract_file_path(Some(&json!({"path": "/y"}))),
            Some("/y".into())
        );
        assert_eq!(
            extract_file_path(Some(&json!({"notebook_path": "/z"}))),
            Some("/z".into())
        );
        assert_eq!(extract_file_path(Some(&json!({"unrelated": "x"}))), None);
        assert_eq!(extract_file_path(None), None);
    }
}
