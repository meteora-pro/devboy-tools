# Paper 3: Tool-Aware Knapsack Enrichment via Provider-Annotated Value Models

**Status:** draft
**Target venue:** ACL 2026 / NAACL 2026
**Authors:** Andrei Mazniak

---

## Problem

Modern LLM agents (Claude Code, OpenAI Assistants, Aider) waste a
large fraction of every conversation on tool calls that did not need
to happen, on response fields the agent never reads, and on duplicate
queries the agent could have answered from prior context.

Paper 1 (TrimTree) decided **which items inside one tool response** to
include under a budget. Paper 2 (MCKP) decided **how to encode** the
chosen items so the LLM still understands them. Paper 3 asks the
upstream question:

> Given the agent's recent activity, the user's intent, and a per-turn
> token budget, **which tool calls should fire next, with which field
> projections**, to maximise the value the LLM receives?

This is the *inter-tool* knapsack — and it cannot be solved without
help from the providers themselves. A planner that knows nothing
about a tool's response shape, expected size, or downstream
consumers can only react after the fact. A planner that *does* know
those facts can pre-fetch likely follow-ups, drop low-value fields
before the LLM ever sees them, and refuse to re-issue a polling call
the agent has already exhausted.

The contract between providers and the planner is an annotation:
[`ToolValueModel`][model]. Providers ship one per tool; users
override anything they disagree with through a `[tools.<name>]` block
in `pipeline_config.toml`. The runtime composes the two layers and
hands the result to the knapsack solver.

[model]: ../../crates/devboy-core/src/tool_value_model.rs

## Core idea

```
┌──────────────────────────────────────────────────────────────────┐
│  Paper 1 (TrimTree) — items inside ONE response                  │
│  Paper 2 (MCKP)     — encoding of the chosen items               │
│  Paper 3 (this)     — WHICH tool calls fire & with what fields   │
└──────────────────────────────────────────────────────────────────┘
```

Three knapsacks, three layers, one composable cost-of-context model.

The Paper 3 contract is the new piece: every tool ships a value
model that lets the planner score it without running it.

## Annotation taxonomy

```rust
pub struct ToolValueModel {
    pub value_class:        ValueClass,                          // 1
    pub field_groups:       BTreeMap<String, FieldGroup>,        // 2
    pub cost_model:         CostModel,                           // 3
    pub follow_up:          Vec<FollowUpLink>,                   // 4
    pub invalidates:        Vec<String>,                         // 5
    pub fail_fast_after_n:  Option<u32>,                         // 6
}
```

1. **`value_class`** — first-pass importance filter:

   - `Critical` — file content, search results. Always included.
   - `Supporting` — useful context. Dropped second under budget.
   - `Optional` — nice-to-have. Dropped first.
   - `AuditOnly` — agent-internal noise (TaskUpdate, TodoWrite).
     Excluded from budget accounting entirely.

2. **`field_groups`** — named subsets of the response. By convention
   `must_have` / `nice_to_have` / `debug`, with per-group
   `estimated_value` (0..1) and `default_include`. Lets the planner
   drop snippets without dropping the call.

3. **`cost_model`** — typical_kb (anchor), max_kb, latency_ms_p50,
   dollars, freshness_ttl_s. Live priors; refined by `tune analyze`.

4. **`follow_up`** — empirical edges `(tool, probability,
   projection?)` mined from the user's session history. Drives
   speculative pre-fetch.

5. **`invalidates`** — cross-tool cache busting. Generalises Paper 2's
   file-mutation hook (`Edit → Read`) to arbitrary relationships
   (`update_issue → get_issue`).

6. **`fail_fast_after_n`** — corpus showed `ToolSearch` returns 0
   results in 50%+ of repeated calls; this knob lets the planner
   short-circuit unproductive loops.

The shipped defaults for the top tools live in
[`tool_defaults.rs`][defaults] and are anchored on
[`paper3_corpus_findings.md`](paper3_corpus_findings.md).

[defaults]: ../../crates/plugins/format-pipeline/src/tool_defaults.rs

## Provider extensibility

```
                  ┌── built-in defaults  (tool_defaults.rs)  ──┐
                  │                                            │
                  ▼                                            │
[tools.<name>] in pipeline_config.toml ── user override ──►  AdaptiveConfig.tools
                  ▲                                            │
                  │                                            │
                  └── provider crate ToolEnricher::value_model──┘
```

Three sources, merged right-wins:

1. **`tool_defaults::default_tool_value_models()`** — corpus-anchored
   priors shipped with the pipeline.
2. **Provider crate `ToolEnricher::value_model(tool_name)`** —
   per-provider customisation that lives next to the tool itself.
3. **`[tools.<name>]` in `~/.devboy/pipeline_config.toml`** — user
   override at runtime, hot-reloaded the same way Paper 2's profiles
   are.

