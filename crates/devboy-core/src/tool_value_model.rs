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

/// Side-effect classification — controls whether a tool is safe to
/// run *speculatively* (i.e. before the LLM asks for it).
///
/// Speculative pre-fetch is the killer feature of Paper 3, but it is
/// only safe when re-issuing the call has **no observable consequence
/// beyond what the LLM was going to do anyway**. Anything that mutates
/// state (local files, remote APIs, user-visible objects) must never
/// be speculated — otherwise we double-execute writes.
///
/// The default is the most conservative reading: `Indeterminate`. New
/// tools are non-speculatable until a provider explicitly opts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SideEffectClass {
    /// Deterministic + idempotent: same input → same output, no
    /// state. Safe to speculate freely. Examples: `Read` of an
    /// unchanged file, hash computations, pure functions over args.
    Pure,
    /// No external mutation, but the result *can* change between
    /// calls (TTL applies). Safe to speculate when `freshness_ttl_s`
    /// has not expired. Examples: `get_issues`, `WebFetch`, `Glob`,
    /// `Grep`, list-style endpoints. The bulk of the planner's wins.
    ReadOnly,
    /// Mutates host-local state (files, in-memory caches). Never
    /// speculate — re-running would duplicate the edit. Examples:
    /// `Edit`, `Write`, `MultiEdit`, `NotebookEdit`.
    MutatesLocal,
    /// Mutates remote state (creates issues, sends messages, runs
    /// pipelines, `git push`). Never speculate — the consequence is
    /// visible to other actors. Examples: `create_issue`,
    /// `create_merge_request`, `add_issue_comment`, `Bash` for
    /// destructive commands.
    MutatesExternal,
    /// Outcome cannot be classified statically (most prominently
    /// `Bash` — its effect depends on the command string). Default
    /// for any tool that has not been annotated. Treated as
    /// non-speculatable; the planner only emits a hint to the LLM.
    #[default]
    Indeterminate,
}

impl SideEffectClass {
    /// `true` iff the planner is allowed to issue this tool ahead of
    /// the LLM asking for it. Currently `Pure` and `ReadOnly` only;
    /// the other variants are bypassed even if `enrichment.enabled`
    /// is on.
    pub fn is_speculatable(&self) -> bool {
        matches!(self, Self::Pure | Self::ReadOnly)
    }
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
///
/// `Default` returns an empty link (`tool = ""`,
/// `probability = default_followup_probability`). The implementation
/// is mostly there so call sites can use struct-update syntax
/// (`FollowUpLink { tool: …, probability: …, ..Default::default() }`)
/// — the empty `tool` would never resolve in `enumerate_candidates`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FollowUpLink {
    /// Name of the tool that typically fires next.
    pub tool: String,

    /// Probability of this follow-up firing, mined from corpus.
    /// Range 0.0–1.0; 0.5+ is a reasonable prefetch threshold.
    #[serde(default = "default_followup_probability")]
    pub probability: f32,

    /// Optional argument projection — name of the field from the
    /// previous response to read. For example,
    /// `Glob.follow_up = [{tool: "Read", projection: "match_path",
    /// projection_arg: "file_path"}]` tells the planner to take each
    /// glob result's `match_path` and feed it as the `file_path`
    /// argument to `Read`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<String>,

    /// Optional argument *name* on the follow-up tool that the
    /// extracted `projection` value should populate. When `None`, the
    /// provider's `ToolEnricher::project_args` is asked to build the
    /// arguments instead — that's the right path for built-in tools
    /// where mapping is hard-coded. Custom MCP tools that the user
    /// annotates by hand in `pipeline_config.toml` should set both
    /// `projection` and `projection_arg` so the planner can build the
    /// follow-up args without provider code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_arg: Option<String>,
}

fn default_followup_probability() -> f32 {
    0.5
}

/// Paper 4 — provider-declared column schema for a Parquet-eligible
/// response. The serialiser uses this when `parquet_eligible = true`
/// to decide column types up-front (instead of inferring from the
/// first N rows), and the notebook header reflects it verbatim so
/// the LLM sees the schema even on empty / one-row results.
///
/// Joins are optional but powerful — when two datasets in the same
/// session declare a join, the notebook header lists it as a
/// comment and the DuckDB executor accepts JOIN queries naturally.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ParquetSchemaHint {
    /// Ordered list of `(name, type)` pairs. Type is one of
    /// `"int64"`, `"float64"`, `"utf8"`, `"bool"`, `"datetime"`,
    /// `"list<utf8>"`. Unknown types fall back to `"utf8"` so a
    /// hint that omits a column never breaks serialisation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ParquetColumn>,

    /// Optional join graph. Each entry says "my column X matches
    /// dataset D's column Y", letting the notebook header surface
    /// `issues.id ← mr_issues.issue_id → mrs.id` chains for the LLM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joins: Vec<ParquetJoin>,

    /// Provider-suggested helper function names the notebook header
    /// should pre-define (e.g. `"high_priority"`, `"open_only"`,
    /// `"recent_30d"`). Bodies are generated from the column list
    /// — we only need the LLM-facing names here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub helpers: Vec<String>,
}

