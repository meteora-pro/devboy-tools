//! Paper 3 — built-in default `ToolValueModel`s for selected common
//! tools by corpus volume.
//!
//! Anchored on `docs/research/paper3_corpus_findings.md` (P-3-01).
//! We ship defaults for a curated subset of the highest-volume tools
//! (the canonical patterns from §Real-world patterns); the corpus has
//! more tools above the 100-session threshold, but the long tail
//! either reuses one of the shipped defaults via `[tools."*"]` or is
//! overridden per-installation. Users can override any seeded
//! annotation through `[tools.<name>]` in `pipeline_config.toml`
//! (Paper 3 §Provider extensibility).
//!
//! The numbers below are *priors* — `tune analyze` will refine them
//! against real telemetry, the same way Paper 2's adaptive tuner
//! refines the encoder profiles.

use std::collections::BTreeMap;

use devboy_core::{CostModel, FollowUpLink, ToolValueModel, ValueClass};

/// Returns the seeded `[tools.*]` map every layered pipeline starts
/// with. `merge_right_wins` lets the user's TOML overrides win over
/// these defaults.
pub fn default_tool_value_models() -> BTreeMap<String, ToolValueModel> {
    let mut m = BTreeMap::new();

    // ─── Read — workhorse pattern (50 675 calls, 1 363 sessions) ─────
    // Median 2.5 kB, p99 43 kB. Critical: file content is non-negotiable
    // for code-edit work. Mutation hook (already wired in P-203-04)
    // invalidates on Edit/Write/MultiEdit/NotebookEdit.
    m.insert(
        "Read".into(),
        ToolValueModel {
            value_class: ValueClass::Critical,
            cost_model: CostModel {
                typical_kb: 2.5,
                max_kb: Some(43.0),
                latency_ms_p50: Some(50),
                ..CostModel::default()
            },
            follow_up: vec![FollowUpLink {
                tool: "Read".into(),
                probability: 0.45,
                projection: None,
            }],
            ..ToolValueModel::default()
        },
    );

    // ─── Edit / Write / MultiEdit — mutating tools ───────────────────
    // Their responses are tiny (median 162 / 137 / negligible bytes).
    // Their value is in *invalidating* the read cache — handled by the
    // mutation hook, but we declare `invalidates` so cross-tool
    // invalidation in P-3-07 picks them up uniformly.
    m.insert(
        "Edit".into(),
        ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: CostModel {
                typical_kb: 0.2,
                max_kb: Some(1.0),
                latency_ms_p50: Some(20),
                ..CostModel::default()
            },
            follow_up: vec![
                FollowUpLink {
                    tool: "Bash".into(),
                    probability: 0.27,
                    projection: None,
                },
                FollowUpLink {
                    tool: "Read".into(),
                    probability: 0.14,
                    projection: None,
                },
            ],
            invalidates: vec!["Read".into(), "Grep".into()],
            ..ToolValueModel::default()
        },
    );
    m.insert(
        "Write".into(),
        ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: CostModel {
                typical_kb: 0.2,
                ..CostModel::default()
            },
            invalidates: vec!["Read".into(), "Grep".into(), "Glob".into()],
            ..ToolValueModel::default()
        },
    );
    m.insert(
        "MultiEdit".into(),
        ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: CostModel {
                typical_kb: 0.2,
                ..CostModel::default()
            },
            invalidates: vec!["Read".into(), "Grep".into()],
            ..ToolValueModel::default()
        },
    );
    m.insert(
        "NotebookEdit".into(),
        ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: CostModel {
                typical_kb: 0.5,
                ..CostModel::default()
            },
            invalidates: vec!["Read".into()],
            ..ToolValueModel::default()
        },
    );

    // ─── Bash — generic shell (110 930 calls — the most common tool) ─
    // Critical for verification (cargo test, git, etc.) but median
    // response is tiny (223 B). No follow-up annotation: corpus shows
    // Bash → * is too varied to prefetch usefully.
    m.insert(
        "Bash".into(),
        ToolValueModel {
            value_class: ValueClass::Critical,
            cost_model: CostModel {
                typical_kb: 0.2,
                max_kb: Some(9.0),
                latency_ms_p50: Some(200),
                ..CostModel::default()
            },
            ..ToolValueModel::default()
        },
    );

    // ─── Grep — find-then-fix loop core (16 718 calls) ───────────────
    // 1 120 (Grep → Edit) + 1 671 (Edit → Grep) edges. Strong prefetch
    // signal: after Grep, prefetch top-3 file contents as Read.
    m.insert(
        "Grep".into(),
        ToolValueModel {
            value_class: ValueClass::Critical,
            cost_model: CostModel {
                typical_kb: 0.3,
                max_kb: Some(10.5),
                latency_ms_p50: Some(80),
                ..CostModel::default()
            },
            follow_up: vec![
                FollowUpLink {
                    tool: "Read".into(),
                    probability: 0.35,
                    projection: Some("path".into()),
                },
                FollowUpLink {
                    tool: "Edit".into(),
                    probability: 0.07,
                    projection: Some("path".into()),
                },
                FollowUpLink {
                    tool: "Grep".into(),
                    probability: 0.39,
                    projection: None,
                },
            ],
            ..ToolValueModel::default()
        },
    );

    // ─── Glob — bulk listing → inspect-each (6 202 calls) ────────────
    // Glob → Read 2 007 edges, Glob → Grep 775. Speculative prefetch
    // of top-N results when intent is "where is X used".
    m.insert(
        "Glob".into(),
        ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: CostModel {
                typical_kb: 0.2,
                max_kb: Some(16.6),
                latency_ms_p50: Some(60),
                ..CostModel::default()
            },
            follow_up: vec![
                FollowUpLink {
                    tool: "Read".into(),
                    probability: 0.32,
                    projection: Some("match_path".into()),
                },
                FollowUpLink {
                    tool: "Grep".into(),
                    probability: 0.13,
                    projection: Some("match_path".into()),
                },
                FollowUpLink {
                    tool: "Glob".into(),
                    probability: 0.41,
                    projection: None,
                },
            ],
            ..ToolValueModel::default()
        },
    );

    // ─── WebSearch / WebFetch — search → resolve chain (1 081 edges) ─
    // 6 fields surface in our corpus; only `title`/`url` are reliably
    // cited downstream — drop snippets first under tight budget.
    m.insert(
        "WebSearch".into(),
        ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: CostModel {
                typical_kb: 3.1,
                max_kb: Some(7.2),
                latency_ms_p50: Some(900),
                freshness_ttl_s: Some(3600),
                ..CostModel::default()
            },
            follow_up: vec![FollowUpLink {
                tool: "WebFetch".into(),
                probability: 0.65,
                projection: Some("url".into()),
            }],
            field_groups: {
                let mut g = BTreeMap::new();
                g.insert(
                    "must_have".into(),
                    devboy_core::FieldGroup {
                        fields: vec!["title".into(), "url".into()],
                        estimated_value: 1.0,
                        default_include: true,
                    },
                );
                g.insert(
                    "nice_to_have".into(),
                    devboy_core::FieldGroup {
                        fields: vec!["snippet".into()],
                        estimated_value: 0.3,
                        default_include: false,
                    },
                );
                g
            },
            ..ToolValueModel::default()
        },
    );
    m.insert(
        "WebFetch".into(),
        ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: CostModel {
                typical_kb: 1.2,
                max_kb: Some(24.0),
                latency_ms_p50: Some(800),
                freshness_ttl_s: Some(900),
                ..CostModel::default()
            },
            ..ToolValueModel::default()
        },
    );

    // ─── Task management noise (audit_only) ──────────────────────────
    // `TaskUpdate` median 23 B, `TodoWrite` 160 B, `TaskCreate` 78 B.
    // Excluded from budget accounting entirely (Paper 3 §6).
    for name in [
        "TaskUpdate",
        "TaskCreate",
        "TaskGet",
        "TaskList",
        "TodoWrite",
    ] {
        m.insert(name.into(), ToolValueModel::audit_only());
    }

    // ─── ToolSearch — fail-fast loop ─────────────────────────────────
    // 50%+ of repeated calls return zero bytes; planner gives up after
    // two empty calls and emits a "tool not found" note instead.
    m.insert(
        "ToolSearch".into(),
        ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: CostModel {
                typical_kb: 0.0,
                max_kb: Some(0.1),
                ..CostModel::default()
            },
            fail_fast_after_n: Some(2),
            ..ToolValueModel::default()
        },
    );

    // ─── Agent / Task subagent — long, expensive, value-rich ─────────
    // Median 6.6 kB, p99 23.7 kB. `Supporting` because the LLM can
    // proceed without it but accuracy drops.
    m.insert(
        "Agent".into(),
        ToolValueModel {
            value_class: ValueClass::Supporting,
            cost_model: CostModel {
                typical_kb: 6.5,
                max_kb: Some(23.7),
                latency_ms_p50: Some(60_000),
                ..CostModel::default()
            },
            ..ToolValueModel::default()
        },
    );

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_top_tools_from_corpus() {
        let m = default_tool_value_models();
        for required in [
            "Read",
            "Edit",
            "Write",
            "Bash",
            "Grep",
            "Glob",
            "WebSearch",
            "WebFetch",
            "TaskUpdate",
            "TodoWrite",
            "ToolSearch",
            "Agent",
        ] {
            assert!(m.contains_key(required), "missing default for {required}");
        }
    }

    #[test]
    fn audit_only_tools_are_excluded_from_budget() {
        let m = default_tool_value_models();
        for name in ["TaskUpdate", "TaskCreate", "TodoWrite"] {
            assert!(
                m[name].excluded_from_budget(),
                "{name} should be excluded_from_budget"
            );
        }
    }

    #[test]
    fn read_is_critical_with_typical_kb_anchored_on_corpus() {
        let m = default_tool_value_models();
        let read = &m["Read"];
        assert_eq!(read.value_class, ValueClass::Critical);
        assert_eq!(read.cost_model.typical_kb, 2.5);
    }

    #[test]
    fn grep_followup_includes_read_and_edit_with_path_projection() {
        let m = default_tool_value_models();
        let fu = &m["Grep"].follow_up;
        let read_link = fu.iter().find(|l| l.tool == "Read").unwrap();
        assert_eq!(read_link.projection.as_deref(), Some("path"));
        let edit_link = fu.iter().find(|l| l.tool == "Edit").unwrap();
        assert_eq!(edit_link.projection.as_deref(), Some("path"));
    }

    #[test]
    fn web_search_drops_snippets_first_under_budget() {
        let m = default_tool_value_models();
        let groups = &m["WebSearch"].field_groups;
        assert!(groups["must_have"].default_include);
        assert!(!groups["nice_to_have"].default_include);
    }

    #[test]
    fn tool_search_has_fail_fast() {
        let m = default_tool_value_models();
        assert_eq!(m["ToolSearch"].fail_fast_after_n, Some(2));
    }

    #[test]
    fn mutating_tools_invalidate_read_cache() {
        let m = default_tool_value_models();
        for name in ["Edit", "Write", "MultiEdit"] {
            assert!(
                m[name].invalidates.iter().any(|t| t == "Read"),
                "{name} should invalidate Read"
            );
        }
    }
}
