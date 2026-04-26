//! Tool value model — Paper 3 §"how to build tools right".
//!
//! [`ToolValueModel`] is a machine-readable description that every
//! provider attaches to each tool it ships. The Paper 3 enrichment
//! planner reads these models to decide which tools to call, in what
//! order, and which fields of each response are worth keeping under a
//! given turn budget.
//!
//! Design echoes Paper 2's `[profiles.data]` axis: the model is plain
//! `serde`-compatible data, ships with sensible per-provider defaults,
//! and can be overridden through a `[tools.<name>]` block in
//! `pipeline_config.toml` without recompiling. The schema lives here
//! (in `devboy-core`) so provider crates can populate it without taking
//! a dependency on the executor or the pipeline.
//!
//! See `docs/research/paper3_corpus_findings.md` for the empirical
//! basis of the default values, and Paper 3 (issue tracker P-3) for
//! the planner that consumes them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How important the tool's output is to the agent's task.
///
/// The planner uses this as the *first-pass* filter when budget is
/// tight: `Critical` tools are kept whatever the cost; `AuditOnly`
/// tools never enter the budget calculation; `Supporting` and
/// `Optional` are dropped in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ValueClass {
    /// File contents, search results — must always be included.
    #[default]
    Critical,
    /// Useful context that improves answers but is not load-bearing.
    Supporting,
    /// Nice-to-have. First to be dropped under tight budget.
    Optional,
    /// Metadata about the agent's own plan (TaskUpdate, TodoWrite,
    /// telemetry pings). Kept in trace for analysis but never spent
    /// against the per-turn budget.
    AuditOnly,
}

/// One named subset of fields from a tool's response. Providers carve
/// the full result into groups so the planner can drop low-value
/// fields without dropping the call entirely.
///
/// Conventionally a tool ships at least:
///
/// - `must_have` — fields required for the response to be useful;
/// - `nice_to_have` — informative but droppable under budget;
/// - `debug` — low-value diagnostics, dropped first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldGroup {
    /// JSON pointer-style field paths (e.g. `"id"`, `"author.email"`).
    /// Empty list means "all remaining fields" — used by the
    /// `nice_to_have` group as a wildcard.
    #[serde(default)]
    pub fields: Vec<String>,

    /// Expected value contribution of this group, on a 0.0–1.0 scale,
    /// relative to the tool's total `must_have` value. The planner
    /// multiplies this by the tool's `value_class` to get the absolute
    /// value-per-token used in the knapsack.
    #[serde(default = "default_estimated_value")]
    pub estimated_value: f32,

    /// Whether the planner should include this group by default.
    /// `false` means "opt-in" — only included when the user intent
    /// explicitly mentions one of the fields.
    #[serde(default = "default_include_true")]
    pub default_include: bool,
}

fn default_estimated_value() -> f32 {
    0.5
}
fn default_include_true() -> bool {
    true
}

impl Default for FieldGroup {
    fn default() -> Self {
        Self {
            fields: Vec::new(),
            estimated_value: default_estimated_value(),
            default_include: default_include_true(),
        }
    }
}

/// What the call costs in tokens, latency, dollars, and how long the
/// result stays valid in cache.
///
/// The numbers are the planner's *prior*; the actual telemetry from
/// `PipelineEvent` updates them via `tune analyze` (Paper 2 idiom).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CostModel {
    /// Median response size in kilobytes — informs the knapsack
    /// `cost` term. Anchored on the corpus mining in
    /// `docs/research/paper3_corpus_findings.md`.
    #[serde(default = "default_typical_kb")]
    pub typical_kb: f32,

    /// p99 response size — the planner uses this for the *worst-case*
    /// budget reservation when it cannot afford to overshoot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_kb: Option<f32>,

    /// Median end-to-end latency. `None` = unknown, treated as 0
    /// in the planner's latency-aware mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms_p50: Option<u32>,

    /// Per-call dollar cost for paid APIs (Anthropic, OpenAI, …).
    /// `None` = free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dollars: Option<f32>,

    /// How long a cached response stays valid before the planner must
    /// refetch. Used by L0 dedup: a polling endpoint with
    /// `freshness_ttl_s = 15` returns the cached body for 15 s and
    /// then collapses to a near-ref hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness_ttl_s: Option<u32>,
}

