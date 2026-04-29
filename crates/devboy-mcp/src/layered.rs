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
use devboy_format_pipeline::enrichment::{PlannerOptions, TurnContext, build_plan};
use devboy_format_pipeline::layered_pipeline::{LayeredPipeline, ToolResponseInput};
use devboy_format_pipeline::projection::{extract_args, extract_host};
use devboy_format_pipeline::telemetry::{EnrichmentEffectiveness, JsonlSink, Layer, TelemetrySink};

use crate::protocol::{ToolCallParams, ToolCallResult, ToolResultContent};
use crate::speculation::{
    PrefetchDispatcher, PrefetchOutcome, PrefetchRequest, SkipReason, SpeculationEngine,
};

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
    /// Speculative-execution engine. `None` until the host wires a
    /// dispatcher via [`Self::with_speculation`]; the engine is
    /// always live afterwards but only schedules tasks when
    /// `config.enrichment.enabled = true`. Wrapped in
    /// `tokio::sync::Mutex` because dispatch / wait are async.
    speculation: Arc<tokio::sync::Mutex<Option<SpeculationEngine>>>,
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
    pub fn new(mut config: AdaptiveConfig) -> Self {
        // Paper 3 — seed shipped ToolValueModel defaults so every
        // session starts with calibrated cost/value priors for the
        // top-15 corpus tools. User-set entries (loaded from TOML)
        // win — `or_insert` skips keys already populated.
        let defaults = devboy_format_pipeline::tool_defaults::default_tool_value_models();
        for (name, model) in defaults {
            config.tools.entry(name).or_insert(model);
        }

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
            speculation: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Attach a speculative-execution dispatcher. The host calls this
    /// once at startup with a [`PrefetchDispatcher`] that bridges to
    /// its own `tools/call` path. After this, [`Self::speculate_after`]
    /// schedules out-of-band prefetches when the planner finds high-
    /// probability follow-ups.
    pub async fn with_speculation(self, dispatcher: Arc<dyn PrefetchDispatcher>) -> Self {
        let engine = SpeculationEngine::new(self.config.enrichment.clone(), dispatcher);
        *self.speculation.lock().await = Some(engine);
        self
    }

    /// Best-effort drop hook: on session close, abort every still-
    /// pending speculative task. The async-aware version of `Drop`
    /// (Rust's sync `Drop` only sends an abort signal; this method
    /// also drains the JoinSet so the runtime sees the cancellation
    /// before we return).
    pub async fn shutdown(&self) {
        if let Some(engine) = self.speculation.lock().await.as_mut() {
            engine.shutdown().await;
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

    /// Run the Paper 3 planner against the response that just landed
    /// for `tool_name`, dispatch every safe (`Pure` / `ReadOnly`)
    /// follow-up out-of-band, and wait up to
    /// `enrichment.prefetch_timeout_ms` for them to complete.
    ///
    /// Settled prefetches land in the dedup cache so the LLM's next
    /// `tools/call` for the same `tool_name+args` collapses to an L0
    /// hit. Tasks still pending past the timeout keep running and
    /// land later on the same session — never blocking the main
    /// response path.
    ///
    /// Returns a short hint string the host can append to the LLM's
    /// response so the model knows what context arrived early. Empty
    /// when the planner had nothing to schedule (or speculation is
    /// disabled).
    ///
    /// **Speculation is disabled** when:
    ///
    /// - `config.enrichment.enabled = false` (default), or
    /// - no [`PrefetchDispatcher`] has been attached via
    ///   [`Self::with_speculation`].
    ///
    /// In both cases this method is a cheap no-op and returns `""`.
    pub async fn speculate_after(
        &self,
        tool_name: &str,
        prev_response_json: &serde_json::Value,
    ) -> String {
        // Cheap exits when speculation is off — no plan, no dispatch.
        if !self.config.enrichment.enabled {
            return String::new();
        }
        let mut engine_guard = self.speculation.lock().await;
        let Some(engine) = engine_guard.as_mut() else {
            return String::new();
        };
        if !engine.is_enabled() {
            return String::new();
        }

        // First, sweep up any prefetches that finished AFTER the
        // previous turn's `wait_within` timed out. Without this drain
        // the cache loses every late-arrival result and the
        // `prefetch_won_race` metric is permanently zero.
        for outcome in engine.drain_pending().await {
            if let PrefetchOutcome::Settled {
                tool,
                args,
                body,
                predicted_cost_tokens,
            } = outcome
            {
                self.write_prefetch_to_cache(&tool, &args, &body, predicted_cost_tokens);
            }
        }

        // Build the planner's TurnContext from the recent-tools window.
        // The planner reads `recent_tools` to drive `follow_up` lookup;
        // we hand it a fresh snapshot each turn.
        let recent = self.recent_tools_snapshot();
        let ctx = TurnContext::new(&recent, self.config.enrichment.prefetch_budget_tokens);
        // Use a slightly lower probability floor than the default 0.5
        // — corpus mining showed valuable read chains (Glob → Read at
        // 0.32, Grep → Read at 0.35) sit below the default. The host
        // gates speculation entirely via `enrichment.enabled` and the
        // `is_speculatable` filter, so loosening the prob threshold
        // here is safe.
        let opts = PlannerOptions {
            min_followup_probability: 0.3,
            ..PlannerOptions::default()
        };
        let plan = build_plan(&self.config, &ctx, opts);

        // Filter to candidates that are *safe to speculate* and have
        // resolvable args. The planner's `follow_up` graph already
        // includes mutating tools as informational hints — the
        // `is_speculatable()` gate drops them so we never re-issue an
        // Edit/Write/create_*.
        let mut requests: Vec<PrefetchRequest> = Vec::new();
        for call in &plan.calls {
            let Some(model) = self.config.effective_tool_value_model(&call.tool) else {
                continue;
            };
            if !model.is_speculatable() {
                continue;
            }
            // Find the FollowUpLink that produced this candidate so we
            // can recover its `projection` / `projection_arg`. Cheap
            // because every recent_tool's follow_up list is small.
            let Some(link) = self
                .config
                .effective_tool_value_model(tool_name)
                .and_then(|m| m.follow_up.iter().find(|l| l.tool == call.tool))
            else {
                continue;
            };
            let arg_objects = extract_args(tool_name, prev_response_json, link);
            if arg_objects.is_empty() {
                continue;
            }
            for args in arg_objects {
                let host = static_or_url_host(&args, model.rate_limit_host.as_deref());
                requests.push(PrefetchRequest {
                    call: call.clone(),
                    args,
                    rate_limit_host: host,
                });
            }
        }

        if requests.is_empty() {
            return String::new();
        }

        // Record dispatched / total predictions; counters move *here*
        // not in the engine so we honour the contract from
        // EnrichmentEffectiveness::record_prefetch_dispatched.
        let total_to_dispatch = requests.len() as u32;
        let skips = engine.dispatch(requests).await;
        let dispatched = total_to_dispatch.saturating_sub(skips.len() as u32);
        if let Ok(mut e) = self.enrichment.lock() {
            for _ in 0..dispatched {
                e.total_prefetches = e.total_prefetches.saturating_add(1);
                e.record_prefetch_dispatched();
            }
            // Skipped requests still count against the planner's
            // accounting (they were planned, just rate-limited away).
            for s in &skips {
                if let PrefetchOutcome::Skipped { reason, .. } = s {
                    let label = match reason {
                        SkipReason::HostSaturated => "host_saturated",
                        SkipReason::MaxParallelReached => "max_parallel_reached",
                        SkipReason::NotSpeculatable => "not_speculatable",
                    };
                    tracing::debug!(
                        target: "devboy_mcp::speculation",
                        "prefetch skipped: {label}"
                    );
                }
            }
        }

        // Wait inside the prefetch budget. Settled bodies go straight
        // into the dedup cache so the LLM's eventual call collapses
        // to L0; failures are logged + counted as wasted.
        let outcomes = engine.wait_within().await;
        let mut hint_parts: Vec<String> = Vec::new();
        for o in outcomes {
            match o {
                PrefetchOutcome::Settled {
                    tool,
                    args,
                    body,
                    predicted_cost_tokens,
                } => {
                    self.write_prefetch_to_cache(&tool, &args, &body, predicted_cost_tokens);
                    hint_parts.push(format!("{tool}({})", short_args(&args)));
                }
                PrefetchOutcome::Failed { tool, error } => {
                    tracing::warn!(
                        target: "devboy_mcp::speculation",
                        "prefetch failed for {tool}: {error}"
                    );
                    if let Ok(mut e) = self.enrichment.lock() {
                        e.record_prefetch_wasted();
                    }
                }
                PrefetchOutcome::Skipped { .. } => {}
            }
        }

        if hint_parts.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n> [enrichment: pre-fetched {} in background — call as usual, results served from cache]",
                hint_parts.join(", ")
            )
        }
    }

    /// Push a settled prefetch body into the dedup cache so the LLM's
    /// future `tools/call` for the same content collapses to an L0
    /// hint. Best-effort: cache failures are logged and discarded.
    ///
    /// `predicted_cost_tokens` is the planner's estimate at admit
    /// time — feeds the `cost_overrun_rate` metric.
    fn write_prefetch_to_cache(
        &self,
        tool: &str,
        args: &serde_json::Value,
        body: &str,
        predicted_cost_tokens: u32,
    ) {
        let Ok(mut p) = self.inner.lock() else {
            return;
        };
        let request_id = format!(
            "prefetch_{}_{}",
            tool,
            // Cheap fingerprint of the args so different prefetches
            // for the same tool don't share a tool_call_id_hash slot.
            short_args_hash(args)
        );
        let path = args.get("file_path").and_then(|v| v.as_str());
        let input = ToolResponseInput {
            tool_call_id: &request_id,
            tool_name: tool,
            file_path: path,
            content: body,
            is_sidechain: false,
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            // Tag the synthesised event so the JSONL post-pass and
            // EnrichmentEffectiveness::accumulate can attribute
            // citations correctly.
            enricher_prefetched: true,
            enricher_predicted_cost_tokens: predicted_cost_tokens,
        };
        // Reuse the regular path so the dedup cache state stays
        // consistent with main-flow inserts.
        let _out = p.process(input);
    }
}