Resolution by `AdaptiveConfig::effective_tool_value_model(name)`:

1. Exact match in `tools[name]`.
2. Wildcard `tools["*"]` for blanket policies.
3. `None` — caller substitutes the global default.

The TOML file is human-readable; `devboy tune from-claude-logs
--tools` seeds it with sensible defaults for every tool the user has
already called.

## Knapsack algorithm

Greedy by `value / cost` density, prereq closure honoured, AuditOnly
tools admitted free. Source: [`enrichment.rs`][enrichment].

```
SessionContext { recent_tools, budget_tokens, intent_keywords }
                        │
                        ▼
        enumerate candidates from each recent_tool's
        `follow_up` graph; deduplicate by tool, keep
        the highest-probability projection
                        │
                        ▼
        filter by min_followup_probability
                        │
                        ▼
        score:  value_score(class) / cost_tokens
                        │
                        ▼
        sort by density desc; admit greedily until
        budget_tokens exhausts; record DeclineReason
        for everything left out
                        │
                        ▼
        EnrichmentPlan { calls: [...], remaining_budget,
                         declined: [...] }
```

Why greedy? It is provably **1/2-optimal** for the 0/1 knapsack and
runs in `O(N log N)` — orders of magnitude cheaper than the exact DP
on a hot path. The cost numbers we plug in are mined priors, not
ground truth — exact optimality on imprecise numbers buys nothing.

Self-loops are skipped (re-reads are the dedup cache's job, not the
planner's). Tools already in `recent_tools` are skipped (no point
pre-fetching what the agent just used).

[enrichment]: ../../crates/plugins/format-pipeline/src/enrichment.rs

## Adaptive tuning + effectiveness metric

Same idiom as Paper 2: telemetry → offline analyser → annotation
refresh. Four rates the operator reads
([`EnrichmentEffectiveness`][effectiveness]):

| Metric | Computation | Target | What it tells the operator |
|---|---|---|---|
| **prefetch hit rate** | `cited / total_prefetches` | ≥ 60% | Was the planner's speculation worth it? Below means too greedy. |
| **decline recall loss** | `late_invoked / total_declines` | ≤ 10% | Did the planner skip something the LLM ended up needing? |
| **cost overrun rate** | `overruns / total_predictions` (overrun = actual ≥ 130% of predicted) | ≤ 15% | Are `cost_model.typical_kb` priors still valid? |
| **net token savings** | `(baseline_no_planner − actual_with_planner) / baseline_no_planner` | > 0 | The headline ROI number — did the planner pay for itself? |

`PipelineEvent` carries four enricher fields per call:

- `enricher_prefetched: bool`
- `enricher_predicted_cost_tokens: u32`
- `enricher_decline_reason: Option<String>`
- `cited_in_next_n_turns: Option<bool>` (filled by offline post-pass)

`SessionSummary.enrichment` aggregates them. `tune analyze` reads
JSONL, computes the four rates, and prints a one-line report:

```
prefetch_hit=72.1%  decline_recall_loss=8.4%  cost_overrun=11.0%
prefetches=412  declines=187  predictions=599
```

When `prefetch_hit_rate` drops below 50% on a specific tool, the
analyser flags the corresponding `cost_model` and `follow_up`
entries for tightening — exactly the same loop Paper 2 uses for the
encoder profiles.

[effectiveness]: ../../crates/plugins/format-pipeline/src/telemetry.rs

## Real-world patterns

Nine patterns in the 258 000-call Claude Code corpus surface as
default annotations. Detailed write-up in
[`paper3_corpus_findings.md`](paper3_corpus_findings.md). Summary:

| # | Pattern | Volume | What the enricher does |
|---|---|---:|---|
| 1 | Pipeline polling (`mcp__*__get_branch_pipeline → *`) | 614 edges | Near-ref hint after 15 s TTL; bail after 3 unchanged polls |
| 2 | File re-reads (`Read → Read`) | 22 243 edges | L0 dedup + mutation hook |
| 3 | Find-then-fix (`Grep ↔ Edit`) | 23 583 edges | Pre-fetch top-3 file contents after `Grep` |
| 4 | Bulk listing (`Glob → Read`) | 4 547 edges | Speculative pre-fetch of top-N |
| 5 | Web search → fetch | 1 081 edges | Pre-fetch top URL; drop snippets under budget |
| 6 | Task-management noise | 4 488+ edges | `audit_only` — never enters budget |
| 7 | Todo chains | 3 256+ edges | Inferred agent-phase signal |
| 8 | Failed search loops (`ToolSearch → ToolSearch`) | 267 edges | `fail_fast_after_n = 2` |
| 9 | Browser DOM dumps | 340 edges | Cap full_dom under budget unless intent mentions HTML |