fn default_typical_kb() -> f32 {
    1.0
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            typical_kb: default_typical_kb(),
            max_kb: None,
            latency_ms_p50: None,
            dollars: None,
            freshness_ttl_s: None,
        }
    }
}

/// Edge in the empirically observed follow-up graph. After tool A
/// fires, the planner consults A's `follow_up` list to decide which
/// tools to *speculatively* prefetch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FollowUpLink {
    /// Name of the tool that typically fires next.
    pub tool: String,

    /// Probability of this follow-up firing, mined from corpus.
    /// Range 0.0–1.0; 0.5+ is a reasonable prefetch threshold.
    #[serde(default = "default_followup_probability")]
    pub probability: f32,

    /// Optional argument projection — name of the field from the
    /// previous response to forward as an argument to `tool`. For
    /// example, `Glob.follow_up = [{tool: "Read", projection: "match_path"}]`
    /// tells the planner to feed each glob result's path to a `Read`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<String>,
}

fn default_followup_probability() -> f32 {
    0.5
}

/// Provider-shipped, user-overridable description of how a tool fits
/// into the enrichment knapsack.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolValueModel {
    /// First-pass importance class.
    #[serde(default)]
    pub value_class: ValueClass,

    /// Named subsets of the response — `must_have`, `nice_to_have`,
    /// `debug`, and any provider-specific groups.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub field_groups: BTreeMap<String, FieldGroup>,

    /// Token / latency / freshness model.
    #[serde(default)]
    pub cost_model: CostModel,

    /// Empirically observed next tools — drives speculative prefetch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_up: Vec<FollowUpLink>,

    /// Tools whose cached responses become stale when *this* tool runs.
    /// Mirrors the existing file-mutation hook in DedupCache: e.g.
    /// `update_issue.invalidates = ["get_issue", "get_issues"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalidates: Vec<String>,

    /// After how many consecutive empty (or no-change) calls the
    /// planner should stop re-issuing this tool. `None` = never bail.
    /// Set to `Some(2)` for `ToolSearch` per the corpus finding that
    /// 50%+ of repeated `ToolSearch` calls return zero results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_fast_after_n: Option<u32>,
}

impl ToolValueModel {
    /// Ergonomic constructor for the most common case: a critical
    /// tool with a known typical size and one likely follow-up.
    pub fn critical_with_size(typical_kb: f32) -> Self {
        Self {
            value_class: ValueClass::Critical,
            cost_model: CostModel {
                typical_kb,
                ..CostModel::default()
            },
            ..Self::default()
        }
    }

    /// Ergonomic constructor for an `audit_only` tool (TaskUpdate,
    /// TodoWrite). Such tools never enter the knapsack budget.
    pub fn audit_only() -> Self {
        Self {
            value_class: ValueClass::AuditOnly,
            ..Self::default()
        }
    }

