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
    NumberedList,
    BulletList,
    CodeBlock,
    MarkdownTable,
    NestedObject,
    FlatObject,
    ArrayOfObjects,
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
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<PipelineEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn summaries(&self) -> Vec<SessionSummary> {
        self.summaries.lock().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

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
        let parent = std::env::temp_dir().join(format!("devboy_tele_nested_{}", std::process::id()));
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
}
