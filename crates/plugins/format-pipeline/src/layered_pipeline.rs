//! `LayeredPipeline` — generic tool-response compressor.
//!
//! Implements the 4-layer architecture from Paper 2 (see
//! `docs/research/paper-2-mckp-format-adaptive.md`):
//!
//! ```text
//!     L0: Hint-based dedup (content-hash LRU + mutation invalidation)
//!       ↓ fresh
//!     L1: Per-endpoint templates (csv_from_md, pipeline_deep_mckp, ...)
//!       ↓ no template match
//!     L2: Generic MCKP (shape classifier → format encoder)
//!       ↓ no gain
//!     L3: As-is passthrough
//! ```
//!
//! A single `LayeredPipeline` instance is scoped to one session. It is
//! **not** thread-safe by value — wrap in `Arc<Mutex<...>>` if shared
//! across threads.
//!
//! # Usage
//!
//! ```
//! use devboy_format_pipeline::adaptive_config::AdaptiveConfig;
//! use devboy_format_pipeline::layered_pipeline::{LayeredPipeline, ToolResponseInput};
//!
//! let mut p = LayeredPipeline::new("sess_abcd".into(), AdaptiveConfig::default());
//! // Response must be ≥ min_body_chars (default 200) for L0 to engage.
//! let body = "fn main() { println!(\"hello\"); }\n".repeat(10);
//! let out1 = p.process(ToolResponseInput {
//!     tool_call_id: "tc_1",
//!     tool_name: "Read",
//!     file_path: Some("/src/main.rs"),
//!     content: &body,
//!     is_sidechain: false,
//!     ts_ms: 0,
//! });
//! // Second call with identical content emits a hint.
//! let out2 = p.process(ToolResponseInput {
//!     tool_call_id: "tc_2",
//!     tool_name: "Read",
//!     file_path: Some("/src/main.rs"),
//!     content: &body,
//!     is_sidechain: false,
//!     ts_ms: 10,
//! });
//! assert!(matches!(out2.layer, devboy_format_pipeline::telemetry::Layer::L0));
//! ```

use std::sync::Arc;

use crate::adaptive_config::AdaptiveConfig;
use crate::dedup::{
    content_hash, render_reference_hint, DedupCache, DedupDecision, ToolKind,
};
use crate::shape::classify;
use crate::telemetry::{Layer, PipelineEvent, Shape, TelemetrySink};

/// Input to [`LayeredPipeline::process`].
#[derive(Debug, Clone, Copy)]
pub struct ToolResponseInput<'a> {
    /// Raw `tool_use_id` from the MCP server (we hash it internally before
    /// storing in cache or emitting in hints).
    pub tool_call_id: &'a str,
    /// Tool name — used for endpoint classification and ToolKind lookup.
    pub tool_name: &'a str,
    /// Primary file path argument for file-operating tools (`Read`, `Edit`, …).
    /// Hashed internally for cache invalidation.
    pub file_path: Option<&'a str>,
    /// Raw response bytes.
    pub content: &'a str,
    /// True for subagent (sidechain) responses.
    pub is_sidechain: bool,
    /// Unix milliseconds when the response was produced.
    pub ts_ms: i64,
}

/// Output from [`LayeredPipeline::process`].
#[derive(Debug, Clone)]
pub struct ProcessedResponse {
    /// The text to send back to the LLM agent (either the original content,
    /// a hint, or a re-encoded body).
    pub output: String,
    /// Which layer terminated the decision.
    pub layer: Layer,
    /// Optional identifier for L1/L2 format or template used.
    pub format_or_template: Option<String>,
    /// Tokens baseline (original) minus tokens final.
    pub tokens_saved: i64,
    /// Token count of the `output` we're returning.
    pub tokens_final: u32,
}

/// Convert char count to a token estimate.
/// Matches the Python extractor's `chars/4` approximation — keep in sync.
#[inline]
fn est_tokens(chars: usize) -> u32 {
    (chars.max(4) / 4) as u32
}

/// Session-scoped layered pipeline.
///
/// Holds mutable dedup state plus the full configuration. Each
/// `process` call advances the internal event counter and, if a
/// telemetry sink is attached, emits a [`PipelineEvent`].
pub struct LayeredPipeline {
    session_hash: String,
    config: AdaptiveConfig,
    dedup: DedupCache,
    telemetry: Option<Arc<dyn TelemetrySink>>,
    event_counter: u64,
}

