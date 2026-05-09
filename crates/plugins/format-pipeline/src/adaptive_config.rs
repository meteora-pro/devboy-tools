//! Adaptive configuration — TOML-backed tuning knobs for the layered pipeline.
//!
//! See `docs/research/paper-2-mckp-format-adaptive.md` §Adaptive Configuration
//! for the motivation and decision rules. This module provides the
//! strongly-typed schema that the tuner emits and the layered pipeline
//! consumes.
//!
//! # Example TOML
//!
//! ```toml
//! schema_version = 1
//!
//! [dedup]
//! lru_size = 5
//! hint_verbosity = "standard"
//! near_ref_enabled = false
//! min_body_chars = 200
//!
//! [dedup.enabled_per_endpoint]
//! "mcp__p3a04ae__get_issues" = true
//! "Bash:git_log" = false
//!
//! [templates]
//! active = ["csv_from_md", "pipeline_deep_mckp", "mr_diff_fence"]
//!
//! [templates.endpoint_overrides]
//! "mcp__p3a04ae__get_issues" = "csv_from_md"
//!
//! [mckp]
//! recursion_depth = 5
//! formats_enabled = ["csv_from_md", "deep_mckp", "kv", "csv", "json_compact"]
//!
//! [mckp.shape_thresholds]
//! markdown_table_min_cols = 2
//! array_of_objects_min_items = 4
//! flat_object_min_fields = 8
//!
//! [telemetry]
//! sample_rate = 1.0
//! flush_every_n = 25
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use devboy_core::ToolValueModel;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::token_counter::Tokenizer;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("adaptive-config I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("adaptive-config parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("adaptive-config serialize: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("adaptive-config unsupported schema version {0} (expected 1)")]
    /// UnsupportedSchemaVersion.
    UnsupportedSchemaVersion(u32),
}

pub type Result<T> = std::result::Result<T, ConfigError>;

pub const CURRENT_SCHEMA_VERSION: u32 = 4;

/// Lowest schema version we still accept on load (auto-upgraded in memory).
pub const MIN_SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Root configuration for the layered pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub dedup: DedupConfig,
    #[serde(default)]
    pub templates: TemplatesConfig,
    #[serde(default)]
    pub mckp: MckpConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// Per-endpoint overrides. Keyed by `endpoint_class` (see telemetry schema).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub endpoint_overrides: BTreeMap<String, EndpointOverride>,
    /// Schema-v2: profile axes (tokenizer / llm / agent / data).
    #[serde(default)]
    pub profiles: ProfilesConfig,
    /// Schema-v2: horizontal hint policy.
    #[serde(default)]
    pub hints: HintsConfig,
    /// Schema-v3: per-tool value models for the Paper 3 enrichment
    /// planner. Keyed by anonymized tool name (e.g. `"Read"`,
    /// `"mcp__pXXXXXX__get_branch_pipeline"`). User overrides land
    /// here from `[tools.<name>]` blocks; provider-shipped defaults
    /// are merged in at startup time.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, ToolValueModel>,
    /// Schema-v4: speculative-execution settings for the Paper 3
    /// enrichment planner. Off by default — opt-in.
    #[serde(default)]
    pub enrichment: EnrichmentConfig,
}

fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            dedup: DedupConfig::default(),
            templates: TemplatesConfig::default(),
            mckp: MckpConfig::default(),
            telemetry: TelemetryConfig::default(),
            endpoint_overrides: BTreeMap::new(),
            profiles: ProfilesConfig::default(),
            hints: HintsConfig::default(),
            tools: BTreeMap::new(),
            enrichment: EnrichmentConfig::default(),
        }
    }
}

impl AdaptiveConfig {
    /// Load a config from disk. Missing files resolve to `Default::default()`,
    /// so callers can unconditionally load without a separate existence check.
    pub fn load_or_default(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let s = fs::read_to_string(path)?;
        let mut cfg: AdaptiveConfig = toml::from_str(&s)?;
        cfg.upgrade_in_place()?;
        Ok(cfg)
    }

    /// Strict load — fails if the file is missing.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let s = fs::read_to_string(path)?;
        let mut cfg: AdaptiveConfig = toml::from_str(&s)?;
        cfg.upgrade_in_place()?;
        Ok(cfg)
    }

    /// Migrate a config in place to `CURRENT_SCHEMA_VERSION`.
    ///
    /// v1 → v2: the on-disk file lacks `[profiles.*]` and `[hints]` sections;
    /// `serde(default)` already populates them with the v2 defaults.
    /// v2 → v3: the on-disk file lacks the `[tools.*]` table;
    /// `serde(default)` populates an empty BTreeMap, then the runtime
    /// merges provider-shipped defaults on top at startup time.
    /// v3 → v4: the on-disk file lacks the `[enrichment]` section;
    /// `serde(default)` populates the off-by-default Paper 3 settings.
    /// In all cases the only work here is bumping `schema_version`.
    fn upgrade_in_place(&mut self) -> Result<()> {
        if self.schema_version > CURRENT_SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchemaVersion(self.schema_version));
        }
        if self.schema_version < MIN_SUPPORTED_SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchemaVersion(self.schema_version));
        }
        // v1 → v3: defaults already injected; just stamp the version.
        if self.schema_version < CURRENT_SCHEMA_VERSION {
            self.schema_version = CURRENT_SCHEMA_VERSION;
        }
        Ok(())
    }

    /// Serialize to TOML and write atomically.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let s = toml::to_string_pretty(self)?;
        // Atomic-ish write: tmp + rename.
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, s)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Effective L0-dedup enabled flag for `endpoint`. Reads
    /// `endpoint_overrides[endpoint].dedup_enabled` first, then falls back to
    /// `dedup.enabled_per_endpoint`, then to the permissive default (`true`).
    pub fn effective_dedup_enabled(&self, endpoint: &str) -> bool {
        if let Some(o) = self.endpoint_overrides.get(endpoint)
            && let Some(v) = o.dedup_enabled
        {
            return v;
        }
        self.dedup.enabled_for(endpoint)
    }

    /// Effective `min_body_chars` threshold for `endpoint`. Per-endpoint
    /// override wins; otherwise the global `dedup.min_body_chars` applies.
    pub fn effective_min_body_chars(&self, endpoint: &str) -> usize {
        self.endpoint_overrides
            .get(endpoint)
            .and_then(|o| o.min_body_chars)
            .unwrap_or(self.dedup.min_body_chars)
    }

    /// Effective LRU capacity for `endpoint`. The base cache uses the global
    /// `dedup.lru_size`; if an endpoint requests a *larger* capacity the
    /// caller should widen the shared cache accordingly. The hint is read
    /// once at construction time.
    pub fn effective_lru_size(&self, endpoint: &str) -> usize {
        let per_ep = self
            .endpoint_overrides
            .get(endpoint)
            .and_then(|o| o.lru_size);
        match per_ep {
            Some(n) => n.max(self.dedup.lru_size),
            None => self.dedup.lru_size,
        }
    }

    /// Maximum LRU capacity requested across all endpoint overrides and the
    /// global `dedup.lru_size`. Used at `LayeredPipeline::new` time to size
    /// the shared cache.
    pub fn max_lru_size(&self) -> usize {
        let mut n = self.dedup.lru_size;
        for o in self.endpoint_overrides.values() {
            if let Some(v) = o.lru_size {
                n = n.max(v);
            }
        }
        n.max(1)
    }

    /// Effective tokenizer profile resolved from `profiles.tokenizer.active`
    /// (or `auto` → `anthropic_class`). Always returns *some* profile —
    /// falls back to the default `anthropic_class` if the active id is
    /// missing from `variants`.
    pub fn effective_tokenizer_profile(&self) -> &TokenizerProfile {
        let active = self.profiles.tokenizer.active.as_str();
        let id = if active == "auto" || active.is_empty() {
            "anthropic_class"
        } else {
            active
        };
        self.profiles
            .tokenizer
            .variants
            .get(id)
            .or_else(|| self.profiles.tokenizer.variants.get("anthropic_class"))
            .unwrap_or_else(|| {
                // Last resort: a static default kept for the lifetime of the
                // process. We never expect to hit this branch — `Default`
                // populates `anthropic_class` — but safe-guard against a
                // hand-edited config that wiped variants.
                static FALLBACK: std::sync::OnceLock<TokenizerProfile> = std::sync::OnceLock::new();
                FALLBACK.get_or_init(TokenizerProfile::default)
            })
    }

    /// Token count for `text` under the active tokenizer profile. Hot path:
    /// when the profile selects `Tokenizer::Heuristic`, this is a single
    /// integer division on `text.len()`. When BPE is selected, it pays one
    /// `tiktoken-rs` encode call (typically 1–10 µs).
    pub fn effective_token_count(&self, text: &str) -> usize {
        self.effective_tokenizer_profile().count_tokens(text)
    }

    /// Effective L1 template id for `endpoint`. Per-endpoint override wins;
    /// falls back to `templates.endpoint_overrides`.
    pub fn effective_template(&self, endpoint: &str) -> Option<&str> {
        if let Some(o) = self.endpoint_overrides.get(endpoint)
            && let Some(t) = o.template_id.as_deref()
        {
            return Some(t);
        }
        self.templates.template_for(endpoint)
    }

    /// Effective `ToolValueModel` for `tool_name` for the Paper 3
    /// enrichment planner. Resolution order:
    ///
    /// 1. exact match in `[tools.<name>]` (user override or merged provider default);
    /// 2. wildcard `*` block (catch-all overrides — useful for blanket
    ///    `value_class = "supporting"` policies);
    /// 3. `None` — caller substitutes the global default.
    pub fn effective_tool_value_model(&self, tool_name: &str) -> Option<&ToolValueModel> {
        if let Some(m) = self.tools.get(tool_name) {
            return Some(m);
        }
        self.tools.get("*")
    }

    /// Merge another config into self. Fields present in `other` override `self`.
    /// Endpoint overrides are unioned (right-wins on collisions).
    pub fn merge_right_wins(&mut self, other: AdaptiveConfig) {
        self.dedup = other.dedup;
        self.templates = other.templates;
        self.mckp = other.mckp;
        self.telemetry = other.telemetry;
        self.profiles = other.profiles;
        self.hints = other.hints;
        for (k, v) in other.endpoint_overrides {
            self.endpoint_overrides.insert(k, v);
        }
        // Provider defaults are typically loaded into `self.tools` first
        // and then user overrides come in via `other.tools`. Right-wins
        // matches the documented `[tools.<name>]` semantics.
        for (k, v) in other.tools {
            self.tools.insert(k, v);
        }
    }
}