/// Pull the rate-limit host for a single prefetch. Tries the runtime
/// URL first (so WebFetch / generic HTTP wrappers resolve correctly),
/// then falls back to the static `ToolValueModel.rate_limit_host`.
fn static_or_url_host(args: &serde_json::Value, static_host: Option<&str>) -> Option<String> {
    if let Some(url) = args.get("url").and_then(|v| v.as_str())
        && let Some(h) = extract_host(url)
    {
        return Some(h);
    }
    static_host.map(String::from)
}

/// Compact one-line stringified args for the LLM hint (full JSON
/// would be noisy). Returns the first string field's value if any,
/// else `""`. Bounded to ~40 chars.
fn short_args(args: &serde_json::Value) -> String {
    let Some(obj) = args.as_object() else {
        return String::new();
    };
    for (_, v) in obj {
        if let Some(s) = v.as_str() {
            let mut t = s.to_string();
            if t.len() > 40 {
                t.truncate(40);
                t.push('…');
            }
            return t;
        }
    }
    String::new()
}

/// Stable short fingerprint for `args` — used to namespace prefetched
/// entries in the dedup cache so two prefetches for the same tool but
/// different args don't collide. Just a hex DJB2 — collisions are
/// fine, the dedup cache uses content-hash for actual uniqueness.
fn short_args_hash(args: &serde_json::Value) -> String {
    let s = args.to_string();
    let mut h: u64 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u64);
    }
    format!("{h:08x}")
}

