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

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("adaptive-config I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("adaptive-config parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("adaptive-config serialize: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("adaptive-config unsupported schema version {0} (expected 1)")]
    UnsupportedSchemaVersion(u32),
}

pub type Result<T> = std::result::Result<T, ConfigError>;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

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
        let cfg: AdaptiveConfig = toml::from_str(&s)?;
        if cfg.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchemaVersion(cfg.schema_version));
        }
        Ok(cfg)
    }

    /// Strict load — fails if the file is missing.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let s = fs::read_to_string(path)?;
        let cfg: AdaptiveConfig = toml::from_str(&s)?;
        if cfg.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchemaVersion(cfg.schema_version));
        }
        Ok(cfg)
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

    /// Merge another config into self. Fields present in `other` override `self`.
    /// Endpoint overrides are unioned (right-wins on collisions).
    pub fn merge_right_wins(&mut self, other: AdaptiveConfig) {
        self.dedup = other.dedup;
        self.templates = other.templates;
        self.mckp = other.mckp;
        self.telemetry = other.telemetry;
        for (k, v) in other.endpoint_overrides {
            self.endpoint_overrides.insert(k, v);
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
    pub fn is_template_active(&self, id: &str) -> bool {
        self.active.iter().any(|s| s == id)
    }
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
    /// Fraction of events to record (1.0 = all).
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f32,
    /// Flush the sink every N recorded events.
    #[serde(default = "default_flush_every")]
    pub flush_every_n: usize,
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
            sample_rate: default_sample_rate(),
            flush_every_n: default_flush_every(),
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
        assert_eq!(
            a.endpoint_overrides["keep"].dedup_enabled,
            Some(true)
        );
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
}