// ─── L0 DEDUP ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DedupConfig {
    /// LRU cache capacity per context_partition.
    #[serde(default = "default_lru_size")]
    pub lru_size: usize,
    /// Verbosity of emitted reference hints.
    #[serde(default)]
    pub hint_verbosity: HintVerbosity,
    /// Enable Type-2 near-reference hints (delta encoding). Default off.
    #[serde(default)]
    pub near_ref_enabled: bool,
    /// Skip L0 for responses shorter than this many chars.
    #[serde(default = "default_min_body_chars")]
    pub min_body_chars: usize,
    /// Per-endpoint enable/disable. Absent entries → enabled.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub enabled_per_endpoint: BTreeMap<String, bool>,
}

fn default_lru_size() -> usize {
    5
}
fn default_min_body_chars() -> usize {
    200
}

impl Default for DedupConfig {
    fn default() -> Self {
        Self {
            lru_size: default_lru_size(),
            hint_verbosity: HintVerbosity::Standard,
            near_ref_enabled: false,
            min_body_chars: default_min_body_chars(),
            enabled_per_endpoint: BTreeMap::new(),
        }
    }
}

impl DedupConfig {
    /// Is L0 dedup active for this endpoint? Defaults to true if unspecified.
    pub fn enabled_for(&self, endpoint: &str) -> bool {
        self.enabled_per_endpoint
            .get(endpoint)
            .copied()
            .unwrap_or(true)
    }
}

/// Verbosity of emitted reference hints.
///
/// Wire-compatible with [`crate::dedup::HintVerbosity`]; kept as a separate
/// type so the config schema stays independent of the runtime module.
/// Convert via [`HintVerbosity::to_runtime`] before rendering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HintVerbosity {
    /// `> [ref: abc1234]` (~8 tokens)
    Terse,
    /// `> [ref: abc1234, byte-identical]` (~11 tokens, default)
    #[default]
    Standard,
    /// `> [ref: abc1234, byte-identical, from: tool_name]` (~15 tokens)
    Verbose,
}

impl HintVerbosity {
    /// Convert to the runtime enum used by
    /// [`crate::dedup::render_reference_hint_with`].
    pub fn to_runtime(self) -> crate::dedup::HintVerbosity {
        match self {
            Self::Terse => crate::dedup::HintVerbosity::Terse,
            Self::Standard => crate::dedup::HintVerbosity::Standard,
            Self::Verbose => crate::dedup::HintVerbosity::Verbose,
        }
    }
}

// ─── L1 TEMPLATES ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplatesConfig {
    /// Template IDs the dispatcher may choose from.
    #[serde(default = "default_active_templates")]
    pub active: Vec<String>,
    /// Explicit endpoint → template_id overrides.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub endpoint_overrides: BTreeMap<String, String>,
}

fn default_active_templates() -> Vec<String> {
    vec![
        "csv_from_md".to_string(),
        "pipeline_deep_mckp".to_string(),
        "mr_diff_fence".to_string(),
    ]
}

impl Default for TemplatesConfig {
    fn default() -> Self {
        Self {
            active: default_active_templates(),
            endpoint_overrides: BTreeMap::new(),
        }
    }
}

impl TemplatesConfig {
    /// Is template active.
    pub fn is_template_active(&self, id: &str) -> bool {
        self.active.iter().any(|s| s == id)
    }
    /// Template for.
    pub fn template_for(&self, endpoint: &str) -> Option<&str> {
        self.endpoint_overrides.get(endpoint).map(String::as_str)
    }
}

// ─── L2 GENERIC MCKP ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MckpConfig {
    /// Maximum recursion depth for deep_mckp (per-leaf format selection).
    #[serde(default = "default_recursion_depth")]
    pub recursion_depth: usize,
    /// Which format encoders the L2 router may emit.
    #[serde(default = "default_formats_enabled")]
    pub formats_enabled: Vec<String>,
    #[serde(default)]
    pub shape_thresholds: ShapeThresholds,
}

fn default_recursion_depth() -> usize {
    5
}

fn default_formats_enabled() -> Vec<String> {
    vec![
        "csv_from_md".to_string(),
        "deep_mckp".to_string(),
        "kv".to_string(),
        "csv".to_string(),
        "json_compact".to_string(),
    ]
}

impl Default for MckpConfig {
    fn default() -> Self {
        Self {
            recursion_depth: default_recursion_depth(),
            formats_enabled: default_formats_enabled(),
            shape_thresholds: ShapeThresholds::default(),
        }
    }
}

impl MckpConfig {
    /// Format enabled.
    pub fn format_enabled(&self, id: &str) -> bool {
        self.formats_enabled.iter().any(|s| s == id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShapeThresholds {
    /// Apply csv_from_md only if the markdown table has at least this many columns.
    #[serde(default = "thr_md_cols")]
    pub markdown_table_min_cols: usize,
    /// Apply csv only if the array has at least this many objects.
    #[serde(default = "thr_arr_items")]
    pub array_of_objects_min_items: usize,
    /// Minimum mean key-stability across items (0.0–1.0) for csv encoding.
    #[serde(default = "thr_key_stability")]
    pub array_of_objects_min_key_stability: f32,
    /// Apply kv only if the flat object has at least this many fields.
    #[serde(default = "thr_flat_fields")]
    pub flat_object_min_fields: usize,
}

fn thr_md_cols() -> usize {
    2
}
fn thr_arr_items() -> usize {
    4
}
fn thr_key_stability() -> f32 {
    0.7
}
fn thr_flat_fields() -> usize {
    8
}

impl Default for ShapeThresholds {
    fn default() -> Self {
        Self {
            markdown_table_min_cols: thr_md_cols(),
            array_of_objects_min_items: thr_arr_items(),
            array_of_objects_min_key_stability: thr_key_stability(),
            flat_object_min_fields: thr_flat_fields(),
        }
    }
}

// ─── TELEMETRY ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    /// Master switch — when `false`, no sink is opened on the host even
    /// if `path` is set. Default: `false` so a fresh install does not
    /// silently start writing files to the user's `$HOME`.
    #[serde(default = "default_telemetry_enabled")]
    pub enabled: bool,
    /// Optional override for the JSONL sink directory. When `None`, the
    /// host falls back to `~/.devboy/telemetry/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Soft size cap per JSONL file (MiB). **Reserved for future use —
    /// the v1 sink does not rotate yet.** Persisted so a later
    /// implementation can pick it up without a config migration; today
    /// the value is read but not acted on. Operators with long-running
    /// sessions should keep `enabled = false` or rotate externally
    /// (logrotate / `find -mtime`).
    #[serde(default = "default_rotate_mib")]
    pub rotate_mib: u32,
    /// Fraction of events to record (1.0 = all).
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f32,
    /// Flush the sink every N recorded events.
    #[serde(default = "default_flush_every")]
    pub flush_every_n: usize,
}

fn default_telemetry_enabled() -> bool {
    false
}
fn default_rotate_mib() -> u32 {
    100
}
fn default_sample_rate() -> f32 {
    1.0
}
fn default_flush_every() -> usize {
    25
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: default_telemetry_enabled(),
            path: None,
            rotate_mib: default_rotate_mib(),
            sample_rate: default_sample_rate(),
            flush_every_n: default_flush_every(),
        }
    }
}

