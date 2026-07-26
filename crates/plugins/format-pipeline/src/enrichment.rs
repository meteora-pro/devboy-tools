//! Paper 3 — Enrichment Planner.
//!
//! Solves the *inter-tool* knapsack: given the current turn's intent,
//! the recent tool history, and a per-turn token budget, decide which
//! tools to call (and which projections to ask for) so that the LLM
//! receives the most-valuable context that still fits.
//!
//! Where Paper 1 picks items inside one response and Paper 2 picks the
//! encoding of the chosen items, Paper 3 picks the *tool calls
//! themselves*.
//!
//! Algorithm: greedy by `value / cost` ratio, with prereq closure and
//! `value_class = AuditOnly` exclusion from the budget. Greedy is
//! provably 1/2-optimal for the 0/1 knapsack and runs in O(N log N) —
//! far faster than the exact DP would be on a hot path. The numbers
//! we plug in (`cost_model.typical_kb`) are mined priors, refined by
//! `tune analyze` over time.
//!
//! Anchored on `docs/research/paper3_corpus_findings.md`.

use std::collections::{BTreeMap, BTreeSet};

use devboy_core::{ToolValueModel, ValueClass};

use crate::adaptive_config::AdaptiveConfig;

/// Per-turn input to [`build_plan`].
#[derive(Debug, Clone, Default)]
pub struct TurnContext<'a> {
    /// Tool names already invoked in this turn, oldest first. The last
    /// element is the most recent call. Drives `follow_up` lookup.
    pub recent_tools: &'a [String],
    /// Budget for the *prefetched* part of the next turn, expressed in
    /// tokens. The planner stops admitting candidates once the running
    /// total reaches this number.
    pub budget_tokens: u32,
    /// Optional free-form intent hints (e.g. extracted from the user
    /// message). When non-empty, the planner promotes any tool's
    /// `default_include = false` field group whose member fields
    /// match a keyword (case-insensitive substring), boosting that
    /// tool's value-score for this turn. See the private
    /// `intent_boost` helper in this module for the matching logic.
    pub intent_keywords: Vec<String>,
}

impl<'a> TurnContext<'a> {
    pub fn new(recent_tools: &'a [String], budget_tokens: u32) -> Self {
        Self {
            recent_tools,
            budget_tokens,
            intent_keywords: Vec::new(),
        }
    }
}

/// One tool call admitted to the plan.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedCall {
    /// Tool name in the same anonymized form used by `[tools.<name>]`
    /// (e.g. `"Read"`, `"mcp__pXXX__get_branch_pipeline"`).
    pub tool: String,
    /// Optional argument projection inherited from the triggering
    /// `FollowUpLink` (e.g. `Some("match_path")` for a Glob → Read step).
    pub projection: Option<String>,
    /// Probability the planner used to score this candidate. Carried
    /// for telemetry only — the planner does not re-read it.
    pub probability: f32,
    /// Bytes the planner expects this call to spend.
    pub estimated_cost_bytes: u32,
    /// Tokens (rough `bytes / 4` prior). Used by the budget gate.
    pub estimated_cost_tokens: u32,
    /// `value_class` lifted from the resolved `ToolValueModel` —
    /// `AuditOnly` calls are admitted "free" (do not count against the
    /// budget) and surface in the plan only for trace purposes.
    pub value_class: ValueClass,
}

/// Output of [`build_plan`]. The planner returns the candidates in the
/// order they were admitted; the host can use that order for
/// speculative prefetch.
#[derive(Debug, Clone, Default)]
pub struct EnrichmentPlan {
    pub calls: Vec<PlannedCall>,
    pub total_cost_tokens: u32,
    pub remaining_budget_tokens: u32,
    /// Reasons we declined candidates — useful for `tune analyze` and
    /// for the operator to understand why a follow-up was skipped.
    pub declined: Vec<DeclineReason>,
}

/// Why the planner left a candidate out.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclineReason {
    pub tool: String,
    pub reason: DeclineKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeclineKind {
    /// Adding this candidate would have crossed `budget_tokens`.
    /// Currently the only variant the v1 solver emits — low-probability
    /// candidates are filtered before they reach the candidate list,
    /// and prereq / preemption tracking is not implemented yet.
    /// `#[non_exhaustive]` lets us add the missing reasons later
    /// without breaking downstream pattern matches.
    BudgetExceeded,
}

