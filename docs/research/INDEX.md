# Research Papers

This directory captures research ideas developed through analysis of real Claude Code usage logs
from the devboy-tools production system. Each paper builds on empirical data collected via
[devboy-tools-agent-usage](https://github.com/meteora-pro/devboy-tools-agent-usage).

## Papers

| # | Title | Status | Core Idea |
|---|-------|--------|-----------|
| [1](./paper-1-trimtree.md) | TrimTree: Priority-Driven Pagination for LLM Tool Responses | draft | Knapsack-based item selection within token budget; p₁ metric |
| [2](./paper-2-mckp-format-adaptive.md) | Format-Adaptive Tree Encoding via Multi-Choice Knapsack | draft | Per-subtree format selection (CSV / table / key:value) to minimize tokens |
| [3](./paper-3-context-enrichment.md) | Context Enrichment Hypothesis in Agentic Tool Pipelines | draft | Thin initial context → more follow-up enrichment calls; measurement & mitigation |
| [4](./paper-4-notebook-parquet.md) | Dataset-as-Context: Queryable Parquet Artifacts for LLM Agents | draft | Replace paginated tool calls with notebook + Parquet; LLM queries what it needs |

## Research Arc

Papers 1–3 address **push-based** optimization: the server decides what to send and how.
Paper 4 shifts to **pull-based**: the LLM receives a queryable dataset and extracts exactly
what it needs via generated code.

```
Paper 1: binary knapsack  →  which items to include (fixed format)
Paper 2: multi-choice knapsack  →  which items + which format per subtree
Paper 3: enrichment detection  →  measure & reduce reactive follow-up calls
Paper 4: pull-based query  →  LLM writes queries against Parquet artifacts
```

## Shared Empirical Foundation

All papers are grounded in the same dataset: real Claude Code sessions from devboy GitLab
MCP pipeline (523 sessions, 10,644 MCP tool responses analyzed).

Key baseline findings:
- `get_merge_request_diffs`: P90 = 35k chars ≈ 10k tokens; 28% exceed 8k token budget
- `get_epics`: P90 = 43k chars ≈ 12k tokens; 37% exceed 8k token budget
- After large responses: agents always produce text (next turn), never paginate
- Context enrichment: thin issues (< 200 chars/item) → 43% of turns add `get_issue` calls;
  rich issues (1.5k–4k chars/item) → only 2% do. Pearson r = −0.28

## Candidate Benchmarks

| Benchmark | Papers | Why |
|-----------|--------|-----|
| SWE-bench Verified (500 tasks) | 1, 3 | Ground truth which file/issue needed → measure p₁ |
| τ-bench (retail + airline) | 1, 3 | Pass/fail per task → measure E[tool_calls] |
| ACON eval suite | 2 | Direct baseline: also compresses agent observations |
| FRAMES (824 multi-hop) | 1 | Multi-hop → E[chunks] measurement |
| MCPAgentBench (180 tasks) | 2, 4 | MCP-native; Token Efficiency metric |
| CostBench (travel domain) | 1, 4 | Cost-optimality = min tool calls to goal |
| LLMLingua-2 eval suite | 2 | Standard token-savings measurement |
| ToolBench RapidAPI dataset | all | 16k real REST responses → unit test fixtures |
| Terminal-Bench / tbench.ai (89 tasks) | 1, 2 | Software eng + ML + sysadmin tasks; custom adapter → log tool calls for token savings |