impl SessionPipeline {
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
                        // Main-flow call (LLM-emitted) — defaults stay 0/false.
                        enricher_prefetched: false,
                        enricher_predicted_cost_tokens: 0,
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

    // ─── Speculation end-to-end ───────────────────────────────────────

    use crate::speculation::{PrefetchDispatcher, PrefetchError};
    use async_trait::async_trait;
    use serde_json::Value;

    /// Mock dispatcher that returns a canned body per tool. Does not
    /// touch real MCP transport.
    struct MapDispatcher {
        bodies: std::collections::HashMap<String, String>,
        delay_ms: u64,
    }

    #[async_trait]
    impl PrefetchDispatcher for MapDispatcher {
        async fn dispatch(
            &self,
            tool: &str,
            _args: serde_json::Value,
        ) -> Result<String, PrefetchError> {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            self.bodies
                .get(tool)
                .cloned()
                .ok_or_else(|| PrefetchError::Rejected(format!("no body for {tool}")))
        }
    }

    fn enrichment_on_config() -> AdaptiveConfig {
        let mut cfg = AdaptiveConfig {
            tools: devboy_format_pipeline::tool_defaults::default_tool_value_models(),
            ..AdaptiveConfig::default()
        };
        cfg.enrichment.enabled = true;
        cfg.enrichment.prefetch_timeout_ms = 500;
        cfg.enrichment.max_parallel_prefetches = 3;
        // Speculation pre-fetch budget needs to clear Read.cost (~640
        // tokens) so the test fixture isn't dominated by budget gating.
        cfg.enrichment.prefetch_budget_tokens = 4_000;
        cfg
    }