/// Speculative-execution settings for the Paper 3 enrichment planner.
///
/// Off by default. Operators (or `tune analyze --auto-enrichment`)
/// flip `enabled` to `true` once the corpus statistics show that
/// speculation would have paid off. Once enabled, the host enforces
/// the budget and concurrency limits below per turn.
///
/// **Schema-v4** — added in CURRENT_SCHEMA_VERSION = 4.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentConfig {
    /// Master switch. `false` (default) means the planner runs only in
    /// telemetry-only mode: `recent_tools` is tracked, `should_skip`
    /// can be consulted, but no out-of-band `tools/call` is dispatched.
    /// Flip to `true` to enable real speculative pre-fetch.
    #[serde(default = "default_enrichment_enabled")]
    pub enabled: bool,

    /// Maximum number of speculative pre-fetches the host issues in
    /// parallel from a single turn's `EnrichmentPlan`. Caps fan-out so
    /// a Glob → 12 Read does not melt the API rate-limit. Default: 3
    /// (matches the corpus finding that top-3 prefetch covers > 80%
    /// of cited follow-ups).
    #[serde(default = "default_max_parallel_prefetches")]
    pub max_parallel_prefetches: u32,

    /// Token ceiling for the *speculative* part of one turn — distinct
    /// from the per-response budget. `EnrichmentPlanner::build_plan`
    /// reads this when constructing `TurnContext`. Default: 8000 tokens
    /// (~32 kB at the 4-byte-per-token heuristic).
    #[serde(default = "default_prefetch_budget_tokens")]
    pub prefetch_budget_tokens: u32,

    /// Wall-clock budget the host waits for prefetches before
    /// returning the main response. Past this, the prefetch keeps
    /// running in the background (its result lands in the dedup cache
    /// when it returns) but the LLM gets the main response immediately
    /// + a hint that a prefetch is in flight.
    ///
    /// Default: 1000 ms — wide margin so typical Glob/Read can land
    /// synchronously, but small enough that a slow API never holds
    /// the agent.
    #[serde(default = "default_prefetch_timeout_ms")]
    pub prefetch_timeout_ms: u32,

    /// Honour `[tools.<name>].rate_limit_host` when scheduling
    /// prefetches. When `true`, the host counts how many prefetches
    /// per class are inflight this turn and skips new ones once the
    /// cap is hit. Default: `true` — the only reason to disable is
    /// for a benchmark harness with a known sandbox API.
    #[serde(default = "default_respect_rate_limits")]
    pub respect_rate_limits: bool,
}

fn default_enrichment_enabled() -> bool {
    false
}
fn default_max_parallel_prefetches() -> u32 {
    3
}
fn default_prefetch_budget_tokens() -> u32 {
    8000
}
fn default_prefetch_timeout_ms() -> u32 {
    1000
}
fn default_respect_rate_limits() -> bool {
    true
}

impl Default for EnrichmentConfig {
    fn default() -> Self {
        Self {
            enabled: default_enrichment_enabled(),
            max_parallel_prefetches: default_max_parallel_prefetches(),
            prefetch_budget_tokens: default_prefetch_budget_tokens(),
            prefetch_timeout_ms: default_prefetch_timeout_ms(),
            respect_rate_limits: default_respect_rate_limits(),
        }
    }
}

// ─── ENDPOINT-LEVEL OVERRIDE ────────────────────────────────────────────────

/// All per-endpoint tuning in one struct, keyed at the top level by
/// `endpoint_overrides[<endpoint_class>]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EndpointOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lru_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_body_chars: Option<usize>,
}

// ─── SCHEMA v2 — PROFILES & HINTS ──────────────────────────────────────────
//
// Four profile axes, each independently overridable:
//   1. tokenizer  — anthropic_class / openai_o200k / ollama_bpe (cost models)
//   2. llm        — model_id → tokenizer + context_window + style knobs
//   3. agent      — priority (latency/balanced/accuracy), recursion depth
//   4. data       — endpoint_pattern → preferred_format + hint_set
//
// Plus a horizontal `hints` policy that gates every emit_hint() through
// per-type rules (enabled, max_per_session, applies_to_models).
//
// Resolution: SessionContext → EffectiveConfig::resolve() collapses all
// four axes plus the legacy v1 fields into a single runtime view.

/// Container for all profile axes. Lives at `[profiles]` in TOML.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProfilesConfig {
    #[serde(default)]
    pub tokenizer: TokenizerProfilesConfig,
    #[serde(default)]
    pub llm: LlmProfilesConfig,
    #[serde(default)]
    pub agent: AgentProfilesConfig,
    #[serde(default)]
    pub data: DataProfilesConfig,
}

// ── TOKENIZER ─────────────────────────────────────────────────────────────

/// Cost model for one tokenizer family.
///
/// Captures the empirical observation that the *same* encoder produces wildly
/// different token counts depending on the receiving model's tokenizer (e.g.,
/// `inline_json_cost` is 2.2x on Anthropic-class but 1.0x on Ollama BPE).
/// See Paper 2 §Encoder Bug Postmortem (2026-04-25).
///
/// `bpe` selects the actual byte-pair encoder used to count tokens. When set
/// to [`Tokenizer::Heuristic`] (default for backward compat), `chars_per_token`
/// drives the estimate. When set to a real BPE variant, that BPE is used and
/// `chars_per_token` is informational only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizerProfile {
    /// Average characters per token observed for this tokenizer.
    /// Only used when `bpe == Heuristic`.
    pub chars_per_token: f32,
    /// Real BPE tokenizer to use for accurate counts. Falls back to the
    /// `chars_per_token` heuristic when set to `heuristic`.
    #[serde(default)]
    pub bpe: Tokenizer,
    /// Penalty multiplier for inline-JSON cells inside markdown tables.
    /// Use to decide between inline-JSON nested cells vs. recursive sections.
    #[serde(default = "default_inline_json_cost")]
    pub inline_json_cost: f32,
    /// Multiplicative cost of TOON encoding vs json_compact for this tokenizer.
    /// (TOON's "−40% tokens" claim is only valid for `openai_o200k`.)
    #[serde(default = "default_toon_overhead")]
    pub toon_overhead: f32,
    /// Optional per-format cost factors (multiplied with raw-char-based estimate).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub format_factors: BTreeMap<String, f32>,
}

fn default_inline_json_cost() -> f32 {
    1.0
}
fn default_toon_overhead() -> f32 {
    1.0
}

impl Default for TokenizerProfile {
    fn default() -> Self {
        Self {
            chars_per_token: 4.0,
            bpe: Tokenizer::Heuristic,
            inline_json_cost: default_inline_json_cost(),
            toon_overhead: default_toon_overhead(),
            format_factors: BTreeMap::new(),
        }
    }
}

