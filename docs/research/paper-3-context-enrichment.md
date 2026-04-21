# Paper 3: Context Enrichment Hypothesis in Agentic Tool Pipelines

**Status:** draft  
**Target venue:** EMNLP 2026 (short paper) / findings track  
**Authors:** Andrei Mazniak

---

## Problem

When an LLM agent receives a list of issues or merge requests with thin per-item context
(few characters per item), it makes additional follow-up tool calls to enrich that context —
fetching comments, linked issues, epic details, etc. This enrichment overhead is implicit,
uncontrolled, and contributes to token cost and latency.

We call this the **Context Enrichment Hypothesis**:
> Items with low chars_per_item trigger more enrichment tool calls than items with high
> chars_per_item, because the agent needs more information before it can act.

## Empirical Evidence

Analysis of 523 real Claude Code sessions with 10,644 MCP tool responses:

**get_issues** (87 records with known item count):

| chars/item bucket | N | E[enrichment] | % turns with enrichment |
|-------------------|---|---------------|-------------------------|
| tiny < 200        | 7 | 0.43          | **43%**                 |
| med 500–1.5k      | 21 | 0.29          | 14%                     |
| large 1.5k–4k     | 55 | 0.02          | **2%**                  |

Pearson r(chars_per_item, enrichment_count) = **−0.280**  
Direction confirmed: thin context → more enrichment.

**get_merge_requests** (196 records):

| chars/item bucket | N | E[enrichment] | top enrichment tool |
|-------------------|---|---------------|---------------------|
| tiny < 200        | 53 | 1.74          | get_merge_request_discussions (127% of turns) |
| small 200–500     | 143 | 1.10         | same |

Agents almost always drill into discussions after seeing MR lists, regardless of context size —
suggesting `get_merge_request_discussions` should be included in the primary response.

## Formal Definition

```
chars_per_item(response) = total_chars(response) / items_shown(response)

enrichment_call = a tool call in the same turn as the primary call, where
  tool ∈ ENRICHMENT_TOOLS[primary_tool]

E[enrichment] = mean enrichment_count across all invocations of primary_tool
p(enrichment) = fraction of invocations with ≥ 1 enrichment call
```

Tool-specific enrichment sets:
- `get_issues` → {`get_issue`, `get_issue_comments`, `get_issue_relations`, `get_epics`}
- `get_merge_requests` → {`get_merge_request_discussions`, `get_merge_request_diffs`}
- `get_meeting_notes` → {`get_meeting_transcript`, `search_meeting_notes`}

## Intervention: Preemptive Enrichment

If the hypothesis holds, the optimal server behavior is:
1. Detect thin context (chars_per_item < threshold)
2. Automatically inline enrichment data in the primary response
3. Measure reduction in E[enrichment_calls] with vs without preemptive enrichment

This connects to Paper 2: the MCKP encoder can treat enrichment fields as low-cost nodes
that get included when the budget allows and the primary items are thin.

## Experiments

1. **Hypothesis validation** — larger dataset with more sessions.
   Target: 500+ records per tool, stratified by chars/item.
   Metric: Pearson r, Spearman ρ, Mann-Whitney U (thin vs rich groups).

2. **Causal study** — ablation using τ-bench:
   Condition A: thin initial response (no descriptions)
   Condition B: rich initial response (full descriptions)
   Measure: number of enrichment tool calls to complete the task.

3. **Preemptive enrichment** — modify devboy MCP to inline comments when chars_per_item < 300.
   Compare E[enrichment_calls] before and after.

4. **Cross-tool generalization** — test hypothesis on meeting tools:
   `get_meeting_notes` (thin) → `get_meeting_transcript` follow-up rate vs
   `get_meeting_notes` (rich).

## Key Claims

1. Context Enrichment Hypothesis holds: Pearson r < −0.25 for `get_issues` (confirmed empirically)
2. `get_merge_request_discussions` should be bundled with `get_merge_requests` (127% co-occurrence)
3. Preemptive enrichment when chars_per_item < 300 reduces E[enrichment_calls] by ≥ 30%

## Implementation Status

- [x] `context-enrichment` CLI command in track-claude-usage
- [x] Tool-specific enrichment sets
- [x] Pearson r computation
- [x] Markdown table item count parser
- [ ] Larger dataset collection (need 500+ records per tool)
- [ ] Causal study design on τ-bench
- [ ] Preemptive enrichment implementation in devboy-tools

## Data Collection

`track-claude-usage context-enrichment --tool get_issues --format csv > enrichment_data.csv`

Current dataset: 87 records for `get_issues`, 196 for `get_merge_requests`.
Target for submission: ≥ 500 per tool (needs ~3 months of additional sessions).

## Related Work

- Retrieval-Augmented Generation (RAG): related in that both address "not enough context"
- Chain-of-thought tool use (ReAct, Toolformer): agents decide when to call tools
- APIBank: evaluates API call accuracy, not enrichment pattern
- τ-bench: closest execution environment for causal study