    #[tokio::test]
    async fn speculate_after_dispatches_glob_to_read_chain() {
        let cfg = enrichment_on_config();
        let mut bodies = std::collections::HashMap::new();
        bodies.insert("Read".into(), "long body of file/main.rs ".repeat(40));
        let dispatcher = Arc::new(MapDispatcher {
            bodies,
            delay_ms: 5,
        });
        let pipeline = SessionPipeline::new(cfg).with_speculation(dispatcher).await;

        // Step 1: Glob result lands first — register it in recent_tools.
        let glob_body = "src/main.rs\nsrc/lib.rs\nsrc/api.rs\n";
        let _ = pipeline.process(
            "req_1",
            &ToolCallParams {
                name: "Glob".to_string(),
                arguments: Some(json!({"pattern": "src/**/*.rs"})),
            },
            ToolCallResult::text(glob_body.to_string()),
            0,
        );

        // Step 2: trigger speculation. Glob's follow_up has Read with
        // probability 0.32 — the planner picks it up via projection
        // arg `file_path`.
        let prev_response = Value::String(glob_body.to_string());
        let hint = pipeline.speculate_after("Glob", &prev_response).await;

        let snap = pipeline.enrichment_snapshot();
        // At least one prefetch must have been scheduled and observed.
        assert!(
            snap.total_prefetches > 0,
            "expected total_prefetches > 0, got {snap:?}"
        );
        assert!(
            snap.prefetch_dispatched > 0,
            "expected prefetch_dispatched > 0, got {snap:?}"
        );
        // Hint must mention Read — proves we appended user-visible
        // text after dispatch.
        assert!(
            hint.contains("Read"),
            "expected Read in hint, got: {hint:?}"
        );
        pipeline.shutdown().await;
    }

    #[tokio::test]
    async fn speculate_after_is_noop_when_disabled() {
        // enrichment.enabled = false (default) — no dispatch at all.
        let pipeline = SessionPipeline::new(AdaptiveConfig {
            tools: devboy_format_pipeline::tool_defaults::default_tool_value_models(),
            ..AdaptiveConfig::default()
        });
        let _ = pipeline.process(
            "req_1",
            &ToolCallParams {
                name: "Glob".to_string(),
                arguments: Some(json!({"pattern": "src/**/*.rs"})),
            },
            ToolCallResult::text("src/main.rs\n".into()),
            0,
        );
        let hint = pipeline
            .speculate_after("Glob", &Value::String("src/main.rs\n".into()))
            .await;
        assert!(hint.is_empty(), "speculation must be silent when disabled");
        let snap = pipeline.enrichment_snapshot();
        assert_eq!(snap.total_prefetches, 0);
        assert_eq!(snap.prefetch_dispatched, 0);
    }

