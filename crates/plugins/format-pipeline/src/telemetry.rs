//! Pipeline telemetry — per-response event capture for adaptive tuning.
//!
//! The pipeline emits one [`PipelineEvent`] per tool-result it handles.
//! Events are anonymized by construction: no raw response text, no tool
//! arguments, no user-facing strings leave this module. The schema carries
//! just enough signal to drive the tuning rules described in
//! `docs/research/paper-2-mckp-format-adaptive.md` §Adaptive Configuration.
//!
//! # Design
//!
//! - **Sink trait** — [`TelemetrySink`] abstracts persistence. Default impl
//!   [`JsonlSink`] appends one JSON line per event to a per-session file;
//!   alternate impls can stream to stdout, discard for tests, or forward
//!   to an in-process aggregator.
//! - **Zero-cost when disabled** — sinks are dyn-dispatched via an
//!   `Option<Arc<dyn TelemetrySink>>`; setting `None` eliminates all
//!   per-call allocation.
//! - **Append-only, crash-safe** — JsonlSink opens with `O_APPEND`; no
//!   in-memory buffering beyond the default kernel write buffer.
//! - **Schema additions only** — `PipelineEvent` is `non_exhaustive`;
//!   downstream analyzers project specific fields.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use devboy_format_pipeline::telemetry::{
//!     JsonlSink, PipelineEvent, Shape, Layer, TelemetrySink,
//! };
//!
//! let sink: Arc<dyn TelemetrySink> =
//!     Arc::new(JsonlSink::open("/tmp/devboy-telemetry/sess_abcd.jsonl").unwrap());
//!
//! // PipelineEvent is #[non_exhaustive]; construct via Default + mutation.
//! let mut evt = PipelineEvent::default();
//! evt.session_hash = "abcdef01".into();
//! evt.tool_call_id_hash = "feed1234".into();
//! evt.tool_name_anon = "Read".into();
//! evt.endpoint_class = "Read".into();
//! evt.response_chars = 1234;
//! evt.shape = Shape::NumberedList;
//! evt.content_sha_prefix_hex = "0123456789abcdef".into();
//! evt.file_path_hash = Some("abc12345".into());
//! evt.layer_used = Layer::L3;
//! evt.tokens_baseline = 308;
//! evt.tokens_final = 308;
//! evt.ts_ms = 1_700_000_000_000;
//!
//! sink.record(&evt).unwrap();
//! ```

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum TelemetryError {
    #[error("telemetry I/O: {0}")]
    Io(#[from] io::Error),
    #[error("telemetry serialization: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, TelemetryError>;

/// Structural classification of a tool response.
///
/// Must be kept in sync with `docs/research/scripts/extract_paper2_format_events.py`
/// so offline analyses and online collection share the same taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    Prose,
    /// NumberedList.
    NumberedList,
    /// BulletList.
    BulletList,
    /// CodeBlock.
    CodeBlock,
    /// MarkdownTable.
    MarkdownTable,
    /// NestedObject.
    NestedObject,
    /// FlatObject.
    FlatObject,
    /// ArrayOfObjects.
    ArrayOfObjects,
    /// ArrayOfPrimitives.
    ArrayOfPrimitives,
    Empty,
    #[default]
    Unknown,
}

/// Which pipeline layer produced the terminal decision for this response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Layer {
    /// Content-hash dedup emitted a reference hint.
    L0,
    /// A per-endpoint template was applied.
    L1,
    /// Generic MCKP reformatted the response.
    L2,
    /// Passed through unchanged (text shape, below threshold, or no gain).
    #[default]
    L3,
}

