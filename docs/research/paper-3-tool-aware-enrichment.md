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
refresh. Three diagnostic rates plus a ROI counter, all on
[`EnrichmentEffectiveness`][effectiveness]:

| Metric | Computation | Target | What it tells the operator |
|---|---|---|---|
| **prefetch hit rate** | `cited / total_prefetches` | ≥ 60% | Was the planner's speculation worth it? Below means too greedy. |
| **decline recall loss** | `late_invoked / total_declines` | ≤ 10% | Did the planner skip something the LLM ended up needing? |
| **cost overrun rate** | `overruns / total_predictions` (overrun = actual ≥ 130% of predicted) | ≤ 15% | Are `cost_model.typical_kb` priors still valid? |
| **inference calls saved** | `prefetch + dedup + fail_fast` buckets | as high as possible | Headline ROI: how many LLM tool-use round-trips the planner short-circuited. |

### Inference calls saved

Three independent mechanisms by which the planner removes a tool
round-trip from the LLM's loop:

| Bucket | When it increments | Where it's wired |
|---|---|---|
| `inference_calls_saved_prefetch` | `enricher_prefetched && cited_in_next_n_turns == Some(true)` — the planner pre-fetched a result and the LLM textually cited it | `EnrichmentEffectiveness::accumulate` reads `PipelineEvent` |
| `inference_calls_saved_dedup` | `is_dedup_hit == true` — Paper 2 L0 replaced the body with a near-ref hint, so the LLM never gets the full payload | same accumulator |
| `inference_calls_saved_fail_fast` | the planner refused to issue a `fail_fast_after_n` repeat (e.g. third empty `ToolSearch`) — no `PipelineEvent` is emitted | `EnrichmentEffectiveness::record_fail_fast_skip(predicted_tokens)` from the planner side |

`inference_tokens_saved` is the sum of `tokens_baseline` (or
`predicted_cost_tokens` for the fail-fast bucket) across all three
buckets — the "we kept this much context out of the LLM's window"
number.

`PipelineEvent` carries four enricher fields per call:

- `enricher_prefetched: bool`
- `enricher_predicted_cost_tokens: u32`
- `enricher_decline_reason: Option<String>`
- `cited_in_next_n_turns: Option<bool>` (filled by offline post-pass)

`SessionSummary.enrichment` aggregates them via
`EnrichmentEffectiveness::accumulate`, the same accumulator that
`tune analyze` runs over historical JSONL. One-line report:

```
prefetch_hit=72.1% decline_recall_loss=8.4% cost_overrun=11.0%
calls_saved=89 (prefetch=42, dedup=39, fail_fast=8) tokens_saved=128400
prefetches=412 declines=187 predictions=599
```

`tune analyze` prints this line under `# enrichment:` whenever any
bucket is non-zero, alongside the existing dedup / encoder split. On
a corpus with no enrichment activity the line is omitted to keep the
default output unchanged.

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

## Race strategy

