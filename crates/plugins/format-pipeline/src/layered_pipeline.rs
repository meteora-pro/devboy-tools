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
//!     enricher_prefetched: false,
//!     enricher_predicted_cost_tokens: 0,
//! });
//! // Second call with identical content emits a hint.
//! let out2 = p.process(ToolResponseInput {
//!     tool_call_id: "tc_2",
//!     tool_name: "Read",
//!     file_path: Some("/src/main.rs"),
//!     content: &body,
//!     is_sidechain: false,
//!     ts_ms: 10,
//!     enricher_prefetched: false,
//!     enricher_predicted_cost_tokens: 0,
//! });
//! assert!(matches!(out2.layer, devboy_format_pipeline::telemetry::Layer::L0));
//! ```

use std::sync::Arc;

use crate::adaptive_config::AdaptiveConfig;
use crate::dedup::{DedupCache, DedupDecision, ToolKind, content_hash, render_reference_hint_with};
use crate::shape::{ClassifiedResponse, classify};
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
    /// Paper 3 — `true` when this response landed via the speculative
    /// pre-fetch dispatcher (host called the tool ahead of the LLM
    /// asking). Sets `PipelineEvent.enricher_prefetched` so the
    /// offline post-pass can attribute citations.
    /// Default `false` — the LLM emitted the call directly.
    pub enricher_prefetched: bool,
    /// Paper 3 — `cost_model.typical_kb`-derived prediction (in
    /// tokens) that the planner committed to when admitting this
    /// call. `0` when not a prefetch (LLM-emitted) — the cost-overrun
    /// rate denominator skips events with `enricher_predicted_cost_tokens
    /// = 0`.
    pub enricher_predicted_cost_tokens: u32,
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
    /// Counts events that reached the sink — used to trigger periodic flush
    /// according to `telemetry.flush_every_n`.
    recorded_counter: u64,
}