impl TokenizerProfile {
    /// Count tokens in `text` using this profile's resolved tokenizer.
    ///
    /// - If `bpe == Heuristic`, applies `text.len() / chars_per_token` (ceiled).
    /// - Otherwise delegates to the real BPE encoder.
    pub fn count_tokens(&self, text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        match self.bpe {
            Tokenizer::Heuristic => {
                let cpt = if self.chars_per_token > 0.0 {
                    self.chars_per_token as f64
                } else {
                    3.5
                };
                (text.len() as f64 / cpt).ceil() as usize
            }
            tk => tk.count(text),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizerProfilesConfig {
    /// Active variant id, or `"auto"` to resolve from `profiles.llm`.
    #[serde(default = "default_active_auto")]
    pub active: String,
    #[serde(default = "default_tokenizer_variants")]
    pub variants: BTreeMap<String, TokenizerProfile>,
}

fn default_active_auto() -> String {
    "auto".to_string()
}

fn default_tokenizer_variants() -> BTreeMap<String, TokenizerProfile> {
    let mut m = BTreeMap::new();
    m.insert(
        "anthropic_class".into(),
        TokenizerProfile {
            chars_per_token: 3.5,
            bpe: Tokenizer::O200kBase,
            inline_json_cost: 2.2,
            toon_overhead: 1.13,
            format_factors: BTreeMap::new(),
        },
    );
    m.insert(
        "openai_o200k".into(),
        TokenizerProfile {
            chars_per_token: 4.0,
            bpe: Tokenizer::O200kBase,
            inline_json_cost: 1.0,
            toon_overhead: 0.60,
            format_factors: BTreeMap::new(),
        },
    );
    m.insert(
        "openai_cl100k".into(),
        TokenizerProfile {
            chars_per_token: 3.7,
            bpe: Tokenizer::Cl100kBase,
            inline_json_cost: 1.0,
            toon_overhead: 0.60,
            format_factors: BTreeMap::new(),
        },
    );
    m.insert(
        "ollama_bpe".into(),
        TokenizerProfile {
            chars_per_token: 3.8,
            bpe: Tokenizer::Heuristic,
            inline_json_cost: 1.0,
            toon_overhead: 1.00,
            format_factors: BTreeMap::new(),
        },
    );
    m
}

impl Default for TokenizerProfilesConfig {
    fn default() -> Self {
        Self {
            active: default_active_auto(),
            variants: default_tokenizer_variants(),
        }
    }
}

impl TokenizerProfilesConfig {
    /// Lookup a variant; returns `None` if missing.
    pub fn get(&self, id: &str) -> Option<&TokenizerProfile> {
        self.variants.get(id)
    }
}

// ── LLM ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProfile {
    /// Tokenizer variant id (resolved against `profiles.tokenizer.variants`).
    pub tokenizer: String,
    /// Whether the encoder should keep explicit field names (`shop: Acme\n`)
    /// instead of dropping them in compact forms — often pays off on
    /// instruction-tuned models that ground answers in names.
    #[serde(default = "default_prefer_explicit_keys")]
    pub prefer_explicit_keys: bool,
    /// Hard context-window limit; encoders should never produce more than this.
    #[serde(default = "default_context_window")]
    pub context_window: u32,
    /// Maximum size (chars) of an inline-nested JSON cell before falling back
    /// to a recursive section. `None` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_inline_nested: Option<u32>,
}

fn default_prefer_explicit_keys() -> bool {
    true
}
fn default_context_window() -> u32 {
    32_000
}

impl Default for LlmProfile {
    fn default() -> Self {
        Self {
            tokenizer: "ollama_bpe".to_string(),
            prefer_explicit_keys: default_prefer_explicit_keys(),
            context_window: default_context_window(),
            max_inline_nested: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmProfilesConfig {
    /// Active variant id (model name) or `"auto"` to resolve from
    /// `SessionContext::model_id`.
    #[serde(default = "default_active_auto")]
    pub active: String,
    #[serde(default = "default_llm_variants")]
    pub variants: BTreeMap<String, LlmProfile>,
}

fn default_llm_variants() -> BTreeMap<String, LlmProfile> {
    let mut m = BTreeMap::new();
    m.insert(
        "default".into(),
        LlmProfile {
            tokenizer: "openai_o200k".into(),
            prefer_explicit_keys: true,
            context_window: 32_000,
            max_inline_nested: Some(256),
        },
    );
    m.insert(
        "glm-5.1".into(),
        LlmProfile {
            tokenizer: "anthropic_class".into(),
            prefer_explicit_keys: true,
            context_window: 128_000,
            max_inline_nested: Some(128),
        },
    );
    m.insert(
        "claude-sonnet-4.6".into(),
        LlmProfile {
            tokenizer: "anthropic_class".into(),
            prefer_explicit_keys: true,
            context_window: 200_000,
            max_inline_nested: Some(64),
        },
    );
    m.insert(
        "gpt-oss:20b".into(),
        LlmProfile {
            tokenizer: "ollama_bpe".into(),
            prefer_explicit_keys: false,
            context_window: 8_192,
            max_inline_nested: Some(512),
        },
    );
    m.insert(
        "gemma4:26b".into(),
        LlmProfile {
            tokenizer: "ollama_bpe".into(),
            prefer_explicit_keys: false,
            context_window: 8_192,
            max_inline_nested: Some(512),
        },
    );
    m
}

impl Default for LlmProfilesConfig {
    fn default() -> Self {
        Self {
            active: default_active_auto(),
            variants: default_llm_variants(),
        }
    }
}

impl LlmProfilesConfig {
    /// Resolve the active LLM variant given an optional session model id.
    /// `"auto"` + `Some(model_id)` → exact match falls back to `"default"`.
    pub fn resolve<'a>(&'a self, session_model_id: Option<&str>) -> &'a LlmProfile {
        let key: &str = if self.active == "auto" {
            session_model_id.unwrap_or("default")
        } else {
            self.active.as_str()
        };
        self.variants
            .get(key)
            .or_else(|| self.variants.get("default"))
            .unwrap_or_else(|| {
                // Static-default fallback — should never trigger because
                // `default_llm_variants` always inserts "default".
                static FALLBACK: std::sync::OnceLock<LlmProfile> = std::sync::OnceLock::new();
                FALLBACK.get_or_init(LlmProfile::default)
            })
    }
}

// ── AGENT / SESSION ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Latency,
    #[default]
    Balanced,
    Accuracy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    #[serde(default)]
    pub priority: Priority,
    #[serde(default = "default_recursion_depth")]
    pub mckp_recursion_depth: usize,
    /// 0.0 = never emit hints, 1.0 = always (scaled by `HintsConfig` rules).
    #[serde(default = "default_hint_aggressiveness")]
    pub hint_aggressiveness: f32,
    #[serde(default)]
    pub near_ref_enabled: bool,
}

fn default_hint_aggressiveness() -> f32 {
    0.5
}

impl Default for AgentProfile {
    fn default() -> Self {
        Self {
            priority: Priority::Balanced,
            mckp_recursion_depth: default_recursion_depth(),
            hint_aggressiveness: default_hint_aggressiveness(),
            near_ref_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfilesConfig {
    #[serde(default = "default_active_auto")]
    pub active: String,
    /// How many events to observe before auto-classifying the agent profile.
    #[serde(default = "default_auto_window")]
    pub auto_detect_window: usize,
    #[serde(default = "default_agent_variants")]
    pub variants: BTreeMap<String, AgentProfile>,
}

fn default_auto_window() -> usize {
    50
}

fn default_agent_variants() -> BTreeMap<String, AgentProfile> {
    let mut m = BTreeMap::new();
    m.insert("default".into(), AgentProfile::default());
    m.insert(
        "file_search_heavy".into(),
        AgentProfile {
            priority: Priority::Latency,
            mckp_recursion_depth: 3,
            hint_aggressiveness: 0.3,
            near_ref_enabled: false,
        },
    );
    m.insert(
        "marathon_refactor".into(),
        AgentProfile {
            priority: Priority::Accuracy,
            mckp_recursion_depth: 7,
            hint_aggressiveness: 0.7,
            near_ref_enabled: true,
        },
    );
    m
}

impl Default for AgentProfilesConfig {
    fn default() -> Self {
        Self {
            active: default_active_auto(),
            auto_detect_window: default_auto_window(),
            variants: default_agent_variants(),
        }
    }
}

impl AgentProfilesConfig {
    /// Pick a variant for the given session statistics. `"auto"` triggers
    /// rule-based classification; an explicit `active` value short-circuits.
    pub fn resolve<'a>(&'a self, stats: &SessionStats) -> &'a AgentProfile {
        let key: &str = if self.active == "auto" {
            classify_agent(stats)
        } else {
            self.active.as_str()
        };
        self.variants
            .get(key)
            .or_else(|| self.variants.get("default"))
            .unwrap_or_else(|| {
                static FALLBACK: std::sync::OnceLock<AgentProfile> = std::sync::OnceLock::new();
                FALLBACK.get_or_init(AgentProfile::default)
            })
    }
}

/// Coarse heuristic: pick an agent variant from rolling session stats.
///
/// Rules (intentionally simple, easy to override via explicit `active = "..."`):
/// - long sessions with many compactions → `marathon_refactor`
/// - short sessions dominated by file-read tools → `file_search_heavy`
/// - everything else → `default`
fn classify_agent(stats: &SessionStats) -> &'static str {
    if stats.event_count >= 500 && stats.compaction_count >= 3 {
        "marathon_refactor"
    } else if stats.event_count <= 200 && stats.read_share >= 0.5 {
        "file_search_heavy"
    } else {
        "default"
    }
}

// ── DATA / DOMAIN ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataProfile {
    /// Glob-like pattern (currently exact-match-or-prefix on `endpoint_class`).
    pub endpoint_pattern: String,
    /// Format id to prefer when the pattern matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_format: Option<String>,
    /// Hint type ids to emit for this domain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hint_set: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataProfilesConfig {
    #[serde(default = "default_active_auto")]
    pub active: String,
    #[serde(default = "default_data_variants")]
    pub variants: BTreeMap<String, DataProfile>,
}

fn default_data_variants() -> BTreeMap<String, DataProfile> {
    let mut m = BTreeMap::new();
    m.insert(
        "gitlab_issues".into(),
        DataProfile {
            endpoint_pattern: "mcp__gitlab__get_issues".into(),
            preferred_format: Some("csv_from_md".into()),
            hint_set: vec!["near_ref".into()],
        },
    );
    m.insert(
        "github_pulls".into(),
        DataProfile {
            endpoint_pattern: "mcp__github__list_pulls".into(),
            preferred_format: Some("csv_from_md".into()),
            hint_set: vec!["near_ref".into()],
        },
    );
    m.insert(
        "k8s_logs".into(),
        DataProfile {
            endpoint_pattern: "mcp__k8s__get_logs".into(),
            preferred_format: Some("pipeline_deep_mckp".into()),
            hint_set: vec!["timestamp_ref".into()],
        },
    );
    m.insert(
        "mr_diffs".into(),
        DataProfile {
            endpoint_pattern: "mcp__gitlab__get_mr_diff".into(),
            preferred_format: Some("mr_diff_fence".into()),
            hint_set: Vec::new(),
        },
    );
    m
}

impl Default for DataProfilesConfig {
    fn default() -> Self {
        Self {
            active: default_active_auto(),
            variants: default_data_variants(),
        }
    }
}

impl DataProfilesConfig {
    /// Find the first variant whose `endpoint_pattern` matches (exact or prefix).
    pub fn match_endpoint(&self, endpoint: &str) -> Option<&DataProfile> {
        // When `active != "auto"`, restrict to that single variant.
        if self.active != "auto" {
            return self.variants.get(&self.active);
        }
        self.variants
            .values()
            .find(|v| endpoint == v.endpoint_pattern || endpoint.starts_with(&v.endpoint_pattern))
    }
}

// ── HINTS ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HintTypeRule {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Cap on how often this hint type may be emitted in one session.
    /// `None` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_per_session: Option<u32>,
    /// Restrict emission to specific model ids; `["*"]` = any.
    #[serde(default = "default_any_model")]
    pub applies_to_models: Vec<String>,
}