Patterns 1, 2, 6 already pay off through Paper 2's L0 dedup +
audit-only filter. Patterns 3, 4, 5 are the Paper 3 wins — the
planner pre-fetches before the LLM has to ask. Patterns 8, 9 are
small but free: annotations alone, no new code.

## Validation strategy

The planner is implemented and unit-tested
([`enrichment.rs::tests`][enrichment-tests]); the corpus-replay
validation that produces the headline numbers is the next milestone:

1. Replay every session in the 144 658-event corpus twice — once with
   the planner active, once without.
2. Compute the four metrics above for each session.
3. Aggregate across the corpus. Targets:
   - prefetch hit rate ≥ 0.60
   - decline recall loss ≤ 0.10
   - cost overrun rate ≤ 0.15
   - net token savings > 10% over the no-planner baseline

The replay harness lives next to Paper 2's
`simulate_paper2_pipeline.py`; the post-pass that fills in
`cited_in_next_n_turns` scans the assistant text following each
prefetch for textual references to the prefetched body's
`content_sha_prefix_hex`.

[enrichment-tests]: ../../crates/plugins/format-pipeline/src/enrichment.rs

## Implementation status

### Shipped

- `ToolValueModel` schema + constructors + serde
  ([`tool_value_model.rs`][model]). +9 unit tests.
- `[tools.*]` section + `effective_tool_value_model` resolution
  + schema v3 migration on `AdaptiveConfig`. +6 tests.
- `ToolEnricher::value_model` trait extension (default-impl `None`).
- Built-in defaults for the top 15 tools by corpus volume
  ([`tool_defaults.rs`][defaults]). +7 tests.
- `EnrichmentPlanner` greedy solver with prereq closure, audit-only
  free admission, self-loop / already-used filtering, declined-with-
  reason output ([`enrichment.rs`][enrichment]). +8 tests.
- Cross-tool invalidation: `DedupCache::invalidate_by_tool` +
  `LayeredPipeline.process` hook reading `value_model.invalidates`.
  +2 dedup tests + 1 layered_pipeline integration test.
- Telemetry: 4 new `PipelineEvent` fields +
  `EnrichmentEffectiveness` summary with `prefetch_hit_rate`,
  `decline_recall_loss`, `cost_overrun_rate`, and `report()`. +6
  tests.
- `devboy tune from-claude-logs --tools` seeds `[tools.*]` from the
  user's observed tool mix without overwriting hand overrides. +2
  tests.

### Deferred

- **Host integration** — the MCP server reads `AdaptiveConfig.tools`
  but does not yet call `EnrichmentPlanner::build_plan` before
  emitting tool-use blocks. Wiring that in is the next milestone;
  the planner is already side-effect-free and ready.
- **Speculative pre-fetch execution** — once the host calls the
  planner, the resulting `EnrichmentPlan.calls` need to be issued
  out-of-band before the LLM's next message lands. Implementation
  shape: same MCP `tools/call` path the LLM uses, but tagged
  `enricher_prefetched = true` so telemetry can attribute citations.
- **Cited-in-next-n-turns post-pass** — Python scanner that walks
  assistant messages following each prefetch and sets
  `cited_in_next_n_turns` on the JSONL. Lives next to
  `extract_paper3_followups.py`.
- **Corpus replay validation** — produces the headline numbers in
  the §Validation strategy table.

## Related work

- TrimTree (Paper 1) — within-response item knapsack.
- MCKP format-adaptive encoding (Paper 2) — encoding of chosen items.
- MCP `ToolAnnotations` (2025-03) — static, per-tool metadata
  (readOnlyHint, destructiveHint). Paper 3 generalises these to
  cross-turn value/cost annotations.
- Anthropic *Writing Tools for Agents* (2026) — `ResponseFormat`
  enum, 25 k default budget. Paper 3's `field_groups` is the
  per-call analogue.
- ResourceLink `DualResponseToolResult` (arXiv 2510.05968) —
  preview + resource-link with `QueryMetadata.total_count`. Paper
  3's `cost_model.max_kb` aligns with this preview budget.
- Speculative decoding for token output (Leviathan et al. 2023) —
  conceptually adjacent but operates at the token level. Paper 3 is
  speculative *tool-call* pre-fetch.

## References

- Paper 1: [`paper-1-trimtree.md`](paper-1-trimtree.md)
- Paper 2: [`paper-2-mckp-format-adaptive.md`](paper-2-mckp-format-adaptive.md)
- Corpus mining: [`paper3_corpus_findings.md`](paper3_corpus_findings.md)
- Aggregate data: [`data/paper3_followup_edges.csv`](data/paper3_followup_edges.csv),
  [`data/paper3_tool_volume.csv`](data/paper3_tool_volume.csv)