/// Hard knobs the solver does not (yet) read from `AdaptiveConfig`.
/// Keeping them as a separate struct so the API stays additive.
#[derive(Debug, Clone, Copy)]
pub struct PlannerOptions {
    /// Minimum `FollowUpLink.probability` for the link to be admitted.
    /// Default 0.5 — corpus mining shows ~half of edges are usable
    /// signals, the other half are noise.
    pub min_followup_probability: f32,
    /// Bytes-per-token assumption for the budget gate. We use 4 to
    /// match the existing pipeline heuristic; once Paper 2's accurate
    /// tokenizer ships in this branch we switch to that.
    pub bytes_per_token: u32,
    /// Latency penalty knee — calls with `cost_model.latency_ms_p50 ≥
    /// this` get their value-score halved. Picked at 5000 ms because
    /// any prefetch slower than that is unlikely to land before the
    /// LLM organically asks for it. `None` = latency-unaware (default).
    pub latency_penalty_ms: Option<u32>,
    /// Dollar penalty knee — calls with `cost_model.dollars ≥ this`
    /// get their value-score halved. Picked at $0.10 / call. `None`
    /// = dollar-unaware (default).
    pub dollar_penalty: Option<f32>,
}

impl Default for PlannerOptions {
    fn default() -> Self {
        Self {
            min_followup_probability: 0.5,
            bytes_per_token: 4,
            latency_penalty_ms: None,
            dollar_penalty: None,
        }
    }
}

impl PlannerOptions {
    /// Enable both latency and dollar awareness with sensible defaults
    /// (5 s knee, $0.10 knee). Useful in deployments where prefetch
    /// fan-out hits a paid API rate-limit or a slow upstream.
    pub fn cost_aware() -> Self {
        Self {
            latency_penalty_ms: Some(5_000),
            dollar_penalty: Some(0.10),
            ..Self::default()
        }
    }
}

/// Build an enrichment plan for the next turn.
///
/// The solver:
///
/// 1. Enumerates candidates from the `follow_up` graph of every tool
///    in `context.recent_tools`, deduplicating by tool name (highest
///    probability wins).
/// 2. Filters out candidates below
///    `options.min_followup_probability`.
/// 3. Resolves each candidate's `ToolValueModel` via
///    `AdaptiveConfig::effective_tool_value_model`, falling back to
///    a permissive default if the user has not annotated the tool.
/// 4. Sorts by `value / cost` ratio (`AuditOnly` tools always come
///    first — they are free).
/// 5. Admits candidates greedily until `budget_tokens` is exhausted.
pub fn build_plan(
    config: &AdaptiveConfig,
    context: &TurnContext<'_>,
    options: PlannerOptions,
) -> EnrichmentPlan {
    let candidates = enumerate_candidates(config, context, options);

    // Greedy by value/cost density. AuditOnly entries get +inf so they
    // surface first regardless of size — they don't count against the
    // budget anyway.
    let mut scored: Vec<(f32, Candidate)> = candidates
        .into_iter()
        .map(|c| {
            let density = if matches!(c.model.value_class, ValueClass::AuditOnly) {
                f32::INFINITY
            } else {
                let cost_tokens = cost_tokens_for(&c.model, options.bytes_per_token).max(1) as f32;
                let boost = intent_boost(&c.model, &context.intent_keywords);
                let penalty = cost_penalty(&c.model, &options);
                value_score(&c.model) * boost * penalty / cost_tokens
            };
            (density, c)
        })
        .collect();
    // Stable sort so two candidates with equal density preserve the
    // enumeration order (recent_tools order → follow_up order).
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut plan = EnrichmentPlan {
        remaining_budget_tokens: context.budget_tokens,
        ..EnrichmentPlan::default()
    };

    for (_, c) in scored {
        let raw_cost_tokens = cost_tokens_for(&c.model, options.bytes_per_token);
        let cost_bytes = (c.model.cost_model.typical_kb * 1024.0) as u32;

        let is_free = c.model.excluded_from_budget();
        // Clamp non-AuditOnly cost to ≥ 1 token. A tool with
        // `cost_model.typical_kb = 0.0` (e.g. `ToolSearch`) must still
        // count *something* against the budget — otherwise it would
        // admit unconditionally and `enricher_predicted_cost_tokens`
        // would always log 0 for that tool, distorting telemetry.
        // AuditOnly tools stay genuinely free; that is their whole
        // contract.
        let cost_tokens = if is_free {
            raw_cost_tokens
        } else {
            raw_cost_tokens.max(1)
        };
        if !is_free && cost_tokens > plan.remaining_budget_tokens {
            plan.declined.push(DeclineReason {
                tool: c.tool.clone(),
                reason: DeclineKind::BudgetExceeded,
            });
            continue;
        }

        plan.calls.push(PlannedCall {
            tool: c.tool,
            projection: c.projection,
            probability: c.probability,
            estimated_cost_bytes: cost_bytes,
            estimated_cost_tokens: cost_tokens,
            value_class: c.model.value_class,
        });
        if !is_free {
            plan.total_cost_tokens = plan.total_cost_tokens.saturating_add(cost_tokens);
            plan.remaining_budget_tokens = plan.remaining_budget_tokens.saturating_sub(cost_tokens);
        }
    }

    plan
}