A speculative pre-fetch is **only safe when re-issuing the call has
no observable consequence beyond what the LLM was going to do anyway**.
The pipeline encodes this as `SideEffectClass` on every annotated
tool — `Pure` (deterministic + idempotent) and `ReadOnly` (mutates
nothing remote, freshness via TTL) are eligible; `MutatesLocal`,
`MutatesExternal`, and `Indeterminate` are blocked even when
`enrichment.enabled = true`. A per-tool `speculate: Some(false)`
override (set by `tune analyze`'s R7 rule) trumps the static class.

The host runs a "**bounded synchronous wait + LLM hint**" race:

```
T=0    LLM → tools/call(Glob "src/**/*.rs")
T=10   server post-processes the response, runs build_plan,
       speculation engine spawns 2 Read prefetches in parallel
T=10   wait_within(prefetch_timeout_ms = 1000ms)
T=15   both prefetches landed → write bodies into DedupCache,
       append a hint to the response:
       > [enrichment: pre-fetched Read(src/main.rs), Read(src/lib.rs)
       >              in background — call as usual, results served
       >              from cache]
       LLM sees the hint AND the dedup cache is already warm — its
       follow-up Read calls collapse to L0 hits.

       (timeout path)
T=10.5 budget exhausted → response goes back without bodies; tasks
       keep running in background and land in the cache by the time
       the LLM actually issues the call.
```

This is "best-of-both": no extra latency on the main response when
the prefetch is fast, fallback to cached-replay when it's slow, and
the LLM gets a textual heads-up either way. Cancellation cascades
on session shutdown — `JoinSet::abort_all()` plus a drained
`join_next` loop guarantees no orphan IO outlives the connection.

Per-host concurrency is capped through `rate_limit_host` —
`api.github.com`, `api.clickup.com`, `api.fireflies.ai`, host
extracted from the URL for `WebFetch`. Two providers hitting the
same domain correctly share the budget; self-hosted GitLab / Jira
deployments set `[tools.<name>].rate_limit_host` per-tool in TOML.

## Validation strategy

The replay harness `replay_paper3_pipeline.py` reads JSONL telemetry
emitted by the live pipeline (or by `annotate_cited_prefetches.py`)
and computes the four headline metrics per session:

| Metric | Computation | Target |
|---|---|---|
| prefetch hit rate | `cited / total_prefetches` | ≥ 0.60 |
| decline recall loss | `late_invoked / total_declines` | ≤ 0.10 |
| cost overrun rate | `overruns(≥ 130%) / total_predictions` | ≤ 0.15 |
| inference calls saved | `prefetch + dedup + fail_fast` buckets | as high as possible |

The harness is **strictly an aggregator** — it doesn't re-spawn the
planner. Numbers come straight from `enricher_prefetched`,
`cited_in_next_n_turns`, `enricher_decline_reason`,
`enricher_predicted_cost_tokens`, `is_dedup_hit` and
`tokens_baseline` — fields the live pipeline already wrote. So
results are reproducible across runs.

The "did the planner pay for itself" answer is the headline
`tokens_saved` number from the harness output, plus the `# enrichment:`
line `tune analyze` prints whenever any saving was recorded:

```
# enrichment: prefetch_hit=72.1% decline_recall_loss=8.4% cost_overrun=11.0%
              race_win=58.0% waste=12.0%
              calls_saved=89 (prefetch=42, dedup=39, fail_fast=8)
              tokens_saved=128400  prefetches=412 dispatched=398
              declines=187  predictions=599
```

Real numbers are pending the first production deployment with
`enrichment.enabled = true`; on the synthetic 12-event smoke corpus
the harness reports `prefetch_hit=0.0% calls_saved=0` (nothing was
cited, as designed for that fixture).

[enrichment-tests]: ../../crates/plugins/format-pipeline/src/enrichment.rs

## Implementation status

### Shipped

- **Schema** — `ToolValueModel` + `SideEffectClass` (Pure / ReadOnly /
  MutatesLocal / MutatesExternal / Indeterminate) + per-tool
  `speculate` override + `rate_limit_host` (per-domain). `FollowUpLink`
  carries `projection` + `projection_arg` so user-annotated MCP
  follow-ups work entirely in TOML. `AdaptiveConfig` schema v3 → v4
  migration with `[enrichment]` section (off by default,
  `prefetch_timeout_ms = 1000`, `max_parallel_prefetches = 3`,
  `prefetch_budget_tokens = 8000`, `respect_rate_limits = true`).
  ([`tool_value_model.rs`][model], [`adaptive_config.rs`][adaptive]).
  +18 unit tests.
- **Built-in defaults** — `tool_defaults.rs` annotates the top 15
  tools with `side_effect_class` (Read/Grep `Pure`, Glob/WebSearch/
  WebFetch/ToolSearch `ReadOnly`, Edit/Write/MultiEdit/NotebookEdit
  `MutatesLocal`, Bash/Agent `Indeterminate`). +7 tests.
- **Provider extensions** — `ToolEnricher::value_model` +
  `project_args` + `rate_limit_host` for **5 providers**: gitlab,
  github, jira, clickup, fireflies. Each ships read-only chains
  (e.g. GitLab `get_merge_requests → get_merge_request_discussions`
  with `iid → merge_request_id`). All mutating endpoints flagged
  `MutatesExternal`. +10 tests.
- **Projection extractors** — `format-pipeline/src/projection.rs`:
  built-in extractors for Glob/Grep/WebSearch chains, generic JSON-
  tree fallback driven by `(projection, projection_arg)`,
  `extract_host` URL parser (no `url` crate dependency), capped at
  `MAX_PROJECTIONS_PER_LINK = 3`. +15 tests.
- **`EnrichmentPlanner`** — greedy 1/2-optimal solver, audit-only
  free admission, cost-token clamp (≥ 1 for non-AuditOnly),
  self-loop / already-used filtering, declined-with-reason output.
  +9 tests.
- **`SpeculationEngine`** (`devboy-mcp/src/speculation.rs`) —
  `PrefetchDispatcher` trait, JoinSet-based dispatch, per-host
  `HostBudget` cap, `wait_within(prefetch_timeout_ms)` race with
  abort-on-shutdown cascade. +9 tests.
- **Wiring in `SessionPipeline`** — async `with_speculation`,
  `speculate_after(tool, prev_response)`, `should_skip`,
  `record_fail_fast_skip`, `enrichment_snapshot`,
  `recent_tools_snapshot`, async `shutdown` (also via Drop). Every
  L0 dedup hit auto-increments
  `inference_calls_saved_dedup`/`inference_tokens_saved`. +5
  integration tests including end-to-end Glob → Read race-win.
- **Cross-tool invalidation** — `DedupCache::invalidate_by_tool` +
  `LayeredPipeline.process` reading `value_model.invalidates`. +3
  tests.
- **Telemetry** — 4 enricher fields on `PipelineEvent`
  (`enricher_prefetched`, `enricher_predicted_cost_tokens`,
  `enricher_decline_reason`, `cited_in_next_n_turns`).
  `EnrichmentEffectiveness` aggregate with `prefetch_hit_rate`,
  `decline_recall_loss`, `cost_overrun_rate`, race instrumentation
  (`prefetch_dispatched`, `prefetch_won_race`, `prefetch_wasted`),
  three `inference_calls_saved_*` buckets + `inference_tokens_saved`,
  `accumulate(&PipelineEvent)`, `record_fail_fast_skip`,
  `record_prefetch_*` helpers, and `report()`. +21 tests.
- **`tune analyze`** prints the planner ROI line whenever any saving
  was recorded; `--auto-enrichment` flag runs **R7** — per-tool
  `speculate = false` for hit rate < 50% (≥ 10 prefetches) and
  global `enrichment.enabled = true` flip when corpus-wide hit rate
  ≥ 60% with no per-tool disables. +4 tests + CLI smoke.
- **`devboy tune from-claude-logs --tools`** seeds `[tools.*]` from
  observed tool mix without overwriting hand overrides. +2 tests.
- **`annotate_cited_prefetches.py`** — offline post-pass that walks
  JSONL and sets `cited_in_next_n_turns` based on whether the same
  tool was organically called in the next 1-3 events.
- **`replay_paper3_pipeline.py`** — replay validation harness; CSV
  per-session + grand-total + headline summary on stderr.
- **`McpPrefetchDispatcher`** — production bridge from
  `SpeculationEngine` to `McpServer::execute_for_prefetch`. Routes
  prefetches through the same routing engine / proxy / fallback
  machinery as the main flow. Internal context-management tools
  (`use_context`, `list_contexts`, …) are explicitly rejected from
  the speculation path. +4 tests in
  [`prefetch_adapter.rs`](../../crates/devboy-mcp/src/prefetch_adapter.rs).
- **Intent-aware boost** — `TurnContext.intent_keywords` now drives
  planner scoring: every `default_include = false` field group
  whose member fields case-insensitively match a keyword raises the
  tool's value-score by `1.0 + Σ estimated_value` (capped at 2.5×).
  Lets a query like *"snippets about retry logic"* lift WebSearch's
  opt-in `snippet` group for that turn. +5 tests.
- **Latency / dollar awareness** —
  [`PlannerOptions::cost_aware()`](../../crates/plugins/format-pipeline/src/enrichment.rs)
  enables two compounding penalties: tools with
  `cost_model.latency_ms_p50 ≥ 5000 ms` get value halved; tools
  with `cost_model.dollars ≥ $0.10` get value halved; both
  triggers → `0.25× value`. +6 tests including end-to-end
  ordering check (FastTool admitted before SlowTool only when
  cost-aware is on).

### Deferred

- **Production replay numbers** — gated on the first deployment
  with `enrichment.enabled = true`. The harness
  (`replay_paper3_pipeline.py`), post-pass
  (`annotate_cited_prefetches.py`), production dispatcher
  (`McpPrefetchDispatcher`), and target table are all ready;
  headline numbers will land with the first real session.

[adaptive]: ../../crates/plugins/format-pipeline/src/adaptive_config.rs

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