    #[tokio::test]
    async fn prefetched_call_emits_telemetry_event_tagged_correctly() {
        // S5: ensure the JSONL sink captures the synthetic event with
        // enricher_prefetched=true and enricher_predicted_cost_tokens>0
        // so the offline post-pass can attribute citations.
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = enrichment_on_config();
        cfg.telemetry.enabled = true;
        cfg.telemetry.path = Some(tmp.path().to_string_lossy().into_owned());
        cfg.telemetry.flush_every_n = 1;

        let mut bodies = std::collections::HashMap::new();
        // Body must clear min_body_chars (200) for the dedup cache to
        // touch it; otherwise the L0 path skips telemetry too.
        bodies.insert("Read".into(), "fn main() {}\n".repeat(40));
        let dispatcher = Arc::new(MapDispatcher {
            bodies,
            delay_ms: 5,
        });
        let pipeline = SessionPipeline::new(cfg).with_speculation(dispatcher).await;

        // Glob result → triggers Read prefetch
        let glob_body = "src/main.rs\n";
        let _ = pipeline.process(
            "req_1",
            &ToolCallParams {
                name: "Glob".to_string(),
                arguments: Some(json!({"pattern": "src/**/*.rs"})),
            },
            ToolCallResult::text(glob_body.into()),
            0,
        );
        let _hint = pipeline
            .speculate_after("Glob", &Value::String(glob_body.into()))
            .await;
        pipeline.shutdown().await;

        // Drop the pipeline to flush the JsonlSink BufWriter.
        drop(pipeline);

        // Find the JSONL file and confirm one of the events carries
        // the prefetched flag.
        let mut prefetched_event_lines: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(tmp.path()).unwrap() {
            let entry = entry.unwrap();
            if entry.path().extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            for line in std::fs::read_to_string(entry.path()).unwrap().lines() {
                if line.contains("\"enricher_prefetched\":true") {
                    prefetched_event_lines.push(line.into());
                }
            }
        }

        assert!(
            !prefetched_event_lines.is_empty(),
            "expected at least one event tagged enricher_prefetched=true"
        );
        // The captured event must also carry a non-zero predicted cost.
        assert!(
            prefetched_event_lines
                .iter()
                .any(|l| l.contains("\"enricher_predicted_cost_tokens\":")),
            "expected enricher_predicted_cost_tokens to be set in the event JSON"
        );
    }

    #[tokio::test]
    async fn shutdown_drains_pending_speculation() {
        let mut cfg = enrichment_on_config();
        // Tiny timeout — guarantees the prefetch is still in flight
        // when we shutdown.
        cfg.enrichment.prefetch_timeout_ms = 1;
        let mut bodies = std::collections::HashMap::new();
        bodies.insert("Read".into(), "any body".into());
        let dispatcher = Arc::new(MapDispatcher {
            bodies,
            delay_ms: 200, // longer than prefetch_timeout_ms
        });
        let pipeline = SessionPipeline::new(cfg).with_speculation(dispatcher).await;
        let _ = pipeline.process(
            "req_1",
            &ToolCallParams {
                name: "Glob".to_string(),
                arguments: Some(json!({"pattern": "x"})),
            },
            ToolCallResult::text("src/main.rs\n".into()),
            0,
        );
        let _hint = pipeline
            .speculate_after("Glob", &Value::String("src/main.rs\n".into()))
            .await;
        // Prefetch was dispatched but timed out — still pending.
        // shutdown() must abort it cleanly without panic.
        pipeline.shutdown().await;
        // Idempotent — safe to call twice.
        pipeline.shutdown().await;
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