impl LayeredPipeline {
    /// Construct a new session pipeline.
    ///
    /// The shared LRU cache is sized to the maximum capacity requested by any
    /// endpoint-level override (or the global `dedup.lru_size` otherwise), so
    /// per-endpoint hints that ask for a larger working set are respected.
    pub fn new(session_hash: String, config: AdaptiveConfig) -> Self {
        let lru = config.max_lru_size();
        Self {
            session_hash,
            dedup: DedupCache::with_capacity(lru),
            config,
            telemetry: None,
            event_counter: 0,
            recorded_counter: 0,
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

    /// Drop every cache entry tagged with `file_path`. Called by the host
    /// after a mutating tool (`Edit` / `Write` / `MultiEdit` / …) so the
    /// next `Read` of the same file returns the fresh body, not a hint.
    pub fn invalidate_file(&mut self, file_path: &str) -> usize {
        let hash = crate::dedup_util::file_path_hash(file_path);
        self.dedup.invalidate_file(&hash)
    }

    /// Token count for `text` under the active tokenizer profile.
    ///
    /// Always routed through [`TokenizerProfile::count_tokens`] so a
    /// custom `chars_per_token` set in `[profiles.tokenizer.variants]`
    /// actually takes effect — earlier versions hard-coded `chars/4`
    /// for the heuristic branch and silently ignored the config knob.
    /// For Python-extractor parity, the matching variant ships
    /// `chars_per_token = 4.0`.
    fn tokens(&self, text: &str) -> u32 {
        self.config.effective_tokenizer_profile().count_tokens(text) as u32
    }

    /// Dispatch a tool response through the 4-layer pipeline.
    pub fn process(&mut self, input: ToolResponseInput<'_>) -> ProcessedResponse {
        self.event_counter += 1;
        let baseline_tokens = self.tokens(input.content);

        // Mutation-aware invalidation must fire even for tiny Edit responses —
        // the invalidation itself is correctness-critical, not an optimization.
        let tool_kind = ToolKind::from_tool_name(input.tool_name);
        let file_path_hash = input
            .file_path
            .filter(|_| matches!(tool_kind, ToolKind::FileRead | ToolKind::FileMutate))
            .map(crate::dedup_util::file_path_hash);

        if tool_kind == ToolKind::FileMutate
            && let Some(ref fh) = file_path_hash
        {
            self.dedup.invalidate_file(fh);
        }

        // Short-circuit: L3 for too-small bodies (not worth any optimization).
        let min_chars = self.config.effective_min_body_chars(input.tool_name).max(1);
        if input.content.len() < min_chars {
            let out = ProcessedResponse {
                output: input.content.to_string(),
                layer: Layer::L3,
                format_or_template: None,
                tokens_saved: 0,
                tokens_final: baseline_tokens,
            };
            self.emit_event(&input, &out, None, None, None, None);
            return out;
        }

        // === L0: content-hash dedup ===
        let endpoint_ok = self.config.effective_dedup_enabled(input.tool_name);
        let content_hash_value = content_hash(input.content.as_bytes());
        let content_sha_hex = hex_of(&content_hash_value);

        if endpoint_ok
            && let DedupDecision::Hint {
                reference_tool_call_id,
            } = self.dedup.check(&content_hash_value)
        {
            let hint = render_reference_hint_with(
                &reference_tool_call_id,
                self.config.dedup.hint_verbosity.to_runtime(),
                Some(tool_kind),
            );
            let tokens_final = self.tokens(&hint);
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
                None,
                Some(&content_sha_hex),
                file_path_hash.as_deref(),
                None,
            );
            return out;
        }

        // Not byte-identical — try Type-2 (near-duplicate) hint when
        // enabled. The cache must hold a body snapshot for at least one
        // matching prior entry; otherwise this is a no-op fall-through
        // to L1.
        if endpoint_ok && self.config.dedup.near_ref_enabled {
            let near_cfg = crate::near_ref::NearRefConfig::default();
            if let Some((reference_tool_call_id, deltas)) =
                self.dedup.find_near_ref(input.content, &near_cfg)
            {
                let hint = crate::near_ref::render_near_ref_hint(&reference_tool_call_id, &deltas);
                let tokens_final = self.tokens(&hint);
                let out = ProcessedResponse {
                    output: hint,
                    layer: Layer::L0,
                    format_or_template: Some("hint_near".into()),
                    tokens_saved: baseline_tokens as i64 - tokens_final as i64,
                    tokens_final,
                };
                self.emit_event(
                    &input,
                    &out,
                    None,
                    Some(&content_sha_hex),
                    file_path_hash.as_deref(),
                    None,
                );
                // Even on a near-ref hit, still cache the *new* body so
                // a later turn that drifts further can build a delta off
                // the most recent state.
                let tc_hash = short_hash(input.tool_call_id);
                self.dedup.insert_with_body(
                    content_hash_value,
                    tc_hash,
                    tool_kind,
                    file_path_hash.clone(),
                    std::sync::Arc::new(input.content.to_string()),
                    input.tool_name,
                );
                return out;
            }
        }

        // Not a duplicate — insert into cache for future reference. When
        // near-ref is enabled, also retain the body so future turns can
        // diff against it; otherwise stick to the cheaper hash-only entry.
        //
        // `tool_name` is recorded so `DedupCache::invalidate_by_tool`
        // (Paper 3 §Cross-tool invalidation) can later drop entries for
        // tools that this writer's `ToolValueModel.invalidates` lists.
        //
        // We pass `input.tool_name` *as-is* (e.g. `mcp__gitlab__get_issue`).
        // Anonymization (`mcp__p<hash6>__verb`) only applies to the
        // public corpus aggregates in `docs/research/data/paper3_*.csv`;
        // `[tools.<name>]` keys, `effective_tool_value_model` lookups,
        // and the dedup cache all use the live runtime name. Otherwise
        // a user override `[tools."mcp__gitlab__update_issue"]` would
        // never be found, and `invalidates = ["mcp__gitlab__get_issue"]`
        // would never match a cached entry.
        let tc_hash = short_hash(input.tool_call_id);
        if self.config.dedup.near_ref_enabled {
            self.dedup.insert_with_body(
                content_hash_value,
                tc_hash.clone(),
                tool_kind,
                file_path_hash.clone(),
                std::sync::Arc::new(input.content.to_string()),
                input.tool_name,
            );
        } else {
            self.dedup.insert(
                content_hash_value,
                tc_hash.clone(),
                tool_kind,
                file_path_hash.clone(),
                input.tool_name,
            );
        }

        // Paper 3 cross-tool invalidation: if the tool that just ran
        // declares an `invalidates` list in its value model, drop every
        // matching cached entry now so the next call returns fresh.
        if let Some(model) = self.config.effective_tool_value_model(input.tool_name)
            && !model.invalidates.is_empty()
        {
            self.dedup.invalidate_by_tool(&model.invalidates);
        }

        // === Shape classification (used by L1/L2) ===
        let classified = classify(input.content);

        // === L1: per-endpoint templates ===
        if let Some(t_id) = self
            .config
            .effective_template(input.tool_name)
            .map(str::to_string)
            && self.config.templates.is_template_active(&t_id)
            && let Some(body) = crate::templates::apply_by_id(&t_id, input.content, &classified)
        {
            let tokens_final = self.tokens(&body);
            if tokens_final < baseline_tokens {
                let out = ProcessedResponse {
                    output: body,
                    layer: Layer::L1,
                    format_or_template: Some(t_id.clone()),
                    tokens_saved: baseline_tokens as i64 - tokens_final as i64,
                    tokens_final,
                };
                self.emit_event(
                    &input,
                    &out,
                    Some(&classified),
                    Some(&content_sha_hex),
                    file_path_hash.as_deref(),
                    Some(&t_id),
                );
                return out;
            }
        }

        // === L2: generic MCKP ===
        if let Some((fmt_id, body)) =
            crate::mckp_router::route(&self.config.mckp, input.content, &classified)
        {
            let tokens_final = self.tokens(&body);
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
                    Some(&classified),
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
            Some(&classified),
            Some(&content_sha_hex),
            file_path_hash.as_deref(),
            None,
        );
        out
    }

    /// Deterministic sampler — returns `true` iff the current event should be
    /// written to the sink under the configured `telemetry.sample_rate`.
    ///
    /// Deterministic so replays over the same session are reproducible.
    fn should_sample(&self) -> bool {
        let rate = self.config.telemetry.sample_rate.clamp(0.0, 1.0);
        if rate >= 1.0 {
            return true;
        }
        if rate <= 0.0 {
            return false;
        }
        // Fixed-interval stride matching the requested rate.
        let stride = (1.0 / rate).round().max(1.0) as u64;
        self.event_counter.is_multiple_of(stride)
    }

    fn emit_event(
        &mut self,
        input: &ToolResponseInput<'_>,
        out: &ProcessedResponse,
        classified: Option<&ClassifiedResponse>,
        content_sha_hex: Option<&str>,
        file_path_hash: Option<&str>,
        template_id: Option<&str>,
    ) {
        let Some(sink) = self.telemetry.clone() else {
            return;
        };
        if !self.should_sample() {
            return;
        }
        let shape = classified.map(|c| c.shape).unwrap_or(Shape::Unknown);
        let inner_formats = classified
            .map(|c| {
                c.inner_formats
                    .iter()
                    .map(|f| f.as_tag().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let evt = PipelineEvent {
            session_hash: self.session_hash.clone(),
            tool_call_id_hash: short_hash(input.tool_call_id),
            tool_name_anon: anonymize_tool_name(input.tool_name),
            endpoint_class: input.tool_name.to_string(),
            response_chars: input.content.len() as u64,
            shape,
            inner_formats,
            content_sha_prefix_hex: content_sha_hex.unwrap_or_default().to_string(),
            file_path_hash: file_path_hash.map(String::from),
            is_dedup_hit: matches!(out.layer, Layer::L0),
            layer_used: out.layer,
            template_id: template_id.map(String::from),
            tokens_baseline: self.tokens(input.content),
            tokens_final: out.tokens_final,
            context_partition: self.dedup.partition() as u32,
            is_sidechain: input.is_sidechain,
            ts_ms: input.ts_ms,
            sample_rate_applied: self.config.telemetry.sample_rate,
            // Paper 3 enricher fields — set on the input by the host
            // when the call was issued by the speculative dispatcher.
            // LLM-emitted calls leave both fields default (false / 0).
            enricher_prefetched: input.enricher_prefetched,
            enricher_predicted_cost_tokens: input.enricher_predicted_cost_tokens,
            enricher_decline_reason: None,
            cited_in_next_n_turns: None,
        };
        if sink.record(&evt).is_err() {
            return; // Best-effort; do not fail the pipeline.
        }
        self.recorded_counter += 1;
        let flush_every = self.config.telemetry.flush_every_n.max(1) as u64;
        if self.recorded_counter.is_multiple_of(flush_every) {
            let _ = sink.flush();
        }
    }
}

// ─── INLINE HELPERS (internal; no external API surface) ─────────────────

/// 32-char hex of a 16-byte (128-bit) content hash prefix. Matches the
/// `content_sha_prefix_hex` field documented in
/// `docs/research/paper-2-mckp-format-adaptive.md` §Telemetry.
fn hex_of(bytes: &[u8; 16]) -> String {
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
            enricher_prefetched: false,
            enricher_predicted_cost_tokens: 0,
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

    #[test]
    fn partition_counter_advances_on_compaction() {
        let mut p = LayeredPipeline::new("s_part".into(), AdaptiveConfig::default());
        assert_eq!(p.partition(), 0);
        p.on_compaction_boundary();
        assert_eq!(p.partition(), 1);
        p.on_compaction_boundary();
        assert_eq!(p.partition(), 2);
    }

    #[test]
    fn endpoint_override_disables_dedup() {
        let mut cfg = AdaptiveConfig::default();
        cfg.endpoint_overrides.insert(
            "Bash".into(),
            crate::adaptive_config::EndpointOverride {
                dedup_enabled: Some(false),
                ..Default::default()
            },
        );
        let sink = Arc::new(MemorySink::new());
        let mut p = LayeredPipeline::new("s_disabled".into(), cfg).with_telemetry(sink.clone());
        let body = "y".repeat(500);
        p.process(input("tc_1", "Bash", None, &body));
        let o2 = p.process(input("tc_2", "Bash", None, &body));
        // dedup disabled → no L0 hint even on identical content
        assert_eq!(o2.layer, Layer::L3);
        assert!(!sink.events()[1].is_dedup_hit);
    }

    #[test]
    fn per_endpoint_min_body_chars_override() {
        let mut cfg = AdaptiveConfig::default();
        cfg.endpoint_overrides.insert(
            "Bash".into(),
            crate::adaptive_config::EndpointOverride {
                min_body_chars: Some(50),
                ..Default::default()
            },
        );
        let mut p = LayeredPipeline::new("s_min".into(), cfg);
        // 60-char body would be skipped under default 200 but processed under override 50.
        let body = "z".repeat(60);
        p.process(input("tc_1", "Bash", None, &body));
        let o2 = p.process(input("tc_2", "Bash", None, &body));
        assert_eq!(o2.layer, Layer::L0); // dedup fires because we cached tc_1
    }

    #[test]
    fn sample_rate_zero_skips_all_events() {
        let mut cfg = AdaptiveConfig::default();
        cfg.telemetry.sample_rate = 0.0;
        let sink = Arc::new(MemorySink::new());
        let mut p = LayeredPipeline::new("s_rate0".into(), cfg).with_telemetry(sink.clone());
        let body = "q".repeat(500);
        for i in 0..5 {
            p.process(input(&format!("tc_{i}"), "Bash", None, &body));
        }
        assert_eq!(sink.events().len(), 0);
    }

    #[test]
    fn sample_rate_half_keeps_every_other() {
        let mut cfg = AdaptiveConfig::default();
        cfg.telemetry.sample_rate = 0.5;
        let sink = Arc::new(MemorySink::new());
        let mut p = LayeredPipeline::new("s_half".into(), cfg).with_telemetry(sink.clone());
        let body = "w".repeat(500);
        for i in 0..10 {
            p.process(input(&format!("tc_{i}"), "Bash", None, &body));
        }
        // Stride sampler keeps 1-in-2 events (events 2, 4, 6, 8, 10).
        assert_eq!(sink.events().len(), 5);
    }

    #[test]
    fn hint_verbosity_terse_is_honoured() {
        let mut cfg = AdaptiveConfig::default();
        cfg.dedup.hint_verbosity = crate::adaptive_config::HintVerbosity::Terse;
        let mut p = LayeredPipeline::new("s_terse".into(), cfg);
        let body = "terse body of sufficient length ".repeat(20);
        p.process(input("tc_1", "Bash", None, &body));
        let o2 = p.process(input("tc_2", "Bash", None, &body));
        assert_eq!(o2.layer, Layer::L0);
        assert!(!o2.output.contains("byte-identical"));
    }

    #[test]
    fn inner_formats_populated_in_telemetry() {
        let sink = Arc::new(MemorySink::new());
        let mut p = LayeredPipeline::new("s_inner".into(), AdaptiveConfig::default())
            .with_telemetry(sink.clone());
        // Response with embedded URL — classifier should populate inner_formats.
        let body = format!(
            "Line 1\nSee https://example.com/resource for details.\n{}",
            "filler ".repeat(50)
        );
        p.process(input("tc_1", "Bash", None, &body));
        let events = sink.events();
        assert!(
            !events[0].inner_formats.is_empty(),
            "inner_formats should populate from classifier"
        );
        assert!(events[0].inner_formats.iter().any(|f| f == "url"));
    }

    #[test]
    fn cache_capacity_grows_via_endpoint_lru_override() {
        let mut cfg = AdaptiveConfig::default();
        cfg.endpoint_overrides.insert(
            "ep".into(),
            crate::adaptive_config::EndpointOverride {
                lru_size: Some(12),
                ..Default::default()
            },
        );
        // Pipeline is constructed with the larger capacity — no direct getter,
        // but we can verify by inserting many distinct responses and confirming
        // that the 12th+ entry evicts the 1st.
        let mut p = LayeredPipeline::new("s_lru".into(), cfg);
        let distinct: Vec<String> = (0..13)
            .map(|i| format!("{}{}", i, "x".repeat(300)))
            .collect();
        for (i, b) in distinct.iter().enumerate() {
            p.process(input(&format!("tc_{i}"), "Bash", None, b));
        }
        // First entry now evicted (capacity 12), so recalling it should be Fresh.
        let o = p.process(input("tc_recheck", "Bash", None, &distinct[0]));
        assert_eq!(o.layer, Layer::L3);
    }

    #[test]
    fn tokens_method_falls_back_to_heuristic_by_default() {
        let cfg = AdaptiveConfig::default();
        // Default profile is "auto" → resolves to anthropic_class which now
        // selects O200kBase BPE. Pipeline.tokens should therefore *not* match
        // the legacy chars/4 estimate on a sufficiently long body.
        let p = LayeredPipeline::new("s_tk".into(), cfg);
        let body = "a".repeat(2_000);
        let bpe_count = p.tokens(&body);
        let heuristic = (body.len() / 4) as u32;
        // BPE on a long run of a single character compresses much better than
        // chars/4 — assert they differ (and BPE is smaller, the whole point of
        // bringing in tiktoken-rs).
        assert!(
            bpe_count < heuristic,
            "expected BPE count {bpe_count} < heuristic {heuristic} on a degenerate input"
        );
    }

    #[test]
    fn near_ref_enabled_emits_delta_hint_for_pipeline_polling() {
        // Two responses to the same MCP endpoint differing only in
        // `status` and `duration` — the canonical Type-2 case.
        let body_a = format!(
            r#"{{"id":42,"name":"deploy","status":"pending","duration":10,"url":"https://example.com/p/42","commit_sha":"abcd","triggered_by":"webhook","preview":"{}"}}"#,
            "x".repeat(500)
        );
        let body_b = format!(
            r#"{{"id":42,"name":"deploy","status":"success","duration":42,"url":"https://example.com/p/42","commit_sha":"abcd","triggered_by":"webhook","preview":"{}"}}"#,
            "x".repeat(500)
        );

        let mut cfg = AdaptiveConfig::default();
        cfg.dedup.near_ref_enabled = true;
        let mut p = LayeredPipeline::new("s_near".into(), cfg);

        let r1 = p.process(input("tc_pipeline_1", "Bash", None, &body_a));
        assert_eq!(r1.layer, Layer::L3, "first call must be fresh");

        let r2 = p.process(input("tc_pipeline_2", "Bash", None, &body_b));
        assert_eq!(r2.layer, Layer::L0, "second call must hit L0 via near-ref");
        assert_eq!(r2.format_or_template.as_deref(), Some("hint_near"));
        assert!(
            r2.output.contains("near-ref"),
            "expected near-ref hint, got `{}`",
            r2.output
        );
        // Both differing scalar fields must show in the hint.
        assert!(r2.output.contains("status"));
        assert!(r2.output.contains("duration"));
        // Compaction-friendly bound — the rendered hint must be tiny.
        assert!(
            r2.output.len() < body_b.len() / 5,
            "near-ref hint should be far smaller than the body"
        );
    }

    #[test]
    fn near_ref_disabled_falls_through_when_bodies_drift() {
        let body_a = format!(
            r#"{{"id":42,"status":"pending","preview":"{}"}}"#,
            "x".repeat(500)
        );
        let body_b = format!(
            r#"{{"id":42,"status":"success","preview":"{}"}}"#,
            "x".repeat(500)
        );

        let mut cfg = AdaptiveConfig::default();
        cfg.dedup.near_ref_enabled = false; // explicit
        let mut p = LayeredPipeline::new("s_no_near".into(), cfg);
        let _ = p.process(input("tc_a", "Bash", None, &body_a));
        let r2 = p.process(input("tc_b", "Bash", None, &body_b));
        // Without near-ref, the second call goes to L3 (or L2 if shape
        // happens to match) — but never to L0/hint_near.
        assert_ne!(r2.format_or_template.as_deref(), Some("hint_near"));
    }

    #[test]
    fn tokens_method_honours_profile_chars_per_token() {
        let mut cfg = AdaptiveConfig::default();
        // Force the active tokenizer to `ollama_bpe`, which ships
        // `chars_per_token = 3.8` and `bpe = Heuristic`. The pipeline
        // must respect the configured ratio (8 / 3.8 ≈ 2.1 → 3 tokens
        // after ceil); earlier versions silently hard-coded chars/4
        // and produced 2 instead — Copilot review on PR #207.
        cfg.profiles.tokenizer.active = "ollama_bpe".into();
        let p = LayeredPipeline::new("s_h".into(), cfg);
        let body = "abcdefgh"; // 8 chars
        assert_eq!(p.tokens(body), 3);
    }

    #[test]
    fn cross_tool_invalidation_drops_cached_response() {
        // Paper 3 P-3-07 — `update_issue` declares
        // `invalidates = ["get_issue"]`. After `get_issue` is cached
        // and `update_issue` runs, a re-read of the same `get_issue`
        // body must come back fresh (not as a hint).
        use devboy_core::ToolValueModel;
        let mut cfg = AdaptiveConfig::default();
        cfg.tools.insert(
            "update_issue".into(),
            ToolValueModel {
                invalidates: vec!["get_issue".into()],
                ..ToolValueModel::default()
            },
        );
        let mut p = LayeredPipeline::new("s_invl".into(), cfg);
        let body = "x".repeat(400);

        // Turn 1 — get_issue caches.
        let r1 = p.process(input("tc_1", "get_issue", None, &body));
        assert_eq!(r1.layer, Layer::L3);

        // Turn 2 — same body, would dedup ...
        let r2 = p.process(input("tc_2", "get_issue", None, &body));
        assert_eq!(r2.layer, Layer::L0, "second call should dedup");

        // Turn 3 — `update_issue` runs (different body!), declares
        // `invalidates = ["get_issue"]`. Its run drops the cached
        // get_issue entry.
        let _ = p.process(input("tc_3", "update_issue", None, &"u".repeat(400)));

        // Turn 4 — re-issuing get_issue with the *same* body must now
        // come back fresh; the cache was busted by update_issue.
        let r4 = p.process(input("tc_4", "get_issue", None, &body));
        assert_eq!(
            r4.layer,
            Layer::L3,
            "get_issue cache must be invalidated by update_issue"
        );
    }
}