/// One column in a `ParquetSchemaHint`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ParquetColumn {
    pub name: String,
    /// One of `"int64"`, `"float64"`, `"utf8"`, `"bool"`,
    /// `"datetime"`, `"list<utf8>"`. See [`ParquetSchemaHint`].
    #[serde(default = "default_parquet_dtype")]
    pub dtype: String,
    /// Short human description surfaced in the notebook header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

fn default_parquet_dtype() -> String {
    "utf8".to_string()
}

/// One join edge in a `ParquetSchemaHint`. `"my <field> matches
/// other_dataset.other_field"`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ParquetJoin {
    pub field: String,
    pub other_dataset: String,
    pub other_field: String,
}

impl Default for FollowUpLink {
    /// Empty link with the default probability — wouldn't resolve in
    /// the planner. Provided so callers can use struct-update syntax:
    /// `FollowUpLink { tool: …, probability: …, ..Default::default() }`
    /// without spelling every optional field. `f32` has no useful
    /// `Default::default()` here (would be `0.0`), so we hand-write
    /// the impl rather than `derive`.
    fn default() -> Self {
        Self {
            tool: String::new(),
            probability: default_followup_probability(),
            projection: None,
            projection_arg: None,
        }
    }
}

/// Provider-shipped, user-overridable description of how a tool fits
/// into the enrichment knapsack.
///
/// **Naming contract.** Keys in `AdaptiveConfig.tools` and in the
/// `[tools.<name>]` TOML section are *runtime tool names* — exactly
/// what the LLM sends in `tool_use.name` (`Read`, `Bash`,
/// `mcp__gitlab__get_issue`, …). Do **not** anonymize them. The
/// `mcp__p<hash6>__verb` form only appears in the public corpus
/// aggregates under `docs/research/data/paper3_*.csv`; resolution
/// (`AdaptiveConfig::effective_tool_value_model`), cross-tool
/// invalidation (`invalidates = […]`) and the dedup cache all match
/// on the live runtime name, so an anonymized key would never resolve.
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

    /// Side-effect classification — gates speculative pre-fetch.
    /// Default `Indeterminate` keeps unannotated tools off the
    /// speculation path; only `Pure` / `ReadOnly` are eligible.
    #[serde(default, skip_serializing_if = "is_default_side_effect")]
    pub side_effect_class: SideEffectClass,

    /// Optional default host for rate-limit grouping
    /// (e.g. `"github.com"`, `"api.openai.com"`,
    /// `"gitlab.example.com"`). The host's speculative dispatcher
    /// caps in-flight prefetches per rate_limit_host; `None` means
    /// no rate budget tracked for this tool.
    ///
    /// **Static vs runtime.** This is the *static* default. For tools
    /// whose target host depends on runtime arguments (e.g. `WebFetch`
    /// where the URL is per-call), the provider's
    /// [`crate::ToolEnricher::rate_limit_host`] override returns the
    /// runtime value; the static field is only consulted as a
    /// fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_host: Option<String>,

    /// Per-tool speculation override. When `Some(false)`, the planner
    /// is forbidden from speculating this tool even if
    /// `side_effect_class.is_speculatable()`. Set automatically by
    /// `tune analyze`'s R7 rule when the observed `prefetch_hit_rate`
    /// for this tool falls below the floor — i.e. the planner was
    /// guessing wrong too often. `None` = honour `side_effect_class`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speculate: Option<bool>,

    /// Paper 4 — `true` when this tool's response is structured
    /// enough to serialise as a Parquet artefact (array-of-objects
    /// or tabular text). When the host's `OutputFormat::Parquet`
    /// path is active and the agent requests `format = "parquet"`,
    /// the response body is replaced with a notebook header
    /// (schema + 5 sample rows + helper functions) and the LLM
    /// queries the dataset via the `query_dataset` tool.
    ///
    /// `false` (default) keeps the legacy push-based pipeline:
    /// the response is rendered through Paper 1 / Paper 2 encoders
    /// and sent to the LLM in full. Set `true` only on read-only
    /// list/detail tools whose result fits the array-of-objects
    /// shape (Glob, Grep, get_issues, get_merge_requests,
    /// search_knowledge_base, …). Bash, prose-returning Agent,
    /// short metadata tools (TaskUpdate, TodoWrite) stay `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub parquet_eligible: bool,

    /// Paper 4 — provider-shipped column hint for the Parquet
    /// serialiser. When `Some`, the writer prefers these column
    /// names + types over JSON-inferred ones; when `None`, the
    /// serialiser infers a schema from the first 32 rows of the
    /// response.
    ///
    /// Hint is also surfaced in the auto-generated notebook header
    /// so the LLM sees the expected schema even when the dataset is
    /// empty (no sample rows to inspect).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parquet_schema_hint: Option<ParquetSchemaHint>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_default_side_effect(s: &SideEffectClass) -> bool {
    matches!(s, SideEffectClass::Indeterminate)
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

    /// True iff the planner is allowed to issue this tool ahead of
    /// the LLM's next message. Combines `side_effect_class` with the
    /// per-tool `speculate` override — `Some(false)` always wins, so
    /// `tune analyze`'s auto-disable rule cannot be bypassed by a
    /// stale `Pure` annotation.
    pub fn is_speculatable(&self) -> bool {
        if matches!(self.speculate, Some(false)) {
            return false;
        }
        if matches!(self.speculate, Some(true)) {
            return true;
        }
        self.side_effect_class.is_speculatable()
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
                projection_arg: Some("url".into()),
            }],
            invalidates: vec![],
            fail_fast_after_n: Some(2),
            side_effect_class: SideEffectClass::ReadOnly,
            rate_limit_host: Some("example.com".into()),
            speculate: None,
            parquet_eligible: false,
            parquet_schema_hint: None,
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
        assert_eq!(back.follow_up[0].projection_arg.as_deref(), Some("url"));
        assert_eq!(back.fail_fast_after_n, Some(2));
        assert_eq!(back.side_effect_class, SideEffectClass::ReadOnly);
        assert_eq!(back.rate_limit_host.as_deref(), Some("example.com"));
        assert!(back.is_speculatable());
    }

    // ─── Side-effect classification ──────────────────────────────────

    #[test]
    fn default_side_effect_class_is_indeterminate_and_blocks_speculation() {
        let m = ToolValueModel::default();
        assert_eq!(m.side_effect_class, SideEffectClass::Indeterminate);
        assert!(
            !m.is_speculatable(),
            "Indeterminate must never be speculated"
        );
    }

    #[test]
    fn pure_and_read_only_are_speculatable() {
        let pure = ToolValueModel {
            side_effect_class: SideEffectClass::Pure,
            ..Default::default()
        };
        let ro = ToolValueModel {
            side_effect_class: SideEffectClass::ReadOnly,
            ..Default::default()
        };
        assert!(pure.is_speculatable());
        assert!(ro.is_speculatable());
    }

    #[test]
    fn mutating_classes_block_speculation() {
        for class in [
            SideEffectClass::MutatesLocal,
            SideEffectClass::MutatesExternal,
        ] {
            let m = ToolValueModel {
                side_effect_class: class,
                ..Default::default()
            };
            assert!(
                !m.is_speculatable(),
                "{class:?} must never be speculated — would duplicate writes"
            );
        }
    }

    #[test]
    fn speculate_override_wins_over_side_effect_class() {
        // `tune analyze`'s R7 disables a Pure tool whose hit rate
        // dropped: must trump the static class.
        let pure_but_disabled = ToolValueModel {
            side_effect_class: SideEffectClass::Pure,
            speculate: Some(false),
            ..Default::default()
        };
        assert!(!pure_but_disabled.is_speculatable());

        // Manual override forcing speculation on an Indeterminate tool
        // (e.g. user knows their custom MCP shell wrapper is safe).
        let forced_on = ToolValueModel {
            side_effect_class: SideEffectClass::Indeterminate,
            speculate: Some(true),
            ..Default::default()
        };
        assert!(forced_on.is_speculatable());
    }

    #[test]
    fn side_effect_class_serialises_snake_case() {
        // Indeterminate is the default and is intentionally
        // skip_serializing_if'd — covered by `default_indeterminate_skipped_on_serialise`.
        for (class, expected) in [
            (SideEffectClass::Pure, "pure"),
            (SideEffectClass::ReadOnly, "read_only"),
            (SideEffectClass::MutatesLocal, "mutates_local"),
            (SideEffectClass::MutatesExternal, "mutates_external"),
        ] {
            let m = ToolValueModel {
                side_effect_class: class,
                ..Default::default()
            };
            let s = toml::to_string_pretty(&m).unwrap();
            assert!(
                s.contains(&format!("side_effect_class = \"{expected}\"")),
                "expected `{expected}`, got: {s}"
            );
            // Round-trip must preserve the class.
            let back: ToolValueModel = toml::from_str(&s).unwrap();
            assert_eq!(back.side_effect_class, class);
        }
    }

    #[test]
    fn default_indeterminate_skipped_on_serialise() {
        let m = ToolValueModel::default();
        let s = toml::to_string_pretty(&m).unwrap();
        assert!(
            !s.contains("side_effect_class"),
            "Indeterminate is the default and must be skip_serializing_if'd, got: {s}"
        );
        assert!(!s.contains("rate_limit_host"));
        assert!(!s.contains("speculate"));
    }

    #[test]
    fn followup_link_projection_arg_round_trips() {
        let l = FollowUpLink {
            tool: "Read".into(),
            probability: 0.8,
            projection: Some("path".into()),
            projection_arg: Some("file_path".into()),
        };
        let s = toml::to_string_pretty(&l).unwrap();
        let back: FollowUpLink = toml::from_str(&s).unwrap();
        assert_eq!(back.projection_arg.as_deref(), Some("file_path"));
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
            ..FollowUpLink::default()
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
        assert!(back.projection_arg.is_none());
    }

    // ─── Paper 4 — parquet_eligible + schema_hint ────────────────────

    #[test]
    fn default_tool_value_model_is_not_parquet_eligible() {
        let m = ToolValueModel::default();
        assert!(!m.parquet_eligible);
        assert!(m.parquet_schema_hint.is_none());
    }

    #[test]
    fn parquet_eligible_skips_serialise_when_default_false() {
        let m = ToolValueModel::default();
        let s = toml::to_string_pretty(&m).unwrap();
        assert!(
            !s.contains("parquet_eligible"),
            "default false must be skip_serializing_if'd, got: {s}"
        );
        assert!(!s.contains("parquet_schema_hint"));
    }

    #[test]
    fn parquet_eligible_round_trips_with_schema_hint() {
        let m = ToolValueModel {
            value_class: ValueClass::Supporting,
            parquet_eligible: true,
            parquet_schema_hint: Some(ParquetSchemaHint {
                columns: vec![
                    ParquetColumn {
                        name: "id".into(),
                        dtype: "int64".into(),
                        doc: Some("Issue id".into()),
                    },
                    ParquetColumn {
                        name: "title".into(),
                        dtype: "utf8".into(),
                        doc: None,
                    },
                ],
                joins: vec![ParquetJoin {
                    field: "id".into(),
                    other_dataset: "issue_comments".into(),
                    other_field: "issue_id".into(),
                }],
                helpers: vec!["high_priority".into(), "open_only".into()],
            }),
            ..Default::default()
        };
        let s = toml::to_string_pretty(&m).unwrap();
        let back: ToolValueModel = toml::from_str(&s).unwrap();
        assert!(back.parquet_eligible);
        let hint = back.parquet_schema_hint.expect("hint must round-trip");
        assert_eq!(hint.columns.len(), 2);
        assert_eq!(hint.columns[0].name, "id");
        assert_eq!(hint.columns[0].dtype, "int64");
        assert_eq!(hint.columns[0].doc.as_deref(), Some("Issue id"));
        assert_eq!(hint.joins.len(), 1);
        assert_eq!(hint.joins[0].other_dataset, "issue_comments");
        assert_eq!(hint.helpers, vec!["high_priority", "open_only"]);
    }

    #[test]
    fn parquet_column_dtype_defaults_to_utf8_on_missing() {
        // A hand-written TOML omitting `dtype` must default to "utf8".
        let raw = "name = \"description\"\n";
        let col: ParquetColumn = toml::from_str(raw).unwrap();
        assert_eq!(col.name, "description");
        assert_eq!(col.dtype, "utf8");
        assert!(col.doc.is_none());
    }

    #[test]
    fn parquet_schema_hint_with_no_columns_or_joins_serialises_empty() {
        let h = ParquetSchemaHint::default();
        let s = toml::to_string_pretty(&h).unwrap();
        assert!(!s.contains("columns"), "empty list must skip: {s}");
        assert!(!s.contains("joins"));
        assert!(!s.contains("helpers"));
        let back: ParquetSchemaHint = toml::from_str(&s).unwrap();
        assert_eq!(back, h);
    }
}