    /// True iff this tool's responses should be excluded from the
    /// per-turn budget. Used by the planner's first-pass filter.
    pub fn excluded_from_budget(&self) -> bool {
        matches!(self.value_class, ValueClass::AuditOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_critical_with_one_kb() {
        let m = ToolValueModel::default();
        assert_eq!(m.value_class, ValueClass::Critical);
        assert_eq!(m.cost_model.typical_kb, 1.0);
        assert!(m.field_groups.is_empty());
        assert!(m.follow_up.is_empty());
        assert!(m.invalidates.is_empty());
        assert!(!m.excluded_from_budget());
    }

    #[test]
    fn audit_only_is_excluded_from_budget() {
        assert!(ToolValueModel::audit_only().excluded_from_budget());
        assert!(!ToolValueModel::critical_with_size(2.5).excluded_from_budget());
    }

    #[test]
    fn critical_with_size_sets_typical_kb() {
        let m = ToolValueModel::critical_with_size(2.5);
        assert_eq!(m.value_class, ValueClass::Critical);
        assert_eq!(m.cost_model.typical_kb, 2.5);
    }

    #[test]
    fn round_trip_via_toml_default() {
        // A blank table must deserialise into the default.
        let m: ToolValueModel = toml::from_str("").unwrap();
        assert_eq!(m.value_class, ValueClass::default());
        assert_eq!(m.cost_model, CostModel::default());
    }

    #[test]
    fn round_trip_via_toml_full() {
        let m = ToolValueModel {
            value_class: ValueClass::Supporting,
            field_groups: {
                let mut g = BTreeMap::new();
                g.insert(
                    "must_have".to_string(),
                    FieldGroup {
                        fields: vec!["title".into(), "url".into()],
                        estimated_value: 1.0,
                        default_include: true,
                    },
                );
                g.insert(
                    "nice_to_have".to_string(),
                    FieldGroup {
                        fields: vec!["snippet".into()],
                        estimated_value: 0.3,
                        default_include: false,
                    },
                );
                g
            },
            cost_model: CostModel {
                typical_kb: 3.1,
                max_kb: Some(8.0),
                latency_ms_p50: Some(900),
                dollars: None,
                freshness_ttl_s: Some(3600),
            },
            follow_up: vec![FollowUpLink {
                tool: "WebFetch".into(),
                probability: 0.65,
                projection: Some("url".into()),
            }],
            invalidates: vec![],
            fail_fast_after_n: Some(2),
        };
        let s = toml::to_string_pretty(&m).unwrap();
        let back: ToolValueModel = toml::from_str(&s).unwrap();
        assert_eq!(back.value_class, ValueClass::Supporting);
        assert_eq!(back.field_groups.len(), 2);
        assert_eq!(
            back.field_groups.get("must_have").unwrap().fields,
            vec!["title".to_string(), "url".to_string()]
        );
        assert_eq!(back.cost_model.typical_kb, 3.1);
        assert_eq!(back.cost_model.max_kb, Some(8.0));
        assert_eq!(back.follow_up[0].tool, "WebFetch");
        assert_eq!(back.follow_up[0].projection.as_deref(), Some("url"));
        assert_eq!(back.fail_fast_after_n, Some(2));
    }

    #[test]
    fn empty_optional_fields_are_skipped_on_serialise() {
        let m = ToolValueModel::default();
        let s = toml::to_string_pretty(&m).unwrap();
        // No `field_groups`, `follow_up`, `invalidates`, `fail_fast_after_n` —
        // they were `Default` and should be skip_serializing_if'd.
        assert!(!s.contains("field_groups"));
        assert!(!s.contains("follow_up"));
        assert!(!s.contains("invalidates"));
        assert!(!s.contains("fail_fast_after_n"));
        assert!(!s.contains("max_kb"));
    }

    #[test]
    fn value_class_serialises_snake_case() {
        let m = ToolValueModel {
            value_class: ValueClass::AuditOnly,
            ..Default::default()
        };
        let s = toml::to_string_pretty(&m).unwrap();
        assert!(s.contains("audit_only"), "expected snake_case, got: {s}");
    }

    #[test]
    fn field_group_default_estimated_value_is_half() {
        let g = FieldGroup::default();
        assert!((g.estimated_value - 0.5).abs() < 1e-6);
        assert!(g.default_include);
    }

    #[test]
    fn followup_link_round_trips_without_projection() {
        let l = FollowUpLink {
            tool: "Bash".into(),
            probability: 0.8,
            projection: None,
        };
        let s = toml::to_string_pretty(&l).unwrap();
        assert!(
            !s.contains("projection"),
            "None should be skipped, got: {s}"
        );
        let back: FollowUpLink = toml::from_str(&s).unwrap();
        assert_eq!(back.tool, "Bash");
        assert!((back.probability - 0.8).abs() < 1e-6);
        assert!(back.projection.is_none());
    }
}