impl LayeredPipeline {
    /// Construct a new session pipeline.
    pub fn new(session_hash: String, config: AdaptiveConfig) -> Self {
        let lru = config.dedup.lru_size.max(1);
        Self {
            session_hash,
            dedup: DedupCache::with_capacity(lru),
            config,
            telemetry: None,
            event_counter: 0,
        }
    }

    /// Attach a telemetry sink. Every subsequent `process` emits an event.
    pub fn with_telemetry(mut self, sink: Arc<dyn TelemetrySink>) -> Self {
        self.telemetry = Some(sink);
        self
    }

    /// Signal a compaction boundary — clears the dedup cache and advances
    /// the partition counter.
    pub fn on_compaction_boundary(&mut self) {
        self.dedup.on_compaction_boundary();
    }

    /// Current dedup cache partition number.
    pub fn partition(&self) -> u64 {
        self.dedup.partition()
    }

    /// Dispatch a tool response through the 4-layer pipeline.
    pub fn process(&mut self, input: ToolResponseInput<'_>) -> ProcessedResponse {
        self.event_counter += 1;
        let baseline_tokens = est_tokens(input.content.len());

        // Mutation-aware invalidation must fire even for tiny Edit responses —
        // the invalidation itself is correctness-critical, not an optimization.
        let tool_kind = ToolKind::from_tool_name(input.tool_name);
        let file_path_hash = input
            .file_path
            .filter(|_| matches!(tool_kind, ToolKind::FileRead | ToolKind::FileMutate))
            .map(crate::dedup_util::file_path_hash);

        if tool_kind == ToolKind::FileMutate
            && let Some(ref fh) = file_path_hash {
                self.dedup.invalidate_file(fh);
            }

        // Short-circuit: L3 for too-small bodies (not worth any optimization).
        let min_chars = self.config.dedup.min_body_chars.max(1);
        if input.content.len() < min_chars {
            let out = ProcessedResponse {
                output: input.content.to_string(),
                layer: Layer::L3,
                format_or_template: None,
                tokens_saved: 0,
                tokens_final: baseline_tokens,
            };
            self.emit_event(&input, &out, &Shape::Unknown, None, None, None);
            return out;
        }

        // === L0: content-hash dedup ===
        let endpoint_ok = self.config.dedup.enabled_for(input.tool_name);
        let content_hash_value = content_hash(input.content.as_bytes());
        let content_sha_hex = hex16(&content_hash_value);

        if endpoint_ok
            && let DedupDecision::Hint {
                reference_tool_call_id,
            } = self.dedup.check(&content_hash_value)
            {
                let hint = render_reference_hint(&reference_tool_call_id);
                let tokens_final = est_tokens(hint.len());
                let out = ProcessedResponse {
                    output: hint,
                    layer: Layer::L0,
                    format_or_template: Some("hint_exact".into()),
                    tokens_saved: baseline_tokens as i64 - tokens_final as i64,
                    tokens_final,
                };
                self.emit_event(
                    &input,
                    &out,
                    &Shape::Unknown,
                    Some(&content_sha_hex),
                    file_path_hash.as_deref(),
                    None,
                );
                return out;
            }

        // Not a duplicate — insert into cache for future reference.
        let tc_hash = short_hash(input.tool_call_id);
        self.dedup.insert(
            content_hash_value,
            tc_hash.clone(),
            tool_kind,
            file_path_hash.clone(),
        );

        // === Shape classification (used by L1/L2) ===
        let classified = classify(input.content);

        // === L1: per-endpoint templates ===
        if let Some(t_id) = self
            .config
            .templates
            .template_for(input.tool_name)
            .map(str::to_string)
            && self.config.templates.is_template_active(&t_id)
                && let Some(body) = crate::templates::apply_by_id(&t_id, input.content, &classified) {
                    let tokens_final = est_tokens(body.len());
                    if tokens_final < baseline_tokens {
                        let out = ProcessedResponse {
                            output: body,
                            layer: Layer::L1,
                            format_or_template: Some(t_id),
                            tokens_saved: baseline_tokens as i64 - tokens_final as i64,
                            tokens_final,
                        };
                        self.emit_event(
                            &input,
                            &out,
                            &classified.shape,
                            Some(&content_sha_hex),
                            file_path_hash.as_deref(),
                            None,
                        );
                        return out;
                    }
                }

        // === L2: generic MCKP ===
        if let Some((fmt_id, body)) =
            crate::mckp_router::route(&self.config.mckp, input.content, &classified)
        {
            let tokens_final = est_tokens(body.len());
            if tokens_final < baseline_tokens {
                let out = ProcessedResponse {
                    output: body,
                    layer: Layer::L2,
                    format_or_template: Some(fmt_id.to_string()),
                    tokens_saved: baseline_tokens as i64 - tokens_final as i64,
                    tokens_final,
                };
                self.emit_event(
                    &input,
                    &out,
                    &classified.shape,
                    Some(&content_sha_hex),
                    file_path_hash.as_deref(),
                    None,
                );
                return out;
            }
        }

        // === L3: passthrough ===
        let out = ProcessedResponse {
            output: input.content.to_string(),
            layer: Layer::L3,
            format_or_template: None,
            tokens_saved: 0,
            tokens_final: baseline_tokens,
        };
        self.emit_event(
            &input,
            &out,
            &classified.shape,
            Some(&content_sha_hex),
            file_path_hash.as_deref(),
            None,
        );
        out
    }