/// Single pipeline decision — emitted once per tool-result.
///
/// See `docs/research/paper-2-mckp-format-adaptive.md` §Telemetry &
/// Observability for field-level documentation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PipelineEvent {
    /// SHA-256 prefix of the session UUID (not the raw UUID).
    pub session_hash: String,
    /// SHA-256 prefix of this tool_use_id — identifies the response for
    /// dedup references without exposing the raw id.
    pub tool_call_id_hash: String,
    /// Anonymized tool name. MCP slugs are hashed (`mcp__p<hash>__verb`).
    pub tool_name_anon: String,
    /// Coarse endpoint classification (e.g. `git_log`, `curl`, or the full
    /// tool_name for single-endpoint tools).
    pub endpoint_class: String,
    /// Raw byte count of the response.
    pub response_chars: u64,
    /// Structural shape of the response.
    pub shape: Shape,
    /// Names of embedded formats detected inside the response (e.g. `diff`,
    /// `log`, `url`, `hash`). Empty when no embedded formats were seen.
    #[serde(default)]
    pub inner_formats: Vec<String>,
    /// Hex-encoded prefix of SHA-256 over the response bytes — 32 hex chars
    /// representing the first 16 bytes (128 bits), matching the paper's
    /// stated fingerprint width and the Python extractor's output.
    pub content_sha_prefix_hex: String,
    /// Anonymized file-path hash for `Read`/`Edit`/`Write`-family tools;
    /// `None` for tools that don't operate on a file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path_hash: Option<String>,
    /// Did the pipeline emit a dedup hint in lieu of full content?
    pub is_dedup_hit: bool,
    /// Terminal layer in the pipeline decision tree.
    pub layer_used: Layer,
    /// L1 template identifier if `layer_used == L1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// Token count before pipeline encoding (baseline).
    pub tokens_baseline: u32,
    /// Token count of what the pipeline emitted.
    pub tokens_final: u32,
    /// Monotonic partition counter; increments on each compaction boundary.
    pub context_partition: u32,
    /// True for subagent (sidechain) tool-results; false for main session.
    pub is_sidechain: bool,
    /// Unix milliseconds at which the response was produced.
    pub ts_ms: i64,
    /// Fraction of events kept when sampling is enabled. `1.0` when every
    /// event is recorded. Consumers of telemetry must scale counts by
    /// `1 / sample_rate_applied`.
    #[serde(default = "default_sample_rate")]
    pub sample_rate_applied: f32,

    // ─── Paper 3 — enricher effectiveness signals ────────────────────
    //
    // The four metrics surface as derived rates in `SessionSummary`
    // and `tune analyze` (P-3-08). Recorded per-event so a tuner
    // session can rebuild the rates without re-running the planner.
    /// True when the planner pre-fetched this tool call (rather than
    /// the LLM emitting it directly). Drives the `prefetch_hit_rate`
    /// metric — paired with `cited_in_next_n_turns` once the post-pass
    /// scanner runs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub enricher_prefetched: bool,

    /// `cost_model.typical_kb`-derived prediction (in tokens) at the
    /// moment the planner admitted the call. Compared with
    /// `tokens_baseline` to compute `cost_overrun_rate`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub enricher_predicted_cost_tokens: u32,

    /// Set on declined candidates that the host emitted anyway as a
    /// telemetry-only event (so `tune analyze` can study what the
    /// planner skipped). One of `"budget"` / `"low_probability"` /
    /// `"preempted"` / `"prereq_missing"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enricher_decline_reason: Option<String>,

    /// Reserved for an offline citation-enrichment post-pass that
    /// re-reads the JSONL log and sets this to `true` when the next
    /// 1–3 LLM messages textually reference any of the response's
    /// `content_sha_prefix_hex` bytes. The live pipeline never sets
    /// this; the post-pass is **not** shipped yet (the existing
    /// `tune from-claude-logs --tools` only seeds `[tools.*]`
    /// defaults, it does not populate citation fields). Stays `None`
    /// until that pass lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_in_next_n_turns: Option<bool>,
}

fn is_false(b: &bool) -> bool {
    !*b
}
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}
fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

fn default_sample_rate() -> f32 {
    1.0
}

/// Session-level roll-up written on session close.
///
/// Separate from per-event sink for two reasons: (1) the summary requires
/// all events to be complete, (2) the summary is a natural unit for the
/// tuner to read without re-scanning JSONL.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_hash: String,
    pub total_events: u64,
    /// Fraction of events where L0 emitted a reference hint.
    pub dedup_hit_rate: f32,
    pub l1_hit_rate: f32,
    pub l2_hit_rate: f32,
    pub avg_response_chars: f32,
    pub compaction_count: u32,
    pub total_baseline_tokens: u64,
    pub total_final_tokens: u64,
    pub savings_pct: f32,
    pub duration_sec: f32,
    pub ended_at_ms: i64,
    /// Fraction of events that were sampled (for scaling counts).
    pub sample_rate_applied: f32,
    /// Paper 3 enricher-effectiveness aggregates. Defaults to all-zero
    /// when no enrichment activity was observed in the session.
    #[serde(default)]
    pub enrichment: EnrichmentEffectiveness,
}

