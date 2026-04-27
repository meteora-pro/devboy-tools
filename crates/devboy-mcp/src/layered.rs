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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use devboy_format_pipeline::adaptive_config::AdaptiveConfig;
use devboy_format_pipeline::layered_pipeline::{LayeredPipeline, ToolResponseInput};
use devboy_format_pipeline::telemetry::{JsonlSink, Layer, TelemetrySink};

use crate::protocol::{ToolCallParams, ToolCallResult, ToolResultContent};

/// Per-session pipeline handle. Cloneable; holds an `Arc` to the inner
/// `LayeredPipeline`.
#[derive(Clone)]
pub struct SessionPipeline {
    inner: Arc<Mutex<LayeredPipeline>>,
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

        for c in result.content {
            match c {
                ToolResultContent::Text { text } => {
                    let input = ToolResponseInput {
                        tool_call_id: request_id,
                        tool_name: &params.name,
                        file_path: file_path.as_deref(),
                        content: &text,
                        is_sidechain: false,
                        ts_ms,
                    };
                    let out = p.process(input);
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