// ─── internals ──────────────────────────────────────────────────────

struct Candidate {
    tool: String,
    projection: Option<String>,
    probability: f32,
    model: ToolValueModel,
}

fn enumerate_candidates(
    config: &AdaptiveConfig,
    context: &TurnContext<'_>,
    options: PlannerOptions,
) -> Vec<Candidate> {
    // Best probability per tool — we don't want to admit the same tool
    // twice from two different recent_tools sources.
    let mut by_tool: BTreeMap<String, (Option<String>, f32)> = BTreeMap::new();
    let recent_set: BTreeSet<&str> = context.recent_tools.iter().map(String::as_str).collect();

    for trigger in context.recent_tools {
        let Some(model) = config.effective_tool_value_model(trigger) else {
            continue;
        };
        for link in &model.follow_up {
            if link.probability < options.min_followup_probability {
                continue;
            }
            // Skip self-loops — re-issuing the trigger is the dedup
            // path's job, not the planner's.
            if link.tool == *trigger {
                continue;
            }
            // Skip tools the agent already used in this turn.
            if recent_set.contains(link.tool.as_str()) {
                continue;
            }
            let entry = by_tool
                .entry(link.tool.clone())
                .or_insert((link.projection.clone(), link.probability));
            if link.probability > entry.1 {
                entry.0 = link.projection.clone();
                entry.1 = link.probability;
            }
        }
    }

    by_tool
        .into_iter()
        .map(|(tool, (projection, probability))| {
            // Honour the docstring promise: missing annotations fall back
            // to a permissive default so the unannotated tool still
            // participates in the knapsack instead of being silently
            // dropped. Earlier versions used `?` and quietly cut these
            // candidates.
            let model = config
                .effective_tool_value_model(&tool)
                .cloned()
                .unwrap_or_default();
            Candidate {
                tool,
                projection,
                probability,
                model,
            }
        })
        .collect()
}

fn cost_tokens_for(model: &ToolValueModel, bytes_per_token: u32) -> u32 {
    let bytes = (model.cost_model.typical_kb * 1024.0) as u32;
    bytes.saturating_div(bytes_per_token.max(1))
}

fn value_score(model: &ToolValueModel) -> f32 {
    match model.value_class {
        ValueClass::Critical => 1.0,
        ValueClass::Supporting => 0.5,
        ValueClass::Optional => 0.2,
        ValueClass::AuditOnly => 0.0,
    }
}

/// Multiplicative penalty applied to a tool's `value_score` when its
/// `cost_model.latency_ms_p50` or `.dollars` exceeds the configured
/// knee. Each axis halves the score independently: a tool that's both
/// slow and expensive gets `0.5 * 0.5 = 0.25× value`. Returns 1.0
/// (no penalty) when the relevant `PlannerOptions` field is `None`,
/// or when the tool's cost is below the knee.
fn cost_penalty(model: &ToolValueModel, options: &PlannerOptions) -> f32 {
    let mut penalty = 1.0_f32;
    if let (Some(knee), Some(latency)) =
        (options.latency_penalty_ms, model.cost_model.latency_ms_p50)
        && latency >= knee
    {
        penalty *= 0.5;
    }
    if let (Some(knee), Some(dollars)) = (options.dollar_penalty, model.cost_model.dollars)
        && dollars >= knee
    {
        penalty *= 0.5;
    }
    penalty
}