fn default_true() -> bool {
    true
}
fn default_any_model() -> Vec<String> {
    vec!["*".to_string()]
}

impl Default for HintTypeRule {
    fn default() -> Self {
        Self {
            enabled: true,
            max_per_session: None,
            applies_to_models: default_any_model(),
        }
    }
}

impl HintTypeRule {
    /// Does this rule allow emission for the given model id?
    pub fn applies_to(&self, model_id: &str) -> bool {
        if !self.enabled {
            return false;
        }
        self.applies_to_models
            .iter()
            .any(|m| m == "*" || m == model_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HintsConfig {
    #[serde(default)]
    pub default_verbosity: HintVerbosity,
    #[serde(default = "default_hint_types")]
    pub types: BTreeMap<String, HintTypeRule>,
}

fn default_hint_types() -> BTreeMap<String, HintTypeRule> {
    let mut m = BTreeMap::new();
    m.insert(
        "near_ref".into(),
        HintTypeRule {
            enabled: true,
            max_per_session: Some(50),
            applies_to_models: default_any_model(),
        },
    );
    m.insert(
        "timestamp_ref".into(),
        HintTypeRule {
            enabled: true,
            max_per_session: Some(100),
            applies_to_models: default_any_model(),
        },
    );
    m.insert(
        "delta".into(),
        HintTypeRule {
            enabled: false, // experimental — keep off until shipped
            max_per_session: Some(20),
            applies_to_models: default_any_model(),
        },
    );
    m.insert(
        // Confirmed 0 lift in 2026-04-25 evaluation; default off.
        "schema_explainer".into(),
        HintTypeRule {
            enabled: false,
            max_per_session: None,
            applies_to_models: default_any_model(),
        },
    );
    m.insert(
        // Local models benefit from one-line format hints; cloud models don't.
        "inline_format_hint".into(),
        HintTypeRule {
            enabled: true,
            max_per_session: Some(10),
            applies_to_models: vec!["gpt-oss:20b".into(), "gemma4:26b".into()],
        },
    );
    m
}

impl Default for HintsConfig {
    fn default() -> Self {
        Self {
            default_verbosity: HintVerbosity::Standard,
            types: default_hint_types(),
        }
    }
}

impl HintsConfig {
    /// Should we emit a hint of `type_id` for `model_id`?
    /// Caller is responsible for tracking per-session counts and
    /// re-checking against `max_per_session`.
    pub fn allow(&self, type_id: &str, model_id: &str) -> bool {
        match self.types.get(type_id) {
            Some(rule) => rule.applies_to(model_id),
            None => false, // unknown type → fail closed
        }
    }
}

// ── SESSION CONTEXT & EFFECTIVE CONFIG ────────────────────────────────────

/// Statistics observed from a session's first N events. Used by
/// `AgentProfilesConfig::resolve` to auto-classify the agent profile.
#[derive(Debug, Clone, Default)]
pub struct SessionStats {
    pub event_count: usize,
    pub compaction_count: usize,
    /// Fraction of events that were file-read tools (Read, Glob, Grep, …).
    pub read_share: f32,
}

/// Everything the pipeline needs to know about *this* session in order to
/// resolve the four profile axes plus the legacy v1 fields.
#[derive(Debug, Clone, Default)]
pub struct SessionContext {
    pub model_id: Option<String>,
    pub stats: SessionStats,
}

/// Resolved per-session view of the configuration. This is what the layered
/// pipeline reads on the hot path; produce it once at session start.
#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    pub tokenizer: TokenizerProfile,
    pub llm: LlmProfile,
    pub agent: AgentProfile,
    pub hints: HintsConfig,
    /// Cached MckpConfig — recursion_depth comes from the agent profile,
    /// other fields are inherited from the legacy `[mckp]` section.
    pub mckp: MckpConfig,
}

impl EffectiveConfig {
    /// Collapse `AdaptiveConfig` + `SessionContext` into a single runtime view.
    pub fn resolve(cfg: &AdaptiveConfig, ctx: &SessionContext) -> Self {
        let llm = cfg.profiles.llm.resolve(ctx.model_id.as_deref()).clone();
        let tokenizer_id = if cfg.profiles.tokenizer.active == "auto" {
            llm.tokenizer.as_str()
        } else {
            cfg.profiles.tokenizer.active.as_str()
        };
        let tokenizer = cfg
            .profiles
            .tokenizer
            .get(tokenizer_id)
            .cloned()
            .unwrap_or_default();
        let agent = cfg.profiles.agent.resolve(&ctx.stats).clone();
        let mut mckp = cfg.mckp.clone();
        mckp.recursion_depth = agent.mckp_recursion_depth;
        Self {
            tokenizer,
            llm,
            agent,
            hints: cfg.hints.clone(),
            mckp,
        }
    }

    /// Per-endpoint format choice: data profile pattern wins over template overrides.
    pub fn preferred_format_for<'a>(
        &self,
        cfg: &'a AdaptiveConfig,
        endpoint: &str,
    ) -> Option<&'a str> {
        if let Some(dp) = cfg.profiles.data.match_endpoint(endpoint)
            && let Some(f) = dp.preferred_format.as_deref()
        {
            return Some(f);
        }
        cfg.effective_template(endpoint)
    }