/// Aggregate scoring of how well the Paper 3 enrichment planner served
/// the agent during a session. Populated by the live pipeline (counters)
/// plus the offline post-pass (`cited_*` numbers, see P-3-08).
///
/// Three primary rates the operator reads:
///
/// - **Prefetch hit rate** — fraction of planner-prefetched calls whose
///   content was textually cited by the LLM in the next 1–3 turns. The
///   north-star efficiency number; target ≥ 60%.
/// - **Decline recall loss** — fraction of declined candidates the LLM
///   ended up calling itself within the next 5 turns. Higher means the
///   planner is too greedy. Target ≤ 10%.
/// - **Cost overrun rate** — fraction of admitted calls whose actual
///   `tokens_baseline` exceeded the predicted cost by ≥ 30%. Drives
///   refresh of `cost_model.typical_kb` priors. Target ≤ 15%.
///
/// And the operator-facing ROI counters:
///
/// - **`inference_calls_saved_*`** — number of LLM round-trips the
///   planner short-circuited, broken into three buckets so the
///   contribution of each mechanism stays visible:
///   `prefetch` (cited speculative calls), `dedup` (Paper 2 L0 hits —
///   tool body replaced with a near-ref hint so the LLM never sees the
///   full payload), and `fail_fast` (e.g. ToolSearch self-loop blocked
///   after `fail_fast_after_n`).
/// - **`inference_tokens_saved`** — sum of `tokens_baseline` from those
///   short-circuited calls. The headline "we saved this much context"
///   number for `tune analyze`.
///
/// Token savings vs a no-planner baseline is the roll-up "did the
/// enricher pay for itself" answer; it lives in the corpus-replay
/// validation harness (Paper 3 §Validation strategy), not on this
/// summary, because it requires running the same session both with
/// and without the planner. This struct carries only the per-session
/// counters that drive the three rates above.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EnrichmentEffectiveness {
    /// Number of calls the planner pre-fetched.
    pub total_prefetches: u32,
    /// Of `total_prefetches`, the count whose content was cited by the
    /// LLM in the next 1–3 turns. Filled in by the offline post-pass;
    /// stays `0` until the post-pass has run.
    pub cited_prefetches: u32,
    /// Number of candidates the planner declined for any reason.
    pub total_declines: u32,
    /// Of `total_declines`, the count where the LLM later issued the
    /// declined tool itself within the next 5 turns. Lower-is-better.
    pub late_invoked_after_decline: u32,
    /// Number of admitted calls whose actual `tokens_baseline` exceeded
    /// the planner's prediction by ≥ 30%.
    pub cost_overrun_count: u32,
    /// Total admitted calls (denominator for `cost_overrun_rate`).
    pub total_predictions: u32,
    /// Sum of predicted-vs-actual prediction error in tokens — useful
    /// for diagnosing systematic under- or over-estimation.
    pub net_prediction_error_tokens: i64,

    // ─── Inference round-trip savings ────────────────────────────────
    /// LLM tool-uses avoided because the planner pre-fetched the
    /// content and the model cited it in the next 1–3 turns. Counted
    /// only when [`PipelineEvent::cited_in_next_n_turns`] is `Some(true)`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub inference_calls_saved_prefetch: u32,
    /// LLM tool-uses avoided because L0 dedup replaced the response
    /// with a near-ref hint. Counted on every event with
    /// `is_dedup_hit = true`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub inference_calls_saved_dedup: u32,
    /// LLM tool-uses avoided because [`crate::enrichment`] short-
    /// circuited a `fail_fast_after_n` loop (e.g. ToolSearch returning
    /// 0 bytes twice in a row). Incremented from the planner side via
    /// [`Self::record_fail_fast_skip`].
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub inference_calls_saved_fail_fast: u32,
    /// Sum of baseline tokens from all three saved-call buckets. The
    /// "we saved this much context" headline for `tune analyze`.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub inference_tokens_saved: u64,

    // ─── Speculative-execution race instrumentation ─────────────────
    /// Number of speculative tool-calls the host actually dispatched
    /// out-of-band (a subset of `total_prefetches`: the fraction the
    /// host *successfully scheduled*, not just plans the planner
    /// produced).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub prefetch_dispatched: u32,
    /// Of `prefetch_dispatched`, the count where the prefetch result
    /// landed in the dedup cache *before* the LLM asked for the same
    /// tool, so the LLM's call collapsed to an L0 hit. The other axis
    /// of "did the speculation pay off" — independent of textual
    /// citation.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub prefetch_won_race: u32,
    /// Prefetches the LLM never asked for in the same session. Wasted
    /// API quota / dollars; high values trigger R7's per-tool
    /// auto-disable in `tune analyze`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub prefetch_wasted: u32,
}

impl EnrichmentEffectiveness {
    /// Fraction of prefetches that paid off (cited by the LLM).
    /// Returns `None` when no prefetches happened — distinct from a
    /// 0% hit rate.
    pub fn prefetch_hit_rate(&self) -> Option<f32> {
        (self.total_prefetches > 0)
            .then(|| self.cited_prefetches as f32 / self.total_prefetches as f32)
    }

    /// Fraction of declined candidates the LLM later called anyway.
    pub fn decline_recall_loss(&self) -> Option<f32> {
        (self.total_declines > 0)
            .then(|| self.late_invoked_after_decline as f32 / self.total_declines as f32)
    }

    /// Fraction of admitted calls whose actual baseline exceeded the
    /// prediction by ≥ 30%.
    pub fn cost_overrun_rate(&self) -> Option<f32> {
        (self.total_predictions > 0)
            .then(|| self.cost_overrun_count as f32 / self.total_predictions as f32)
    }

    /// Total LLM tool-uses the planner short-circuited across all three
    /// buckets. The headline "round-trips avoided" number.
    pub fn total_calls_saved(&self) -> u32 {
        self.inference_calls_saved_prefetch
            .saturating_add(self.inference_calls_saved_dedup)
            .saturating_add(self.inference_calls_saved_fail_fast)
    }

    /// Fold one [`PipelineEvent`] into the per-session counters.
    ///
    /// Inspects the four enricher-specific fields plus `is_dedup_hit`
    /// and `tokens_baseline`/`tokens_final` to maintain:
    ///
    /// 1. `total_prefetches` / `total_predictions` / `cost_overrun_*`
    ///    when `enricher_prefetched = true`.
    /// 2. `cited_prefetches` and `inference_calls_saved_prefetch` when
    ///    the offline post-pass has set `cited_in_next_n_turns = Some(true)`.
    /// 3. `total_declines` when `enricher_decline_reason` is set.
    /// 4. `inference_calls_saved_dedup` (and the corresponding
    ///    `inference_tokens_saved`) on every L0 dedup hit.
    ///
    /// Use it to drive `SessionSummary.enrichment` from the live
    /// pipeline or from a JSONL post-pass — same accumulator either way.
    pub fn accumulate(&mut self, ev: &PipelineEvent) {
        if ev.enricher_prefetched {
            self.total_prefetches = self.total_prefetches.saturating_add(1);
            self.total_predictions = self.total_predictions.saturating_add(1);
            let predicted = ev.enricher_predicted_cost_tokens as i64;
            let actual = ev.tokens_baseline as i64;
            self.net_prediction_error_tokens = self
                .net_prediction_error_tokens
                .saturating_add(actual - predicted);
            // Overrun threshold: actual ≥ 130% of predicted, with a
            // non-zero predicted to avoid trivial true on tiny calls.
            if predicted > 0 && actual * 10 >= predicted * 13 {
                self.cost_overrun_count = self.cost_overrun_count.saturating_add(1);
            }
            if matches!(ev.cited_in_next_n_turns, Some(true)) {
                self.cited_prefetches = self.cited_prefetches.saturating_add(1);
                self.inference_calls_saved_prefetch =
                    self.inference_calls_saved_prefetch.saturating_add(1);
                self.inference_tokens_saved = self
                    .inference_tokens_saved
                    .saturating_add(ev.tokens_baseline as u64);
            }
        }
        if ev.is_dedup_hit {
            self.inference_calls_saved_dedup = self.inference_calls_saved_dedup.saturating_add(1);
            // Save the body's *baseline* tokens — the L0 hint replaces
            // the full payload, so the LLM never gets billed for it.
            // `tokens_final` is the hint itself (~9 tokens) and is
            // trivial; the meaningful saving is `tokens_baseline`.
            self.inference_tokens_saved = self
                .inference_tokens_saved
                .saturating_add(ev.tokens_baseline as u64);
        }
        if ev.enricher_decline_reason.is_some() {
            self.total_declines = self.total_declines.saturating_add(1);
        }
    }

