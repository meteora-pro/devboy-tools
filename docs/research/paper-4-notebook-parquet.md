# Paper 4: Dataset-as-Context — Queryable Parquet Artifacts for LLM Agents

**Status:** draft (early concept)  
**Target venue:** NeurIPS 2026 / ICML 2026  
**Authors:** Andrei Mazniak

---

## Problem

Papers 1–3 optimize **push-based** context delivery: the server selects and encodes what to
send. All three still produce a static snapshot — the LLM reads it passively.

But data has structure and volume that no static snapshot can efficiently represent. A project
with 1,247 issues cannot be meaningfully summarized at 4k tokens without losing something
important.

The key insight: **LLM agents can write code**. Instead of giving the agent a pre-selected
subset of data, give it a queryable dataset and let it retrieve exactly what it needs.

## Core Idea

Replace paginated tool calls with a **notebook + Parquet artifact** pair:

```
API Response (1247 issues, full data)
    ↓ [devboy-tools serializer]
issues.parquet (binary, columnar, ~50KB vs ~5MB JSON)
    +
notebook_header.py (schema + 5 sample rows + helper functions)
    ↓ [LLM generates query]
df[df.priority == 'high'][['id', 'title', 'due_date']].head(20)
    ↓ [code executor]
20 rows × 3 columns = ~200 tokens (vs 5MB full response)
```

## Paradigm Shift

| Approach | Who decides what to fetch | Format | Computed fields |
|----------|--------------------------|--------|-----------------|
| Raw API (no opt.) | Server | JSON | No |
| TrimTree (Paper 1) | Server (knapsack) | Markdown/JSON | No |
| MCKP (Paper 2) | Server (knapsack + format) | Adaptive Markdown | No |
| **Notebook+Parquet (Paper 4)** | **LLM (code)** | Query result | **Yes** |

The LLM becomes an active participant in data retrieval, not a passive consumer.

## Token Economics

For 1,000 issues with 7 fields:

| What the LLM sees | Tokens |
|-------------------|--------|
| Full JSON | ~45,000 |
| TrimTree (top-20, Markdown table) | ~800 |
| Notebook header (schema + 5 rows) | ~200 |
| LLM query code | ~50 |
| Query result (20 rows × 3 cols) | ~300 |
| **Total (notebook approach)** | **~550** |

Token cost is proportional to what the LLM actually needs, not what exists.

## Computed Fields

This is the capability that no static approach can match. LLM computes fields on demand:

```python
# Fields that don't exist in the raw API:
df['days_open'] = (pd.Timestamp.now() - df['created_at']).dt.days
df['is_overdue'] = df['due_date'] < pd.Timestamp.now()
df['complexity'] = df['comments_count'] * df['linked_issues_count']

# Aggregations:
df.groupby('assignee')['story_points'].sum()
df[df.sprint == current_sprint]['status'].value_counts()

# Cross-dataset joins (replacing enrichment tool calls from Paper 3):
issues.join(comments, on='id').join(linked_issues, on='id')
```

## Auto-Generated Notebook Header

devboy-tools (Rust) generates the notebook automatically from any API response:

```python
# === Issues Dataset | project: devboy | branch: DEV-570 ===
# Generated: 2026-04-21 | Rows: 1247 | Source: get_issues(project_id=42)
import polars as pl
df = pl.read_parquet('/tmp/ctx/issues_abc123.parquet')

# Schema:
# id: Int64 | title: Utf8 | status: Utf8 | priority: Utf8
# assignee: Utf8 | created_at: Datetime | due_date: Datetime | story_points: Float64

# Sample (5 rows):
# 524  Fix login bug      open  high   @alex  2026-03-01  2026-04-15  3.0
# 531  Upgrade deps       done  low    @mika  2026-01-10  2026-02-01  1.0

# Cross-references:
# issues.id ← mr_issues.issue_id → mrs.id   (join available)

# Helpers:
def high_priority(): return df.filter(pl.col('priority') == 'high')
def overdue(): return df.filter(pl.col('due_date') < pl.lit(datetime.now()))
```

## Stack

- **Rust serializer**: `arrow2` or `polars` crate — serialize API responses to Parquet
- **Python runtime**: polars (faster than pandas, Rust-native) or DuckDB (SQL dialect)
- **Notebook format**: `.py` script with structured header, or `.ipynb` for richer output
- **Code execution**: sandboxed Python interpreter or DuckDB wasm
- **Cross-dataset joins**: Arrow IPC for zero-copy sharing between datasets

## Relation to Previous Papers

| Paper | Connection |
|-------|-----------|
| Paper 1 | Parquet header is itself a TrimTree output (schema + sample) |
| Paper 2 | Column format selection (int vs string vs category) is MCKP at schema level |
| Paper 3 | JOIN queries replace enrichment tool calls — E[enrichment] → 0 when data is local |

The notebook approach is an orthogonal optimization layer: Papers 1–3 optimize a single
response; Paper 4 eliminates repeated responses by making data local to the agent.

## Experiments

1. **Token savings** — measure tokens(notebook_header + query + result) vs
   tokens(equivalent TrimTree response) across 200 SWE-bench Verified tasks.
   Expected: 60–80% additional savings on top of TrimTree.

2. **Computed field rate** — how often does the LLM generate derived fields?
   Measure on τ-bench: count computed fields per task vs baseline (no Parquet).
   Hypothesis: LLM computes ≥ 1 derived field in > 50% of tasks with Parquet.

3. **E[tool_calls] reduction** — compare total API tool calls per task:
   Condition A: paginated tool calls (current devboy)
   Condition B: Parquet artifact + notebook
   Metric: τ-bench pass rate, CostBench cost-optimality score.

4. **Query correctness** — does the LLM write syntactically and semantically correct queries?
   Measure query execution errors and hallucinated column names.
   Baseline: LLM knowledge of polars/pandas API.

## Key Claims

1. Notebook+Parquet reduces total tokens-per-task by 60–80% vs TrimTree (Papers 1–2) alone
2. LLM computes derived fields in > 50% of tasks when Parquet is available
3. E[enrichment_tool_calls] ≈ 0 with Parquet (replaces Paper 3's enrichment calls via JOIN)
4. Query correctness ≥ 85% on well-typed schemas with helper functions

## Risks and Open Questions

- **Code execution safety**: Parquet queries must run in a sandbox
- **Schema hallucination**: LLM may reference non-existent columns — mitigated by helper fns
- **Query latency**: Parquet read + query execution adds ~50–200ms per query
- **Model capability**: smaller models (Haiku, 7B) may write incorrect queries — needs eval
- **Large Parquet files**: at what row count does Parquet overhead exceed benefit?

## Implementation Status

- [ ] Rust Parquet serializer for MCP responses (`arrow2` or `polars`)
- [ ] Auto-generated notebook header template
- [ ] Sandboxed Python/DuckDB executor
- [ ] Cross-dataset join registration (issues × MRs × comments)
- [ ] Evaluation harness for token savings measurement

## Related Work

- Code Interpreter (OpenAI): LLM executes code, but on user-uploaded files, not API responses
- TabFact / TAPAS: table QA, LLM reasoning over structured tables
- DFSQL / Text-to-SQL: generating SQL from natural language
- Gorilla (UC Berkeley): LLM API calls — related to function/tool calling accuracy
- DuckDB in-process analytics: closest practical component