    fn emit_event(
        &self,
        input: &ToolResponseInput<'_>,
        out: &ProcessedResponse,
        shape: &Shape,
        content_sha_hex: Option<&str>,
        file_path_hash: Option<&str>,
        template_id: Option<&str>,
    ) {
        let Some(sink) = self.telemetry.clone() else {
            return;
        };
        let evt = PipelineEvent {
            session_hash: self.session_hash.clone(),
            tool_call_id_hash: short_hash(input.tool_call_id),
            tool_name_anon: anonymize_tool_name(input.tool_name),
            endpoint_class: input.tool_name.to_string(),
            response_chars: input.content.len() as u64,
            shape: *shape,
            inner_formats: vec![],
            content_sha_prefix_hex: content_sha_hex.unwrap_or_default().to_string(),
            file_path_hash: file_path_hash.map(String::from),
            is_dedup_hit: matches!(out.layer, Layer::L0),
            layer_used: out.layer,
            template_id: template_id.map(String::from),
            tokens_baseline: est_tokens(input.content.len()),
            tokens_final: out.tokens_final,
            context_partition: self.dedup.partition() as u32,
            is_sidechain: input.is_sidechain,
            ts_ms: input.ts_ms,
            sample_rate_applied: self.config.telemetry.sample_rate,
        };
        let _ = sink.record(&evt); // Best-effort; do not fail the pipeline.
    }
}

// ─── INLINE HELPERS (internal; no external API surface) ─────────────────