    /// Record a `fail_fast_after_n` short-circuit — the planner refused
    /// to issue a tool call (e.g. a third empty `ToolSearch`), so no
    /// `PipelineEvent` is ever emitted for it. Call this from the
    /// planner side to keep `inference_calls_saved_fail_fast` honest.
    ///
    /// `predicted_cost_tokens` is the per-call estimate from the
    /// tool's `cost_model` — added to `inference_tokens_saved` so the
    /// fail-fast contribution shows up in the headline number.
    pub fn record_fail_fast_skip(&mut self, predicted_cost_tokens: u32) {
        self.inference_calls_saved_fail_fast =
            self.inference_calls_saved_fail_fast.saturating_add(1);
        self.inference_tokens_saved = self
            .inference_tokens_saved
            .saturating_add(predicted_cost_tokens as u64);
    }

    /// Record that the host actually dispatched a speculative tool
    /// call (a subset of `total_prefetches`: planner produced a plan
    /// *and* the dispatcher succeeded in scheduling it). Increment
    /// alongside `total_prefetches` from the host side; mismatches
    /// between the two surface as "planner produced more than
    /// dispatcher could schedule" — concurrency cap saturated.
    pub fn record_prefetch_dispatched(&mut self) {
        self.prefetch_dispatched = self.prefetch_dispatched.saturating_add(1);
    }

    /// Record that a dispatched prefetch landed in the dedup cache
    /// before the LLM asked for the same tool, so the LLM's call
    /// collapsed to an L0 hit. Independent of textual citation — the
    /// LLM still issued the tool, but our prefetched body served the
    /// answer at zero added latency.
    pub fn record_prefetch_won_race(&mut self) {
        self.prefetch_won_race = self.prefetch_won_race.saturating_add(1);
    }

    /// Record that a dispatched prefetch was never claimed by the
    /// LLM during the rest of the session (offline post-pass tally).
    /// High `prefetch_wasted / prefetch_dispatched` ratio is the
    /// signal `tune analyze` watches for R7's per-tool auto-disable.
    pub fn record_prefetch_wasted(&mut self) {
        self.prefetch_wasted = self.prefetch_wasted.saturating_add(1);
    }

    /// Fraction of dispatched prefetches that beat the LLM to the
    /// dedup cache. `None` when nothing was dispatched.
    pub fn prefetch_race_win_rate(&self) -> Option<f32> {
        (self.prefetch_dispatched > 0)
            .then(|| self.prefetch_won_race as f32 / self.prefetch_dispatched as f32)
    }

    /// Fraction of dispatched prefetches that were never claimed by
    /// the LLM. `None` when nothing was dispatched. Higher means the
    /// planner's speculation was wasted — drive R7's auto-disable.
    pub fn prefetch_waste_rate(&self) -> Option<f32> {
        (self.prefetch_dispatched > 0)
            .then(|| self.prefetch_wasted as f32 / self.prefetch_dispatched as f32)
    }

    /// Compact one-line summary suitable for `tune analyze` output.
    pub fn report(&self) -> String {
        let hit = self
            .prefetch_hit_rate()
            .map(|r| format!("{:.1}%", r * 100.0))
            .unwrap_or_else(|| "n/a".into());
        let loss = self
            .decline_recall_loss()
            .map(|r| format!("{:.1}%", r * 100.0))
            .unwrap_or_else(|| "n/a".into());
        let overrun = self
            .cost_overrun_rate()
            .map(|r| format!("{:.1}%", r * 100.0))
            .unwrap_or_else(|| "n/a".into());
        let race = self
            .prefetch_race_win_rate()
            .map(|r| format!("{:.1}%", r * 100.0))
            .unwrap_or_else(|| "n/a".into());
        let waste = self
            .prefetch_waste_rate()
            .map(|r| format!("{:.1}%", r * 100.0))
            .unwrap_or_else(|| "n/a".into());
        format!(
            "prefetch_hit={hit} decline_recall_loss={loss} cost_overrun={overrun} \
             race_win={race} waste={waste} \
             calls_saved={saved} (prefetch={pf}, dedup={dd}, fail_fast={ff}) \
             tokens_saved={ts} prefetches={p} dispatched={dp} \
             declines={d} predictions={pr}",
            saved = self.total_calls_saved(),
            pf = self.inference_calls_saved_prefetch,
            dd = self.inference_calls_saved_dedup,
            ff = self.inference_calls_saved_fail_fast,
            ts = self.inference_tokens_saved,
            p = self.total_prefetches,
            dp = self.prefetch_dispatched,
            d = self.total_declines,
            pr = self.total_predictions,
        )
    }
}