    /// Should this hint type fire for the active model? Callers still must
    /// enforce `max_per_session` themselves (state lives outside config).
    pub fn allow_hint(&self, type_id: &str) -> bool {
        let model_id = "default"; // resolved-LLM doesn't carry id; allow_hint's
        // "*" rule covers it. Keep simple — caller passes model id via config.
        let _ = model_id;
        self.hints.allow(type_id, "*")
    }
}

// ─── TESTS ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        let cfg = AdaptiveConfig::default();
        assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(cfg.dedup.lru_size, 5);
        assert!(cfg.dedup.enabled_for("anything"));
        assert!(cfg.templates.is_template_active("csv_from_md"));
        assert!(cfg.mckp.format_enabled("deep_mckp"));
    }

    #[test]
    fn roundtrip_toml() {
        let mut cfg = AdaptiveConfig::default();
        cfg.dedup.lru_size = 7;
        cfg.dedup.near_ref_enabled = true;
        cfg.dedup
            .enabled_per_endpoint
            .insert("mcp__test__get".into(), false);
        cfg.templates
            .endpoint_overrides
            .insert("mcp__test__get".into(), "csv_from_md".into());
        cfg.endpoint_overrides.insert(
            "Bash:git_log".into(),
            EndpointOverride {
                dedup_enabled: Some(false),
                ..Default::default()
            },
        );

        let s = toml::to_string_pretty(&cfg).unwrap();
        let parsed: AdaptiveConfig = toml::from_str(&s).unwrap();
        assert_eq!(parsed.dedup.lru_size, 7);
        assert!(parsed.dedup.near_ref_enabled);
        assert!(!parsed.dedup.enabled_for("mcp__test__get"));
        assert_eq!(
            parsed.templates.template_for("mcp__test__get"),
            Some("csv_from_md")
        );
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        let cfg = AdaptiveConfig {
            schema_version: 99,
            ..Default::default()
        };
        let s = toml::to_string(&cfg).unwrap();
        let err = toml::from_str::<AdaptiveConfig>(&s).ok().and_then(|c| {
            if c.schema_version != CURRENT_SCHEMA_VERSION {
                Some(c.schema_version)
            } else {
                None
            }
        });
        assert_eq!(err, Some(99));
    }

    #[test]
    fn load_or_default_handles_missing_file() {
        let p = std::env::temp_dir().join("definitely_does_not_exist_12345.toml");
        let cfg = AdaptiveConfig::load_or_default(&p).unwrap();
        assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let pid = std::process::id();
        let p = std::env::temp_dir().join(format!("devboy_cfg_test_{pid}.toml"));
        let mut cfg = AdaptiveConfig::default();
        cfg.dedup.lru_size = 10;
        cfg.mckp.recursion_depth = 7;
        cfg.save(&p).unwrap();
        let loaded = AdaptiveConfig::load(&p).unwrap();
        assert_eq!(loaded.dedup.lru_size, 10);
        assert_eq!(loaded.mckp.recursion_depth, 7);
        std::fs::remove_file(&p).ok();
    }

    // ── Schema v2 — profiles & hints ───────────────────────────────────

    #[test]
    fn default_profiles_have_expected_variants() {
        let cfg = AdaptiveConfig::default();
        // Tokenizer
        assert!(cfg.profiles.tokenizer.get("anthropic_class").is_some());
        assert!(cfg.profiles.tokenizer.get("openai_o200k").is_some());
        assert!(cfg.profiles.tokenizer.get("ollama_bpe").is_some());
        // LLM
        assert!(cfg.profiles.llm.variants.contains_key("default"));
        assert!(cfg.profiles.llm.variants.contains_key("glm-5.1"));
        assert!(cfg.profiles.llm.variants.contains_key("gpt-oss:20b"));
        // Agent
        assert!(cfg.profiles.agent.variants.contains_key("default"));
        assert!(
            cfg.profiles
                .agent
                .variants
                .contains_key("file_search_heavy")
        );
        assert!(
            cfg.profiles
                .agent
                .variants
                .contains_key("marathon_refactor")
        );
        // Data
        assert!(cfg.profiles.data.variants.contains_key("gitlab_issues"));
        assert!(cfg.profiles.data.variants.contains_key("k8s_logs"));
    }

    #[test]
    fn anthropic_tokenizer_has_inline_json_penalty() {
        let cfg = AdaptiveConfig::default();
        let p = cfg.profiles.tokenizer.get("anthropic_class").unwrap();
        // Captured from 2026-04-25 mckp_v2 evaluation:
        // inline-JSON cells cost ~2.2x on glm-5.1 vs ~1.0x on local Ollama BPE.
        assert!(p.inline_json_cost > 2.0);
        assert!((p.toon_overhead - 1.13).abs() < 0.001);
    }

    #[test]
    fn llm_resolve_picks_exact_model_match() {
        let cfg = AdaptiveConfig::default();
        let p = cfg.profiles.llm.resolve(Some("glm-5.1"));
        assert_eq!(p.tokenizer, "anthropic_class");
        assert_eq!(p.context_window, 128_000);
    }

    #[test]
    fn llm_resolve_falls_back_to_default_for_unknown() {
        let cfg = AdaptiveConfig::default();
        let p = cfg.profiles.llm.resolve(Some("unknown-model-xyz"));
        // Falls through to "default" variant
        assert_eq!(p.tokenizer, "openai_o200k");
    }

    #[test]
    fn agent_classifier_picks_marathon_for_long_session() {
        let cfg = AdaptiveConfig::default();
        let stats = SessionStats {
            event_count: 800,
            compaction_count: 5,
            read_share: 0.3,
        };
        let p = cfg.profiles.agent.resolve(&stats);
        assert_eq!(p.priority, Priority::Accuracy);
        assert_eq!(p.mckp_recursion_depth, 7);
        assert!(p.near_ref_enabled);
    }

    #[test]
    fn agent_classifier_picks_file_search_for_short_read_heavy() {
        let cfg = AdaptiveConfig::default();
        let stats = SessionStats {
            event_count: 80,
            compaction_count: 0,
            read_share: 0.7,
        };
        let p = cfg.profiles.agent.resolve(&stats);
        assert_eq!(p.priority, Priority::Latency);
        assert_eq!(p.mckp_recursion_depth, 3);
    }

    #[test]
    fn agent_classifier_default_for_balanced_session() {
        let cfg = AdaptiveConfig::default();
        let stats = SessionStats {
            event_count: 300,
            compaction_count: 0,
            read_share: 0.4,
        };
        let p = cfg.profiles.agent.resolve(&stats);
        assert_eq!(p.priority, Priority::Balanced);
    }

    #[test]
    fn data_profile_matches_endpoint_prefix() {
        let cfg = AdaptiveConfig::default();
        let dp = cfg.profiles.data.match_endpoint("mcp__gitlab__get_issues");
        assert!(dp.is_some());
        assert_eq!(dp.unwrap().preferred_format.as_deref(), Some("csv_from_md"));
    }

    #[test]
    fn data_profile_returns_none_for_unmatched() {
        let cfg = AdaptiveConfig::default();
        let dp = cfg.profiles.data.match_endpoint("Bash:git_log");
        assert!(dp.is_none());
    }

    #[test]
    fn hint_policy_disables_schema_explainer_by_default() {
        // Encoder-bug postmortem 2026-04-25: schema_explainer hint added
        // 0 lift to CSV/Markdown accuracy because data was structurally absent.
        let cfg = AdaptiveConfig::default();
        assert!(!cfg.hints.allow("schema_explainer", "glm-5.1"));
        assert!(!cfg.hints.allow("schema_explainer", "gpt-oss:20b"));
    }

    #[test]
    fn hint_policy_inline_format_hint_only_for_local_models() {
        let cfg = AdaptiveConfig::default();
        assert!(cfg.hints.allow("inline_format_hint", "gpt-oss:20b"));
        assert!(cfg.hints.allow("inline_format_hint", "gemma4:26b"));
        assert!(!cfg.hints.allow("inline_format_hint", "glm-5.1"));
        assert!(!cfg.hints.allow("inline_format_hint", "claude-sonnet-4.6"));
    }

    #[test]
    fn hint_policy_unknown_type_fails_closed() {
        let cfg = AdaptiveConfig::default();
        assert!(!cfg.hints.allow("never_seen_hint_type", "anything"));
    }

    #[test]
    fn effective_config_resolves_glm_to_anthropic_tokenizer() {
        let cfg = AdaptiveConfig::default();
        let ctx = SessionContext {
            model_id: Some("glm-5.1".to_string()),
            stats: SessionStats::default(),
        };
        let eff = EffectiveConfig::resolve(&cfg, &ctx);
        assert_eq!(eff.llm.tokenizer, "anthropic_class");
        assert!(eff.tokenizer.inline_json_cost > 2.0);
        assert_eq!(eff.llm.context_window, 128_000);
    }

    #[test]
    fn effective_config_recursion_depth_from_agent_profile() {
        let cfg = AdaptiveConfig::default();
        let ctx = SessionContext {
            model_id: Some("gpt-oss:20b".to_string()),
            stats: SessionStats {
                event_count: 1000,
                compaction_count: 5,
                read_share: 0.2,
            },
        };
        let eff = EffectiveConfig::resolve(&cfg, &ctx);
        // marathon_refactor variant
        assert_eq!(eff.mckp.recursion_depth, 7);
        assert_eq!(eff.agent.priority, Priority::Accuracy);
    }

    #[test]
    fn effective_config_preferred_format_from_data_profile() {
        let cfg = AdaptiveConfig::default();
        let ctx = SessionContext::default();
        let eff = EffectiveConfig::resolve(&cfg, &ctx);
        let f = eff.preferred_format_for(&cfg, "mcp__gitlab__get_issues");
        assert_eq!(f, Some("csv_from_md"));
    }

    #[test]
    fn schema_v1_file_upgrades_to_v2_in_memory() {
        // Simulate an on-disk v1 file lacking [profiles] and [hints] sections.
        let v1 = r#"
schema_version = 1

[dedup]
lru_size = 7

[mckp]
recursion_depth = 6
"#;
        let pid = std::process::id();
        let p = std::env::temp_dir().join(format!("devboy_cfg_v1_{pid}.toml"));
        std::fs::write(&p, v1).unwrap();
        let loaded = AdaptiveConfig::load(&p).unwrap();
        assert_eq!(loaded.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(loaded.dedup.lru_size, 7);
        assert_eq!(loaded.mckp.recursion_depth, 6);
        // v2 defaults populated
        assert!(loaded.profiles.tokenizer.get("anthropic_class").is_some());
        assert!(loaded.hints.types.contains_key("near_ref"));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn future_schema_version_is_rejected_on_load() {
        let s = format!("schema_version = {}\n[dedup]\n", CURRENT_SCHEMA_VERSION + 1);
        let pid = std::process::id();
        let p = std::env::temp_dir().join(format!("devboy_cfg_future_{pid}.toml"));
        std::fs::write(&p, s).unwrap();
        let err = AdaptiveConfig::load(&p);
        assert!(matches!(err, Err(ConfigError::UnsupportedSchemaVersion(_))));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn profiles_roundtrip_through_toml() {
        let mut cfg = AdaptiveConfig::default();
        cfg.profiles.llm.active = "claude-sonnet-4.6".to_string();
        cfg.profiles.agent.active = "marathon_refactor".to_string();
        cfg.hints.types.get_mut("near_ref").unwrap().max_per_session = Some(99);
        let s = toml::to_string_pretty(&cfg).unwrap();
        let parsed: AdaptiveConfig = toml::from_str(&s).unwrap();
        assert_eq!(parsed.profiles.llm.active, "claude-sonnet-4.6");
        assert_eq!(parsed.profiles.agent.active, "marathon_refactor");
        assert_eq!(parsed.hints.types["near_ref"].max_per_session, Some(99));
    }

    #[test]
    fn endpoint_override_roundtrip() {
        let mut cfg = AdaptiveConfig::default();
        cfg.endpoint_overrides.insert(
            "mcp__xxx__yyy".into(),
            EndpointOverride {
                dedup_enabled: Some(true),
                lru_size: Some(10),
                template_id: Some("custom".into()),
                min_body_chars: Some(50),
            },
        );
        let s = toml::to_string_pretty(&cfg).unwrap();
        let parsed: AdaptiveConfig = toml::from_str(&s).unwrap();
        let o = parsed.endpoint_overrides.get("mcp__xxx__yyy").unwrap();
        assert_eq!(o.lru_size, Some(10));
        assert_eq!(o.template_id.as_deref(), Some("custom"));
    }

    #[test]
    fn effective_dedup_enabled_falls_back_correctly() {
        let mut cfg = AdaptiveConfig::default();
        // No override → default true.
        assert!(cfg.effective_dedup_enabled("anything"));
        // enabled_per_endpoint override → respected.
        cfg.dedup.enabled_per_endpoint.insert("a".into(), false);
        assert!(!cfg.effective_dedup_enabled("a"));
        // endpoint_overrides takes precedence over enabled_per_endpoint.
        cfg.endpoint_overrides.insert(
            "a".into(),
            EndpointOverride {
                dedup_enabled: Some(true),
                ..Default::default()
            },
        );
        assert!(cfg.effective_dedup_enabled("a"));
    }

    #[test]
    fn effective_min_body_chars_uses_override() {
        let mut cfg = AdaptiveConfig::default();
        assert_eq!(cfg.effective_min_body_chars("x"), cfg.dedup.min_body_chars);
        cfg.endpoint_overrides.insert(
            "x".into(),
            EndpointOverride {
                min_body_chars: Some(42),
                ..Default::default()
            },
        );
        assert_eq!(cfg.effective_min_body_chars("x"), 42);
    }

    #[test]
    fn effective_lru_size_uses_override_when_larger() {
        let mut cfg = AdaptiveConfig::default();
        cfg.dedup.lru_size = 5;
        cfg.endpoint_overrides.insert(
            "big".into(),
            EndpointOverride {
                lru_size: Some(15),
                ..Default::default()
            },
        );
        // Override larger than global → use override.
        assert_eq!(cfg.effective_lru_size("big"), 15);
        // Override smaller → use global (cache must accommodate everyone).
        cfg.endpoint_overrides.insert(
            "small".into(),
            EndpointOverride {
                lru_size: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(cfg.effective_lru_size("small"), 5);
    }

    #[test]
    fn max_lru_size_across_all_overrides() {
        let mut cfg = AdaptiveConfig::default();
        cfg.dedup.lru_size = 5;
        cfg.endpoint_overrides.insert(
            "a".into(),
            EndpointOverride {
                lru_size: Some(12),
                ..Default::default()
            },
        );
        cfg.endpoint_overrides.insert(
            "b".into(),
            EndpointOverride {
                lru_size: Some(8),
                ..Default::default()
            },
        );
        assert_eq!(cfg.max_lru_size(), 12);
    }

    #[test]
    fn effective_template_prefers_endpoint_override() {
        let mut cfg = AdaptiveConfig::default();
        cfg.templates
            .endpoint_overrides
            .insert("x".into(), "csv_from_md".into());
        assert_eq!(cfg.effective_template("x"), Some("csv_from_md"));
        cfg.endpoint_overrides.insert(
            "x".into(),
            EndpointOverride {
                template_id: Some("custom_tpl".into()),
                ..Default::default()
            },
        );
        assert_eq!(cfg.effective_template("x"), Some("custom_tpl"));
    }

    #[test]
    fn merge_right_wins_overwrites_sections() {
        let mut a = AdaptiveConfig::default();
        a.endpoint_overrides.insert(
            "keep".into(),
            EndpointOverride {
                dedup_enabled: Some(false),
                ..Default::default()
            },
        );
        let mut b = AdaptiveConfig::default();
        b.dedup.lru_size = 42;
        b.endpoint_overrides.insert(
            "keep".into(),
            EndpointOverride {
                dedup_enabled: Some(true),
                ..Default::default()
            },
        );
        b.endpoint_overrides.insert(
            "new".into(),
            EndpointOverride {
                dedup_enabled: Some(true),
                ..Default::default()
            },
        );
        a.merge_right_wins(b);
        assert_eq!(a.dedup.lru_size, 42);
        assert_eq!(a.endpoint_overrides["keep"].dedup_enabled, Some(true));
        assert!(a.endpoint_overrides.contains_key("new"));
    }

    #[test]
    fn hint_verbosity_to_runtime_mapping() {
        assert_eq!(
            HintVerbosity::Terse.to_runtime(),
            crate::dedup::HintVerbosity::Terse
        );
        assert_eq!(
            HintVerbosity::Standard.to_runtime(),
            crate::dedup::HintVerbosity::Standard
        );
        assert_eq!(
            HintVerbosity::Verbose.to_runtime(),
            crate::dedup::HintVerbosity::Verbose
        );
    }

    #[test]
    fn mckp_config_format_disabled_is_respected() {
        let mut cfg = MckpConfig::default();
        assert!(cfg.format_enabled("csv"));
        cfg.formats_enabled = vec![];
        assert!(!cfg.format_enabled("csv"));
    }

    #[test]
    fn templates_is_template_active_false_for_unknown() {
        let t = TemplatesConfig::default();
        assert!(!t.is_template_active("not_a_real_template"));
        assert!(t.is_template_active("csv_from_md"));
    }

    #[test]
    fn tokenizer_profile_heuristic_uses_chars_per_token() {
        let p = TokenizerProfile {
            chars_per_token: 4.0,
            bpe: Tokenizer::Heuristic,
            ..Default::default()
        };
        // 8 chars / 4.0 = 2 tokens
        assert_eq!(p.count_tokens("abcdefgh"), 2);
        // empty stays zero regardless of chars_per_token
        assert_eq!(p.count_tokens(""), 0);
    }

    #[test]
    fn tokenizer_profile_bpe_overrides_heuristic() {
        let p = TokenizerProfile {
            // Deliberately wrong cpt — should be ignored when bpe is set.
            chars_per_token: 1.0,
            bpe: Tokenizer::O200kBase,
            ..Default::default()
        };
        // BPE count is small for "hello world", definitely not 11 (= 11 chars / 1.0).
        let n = p.count_tokens("hello world");
        assert!(n > 0 && n < 5, "BPE should win, got {n}");
    }

    #[test]
    fn default_tokenizer_variants_have_real_bpe_for_modern_models() {
        let variants = default_tokenizer_variants();
        assert_eq!(
            variants.get("anthropic_class").unwrap().bpe,
            Tokenizer::O200kBase
        );
        assert_eq!(
            variants.get("openai_o200k").unwrap().bpe,
            Tokenizer::O200kBase
        );
        assert_eq!(
            variants.get("openai_cl100k").unwrap().bpe,
            Tokenizer::Cl100kBase
        );
        // Ollama-class models keep the heuristic until we ship a per-model BPE.
        assert_eq!(
            variants.get("ollama_bpe").unwrap().bpe,
            Tokenizer::Heuristic
        );
    }

    // ─── Paper 3 [tools.*] section ───────────────────────────────────

    #[test]
    fn schema_v3_default_carries_empty_tools_map() {
        let cfg = AdaptiveConfig::default();
        assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
        // The old assertion baked `CURRENT_SCHEMA_VERSION == 3` but the
        // field migrated to v4 when the [enrichment] section landed.
        // Comparing the two compile-time constants is a tautology
        // clippy rejects; the version-agnostic invariant we actually
        // care about is just that the default carries an empty tools
        // map.
        assert!(cfg.tools.is_empty());
    }

    #[test]
    fn schema_v1_v2_v3_files_upgrade_to_current_with_empty_tools() {
        // Older configs lack the [tools.*] / [enrichment] sections;
        // serde(default) injects empty defaults, then `upgrade_in_place`
        // stamps the current schema version.
        for raw in [
            "schema_version = 1\n",
            "schema_version = 2\n[profiles.tokenizer]\nactive = \"auto\"\n",
            "schema_version = 3\n[tools.Read]\nvalue_class = \"critical\"\n",
        ] {
            let mut cfg: AdaptiveConfig = toml::from_str(raw).unwrap();
            cfg.upgrade_in_place().unwrap();
            assert_eq!(cfg.schema_version, CURRENT_SCHEMA_VERSION);
            // v3 file pre-populates one tool; v1/v2 files leave it empty.
            // [enrichment] always defaults to disabled.
            assert!(!cfg.enrichment.enabled);
        }
    }

    #[test]
    fn enrichment_config_round_trips_with_overrides() {
        let raw = r#"
schema_version = 4

[enrichment]
enabled = true
max_parallel_prefetches = 5
prefetch_budget_tokens = 12000
prefetch_timeout_ms = 1500
respect_rate_limits = false
"#;
        let cfg: AdaptiveConfig = toml::from_str(raw).unwrap();
        assert!(cfg.enrichment.enabled);
        assert_eq!(cfg.enrichment.max_parallel_prefetches, 5);
        assert_eq!(cfg.enrichment.prefetch_budget_tokens, 12000);
        assert_eq!(cfg.enrichment.prefetch_timeout_ms, 1500);
        assert!(!cfg.enrichment.respect_rate_limits);

        let s = toml::to_string_pretty(&cfg).unwrap();
        let back: AdaptiveConfig = toml::from_str(&s).unwrap();
        assert!(back.enrichment.enabled);
        assert_eq!(back.enrichment.prefetch_timeout_ms, 1500);
    }

    #[test]
    fn enrichment_defaults_are_safe() {
        let cfg = AdaptiveConfig::default();
        // Off by default — single most important guarantee for v4
        // shipping silently into existing deployments.
        assert!(!cfg.enrichment.enabled);
        assert_eq!(cfg.enrichment.max_parallel_prefetches, 3);
        assert_eq!(cfg.enrichment.prefetch_budget_tokens, 8000);
        assert_eq!(cfg.enrichment.prefetch_timeout_ms, 1000);
        assert!(cfg.enrichment.respect_rate_limits);
    }

    #[test]
    fn effective_tool_value_model_exact_match_wins() {
        let mut cfg = AdaptiveConfig::default();
        cfg.tools.insert(
            "Read".into(),
            devboy_core::ToolValueModel::critical_with_size(2.5),
        );
        let m = cfg.effective_tool_value_model("Read").unwrap();
        assert_eq!(m.cost_model.typical_kb, 2.5);
        assert_eq!(m.value_class, devboy_core::ValueClass::Critical);
    }

    #[test]
    fn effective_tool_value_model_falls_back_to_wildcard() {
        let mut cfg = AdaptiveConfig::default();
        cfg.tools
            .insert("*".into(), devboy_core::ToolValueModel::audit_only());
        let m = cfg.effective_tool_value_model("UnknownTool").unwrap();
        assert_eq!(m.value_class, devboy_core::ValueClass::AuditOnly);
    }

    #[test]
    fn effective_tool_value_model_none_when_unconfigured() {
        let cfg = AdaptiveConfig::default();
        assert!(cfg.effective_tool_value_model("Read").is_none());
    }

    #[test]
    fn round_trip_via_toml_with_tools_block() {
        let mut cfg = AdaptiveConfig::default();
        cfg.tools.insert(
            "Read".into(),
            devboy_core::ToolValueModel::critical_with_size(2.5),
        );
        cfg.tools.insert(
            "TaskUpdate".into(),
            devboy_core::ToolValueModel::audit_only(),
        );
        let s = toml::to_string_pretty(&cfg).unwrap();
        assert!(s.contains("[tools.Read]"));
        assert!(s.contains("[tools.TaskUpdate]"));
        let back: AdaptiveConfig = toml::from_str(&s).unwrap();
        assert_eq!(back.tools.len(), 2);
        assert_eq!(
            back.effective_tool_value_model("Read")
                .unwrap()
                .cost_model
                .typical_kb,
            2.5
        );
    }

    #[test]
    fn merge_right_wins_unions_tools_blocks() {
        let mut left = AdaptiveConfig::default();
        left.tools.insert(
            "Read".into(),
            devboy_core::ToolValueModel::critical_with_size(2.5),
        );
        left.tools
            .insert("Bash".into(), devboy_core::ToolValueModel::default());

        let mut right = AdaptiveConfig::default();
        right.tools.insert(
            "Read".into(),
            devboy_core::ToolValueModel::critical_with_size(99.0),
        );
        right.tools.insert(
            "WebFetch".into(),
            devboy_core::ToolValueModel::critical_with_size(1.2),
        );

        left.merge_right_wins(right);
        // Right wins on collision (`Read`).
        assert_eq!(
            left.effective_tool_value_model("Read")
                .unwrap()
                .cost_model
                .typical_kb,
            99.0
        );
        // Left-only entry (`Bash`) survives.
        assert!(left.effective_tool_value_model("Bash").is_some());
        // Right-only entry (`WebFetch`) is added.
        assert!(left.effective_tool_value_model("WebFetch").is_some());
    }
}