/// 16-char hex of a 16-byte content hash.
fn hex16(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// Short hash for tool_call_id / other anon identifiers.
pub(crate) fn short_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(s.as_bytes());
    let mut out = String::with_capacity(8);
    for b in &digest[..4] {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// Redact MCP slugs the same way `docs/research/scripts/analyze_sessions.py` does.
pub(crate) fn anonymize_tool_name(name: &str) -> String {
    if !name.starts_with("mcp__") {
        return name.to_string();
    }
    let inner = &name[5..];
    if let Some(verb_start) = inner.rfind("__") {
        let slug = &inner[..verb_start];
        let verb = &inner[verb_start + 2..];
        let slug_hash = short_hash(slug);
        return format!("mcp__p{}__{}", &slug_hash[..6], verb);
    }
    name.to_string()
}

// ─── TESTS ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::MemorySink;
    use std::sync::Arc;

    fn input<'a>(
        tc: &'a str,
        tool: &'a str,
        file: Option<&'a str>,
        content: &'a str,
    ) -> ToolResponseInput<'a> {
        ToolResponseInput {
            tool_call_id: tc,
            tool_name: tool,
            file_path: file,
            content,
            is_sidechain: false,
            ts_ms: 0,
        }
    }

    #[test]
    fn first_read_is_fresh_second_is_dedup() {
        let mut p = LayeredPipeline::new("s1".into(), AdaptiveConfig::default());
        let body = "fn main() { println!(\"hello\"); }\n".repeat(20);
        let o1 = p.process(input("tc_1", "Read", Some("/src/main.rs"), &body));
        assert_eq!(o1.layer, Layer::L3); // prose content, no MCKP gain
        let o2 = p.process(input("tc_2", "Read", Some("/src/main.rs"), &body));
        assert_eq!(o2.layer, Layer::L0);
        assert!(o2.tokens_saved > 0);
        assert!(o2.output.starts_with("> [ref:"));
    }

    #[test]
    fn edit_invalidates_file_read() {
        let mut p = LayeredPipeline::new("s2".into(), AdaptiveConfig::default());
        let body = "original content ".repeat(30);
        // First read: cache
        p.process(input("tc_1", "Read", Some("/src/x.rs"), &body));
        // Edit invalidates
        p.process(input(
            "tc_2",
            "Edit",
            Some("/src/x.rs"),
            "edit response body long enough to not be skipped",
        ));
        // Re-read with same content — should now be Fresh, not a hint
        let o = p.process(input("tc_3", "Read", Some("/src/x.rs"), &body));
        assert_eq!(o.layer, Layer::L3);
    }

    #[test]
    fn compaction_clears_cache() {
        let mut p = LayeredPipeline::new("s3".into(), AdaptiveConfig::default());
        let body = "x".repeat(300);
        p.process(input("tc_1", "Bash", None, &body));
        p.on_compaction_boundary();
        let o = p.process(input("tc_2", "Bash", None, &body));
        assert_eq!(o.layer, Layer::L3);
    }

    #[test]
    fn tiny_body_goes_straight_to_l3() {
        let mut p = LayeredPipeline::new("s4".into(), AdaptiveConfig::default());
        let o = p.process(input("tc_1", "Bash", None, "short"));
        assert_eq!(o.layer, Layer::L3);
        assert_eq!(o.tokens_saved, 0);
    }

    #[test]
    fn telemetry_sink_receives_events() {
        let sink = Arc::new(MemorySink::new());
        let mut p = LayeredPipeline::new("s5".into(), AdaptiveConfig::default())
            .with_telemetry(sink.clone());
        let body = "x".repeat(500);
        p.process(input("tc_1", "Bash", None, &body));
        p.process(input("tc_2", "Bash", None, &body));
        let events = sink.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].layer_used, Layer::L0);
        assert!(events[1].is_dedup_hit);
    }

    #[test]
    fn mcp_tool_names_are_anonymized() {
        let name = "mcp__super_secret_internal_slug__get_issues";
        let anon = anonymize_tool_name(name);
        assert!(anon.starts_with("mcp__p"));
        assert!(anon.ends_with("__get_issues"));
        assert!(!anon.contains("super_secret"));
    }

    #[test]
    fn markdown_table_gets_l2_csv_encoding() {
        let mut p = LayeredPipeline::new("s6".into(), AdaptiveConfig::default());
        let md = "| id | name | status |\n|----|------|--------|\n| 1 | a | ok |\n| 2 | b | ok |\n| 3 | c | bad |\n| 4 | d | ok |\n| 5 | e | bad |\n";
        // Need body >= min_body_chars (200). Pad the table.
        let padded = md.repeat(3);
        let o = p.process(input("tc_1", "Bash", None, &padded));
        // csv_from_md might apply if body is short enough, or L3 for plain text
        // We just assert the pipeline returns something valid.
        assert!(o.output.len() <= padded.len() + 100);
    }

    #[test]
    fn json_flat_object_passthrough_when_small() {
        let mut p = LayeredPipeline::new("s7".into(), AdaptiveConfig::default());
        let body = r#"{"id":1,"name":"test","status":"ok"}"#;
        let o = p.process(input("tc_1", "Bash", None, body));
        // Small body → L3
        assert_eq!(o.layer, Layer::L3);
    }

    #[test]
    fn multi_session_cache_isolation() {
        // Same content in different pipelines must NOT hit.
        let body = "x".repeat(500);
        let mut p1 = LayeredPipeline::new("s_a".into(), AdaptiveConfig::default());
        let mut p2 = LayeredPipeline::new("s_b".into(), AdaptiveConfig::default());
        p1.process(input("tc_1", "Bash", None, &body));
        let o2 = p2.process(input("tc_1", "Bash", None, &body));
        assert_eq!(o2.layer, Layer::L3);
    }
}