/// Persistence backend for telemetry events and summaries.
///
/// Implementations must be safe to call from multiple threads via shared
/// ownership (`Arc<dyn TelemetrySink>`).
pub trait TelemetrySink: Send + Sync {
    /// Append a single per-response event.
    fn record(&self, event: &PipelineEvent) -> Result<()>;

    /// Append the session summary at session close. Default no-op for
    /// sinks that don't distinguish rollups.
    fn record_summary(&self, _summary: &SessionSummary) -> Result<()> {
        Ok(())
    }

    /// Flush any buffered writes to durable storage.
    fn flush(&self) -> Result<()> {
        Ok(())
    }
}

/// Append-only JSONL sink backed by a single file.
///
/// Thread-safe via an interior `Mutex<BufWriter>`. Writes one JSON line
/// per event terminated by `'\n'`.
pub struct JsonlSink {
    path: PathBuf,
    writer: Mutex<BufWriter<File>>,
}

impl JsonlSink {
    /// Open (creating parent dirs as needed) the target path in append mode.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            writer: Mutex::new(BufWriter::new(file)),
        })
    }

    /// Returns the file path this sink writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl TelemetrySink for JsonlSink {
    fn record(&self, event: &PipelineEvent) -> Result<()> {
        let line = serde_json::to_string(event)?;
        let mut w = self.writer.lock().expect("telemetry writer mutex poisoned");
        w.write_all(line.as_bytes())?;
        w.write_all(b"\n")?;
        Ok(())
    }

    fn record_summary(&self, summary: &SessionSummary) -> Result<()> {
        // Summaries share the same stream but with an explicit type marker
        // so analyzers can demultiplex.
        let wrapped = serde_json::json!({
            "type": "session_summary",
            "data": summary,
        });
        let line = serde_json::to_string(&wrapped)?;
        let mut w = self.writer.lock().expect("telemetry writer mutex poisoned");
        w.write_all(line.as_bytes())?;
        w.write_all(b"\n")?;
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        self.writer
            .lock()
            .expect("telemetry writer mutex poisoned")
            .flush()?;
        Ok(())
    }
}

/// No-op sink for tests and for code paths where telemetry is explicitly
/// disabled. Always returns `Ok`; records nothing.
#[derive(Default)]
pub struct NullSink;

impl TelemetrySink for NullSink {
    fn record(&self, _event: &PipelineEvent) -> Result<()> {
        Ok(())
    }
}

/// In-memory sink for unit tests — retains events for assertion.
#[derive(Default)]
pub struct MemorySink {
    events: Mutex<Vec<PipelineEvent>>,
    summaries: Mutex<Vec<SessionSummary>>,
}

impl MemorySink {
    /// New.
    pub fn new() -> Self {
        Self::default()
    }

    /// Events.
    pub fn events(&self) -> Vec<PipelineEvent> {
        self.events.lock().unwrap().clone()
    }

    /// Summaries.
    pub fn summaries(&self) -> Vec<SessionSummary> {
        self.summaries.lock().unwrap().clone()
    }