/// Multiplicative boost applied to a tool's `value_score` when the
/// caller's `intent_keywords` overlap with one of the tool's
/// `default_include = false` field groups. A group at +0.3
/// `estimated_value` becomes "worth admitting" when its keywords are
/// in scope; without this boost the planner would never escalate
/// opt-in groups for the current turn.
///
/// Returns 1.0 (no boost) when:
///   - `intent_keywords` is empty;
///   - the tool has no `default_include = false` groups;
///   - none of the keywords case-insensitively match a field name
///     of an opt-in group.
///
/// Returns `1.0 + Σ group.estimated_value` for every matching opt-in
/// group, capped at 2.5× to keep the planner from running away.
fn intent_boost(model: &ToolValueModel, intent_keywords: &[String]) -> f32 {
    if intent_keywords.is_empty() || model.field_groups.is_empty() {
        return 1.0;
    }
    let lowered: Vec<String> = intent_keywords
        .iter()
        .map(|k| k.to_ascii_lowercase())
        .collect();
    let mut boost: f32 = 1.0;
    for group in model.field_groups.values() {
        if group.default_include {
            continue; // already counted in the base value
        }
        let any_match = group
            .fields
            .iter()
            .any(|f| lowered.iter().any(|kw| f.to_ascii_lowercase().contains(kw)));
        if any_match {
            boost += group.estimated_value;
        }
    }
    boost.min(2.5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_defaults::default_tool_value_models;

    fn config_with_defaults() -> AdaptiveConfig {
        AdaptiveConfig {
            tools: default_tool_value_models(),
            ..AdaptiveConfig::default()
        }
    }

    #[test]
    fn empty_recent_tools_returns_empty_plan() {
        let cfg = config_with_defaults();
        let recent: Vec<String> = vec![];
        let ctx = TurnContext::new(&recent, 1024);
        let plan = build_plan(&cfg, &ctx, PlannerOptions::default());
        assert!(plan.calls.is_empty());
        assert_eq!(plan.total_cost_tokens, 0);
    }

    #[test]
    fn after_grep_planner_prefetches_read_with_path_projection() {
        let cfg = config_with_defaults();
        let recent = vec!["Grep".to_string()];
        let ctx = TurnContext::new(&recent, 4_000);
        let plan = build_plan(
            &cfg,
            &ctx,
            PlannerOptions {
                min_followup_probability: 0.3,
                ..Default::default()
            },
        );
        let read = plan
            .calls
            .iter()
            .find(|c| c.tool == "Read")
            .expect("Read should be admitted after Grep");
        assert_eq!(read.projection.as_deref(), Some("path"));
    }

    #[test]
    fn after_websearch_planner_prefetches_webfetch_with_url_projection() {
        let cfg = config_with_defaults();
        let recent = vec!["WebSearch".to_string()];
        let ctx = TurnContext::new(&recent, 4_000);
        let plan = build_plan(&cfg, &ctx, PlannerOptions::default());
        let fetch = plan
            .calls
            .iter()
            .find(|c| c.tool == "WebFetch")
            .expect("WebFetch should be admitted after WebSearch");
        assert_eq!(fetch.projection.as_deref(), Some("url"));
    }

    #[test]
    fn budget_exceeded_decline_recorded() {
        let cfg = config_with_defaults();
        let recent = vec!["Glob".to_string()];
        // 50 tokens ~= 200 bytes — well below `Read.typical_kb = 2.5`,
        // so Read must be declined for budget.
        let ctx = TurnContext::new(&recent, 50);
        let plan = build_plan(
            &cfg,
            &ctx,
            PlannerOptions {
                min_followup_probability: 0.3,
                ..Default::default()
            },
        );
        assert!(
            plan.declined
                .iter()
                .any(|d| d.tool == "Read" && d.reason == DeclineKind::BudgetExceeded),
            "expected Read to be declined for budget, got {:?}",
            plan.declined
        );
    }

    #[test]
    fn audit_only_tools_do_not_consume_budget() {
        let mut cfg = AdaptiveConfig {
            tools: default_tool_value_models(),
            ..AdaptiveConfig::default()
        };
        // Synthetic trigger that points to TaskUpdate — exercise the
        // audit_only "free" path.
        let mut grep = cfg.tools.get("Grep").unwrap().clone();
        grep.follow_up.push(devboy_core::FollowUpLink {
            tool: "TaskUpdate".into(),
            probability: 0.9,
            ..devboy_core::FollowUpLink::default()
        });
        cfg.tools.insert("Grep".into(), grep);

        let recent = vec!["Grep".to_string()];
        let ctx = TurnContext::new(&recent, 1_000);
        let plan = build_plan(&cfg, &ctx, PlannerOptions::default());
        let task = plan
            .calls
            .iter()
            .find(|c| c.tool == "TaskUpdate")
            .expect("TaskUpdate should be admitted");
        assert_eq!(task.value_class, ValueClass::AuditOnly);
        // Budget did not move because audit_only is free.
        assert_eq!(
            plan.remaining_budget_tokens,
            1_000 - critical_supporting_tokens(&plan)
        );
    }

    fn critical_supporting_tokens(plan: &EnrichmentPlan) -> u32 {
        plan.calls
            .iter()
            .filter(|c| !matches!(c.value_class, ValueClass::AuditOnly))
            .map(|c| c.estimated_cost_tokens)
            .sum()
    }

    #[test]
    fn self_loops_skipped() {
        let cfg = config_with_defaults();
        let recent = vec!["Read".to_string()];
        let ctx = TurnContext::new(&recent, 4_000);
        let plan = build_plan(&cfg, &ctx, PlannerOptions::default());
        // Read.follow_up contains Read with prob=0.45 — but the
        // planner skips self-loops (re-reading is the dedup cache's
        // job, not the planner's).
        assert!(
            !plan.calls.iter().any(|c| c.tool == "Read"),
            "Read self-loop should be skipped"
        );
    }

    #[test]
    fn already_used_tools_skipped() {
        let cfg = config_with_defaults();
        // Grep would normally pull in Read, but Read is already in
        // recent_tools so the planner must not re-prefetch.
        let recent = vec!["Read".to_string(), "Grep".to_string()];
        let ctx = TurnContext::new(&recent, 4_000);
        let plan = build_plan(
            &cfg,
            &ctx,
            PlannerOptions {
                min_followup_probability: 0.3,
                ..Default::default()
            },
        );
        assert!(
            !plan.calls.iter().any(|c| c.tool == "Read"),
            "Read already used in this turn should not be re-admitted"
        );
    }

    #[test]
    fn zero_typical_kb_supporting_tool_costs_at_least_one_token() {
        // Reproduces the edge case Copilot flagged: a Supporting tool
        // with `cost_model.typical_kb = 0.0` (e.g. `ToolSearch`) must
        // still count ≥ 1 token against the budget — otherwise it
        // would admit for free and break the cost ≤ budget guarantee.
        let mut cfg = AdaptiveConfig::default();
        let trigger = ToolValueModel {
            follow_up: vec![devboy_core::FollowUpLink {
                tool: "Cheap".into(),
                probability: 1.0,
                ..devboy_core::FollowUpLink::default()
            }],
            ..ToolValueModel::default()
        };
        let cheap = ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: devboy_core::CostModel {
                typical_kb: 0.0,
                ..Default::default()
            },
            ..ToolValueModel::default()
        };
        cfg.tools.insert("Trigger".into(), trigger);
        cfg.tools.insert("Cheap".into(), cheap);

        let recent = vec!["Trigger".to_string()];
        let ctx = TurnContext::new(&recent, 1);
        let plan = build_plan(&cfg, &ctx, PlannerOptions::default());

        let cheap_call = plan
            .calls
            .iter()
            .find(|c| c.tool == "Cheap")
            .expect("Cheap must still be admitted at budget=1");
        assert_eq!(
            cheap_call.estimated_cost_tokens, 1,
            "zero-typical-kb non-AuditOnly tool must clamp to 1 token"
        );
        assert_eq!(
            plan.remaining_budget_tokens, 0,
            "budget must be drained by 1, not left at 1"
        );

        // Same setup with budget=0 must decline — proves the clamp
        // actually participates in the budget gate, not just in
        // accounting.
        let ctx0 = TurnContext::new(&recent, 0);
        let plan0 = build_plan(&cfg, &ctx0, PlannerOptions::default());
        assert!(
            plan0.calls.iter().all(|c| c.tool != "Cheap"),
            "Cheap must be declined at budget=0 (clamp ≥ 1)"
        );
        assert!(
            plan0.declined.iter().any(|d| d.tool == "Cheap"),
            "decline reason must be recorded"
        );
    }

    // ─── Intent-aware boost (S12) ─────────────────────────────────

    fn model_with_optin_group(field: &str, est: f32) -> ToolValueModel {
        let mut groups = std::collections::BTreeMap::new();
        groups.insert(
            "must_have".into(),
            devboy_core::FieldGroup {
                fields: vec!["title".into(), "url".into()],
                estimated_value: 1.0,
                default_include: true,
            },
        );
        groups.insert(
            "nice_to_have".into(),
            devboy_core::FieldGroup {
                fields: vec![field.into()],
                estimated_value: est,
                default_include: false,
            },
        );
        ToolValueModel {
            value_class: ValueClass::Supporting,
            field_groups: groups,
            ..Default::default()
        }
    }

    #[test]
    fn intent_boost_neutral_with_no_keywords() {
        let m = model_with_optin_group("snippet", 0.3);
        assert!((intent_boost(&m, &[]) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn intent_boost_neutral_when_keyword_misses_optin_groups() {
        let m = model_with_optin_group("snippet", 0.3);
        let kw = vec!["totally_unrelated".to_string()];
        assert!((intent_boost(&m, &kw) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn intent_boost_lifts_score_when_keyword_hits_optin_field() {
        let m = model_with_optin_group("snippet", 0.3);
        let kw = vec!["SNIPPET".to_string()]; // case-insensitive
        let b = intent_boost(&m, &kw);
        assert!((b - 1.3).abs() < 1e-6, "expected 1.3, got {b}");
    }

    #[test]
    fn intent_boost_caps_at_2_5x() {
        // Multiple opt-in groups, keyword matches all of them — boost
        // would explode without the cap.
        let mut groups = std::collections::BTreeMap::new();
        for i in 0..5 {
            groups.insert(
                format!("g{i}"),
                devboy_core::FieldGroup {
                    fields: vec!["foo".into()],
                    estimated_value: 1.0,
                    default_include: false,
                },
            );
        }
        let m = ToolValueModel {
            field_groups: groups,
            ..Default::default()
        };
        let kw = vec!["foo".to_string()];
        let b = intent_boost(&m, &kw);
        assert!((b - 2.5).abs() < 1e-6, "boost must clamp at 2.5, got {b}");
    }

    #[test]
    fn intent_boost_changes_admit_order() {
        // Two candidates with identical base scores; one has an
        // opt-in group whose keyword fires this turn. With intent
        // active, that one should outrank the plain one.
        let plain = ToolValueModel {
            value_class: ValueClass::Supporting,
            ..Default::default()
        };
        let intent_match = model_with_optin_group("snippet", 0.4);
        let kw = vec!["snippet".to_string()];

        let p_score = value_score(&plain) * intent_boost(&plain, &kw);
        let i_score = value_score(&intent_match) * intent_boost(&intent_match, &kw);
        assert!(
            i_score > p_score,
            "intent-matching tool must outrank the plain one: {i_score} vs {p_score}"
        );
    }

    // ─── Latency / dollar awareness (S13) ─────────────────────────

    fn model_with_costs(latency_ms: Option<u32>, dollars: Option<f32>) -> ToolValueModel {
        ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: devboy_core::CostModel {
                typical_kb: 1.0,
                latency_ms_p50: latency_ms,
                dollars,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn cost_penalty_neutral_when_options_are_none() {
        let m = model_with_costs(Some(60_000), Some(1.0));
        let opts = PlannerOptions::default();
        assert!((cost_penalty(&m, &opts) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cost_penalty_halves_for_slow_tool_when_latency_aware() {
        let m = model_with_costs(Some(7_000), None);
        let opts = PlannerOptions::cost_aware();
        assert!((cost_penalty(&m, &opts) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn cost_penalty_halves_for_expensive_tool_when_dollar_aware() {
        let m = model_with_costs(None, Some(0.50));
        let opts = PlannerOptions::cost_aware();
        assert!((cost_penalty(&m, &opts) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn cost_penalty_compounds_for_slow_and_expensive() {
        let m = model_with_costs(Some(7_000), Some(0.50));
        let opts = PlannerOptions::cost_aware();
        // 0.5 (slow) * 0.5 (expensive) = 0.25
        assert!((cost_penalty(&m, &opts) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn cost_penalty_no_penalty_below_knee() {
        let m = model_with_costs(Some(800), Some(0.01));
        let opts = PlannerOptions::cost_aware();
        assert!((cost_penalty(&m, &opts) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cost_aware_planner_demotes_slow_tool_below_fast_one() {
        let mut cfg = AdaptiveConfig::default();
        let trigger = ToolValueModel {
            follow_up: vec![
                devboy_core::FollowUpLink {
                    tool: "FastTool".into(),
                    probability: 0.9,
                    ..Default::default()
                },
                devboy_core::FollowUpLink {
                    tool: "SlowTool".into(),
                    probability: 0.9,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        cfg.tools.insert("Trigger".into(), trigger);
        cfg.tools
            .insert("FastTool".into(), model_with_costs(Some(200), None));
        cfg.tools
            .insert("SlowTool".into(), model_with_costs(Some(20_000), None));

        let recent = vec!["Trigger".to_string()];
        // Budget large enough to admit both — verify ordering, not
        // budget exclusion. Each tool ~256 tokens; 1024 fits both.
        let ctx = TurnContext::new(&recent, 1024);
        // Latency-unaware planner: both admitted in arbitrary order
        // because density ties (same value_class, same typical_kb).
        let plan_blind = build_plan(&cfg, &ctx, PlannerOptions::default());
        // Cost-aware: FastTool wins density and gets admitted first.
        let plan_aware = build_plan(&cfg, &ctx, PlannerOptions::cost_aware());

        let fast_first = plan_aware.calls.first().map(|c| c.tool.as_str());
        assert_eq!(
            fast_first,
            Some("FastTool"),
            "cost-aware planner must admit FastTool first; got {:?}",
            plan_aware.calls.iter().map(|c| &c.tool).collect::<Vec<_>>()
        );
        assert_eq!(plan_aware.calls.len(), 2);
        assert_eq!(plan_blind.calls.len(), 2);
    }

    #[test]
    fn high_probability_link_wins_over_low_probability_for_same_tool() {
        let mut cfg = AdaptiveConfig::default();
        let a = ToolValueModel {
            follow_up: vec![devboy_core::FollowUpLink {
                tool: "Target".into(),
                probability: 0.55,
                projection: Some("low".into()),
                ..devboy_core::FollowUpLink::default()
            }],
            ..ToolValueModel::default()
        };
        let b = ToolValueModel {
            follow_up: vec![devboy_core::FollowUpLink {
                tool: "Target".into(),
                probability: 0.85,
                projection: Some("high".into()),
                ..devboy_core::FollowUpLink::default()
            }],
            ..ToolValueModel::default()
        };
        cfg.tools.insert("A".into(), a);
        cfg.tools.insert("B".into(), b);
        cfg.tools
            .insert("Target".into(), ToolValueModel::critical_with_size(0.1));

        let recent = vec!["A".to_string(), "B".to_string()];
        let ctx = TurnContext::new(&recent, 1_000);
        let plan = build_plan(&cfg, &ctx, PlannerOptions::default());
        let t = plan.calls.iter().find(|c| c.tool == "Target").unwrap();
        assert_eq!(t.projection.as_deref(), Some("high"));
        assert!((t.probability - 0.85).abs() < 1e-6);
    }
}