    /// Len.
    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    /// Is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl TelemetrySink for MemorySink {
    fn record(&self, event: &PipelineEvent) -> Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }

    fn record_summary(&self, summary: &SessionSummary) -> Result<()> {
        self.summaries.lock().unwrap().push(summary.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn sample_event() -> PipelineEvent {
        PipelineEvent {
            session_hash: "sess0001".into(),
            tool_call_id_hash: "tc0001".into(),
            tool_name_anon: "Read".into(),
            endpoint_class: "Read".into(),
            response_chars: 1234,
            shape: Shape::NumberedList,
            inner_formats: vec![],
            content_sha_prefix_hex: "0123456789abcdef".into(),
            file_path_hash: Some("fpath001".into()),
            is_dedup_hit: false,
            layer_used: Layer::L3,
            template_id: None,
            tokens_baseline: 308,
            tokens_final: 308,
            context_partition: 0,
            is_sidechain: false,
            ts_ms: 1_700_000_000_000,
            sample_rate_applied: 1.0,
            enricher_prefetched: false,
            enricher_predicted_cost_tokens: 0,
            enricher_decline_reason: None,
            cited_in_next_n_turns: None,
        }
    }

    #[test]
    fn memory_sink_captures_events() {
        let sink = MemorySink::new();
        let e = sample_event();
        sink.record(&e).unwrap();
        assert_eq!(sink.len(), 1);
        assert_eq!(sink.events()[0].tool_call_id_hash, "tc0001");
    }

    #[test]
    fn null_sink_is_noop() {
        let sink = NullSink;
        let e = sample_event();
        sink.record(&e).unwrap();
        // No API to verify, but must not panic.
    }

    #[test]
    fn jsonl_sink_appends_line() {
        let tmp = tempfile();
        {
            let sink = JsonlSink::open(&tmp).unwrap();
            sink.record(&sample_event()).unwrap();
            sink.flush().unwrap();
        }
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert_eq!(body.lines().count(), 1);
        let deserialized: PipelineEvent = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(deserialized.tokens_baseline, 308);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn jsonl_sink_survives_multiple_writes() {
        let tmp = tempfile();
        {
            let sink = JsonlSink::open(&tmp).unwrap();
            for i in 0..10 {
                let mut e = sample_event();
                e.tokens_baseline = i * 10;
                sink.record(&e).unwrap();
            }
            sink.flush().unwrap();
        }
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert_eq!(body.lines().count(), 10);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn jsonl_sink_supports_summary_tag() {
        let tmp = tempfile();
        {
            let sink = JsonlSink::open(&tmp).unwrap();
            sink.record(&sample_event()).unwrap();
            let summary = SessionSummary {
                session_hash: "sess0001".into(),
                total_events: 10,
                dedup_hit_rate: 0.35,
                savings_pct: 0.35,
                ended_at_ms: 1_700_000_100_000,
                sample_rate_applied: 1.0,
                ..Default::default()
            };
            sink.record_summary(&summary).unwrap();
            sink.flush().unwrap();
        }
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert_eq!(body.lines().count(), 2);
        assert!(body.contains("\"session_summary\""));
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn concurrent_writes_are_serialized() {
        let tmp = tempfile();
        {
            let sink = Arc::new(JsonlSink::open(&tmp).unwrap());
            let mut handles = vec![];
            for i in 0..8 {
                let sink = Arc::clone(&sink);
                handles.push(thread::spawn(move || {
                    let mut e = sample_event();
                    e.tool_call_id_hash = format!("tc{i:04}");
                    for _ in 0..25 {
                        sink.record(&e).unwrap();
                    }
                }));
            }
            for h in handles {
                h.join().unwrap();
            }
            sink.flush().unwrap();
        }
        let body = std::fs::read_to_string(&tmp).unwrap();
        // 8 threads × 25 events = 200 lines, each a valid JSON object.
        assert_eq!(body.lines().count(), 200);
        for line in body.lines() {
            let _: PipelineEvent = serde_json::from_str(line).unwrap();
        }
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn schema_is_forward_compatible() {
        // Verify that future-addition of fields (via #[non_exhaustive] +
        // Default + serde defaults) doesn't break parsing.
        let legacy = r#"{
            "session_hash": "s",
            "tool_call_id_hash": "t",
            "tool_name_anon": "Read",
            "endpoint_class": "Read",
            "response_chars": 0,
            "shape": "prose",
            "content_sha_prefix_hex": "",
            "is_dedup_hit": false,
            "layer_used": "L3",
            "tokens_baseline": 0,
            "tokens_final": 0,
            "context_partition": 0,
            "is_sidechain": false,
            "ts_ms": 0
        }"#;
        let parsed: PipelineEvent = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.sample_rate_applied, 1.0); // default applied
        assert!(parsed.inner_formats.is_empty());
        assert!(parsed.file_path_hash.is_none());
    }

    /// Cheap per-test unique path without pulling in the `tempfile` crate.
    fn tempfile() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("devboy_tele_test_{pid}_{n}.jsonl"))
    }

    #[test]
    fn memory_sink_accessors() {
        let sink = MemorySink::new();
        assert!(sink.is_empty());
        assert_eq!(sink.len(), 0);
        sink.record(&sample_event()).unwrap();
        assert!(!sink.is_empty());
        assert_eq!(sink.len(), 1);
        // flush() is a no-op on MemorySink
        sink.flush().unwrap();
    }

    #[test]
    fn memory_sink_captures_summaries() {
        let sink = MemorySink::new();
        let summary = SessionSummary {
            session_hash: "abcd".into(),
            total_events: 7,
            savings_pct: 0.33,
            ..Default::default()
        };
        sink.record_summary(&summary).unwrap();
        assert_eq!(sink.summaries().len(), 1);
        assert_eq!(sink.summaries()[0].total_events, 7);
    }

    #[test]
    fn jsonl_sink_path_getter() {
        let tmp = tempfile();
        let sink = JsonlSink::open(&tmp).unwrap();
        assert_eq!(sink.path(), tmp.as_path());
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn jsonl_sink_creates_parent_dirs() {
        let parent =
            std::env::temp_dir().join(format!("devboy_tele_nested_{}", std::process::id()));
        let tmp = parent.join("deep/sub/events.jsonl");
        assert!(!tmp.parent().unwrap().exists());
        let sink = JsonlSink::open(&tmp).unwrap();
        sink.record(&sample_event()).unwrap();
        sink.flush().unwrap();
        assert!(tmp.exists());
        std::fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn shape_and_layer_defaults() {
        assert_eq!(Shape::default(), Shape::Unknown);
        assert_eq!(Layer::default(), Layer::L3);
    }

    #[test]
    fn shape_serde_snake_case() {
        let j = serde_json::to_string(&Shape::MarkdownTable).unwrap();
        assert_eq!(j, "\"markdown_table\"");
        let parsed: Shape = serde_json::from_str("\"array_of_objects\"").unwrap();
        assert_eq!(parsed, Shape::ArrayOfObjects);
    }

    #[test]
    fn null_sink_flush_is_noop() {
        let sink = NullSink;
        sink.flush().unwrap();
    }

    #[test]
    fn telemetry_error_display() {
        let io_err = TelemetryError::Io(std::io::Error::other("boom"));
        let msg = format!("{io_err}");
        assert!(msg.contains("telemetry"));
    }

    // ─── Paper 3 EnrichmentEffectiveness ────────────────────────────

    #[test]
    fn enrichment_rates_are_none_when_no_activity() {
        let e = EnrichmentEffectiveness::default();
        assert!(e.prefetch_hit_rate().is_none());
        assert!(e.decline_recall_loss().is_none());
        assert!(e.cost_overrun_rate().is_none());
        assert!(e.report().contains("n/a"));
    }

    #[test]
    fn prefetch_hit_rate_handles_zero_and_partial_hits() {
        let mut e = EnrichmentEffectiveness {
            total_prefetches: 10,
            cited_prefetches: 7,
            ..Default::default()
        };
        assert_eq!(e.prefetch_hit_rate(), Some(0.7));
        e.cited_prefetches = 0;
        assert_eq!(e.prefetch_hit_rate(), Some(0.0));
    }

    #[test]
    fn decline_recall_loss_metric() {
        let e = EnrichmentEffectiveness {
            total_declines: 20,
            late_invoked_after_decline: 3,
            ..Default::default()
        };
        let rate = e.decline_recall_loss().unwrap();
        assert!((rate - 0.15).abs() < 1e-6);
    }

    #[test]
    fn cost_overrun_rate_metric() {
        let e = EnrichmentEffectiveness {
            total_predictions: 100,
            cost_overrun_count: 12,
            ..Default::default()
        };
        let rate = e.cost_overrun_rate().unwrap();
        assert!((rate - 0.12).abs() < 1e-6);
    }

    #[test]
    fn report_format_is_human_readable() {
        let e = EnrichmentEffectiveness {
            total_prefetches: 10,
            cited_prefetches: 7,
            total_declines: 20,
            late_invoked_after_decline: 2,
            cost_overrun_count: 3,
            total_predictions: 30,
            ..Default::default()
        };
        let r = e.report();
        assert!(r.contains("70.0%"), "expected prefetch_hit=70.0%, got {r}");
        assert!(
            r.contains("10.0%"),
            "expected decline_recall_loss=10.0%, got {r}"
        );
        assert!(r.contains("10.0%"), "expected cost_overrun=10.0%, got {r}");
    }

    #[test]
    fn pipeline_event_skips_default_enricher_fields_on_serialise() {
        let evt = sample_event();
        let json = serde_json::to_string(&evt).unwrap();
        // Default values for enricher fields must be skip_serializing_if'd
        // so older log files stay compact and parse cleanly.
        assert!(!json.contains("enricher_prefetched"));
        assert!(!json.contains("enricher_predicted_cost_tokens"));
        assert!(!json.contains("enricher_decline_reason"));
        assert!(!json.contains("cited_in_next_n_turns"));
    }

    #[test]
    fn pipeline_event_round_trips_with_enricher_fields_populated() {
        let mut evt = sample_event();
        evt.enricher_prefetched = true;
        evt.enricher_predicted_cost_tokens = 540;
        evt.enricher_decline_reason = Some("budget".into());
        evt.cited_in_next_n_turns = Some(true);
        let json = serde_json::to_string(&evt).unwrap();
        let back: PipelineEvent = serde_json::from_str(&json).unwrap();
        assert!(back.enricher_prefetched);
        assert_eq!(back.enricher_predicted_cost_tokens, 540);
        assert_eq!(back.enricher_decline_reason.as_deref(), Some("budget"));
        assert_eq!(back.cited_in_next_n_turns, Some(true));
    }

    // ─── Inference tool-call savings ─────────────────────────────────

    #[test]
    fn total_calls_saved_sums_three_buckets() {
        let e = EnrichmentEffectiveness {
            inference_calls_saved_prefetch: 7,
            inference_calls_saved_dedup: 12,
            inference_calls_saved_fail_fast: 3,
            ..Default::default()
        };
        assert_eq!(e.total_calls_saved(), 22);
    }

    #[test]
    fn accumulate_dedup_hit_increments_dedup_bucket_and_tokens() {
        let mut e = EnrichmentEffectiveness::default();
        let mut ev = sample_event();
        ev.is_dedup_hit = true;
        ev.tokens_baseline = 800;
        ev.tokens_final = 9;
        e.accumulate(&ev);
        assert_eq!(e.inference_calls_saved_dedup, 1);
        assert_eq!(e.inference_tokens_saved, 800);
        assert_eq!(e.total_calls_saved(), 1);
        // dedup-only path must not move prefetch counters.
        assert_eq!(e.total_prefetches, 0);
        assert_eq!(e.total_predictions, 0);
    }

    #[test]
    fn accumulate_cited_prefetch_increments_prefetch_bucket() {
        let mut e = EnrichmentEffectiveness::default();
        let mut ev = sample_event();
        ev.enricher_prefetched = true;
        ev.enricher_predicted_cost_tokens = 500;
        ev.tokens_baseline = 540;
        ev.cited_in_next_n_turns = Some(true);
        e.accumulate(&ev);
        assert_eq!(e.total_prefetches, 1);
        assert_eq!(e.cited_prefetches, 1);
        assert_eq!(e.inference_calls_saved_prefetch, 1);
        assert_eq!(e.inference_tokens_saved, 540);
        assert_eq!(e.cost_overrun_count, 0); // 540 < 500 * 1.3
    }

    #[test]
    fn accumulate_uncited_prefetch_does_not_count_as_saved() {
        let mut e = EnrichmentEffectiveness::default();
        let mut ev = sample_event();
        ev.enricher_prefetched = true;
        ev.cited_in_next_n_turns = Some(false);
        ev.tokens_baseline = 200;
        e.accumulate(&ev);
        assert_eq!(e.total_prefetches, 1);
        assert_eq!(e.cited_prefetches, 0);
        assert_eq!(e.inference_calls_saved_prefetch, 0);
        assert_eq!(e.inference_tokens_saved, 0);
    }

    #[test]
    fn accumulate_overrun_counts_when_actual_exceeds_130_percent() {
        let mut e = EnrichmentEffectiveness::default();
        let mut ev = sample_event();
        ev.enricher_prefetched = true;
        ev.enricher_predicted_cost_tokens = 100;
        ev.tokens_baseline = 200; // 200 ≥ 100 * 1.3 → overrun
        e.accumulate(&ev);
        assert_eq!(e.cost_overrun_count, 1);
        assert_eq!(e.net_prediction_error_tokens, 100);
    }

    #[test]
    fn accumulate_decline_reason_increments_declines() {
        let mut e = EnrichmentEffectiveness::default();
        let mut ev = sample_event();
        ev.enricher_decline_reason = Some("budget".into());
        e.accumulate(&ev);
        assert_eq!(e.total_declines, 1);
    }

    #[test]
    fn record_fail_fast_skip_increments_counter_and_tokens() {
        let mut e = EnrichmentEffectiveness::default();
        e.record_fail_fast_skip(75);
        e.record_fail_fast_skip(75);
        assert_eq!(e.inference_calls_saved_fail_fast, 2);
        assert_eq!(e.inference_tokens_saved, 150);
        assert_eq!(e.total_calls_saved(), 2);
    }

    #[test]
    fn report_includes_calls_saved_and_tokens_saved() {
        let e = EnrichmentEffectiveness {
            total_prefetches: 10,
            cited_prefetches: 7,
            inference_calls_saved_prefetch: 7,
            inference_calls_saved_dedup: 12,
            inference_calls_saved_fail_fast: 3,
            inference_tokens_saved: 12_345,
            ..Default::default()
        };
        let r = e.report();
        assert!(r.contains("calls_saved=22"), "report missing total: {r}");
        assert!(
            r.contains("prefetch=7") && r.contains("dedup=12") && r.contains("fail_fast=3"),
            "report missing per-bucket breakdown: {r}"
        );
        assert!(
            r.contains("tokens_saved=12345"),
            "report missing tokens_saved: {r}"
        );
    }

    #[test]
    fn enrichment_skips_zero_savings_fields_on_serialise() {
        let e = EnrichmentEffectiveness::default();
        let json = serde_json::to_string(&e).unwrap();
        assert!(!json.contains("inference_calls_saved_prefetch"));
        assert!(!json.contains("inference_calls_saved_dedup"));
        assert!(!json.contains("inference_calls_saved_fail_fast"));
        assert!(!json.contains("inference_tokens_saved"));
    }

    #[test]
    fn enrichment_round_trips_with_savings_populated() {
        let e = EnrichmentEffectiveness {
            inference_calls_saved_prefetch: 4,
            inference_calls_saved_dedup: 9,
            inference_calls_saved_fail_fast: 2,
            inference_tokens_saved: 8_400,
            ..Default::default()
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: EnrichmentEffectiveness = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
    }

    // ─── Speculative-execution race instrumentation ─────────────────

    #[test]
    fn record_prefetch_dispatched_increments_counter() {
        let mut e = EnrichmentEffectiveness::default();
        e.record_prefetch_dispatched();
        e.record_prefetch_dispatched();
        e.record_prefetch_dispatched();
        assert_eq!(e.prefetch_dispatched, 3);
    }

    #[test]
    fn race_win_rate_returns_some_only_when_dispatched() {
        let e0 = EnrichmentEffectiveness::default();
        assert!(e0.prefetch_race_win_rate().is_none());
        let e = EnrichmentEffectiveness {
            prefetch_dispatched: 10,
            prefetch_won_race: 7,
            ..Default::default()
        };
        let rate = e.prefetch_race_win_rate().unwrap();
        assert!((rate - 0.7).abs() < 1e-6);
    }

    #[test]
    fn waste_rate_separates_dispatched_from_total_prefetches() {
        // Distinct from `prefetch_hit_rate`: hit_rate is "did the LLM
        // text-cite the prefetched body", waste_rate is "did the LLM
        // never call the same tool at all". Different denominators.
        let e = EnrichmentEffectiveness {
            total_prefetches: 12,
            prefetch_dispatched: 10, // 2 plans the dispatcher dropped
            prefetch_wasted: 4,      // 4 of 10 dispatched never claimed
            ..Default::default()
        };
        let rate = e.prefetch_waste_rate().unwrap();
        assert!((rate - 0.4).abs() < 1e-6);
        assert!(e.prefetch_race_win_rate().unwrap().abs() < 1e-6); // 0/10 = 0
    }

    #[test]
    fn report_includes_race_and_waste_when_dispatched() {
        let e = EnrichmentEffectiveness {
            total_prefetches: 10,
            prefetch_dispatched: 10,
            prefetch_won_race: 6,
            prefetch_wasted: 2,
            ..Default::default()
        };
        let r = e.report();
        assert!(r.contains("race_win=60.0%"), "report missing race_win: {r}");
        assert!(r.contains("waste=20.0%"), "report missing waste: {r}");
        assert!(
            r.contains("dispatched=10"),
            "report missing dispatched: {r}"
        );
    }

    #[test]
    fn race_fields_skip_serialise_when_zero() {
        let e = EnrichmentEffectiveness::default();
        let json = serde_json::to_string(&e).unwrap();
        assert!(!json.contains("prefetch_dispatched"));
        assert!(!json.contains("prefetch_won_race"));
        assert!(!json.contains("prefetch_wasted"));
    }

    #[test]
    fn race_fields_round_trip_when_populated() {
        let e = EnrichmentEffectiveness {
            prefetch_dispatched: 12,
            prefetch_won_race: 8,
            prefetch_wasted: 3,
            ..Default::default()
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: EnrichmentEffectiveness = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
    }
}
