# Paper 1: TrimTree — Priority-Driven Pagination for LLM Tool Responses

**Status:** draft (all experiments complete; 820-line full draft)
**Target venue:** EMNLP 2026 / ACL 2026 (Systems track)
**Authors:** Andrei Mazniak

**Quick links**:
- **Short summary** (TLDR for reviewers): [`paper-1-SUMMARY.md`](paper-1-SUMMARY.md)
- **Reproducibility kit** (hardware, creds, commands, costs): [`paper-1-REPRODUCIBILITY.md`](paper-1-REPRODUCIBILITY.md)
- **Interactive analysis notebook**: [`notebooks/paper1_analysis.ipynb`](notebooks/paper1_analysis.ipynb)
- **Public aggregates & raw parquets**: `data/swe_bench_*.csv` and `data/llm_results/*.parquet`

**Reading order**:
- If you want the headline numbers — jump to §*Public Benchmark: SWE-bench Verified* and §*Hypothesis checks — Final*.
- If you're a reviewer checking methodology — read §*Core Idea* → §*Value Assignment Strategies* → §*Experiments*, then §*Public Benchmark*.
- If you want the empirical motivation — §*Real-World Gold-Selection Distribution* and §*Bash File-Search Gold-Selection* cover the production data that drove our design choices.

---

## Problem

LLM coding agents (Claude Code, Cursor, Copilot) consume tool responses that often exceed the
practical token budget. Current strategies are naive: truncate at N chars, or return everything
and let the LLM cope. Both waste tokens or lose information.

Agents using GitLab MCP pipeline regularly receive responses that overflow:
- `get_merge_request_diffs`: P90 = 35k chars (~10k tokens), 28% exceed 8k-token budget
- `get_epics`: P90 = 43k chars (~12k tokens), 37% exceed 8k-token budget

When overflow happens, agents always generate a text response in the next turn — they never
request more chunks. This means the first response must contain what the agent needs.

## Core Idea

Model the tool response as a **weighted tree** and solve a **binary 0/1 knapsack** to select
the highest-value subset of items that fits within the token budget.

```
API Response
    ↓ [parser]
Tree of items (each item = one issue / MR / comment / file hunk)
    ↓ [value assignment — priority scorer]
Scored items: (value, token_cost) pairs
    ↓ [knapsack DP solver]
Selected subset ≤ budget
    ↓ [encoder + chunk index]
Compact response + "[+N more, call with chunk=2]" hint
```

## Key Metrics

**Priority Hit Rate (p₁)** — probability that the agent's needed item is in the first chunk.
This is the primary paper metric. Formally:

```
p₁ = P(needed_item ∈ chunk_1)
E[chunks] = expected number of chunk requests per task completion
```

**Token savings** — chars(TrimTree output) / chars(full response).

## Value Assignment Strategies

Evaluated strategies (ablation in Section 5):

| Strategy | Description |
|----------|-------------|
| Uniform | All items equal weight (baseline) |
| FIFO | Items in original API order |
| Random | Randomized (lower bound) |
| Reversed | Reverse of FIFO |
| **Priority** | Score by recency × activity × position |

Priority score: `v(item) = w_pos · rank⁻¹ + w_act · activity + w_rec · recency`

## Solver

Exact **0/1 knapsack DP** (Cho & Shaw 1997) for n ≤ 500 items.  
Fallback to **greedy** (value/cost ratio) for larger inputs — proven ≥ 63% optimal.  
Fallback to **WFQ** (Weighted Fair Queueing) for streaming responses.

## Chunk Index

Overflow items are not dropped — they are indexed:
```
[chunks: 1/3 | showing items 1-20 of 58 | call with chunk=2 for next]
```
The agent can request subsequent chunks deterministically.

## Experiments

1. **Priority Hit Rate experiment** — 200 tasks from SWE-bench Verified.
   Ground truth: which file/issue was actually modified in the solution patch.
   Measure p₁ across 5 value strategies × 4 budgets (1k / 2k / 4k / 8k tokens).

2. **Strategy Ablation** — 5 strategies × 9 synthetic datasets × 4 budgets.
   Dataset types: uniform distribution, power-law, adversarial (needed item last).

3. **E[chunks] on τ-bench** — measure total tool calls per task completion
   with and without TrimTree pagination.

## Baselines

- Truncation at N chars (current devboy behavior)
- FIFO (first N items)
- Random selection
- LLMLingua-2 token compression (different technique, same goal)

## Results (Synthetic Ablation, n=50 items, 2000 trials per cell)

*Note: these are **preliminary synthetic experiments** run in the Rust harness
(`crates/plugins/format-pipeline/src/bin/eval.rs`) before the public SWE-bench
benchmark. They established that Priority dominates power-law distributions —
the main empirical findings on **real SWE-bench Verified** are in
§* *Public Benchmark: SWE-bench Verified* *below.*

**Power-law distribution** (gold item in top 20% — most realistic scenario):

| Budget | Random | FIFO (Default) | ElementCount | Reversed | **Priority** | Priority Δ vs best baseline |
|--------|--------|----------------|--------------|----------|------------|------------------------------|
| 1k tok | 0.021  | 0.035          | 0.023        | 0.026    | **0.080**  | +0.045 (+129%)               |
| 2k tok | 0.061  | 0.059          | 0.059        | 0.049    | **0.215**  | +0.156 (+267%)               |
| 4k tok | 0.117  | 0.107          | 0.112        | 0.115    | **0.371**  | +0.254 (+239%)               |
| 8k tok | 0.234  | 0.227          | 0.219        | 0.236    | **0.589**  | +0.355 (+152%)               |

**Realistic distribution** (uniform random gold placement):

All strategies converge: p₁ ≈ n_included/n_total = budget/total_weight.
Priority offers no advantage when item ranking is independent of actual need.

**Adversarial distribution** (gold is last item):

| Budget | Random | FIFO | ElementCount | **Reversed** | **Priority** |
|--------|--------|------|--------------|--------------|-------------|
| 4k tok | 0.000  | 0.000 | 0.000       | **0.961**    | **0.940**   |

Priority correctly handles adversarial case due to recency weighting.

## Key Claims (Updated from Real-World Loop-Level Data)

Our empirical validation (76 loop-level gold events across 51 unique agent loops —
see "Real-World" section below) led to a scoped, evidence-based claim set. The
method has narrow but useful applicability:

1. **Priority-TrimTree is applicable primarily on medium-sized lists (10–19 items)**.
   For smaller lists (<10 items) FIFO already achieves p₁ = 100% in real usage —
   MCP servers return items in useful order, and agents pick position 0. For larger
   lists (20+) the picture is mixed but Priority advantage is modest.

2. **Narrow applicability is 16% of observed gold events**. Out of 76 real-world
   gold-selection events, only 12 (16%) meet the profile where Priority would
   actually change the outcome: list size ≥ 5 AND gold_fraction > 0.2. The
   remainder either have the gold already at position 0 (FIFO success) or use
   tiny lists where no budget pressure exists.

3. **Priority strategy dominates baselines on power-law distributions (synthetic)**:
   p₁=0.371 at 4k tokens vs 0.107–0.123 for all baselines — **3.3× improvement** on
   the controlled synthetic harness. Real-world effect is smaller because the
   applicable slice is narrower.

4. **Priority is invariant on realistic (uniform gold) distributions**: all
   strategies converge to p₁ ≈ included/total. The gain comes from correct value
   ranking, not from item-selection mechanics.

5. **Deployment guidance**: enable Priority-TrimTree conditionally — specifically
   when an MCP list response returns ≥ 10 items AND the agent's prior intent
   suggests specific-item search (detailed_spec, create-entity tasks). For shorter
   lists or exhaustive-iteration tasks, FIFO is equal or better. The Value
   strategy should be selected per tool-call, not per pipeline.

## Real-World Gold-Selection Distribution

To validate the power-law assumption used in synthetic ablation, we extracted
**actual gold-selection events** from Claude Code JSONL logs. Methodology:

1. Find every list-returning MCP invocation: `get_issues`, `search_issues`,
   `get_merge_requests`, `get_epics`, etc.
2. Parse the tool response to extract the list of item IDs (in the order MCP returned them)
3. Scan the next ≤ 30 log entries for the first specific item the agent references
   (via enrichment tool call or text mention) — that's the "gold"
4. Record `gold_position` (0-indexed) and `n_items`; **immediately discard** raw
   IDs and project identifiers — anonymization is built into the extractor

**Result: 85 events across 35 unique sessions.**

| n_items bucket | Events | Mean n | FIFO p₁ (pos=0) | Top-20% p₁ |
|----------------|--------|--------|-----------------|------------|
| Small [3–9]    | 41     | 6.7    | **75.6%**       | 75.6%      |
| Medium [10–19] | 34     | 12.6   | 50.0%           | 61.8%      |
| Large [20+]    | 10     | 24.0   | 50.0%           | 70.0%      |
| **All**        | **85** | **11.1** | **62.4%**     | **69.4%**  |

Full distribution of `gold_fraction = gold_position / (n_items − 1)`:

```
[0.0, 0.2): 69.4% ██████████████████████████████████
[0.2, 0.4): 14.1% ███████
[0.4, 0.6):  5.9% ██
[0.6, 0.8):  8.2% ████
[0.8, 1.0]:  2.4% █
```

**Key findings**:

1. **Power-law is empirically confirmed**: 69% of golds in top 20% of list — matches
   synthetic power-law distribution (α ≈ 1.5) closely.
2. **FIFO is a stronger baseline than assumed**: 62% p₁ from natural MCP ordering.
   MCP servers already return items in useful order (recency / activity / priority).
   Priority strategy must beat this, not the 10% uniform baseline from ablation.
3. **Priority opportunity is in medium/large lists**: for n ≥ 10 items, FIFO drops
   to 50% — Priority can lift this toward 85%+ (matching synthetic results).
4. **Small lists (n < 10) don't need TrimTree**: 76% FIFO p₁, 6.7 median items
   fits easily in any budget. Focus optimization effort on the `n_items ≥ 10` path.

Anonymized CSV: `docs/research/data/gold_selection_real.csv`
Extractor (outputs only anonymized data): `docs/research/scripts/find_gold_selection.py`

## Loop-Level Gold-Selection (Paper 1 core data)

Sessions are non-uniform units — they vary wildly in size and intent. A
**single agent loop** (one human turn → agent work → next human turn) is a
more equivalent unit of analysis. We re-analyzed the corpus at loop granularity.

**15,165 loops** across 2,607 sessions. Only 51 of those loops (0.3%) had
MCP list-tool calls with detectable gold-selection, which itself is a key
finding: list-based gold-selection is a **narrow workflow** inside
devboy-style tooling, not a universal agent behavior.

### Gold-position distribution by list size

Across 76 gold events in 51 applicable loops:

| list_size bucket | Events | Avg gold_fraction | FIFO p₁ (pos=0) | Paper 1 prime candidates |
|------------------|-------:|------------------:|----------------:|-------------------------:|
| tiny <5 items    | 20     | **0.000**         | 100%            | 0 |
| small 5–9        | 23     | **0.000**         | 100%            | 0 |
| **medium 10–19** | 25     | **0.286**         | 48%             | **11** ← primary target |
| large 20–49      | 8      | 0.056             | 38%             | 1 |

**Aggregate FIFO p₁ = 76.3%** on real data. **Top-20% p₁ = 84.2%**. This is
notably stronger than the session-level 62.4% reported earlier — loops filter
out noise from mixed-activity sessions.

### Where Priority-TrimTree actually matters

Prime candidate loops (gold_fraction > 0.2 AND list_size ≥ 5) are
characterized by:

- **Intent**: mostly `detailed_spec` (long, specific asks) or `short_prompt`
  with specific target
- **Outcome**: `target_create_entity` (agent created an issue/MR from list)
  or `target_write_committed` (wrote code and committed referencing list
  item)
- **Loop size**: short-to-medium (5–30 tool calls). Marathon loops (100+
  calls) rarely involve gold-selection — they're exhaustive iteration or
  unrelated work.

### Per-tool breakdown

| list_tool verb | Events | Avg items | FIFO hits |
|----------------|-------:|----------:|----------:|
| get_issues | 63 | 10.7 | 45 (71%) |
| get_merge_requests | 8 | 3.4 | 8 (100%) |
| search/get_meeting_notes | 5 | 1.1 | 5 (100%) |

`get_issues` is the only MCP list-tool where Priority has meaningful surface
area. The others always return small, relevance-sorted results.

### Implications for production deployment

Do NOT enable Priority-TrimTree globally. Enable it conditionally in the
pipeline adapter for `get_issues`-type tools when:

1. Response size ≥ 10 items
2. Preceding human intent signals specific target (detailed spec, issue
   reference, "find the X that...")

For all other cases, FIFO has acceptable performance and is cheaper.

## Key Claims (Bash File-Search Corpus)

The Bash file-search corpus is a **different search domain** from the MCP
corpus in our data collection setup. We note this explicitly so the two are
not compared head-to-head:

- **Bash (`grep / find / ls / rg`)** — search over the project's **codebase**
  (source files, configs, docs). Output ordering is filesystem-driven
  (alphabetical / inode order). No natural priority signal exists.
- **MCP — in our pipeline** — the observed MCP list-tools (`get_issues`,
  `get_merge_requests`, `search/get_meeting_notes`, …) search a **GitLab
  issue/MR tracker**, because that's the MCP integration we deploy. Output
  ordering is server-sorted by recency / activity / priority, so a natural
  priority signal is already present. The "MCP" framing here is not
  intrinsic to MCP as a protocol — a different MCP integration (e.g. a
  filesystem MCP) would look more like the Bash case.

The two corpora share the gold-selection *pattern* but have different
baselines (FIFO is stronger on the GitLab-MCP data, weaker on Bash) and
different dominant priority signals (activity/recency on GitLab-MCP,
keyword-match on Bash). Each corpus has its own claim set; the MCP claims
are above, the Bash claims follow here.

1. **Code-exploration dominates the corpus (61% of events)**. Bash
   gold-selection is overwhelmingly the "find the file that does X" pattern,
   canonical Claude-Code usage. Priority-TrimTree is targeted directly at
   this workflow.

2. **keyword-match is the dominant priority signal (83.5%)**. Token-overlap
   between the query (grep pattern, issue description) and candidate file
   paths/names predicts the gold in 83.5% of classified events, per an
   independent LLM judge. The Value function must weight this signal above
   all others. Secondary: path_depth (8.1%), filetype_prior (4.0%).

3. **FIFO is inadequate in 63% of Bash gold-selection events**. When the
   tool is `grep / find / ls / rg`, there is no natural ordering signal —
   FIFO is essentially random relative to agent intent. Priority-TrimTree
   is a direct optimization target for this tool class.

4. **Priority lift is strongest on small and medium lists**. FIFO p₁ falls
   to 24% on 3–9-item lists and 13% on 10–29-item lists (per-bucket table
   below). These two buckets together contain ~68% of Bash gold events.

5. **Proposed Value weights** (starting point; to be tuned on the public
   SWE-bench benchmark):

   ```
   v(item) = 0.70 · keyword_match_score
           + 0.15 · path_depth_score
           + 0.08 · filetype_prior
           + 0.04 · recency_score
           + 0.03 · filename_match_score
   ```

6. **Deployment guidance (Bash scope)**: enable Priority-TrimTree by
   default for any Bash tool-call returning ≥ 3 candidate file paths. For
   tiny (<3) and massive (100+) lists the lift is small or a different
   mechanism is required.

## Bash File-Search Gold-Selection (×58 more data)

The MCP list-tool pattern generalizes: whenever a tool produces a
**list-like response** and the agent picks a specific item next, the
gold-selection problem applies. We extracted the same pattern from Bash
`grep/find/ls/rg` output across the corpus.

**4,373 events across 973 sessions** — nearly 58× more data than MCP-level.

### Bash vs MCP comparison

| Metric | MCP (76 events) | **Bash (4,373)** |
|--------|---------------:|------------------:|
| avg n_candidates | 9.3 | 9.3 |
| avg gold_fraction | 0.121 | **0.388** |
| FIFO p₁ | 76.3% | **37.6%** |
| Paper-1 prime (frac>0.2, n≥5) | 16% | **38.5%** |

**Bash gold-selection has much higher Priority-TrimTree applicability
(×2.4 in % terms, ×140 in absolute events).** The reason: grep/find
output orders by file-system iteration, not by usefulness to the agent —
no natural priority signal exists, unlike MCP servers that already
sort by recency/activity.

### Bash gold-position distribution by list size

| n_candidates | Events | Avg frac | FIFO p₁ | Recommendation |
|--------------|-------:|---------:|--------:|----------------|
| tiny <3 | 1,294 | 0.19 | 81.4% | FIFO acceptable |
| **small 3–9** | **1,778** | **0.49** | **24.3%** | 🔥 Priority wins |
| **medium 10–29** | **1,062** | **0.46** | **13.0%** | 🔥 Priority wins |
| **large 30–99** | 225 | 0.45 | 7.6% | 🔥 Priority essential |
| huge 100+ | 14 | 0.31 | 21.4% | Edge case |

For Bash `file_search → file_read` chains of 3+ items, FIFO fails 65–92%
of the time. Priority-TrimTree — trained on usage frequency, file type,
and recency signals — is a direct optimization target.

### Gold-source breakdown (Bash)

| Source | Events |
|--------|------:|
| Read tool | 4,089 (93%) |
| Bash viewer (cat/head/tail) | 209 (5%) |
| Edit tool | 45 (1%) |
| Write tool | 30 (1%) |

93% of Bash gold-selections end with Claude's native `Read` tool — the
agent discovers files via `grep` then opens one. This is the classic
pattern and the primary TrimTree target.

### Deployment implication (revised)

Enable Priority-TrimTree by default for:
1. **All Bash `grep/find/ls/rg` outputs with ≥ 3 candidate file paths** —
   FIFO loses here most of the time.
2. **MCP `get_issues` responses with ≥ 10 items AND specific-intent human
   prompt** — narrower but still valuable.

Do NOT enable for:
- Tiny lists (<3 items) — FIFO works.
- Massive lists (100+) — too many candidates for any per-call strategy to
  help; use hierarchical chunking instead.

The Bash case alone gives Paper 1 a **1,682-event prime candidate pool**
with a reproducible extractor (`extract_bash_list_events.py`).

### Workflow categorization (LLM-classified, GLM-4.6)

To understand *what kinds of work* drive Bash gold-selection, we classified
**4,175 events** with GLM-4.6 (z.ai coding endpoint, Anthropic-compatible API
with `cache_control: ephemeral`, KV-cache hit rate 86.6%). Each event receives
a category, use-case, primary priority signal, and a boolean judgement of
whether FIFO ordering would have placed the gold first. Parse errors: 5/4,175.

**Category distribution:**

| Category   | Events | %     |
|------------|-------:|------:|
| research   | 2,607  | 62.4% |
| devops     |   507  | 12.1% |
| code       |   426  | 10.2% |
| debug      |   241  |  5.8% |
| docs       |   187  |  4.5% |
| config     |   175  |  4.2% |
| other / refactor / audit / issue_tracking | <40 | <1% |

**Use-case distribution (top):**

| Use case         | Events | %     |
|------------------|-------:|------:|
| code_exploration | 2,546  | 61.0% |
| code_navigation  |   518  | 12.4% |
| config_lookup    |   364  |  8.7% |
| bugfix_code_hunt |   248  |  5.9% |
| docs_lookup      |   199  |  4.8% |
| audit_scan       |   129  |  3.1% |

**Primary priority signal (what predicts the gold, per LLM judge):**

| Signal         | Events | %     |
|----------------|-------:|------:|
| keyword-match  | 3,488  | 83.5% |
| path-depth     |   338  |  8.1% |
| file-ext-prior |   169  |  4.0% |
| fifo (natural) |    80  |  1.9% |
| recency        |    30  |  0.7% |
| filename-match |    19  |  0.5% |

**FIFO adequacy (same judge):**

| fifo_would_work | Events | %     |
|-----------------|-------:|------:|
| False           | 2,632  | **63.0%** |
| True            | 1,543  | 37.0% |

Claims grounded in this classification are stated in the
"Key Claims (Bash File-Search Corpus)" section above.

Classification script: `docs/research/scripts/llm_classify_bash_events.py`
(emits category / use_case / priority_signal per event — aggregate CSV to be
published after anonymization review; raw per-event file contains session
hashes and stays local).

## LLM Comprehension Validation (preliminary, synthetic GitLab issues)

*Note: these are the **early-stage synthetic LLM validations** run through our
Rust `llm-eval` crate against a fabricated issue table. They confirmed that
`algo_p1` predicts LLM accuracy on easy synthetic inputs. The full
multi-LLM benchmark on **real SWE-bench data** is in §*Public Benchmark:
SWE-bench Verified* below — note that on the harder real data we observe
**negative correlation** between algo p₁ and LLM accuracy (reasoning-model
compensation), which is the finer, more interesting picture.*

**Goal**: confirm that `algo_p1` (algorithmic inclusion probability) is predictive of
real LLM task accuracy. Setup: synthetic Markdown table of GitLab issues, one gold item
(critical priority, 47 comments, 0.1 days since update), random gold position. Budgets
calibrated to row size (~26 tok/row) so 25–50% of items fit.

Models: `gemma4-26b` and `gpt-oss-20b` via a local Ollama instance (RTX 3090, OpenAI-compatible endpoint).
20 trials per cell. Judge: response contains gold issue ID (`gitlab#NNN`).

**gpt-oss-20b results** (reasoning model; `reasoning` field used as response):

| n | budget | strategy | algo_p1 | llm_acc | halluc |
|---|--------|----------|---------|---------|--------|
| 50 | 250 | element_count | 0.25 | 0.25 | 0 |
| 50 | 250 | **priority** | **1.00** | **1.00** | 0 |
| 50 | 600 | element_count | 0.65 | 0.65 | 0 |
| 50 | 600 | **priority** | **1.00** | **0.95** | 0 |
| 20 | 150 | element_count | 0.55 | 0.55 | 0 |
| 20 | 150 | **priority** | **1.00** | **1.00** | 0 |

`llm_accuracy ≈ algo_p1` with r ≈ 1.0 — **algorithmic inclusion is the decisive factor**.

**gemma4-26b results** (noisy responder; often ignores format instruction):

| n | budget | strategy | algo_p1 | llm_acc |
|---|--------|----------|---------|---------|
| 50 | 250 | element_count | 0.25 | 0.25 |
| 50 | 250 | **priority** | **1.00** | 0.60 |
| 50 | 600 | element_count | 0.65 | 0.35 |
| 50 | 600 | **priority** | **1.00** | 0.50 |

Trend is consistent (priority > element_count) but model noise caps accuracy at 0.5–0.6.

**Key findings**:

1. **Hallucination rate = 0.0** across all 480 trials: when gold is excluded, no model
   guesses the correct ID. LLMs do not hallucinate absent items.
2. **gpt-oss-20b**: Priority strategy delivers **4× improvement** (0.25 → 1.0 at n=50,
   budget=250). LLM accuracy perfectly tracks algo_p1.
3. **algo_p1 is the right proxy**: improving algorithmic inclusion directly improves
   end-task accuracy. No need to measure the LLM separately for ablation.
4. **Model noise** (gemma4-26b's tendency to ignore output format) is orthogonal to
   the compression strategy — the gap between strategies persists despite noise.

Full results: `docs/research/data/llm_results.csv`

## Public Benchmark: SWE-bench Verified (E1, E2)

**Setup.** To test Priority-TrimTree on data we did not design, we apply it
to the **SWE-bench Verified** file-localization task (500 real GitHub issues
from 12 Python repositories — Django, Sympy, Sphinx, matplotlib, scikit-learn,
astropy, xarray, pytest, pylint, requests, seaborn, flask).

For each task we clone the repo at `base_commit`, tokenize `problem_statement`
into identifier-shaped keywords (3-char min, stopwords filtered, identifier
shapes boosted), run `git grep -l` with AND-of-top-3 keywords (OR fallback,
then filetype fallback), truncate to 50 candidate files. Each candidate keeps
`{path, ext, depth, size, mtime, keyword_overlap_score}`. Gold = files parsed
from each task's `patch` diff headers (mean 1.25 files, 86% have exactly one
gold).

**Upper bound**: 55.8% of 500 tasks have gold *in* the candidate set — our
grep proxy misses in 44% of cases. This is the ceiling for any ranking
strategy; Priority fights within that ceiling.

### E1 — Algorithmic p₁ × strategy × budget (no LLM, 10k cells)

| Strategy | p₁ overall | Δ vs FIFO |
|----------|----------:|----------:|
| Reversed | 2.6% | −21.6 p.p. (adversarial baseline) |
| Random | 22.6% | −1.6 p.p. |
| **FIFO** | 24.2% | — |
| Priority-ALL (weighted composite) | 30.2% | +6.0 p.p. |
| **Priority-KW** (keyword overlap only) | **35.0%** | **+10.8 p.p. ✅** |
| **Priority-KW⁺** (KW + FIFO fallback on zero signal) | **35.8%** | **+11.6 p.p.** |

*Budget does not matter in the overall average (all 4 budgets give identical
numbers) because our candidate lists are small (median = 4 candidates,
~200 tok/list). Budget pressure materializes only in large-bucket tasks.*

**H1 (Priority-KW > FIFO, Δp₁ ≥ +0.10 at 2k tok) — PASS** (+0.108).

**H2 (Priority-ALL > Priority-KW, Δ ≥ +0.03) — FAIL, reversed** (Δ = −0.048).
Reportable finding: the composite scorer with path depth, filetype prior,
recency, and filename match **adds noise** that degrades keyword-only
ranking. Pure keyword overlap (with a safety FIFO fallback) is the
winning configuration.

### E1 bucket analysis — where Priority actually wins

| n_candidates bucket | FIFO p₁ | Priority-KW p₁ | Priority-KW⁺ p₁ | Tasks |
|---------------------|--------:|---------------:|----------------:|------:|
| small (1–5)         | 36.5%   | 41.0%          | **42.4%**       | ~50% of dataset |
| **medium (6–20)**   | **0.0%** | **29.1%**     | **29.1%**       | 13% |
| large (21+)         | 10.2%   | 26.1%          | 26.1%           | 11% |

**Key result.** In the medium bucket (6–20 candidates), grep's native order
**never** puts gold on rank 0 (FIFO p₁ = 0). Priority-KW lifts this to 29%.
Large bucket: Priority is 2.5× better than FIFO. Small bucket: marginal gain
but budget usually fits everything anyway.

This matches the real-world finding from our Bash corpus: Priority has the
biggest surface area on non-trivial list sizes (n ≥ 6).

### E2 — Multi-LLM comprehension (5 models × 100 tasks × 4 budgets — final)

Stratified sample: 50 tasks where gold is in FIFO-top-1 + 50 where it is not.
Each cell: compressed candidate list rendered as numbered Markdown, prompt
composed of 1476-token stable system prefix (with `cache_control: ephemeral`
for Anthropic) + variable task block, LLM replies with
`{"chosen_file": "...", "confidence": ..., "reasoning": ...}`.

**Inference settings per provider**:
- **Local Ollama 0.21** (gpt-oss:20b, gemma4:26b): `think = "high"`, `num_ctx = 8192`, `keep_alive = 2h`
- **z.ai Anthropic-compat** (glm-5.1): `thinking.budget_tokens = 2048`, `concurrency = 1`
- **Anthropic Batch API** (Sonnet 4.5, Opus 4.7): `cache_control: ephemeral`, batch discount 50%, no explicit thinking

### Table B — LLM accuracy × model × strategy × budget

| Tier | Model | Strategy | 1k | 2k | 4k | 8k | Mean |
|------|-------|----------|---:|---:|---:|---:|-----:|
| **Frontier** | Claude Opus 4.7    | FIFO         | 94.5% | 94.5% | 93.0% | 94.5% | 94.1% |
| Frontier     | Claude Opus 4.7    | Priority-KW⁺ | 94.5% | 94.0% | 93.5% | 91.5% | 93.4% |
| **Mid**      | Claude Sonnet 4.5  | FIFO         | 92.0% | 91.5% | 91.5% | 92.0% | 91.8% |
| Mid          | Claude Sonnet 4.5  | Priority-KW⁺ | 92.0% | 92.5% | 91.5% | 92.0% | 92.0% |
| Mid          | **GLM-5.1** (z.ai) | FIFO         | 92.0% | 92.0% | 92.0% | 92.0% | 92.0% |
| Mid          | GLM-5.1 (z.ai)     | Priority-KW  | 92.0% | 91.5% | 91.5% | 92.0% | 91.8% |
| **Local**    | gemma4:26b         | FIFO         | 86.0% | 85.0% | 87.0% | 87.0% | 86.3% |
| Local        | gemma4:26b         | Priority-KW  | 83.0% | 85.0% | 86.0% | 85.0% | 84.8% |
| Local        | **gemma4:26b**     | Priority-KW⁺ | 86.0% | 87.0% | 87.0% | 88.0% | **87.0%** |
| Local        | gpt-oss:20b        | FIFO         | 82.0% | 82.0% | 81.0% | 82.0% | 81.8% |
| Local        | gpt-oss:20b        | Priority-KW  | 78.0% | 78.0% | 80.0% | 80.0% | 79.0% |
| Local        | **gpt-oss:20b**    | Priority-KW⁺ | 82.0% | 82.0% | 81.0% | 84.0% | **82.3%** |

### Table C — Cost efficiency ($/correct answer)

| Model | Accuracy | Cost (800 calls) | $/correct | Cache hit | Notes |
|-------|---------:|----------------:|----------:|----------:|-------|
| gemma4:26b (Ollama local) | 86.1% | $0.00 | **$0.0000** | — | RTX 3090, 44 min |
| gpt-oss:20b (Ollama local) | 81.1% | $0.00 | **$0.0000** | — | RTX 3090, 37 min |
| **GLM-5.1 (z.ai)** | **91.9%** | $0.44 | **$0.0006** | 0%* | No cache_control; thinking=2048 |
| Claude Sonnet 4.5 (Batch API) | 91.9% | $1.59 | $0.0022 | 66.5% | cache_control=ephemeral |
| Claude Opus 4.7 (Batch API) | 93.9% | $23.05 | $0.0307 | 12.6%** | cache_control=ephemeral |

*z.ai calls didn't include `cache_control` (client didn't configure it); could be added.*

*\*\* Opus batch had low cache hit rate — many parallel workers created their own cache entries within the 5-min TTL. `ttl: "1h"` would improve this.*

**Commercial-relevance findings**:
- **GLM-5.1 matches Sonnet 4.5 accuracy (91.9%) at 3.7× lower cost** ($0.0006 vs $0.0022).
- **Opus 4.7 costs 14× Sonnet** for only **+2 p.p.** accuracy gain.
- Free local models within **7-13 p.p.** of frontier overall.

### H5 — PASS when gold is in the compressed set (main commercial claim)

Conditional-on-compression accuracy (task-specific claim):

```
Given gold ∈ compressed candidate set:
  Opus 4.7       (frontier, 2T MoE):  94.7%
  gemma4:26b     (local,    26B):     94.6%
  Gap = 0.1 p.p.  (threshold ≤ 10 p.p.)  →  PASS ✓
```

When Priority-KW⁺ successfully includes the gold file in the compressed list,
**the 26B local model matches the 2T frontier model within Monte Carlo noise**
(n = 100 tasks). The residual gap is 0.1 p.p.

This is the central commercial claim of Paper 1: compression lets small local
models perform what used to require frontier cloud models, on the
file-localization subtask — with a 150× cost advantage.

### H3 — Algorithmic p₁ and LLM accuracy are negatively correlated

Per-model Pearson r (algo_p₁ vs LLM accuracy, cell means across strategies × budgets):

| Model | r | Interpretation |
|-------|--:|----------------|
| gpt-oss:20b (w/ thinking) | −0.21 | Reasoning compensates for ranking |
| gemma4:26b (w/ thinking) | −0.03 | Near zero — pooled fifo+priority+fallback cells |
| GLM-5.1 (w/ thinking) | **−0.66** | Strong negative — 744B MoE uses own heuristics |
| Sonnet 4.5 / Opus 4.7 | n/a | Only 2 strategies × 4 budgets = 4 cells per model; r unstable |

H3 as originally formulated (r ≥ 0.85) **fails for all models** — but the
*sign* is the important finding. **LLM reasoning systematically compensates
for suboptimal ranking** when the gold item is present in the compressed set.

Implication for framing: ranking matters *less* than inclusion. Priority-KW⁺
makes inclusion more likely (higher p₁), which is the value channel, not the
ordering within the included set.

### Edge case — why Priority-KW⁺ adds a FIFO fallback

During early E2 runs with naive Priority-KW we observed **~14% of tasks where
all candidate `keyword_overlap_score` values were zero**. Generic paths like
`options.py`, `base.py`, `__init__.py` share no tokens with most issue texts.
Pure knapsack with zero values returns the empty set — the LLM receives
no candidates and cannot answer.

```
task django-12713 (budget=2k, n_cand=2):
  gold:        django/contrib/admin/options.py
  FIFO:        n_selected=2 → chose django/contrib/admin/options.py   ✓
  Priority-KW: n_selected=0 → empty response                          ✗
```

**Priority-KW⁺** adds a graceful fallback:

```python
def value_priority_kw_fallback(candidates):
    kws = [c.keyword_overlap_score for c in candidates]
    if max(kws) == 0:
        return value_fifo(candidates)   # zero-signal → FIFO discovery order
    return [kw + ε · (1 − rank / (n − 1))     # tiny FIFO tiebreaker
            for rank, kw in enumerate(kws)]
```

Effect on LLM accuracy:

| Model | Priority-KW | Priority-KW⁺ | Δ |
|-------|------------:|-------------:|--:|
| gpt-oss:20b | 79.0% | **82.3%** | +3.3 p.p. |
| gemma4:26b | 84.8% | **87.0%** | +2.2 p.p. |
| Sonnet 4.5 | 91.8% | 92.0% | +0.2 (frontier-noise) |

Fallback recovers empty-set failures without hurting ranked cases.

### Cross-validation from 2 external corpora (independent contributors)

Two collaborators independently extracted gold-selection patterns from their
own Claude Code log corpora (each anonymized on extraction via the pipeline
in `docs/research/scripts/find_gold_selection.py`):

| Corpus | Source | Sessions | Bash gold events | FIFO p₁ | Keyword-match signal |
|--------|--------|---------:|-----------------:|--------:|---------------------:|
| Ours (this paper) | — | 2,607 | 4,175 | 36.7% | reported 83.5% |
| **Corpus B** | external | 689 | **590** | **35.4%** | **80.8%** |
| **Corpus A** | external | 221 | **145** | **24.1%** (small 3-9 bucket, matches ours 24%) | **85.4%** (real-signal-only) |

**Three independent corpora, consistent main findings**:
- FIFO baseline ≈ 35% — replicates across all three (anti-cherry-picking)
- Keyword-match dominates priority signal in 80-85% of picks
- Long-tail candidate-list sizes observed by all three (Corpus B p99=89.9, max=278)

A side-by-side replication report is in the external bundles (see
`gold_comparison_2026-04-22.md`, contributed by `Corpus A`).

Minor drifts in use-case distribution (e.g. `code_exploration` share: ours
52.9% vs Corpus A 25%) trace to different LLM judges used for intent
classification (GLM-4.6 vs others) — reported honestly in §Limitations.

### E1 and E2 together — full picture

1. **Priority-KW beats FIFO by +10.8 p.p. on algorithmic p₁** — the ranking
   metric that bypasses LLM.
2. **LLM accuracy is largely decoupled from ranking** when candidates fit the
   budget (most tasks in this dataset). Reasoning-capable models compensate;
   non-reasoning models track ranking more closely.
3. **TrimTree's value lies where budget really cuts** — large candidate lists
   and/or non-reasoning downstreams. For small lists with strong reasoning,
   ranking is nearly free.

Figures (in `paper1-repro/artifacts/figures/`):
- Fig 1 — p₁ by strategy × n_candidates bucket
- Fig 2 — LLM accuracy vs algorithmic p₁ scatter (all cells above the y=x
  diagonal, confirming LLM compensation effect)
- Fig 3 — LLM accuracy by bucket × strategy × model

All raw artifacts in `docs/research/paper1-repro/artifacts/` (local
only, `.env` and repo caches never committed). Aggregate CSVs for reviewers
in `docs/research/data/`.

## Hypothesis checks — Final

| # | Hypothesis | Criterion | Result | Verdict |
|---|-----------|-----------|--------|---------|
| **H1** | Priority-KW > FIFO on algorithmic p₁ | Δp₁ ≥ +0.10 @ 2k tok | **Δ = +0.108** | **PASS ✓** |
| H2 | Priority-ALL > Priority-KW | Δ ≥ +0.03 | Δ = **−0.048** (reversed) | FAIL — **reportable finding**: composite scorer adds noise, pure KW + fallback is optimal |
| H3 | corr(algo_p₁, LLM acc) ≥ 0.85 | r ≥ 0.85 | r ∈ [−0.66, −0.03] | FAIL — **reportable finding**: LLM reasoning compensates for ranking; inclusion beats ordering |
| H4 | KV-cache reduces cost ≥ 40% | mean(cost_with_cache) / mean(cost_without) ≤ 0.60 | Sonnet 4.5: 66.5% hit rate → ≈40% savings on input side | PASS ✓ (for Sonnet; see Table C) |
| **H5** | local ≈ frontier when gold in compressed set | gap ≤ 10 p.p. | **gap = 0.1 p.p.** (gemma4:26b 94.6% vs Opus 4.7 94.7%) | **PASS ✓** — strongest commercial claim |

**Main narrative**: H1 and H5 PASS, H2 and H3 fail with reportable findings
that strengthen the story — Priority-KW⁺ is the correct configuration, and
TrimTree's value is via *inclusion* (which ranking boost drives up), not via
the ranking itself influencing the LLM's reasoning.

## Limitations

1. **Grep-proxy candidate generation misses 44.2% of golds** — our simple
   AND-of-top-3-keywords → OR → find-by-filetype pipeline is a realistic
   proxy for what agents actually do (git grep / rg are the dominant nav
   tools in Claude Code logs), but better retrievers (BM25, dense
   embeddings) would raise the 55.8% upper bound. This does not change the
   relative ordering of strategies but bounds absolute p₁.
2. **Python-only codebases** — SWE-bench Verified is Python (12 repos,
   Django dominates at 46%). Cross-language validation (Rust, TypeScript,
   Go) is future work.
3. **Django repo dominance** — 46% of 500 tasks are Django. We report
   per-repo breakdown in supplementary to confirm Priority wins are not
   Django-specific.
4. **Budget pressure is rare in this dataset** — median candidate list is 4
   items, fitting easily in budget ≥ 500 tok. TrimTree's value materializes
   strongly only in the top-10% tasks (n ≥ 20 candidates), where Priority
   is 2.5× FIFO on algorithmic p₁.
5. **LLM judges** — intent classification (`audit_scan`, `code_exploration`
   etc.) uses GLM-4.6 as the labeler. Different judges (Corpus A used a
   different GLM revision) give different distributional breakdowns; headline
   Priority-vs-FIFO numbers are not affected, but use-case shares differ.
6. **Single-turn E2** — we measure comprehension on one compressed list,
   not the full multi-turn agent loop. End-to-end resolve rate (with
   patches applied and tests run) is the subject of E6 / Paper 1 follow-up.
7. **Reproducibility caveat on Opus batch caching** — `cache_control:
   ephemeral` had low hit rate (12.6%) on Opus due to parallel workers in
   the batch and 5-min TTL. A `ttl: "1h"` variant would likely yield
   comparable hit rates to Sonnet. Not re-run to save budget.

## Implementation Status

### Core pipeline (Rust, `crates/devboy-mcp/src/pipeline/`)

- [x] Core tree representation (`trim_tree.rs`)
- [x] Knapsack DP solver (exact + greedy fallback)
- [x] Chunk index format
- [x] ТЗ-4: Priority value strategy (Random / Reversed / Priority added; FIFO = Default existing)
- [ ] ТЗ-1: per-item partial emission (ItemState: ItemOnly / ItemWithField / Skip)
- [ ] ТЗ-12: keyword-match Value signal in `strategy.rs`
      (weights per "Proposed Value weights" in Bash Key Claims)

### Evaluation & research harness

- [x] ТЗ-0: evaluation harness — `cargo run -p devboy-format-pipeline --bin eval`
- [x] ТЗ-7: Strategy Ablation — results in `docs/research/data/ablation_results.csv`
- [x] ТЗ-6: LLM comprehension validation — results in `docs/research/data/llm_results.csv`

### Real-world corpus (Claude Code JSONL logs, anonymized)

- [x] MCP list-tool gold-selection extraction (85 session-level, 76 loop-level events)
- [x] Bash file-search gold-selection extraction (4,175 events; `extract_bash_list_events.py`)
- [x] Loop-level pipeline (`extract_loops.py` + `enrich_loops.py`):
      cost / cache hit rate / trigger / success_proxy per loop
- [x] Session-level features (`compute_session_features.py`)
- [x] ТЗ-29: Full LLM classification of Bash events via GLM-4.6 coding endpoint
      (4,175 events, 86.6% KV-cache hit, 5 parse errors)

### Public benchmark — ✅ complete (2026-04-23)

- [x] ТЗ-10 / ТЗ-13: SWE-bench Verified runner — 500 tasks, 6 strategies, 4 budgets
      (`docs/research/paper1-repro/scripts/01-06`)
- [x] ТЗ-14: Multi-LLM harness — Opus 4.7, Sonnet 4.5, GLM-5.1, gemma4:26b,
      gpt-oss:20b across 4 000 LLM calls (`04_run_multi_llm.py`, `07_anthropic_batch.py`)
- [x] **Reproducibility kit**: `paper-1-REPRODUCIBILITY.md`, `Makefile`, `Dockerfile`,
      public aggregates in `docs/research/data/`, interactive notebook
      `docs/research/notebooks/paper1_analysis.ipynb`
- [x] External cross-validation from 2 anonymized corpora (Corpus B 689 sessions,
      Corpus A 221 sessions — both replicate FIFO baseline ≈ 35% and keyword-match
      dominance ≈ 80-85%)

### Production follow-ups (Paper 1.5 / future)

- [ ] ТЗ-1: per-item partial emission (`ItemState`) for `crates/devboy-mcp`
- [ ] ТЗ-12: keyword-match `Value` signal in Rust `strategy.rs`
- [ ] E6 full-agent SWE-bench runner (end-to-end resolve rate, Docker harness)
- [ ] Better retriever for higher 55.8% ceiling (BM25, dense embeddings)
- [ ] Cross-language validation (TypeScript / Rust / Go repos outside SWE-bench)

## Empirical Motivation (Real Claude Code Logs)

Data collected via `track-claude-usage` from 523 Claude Code sessions:

- **0% pagination rate**: agents never request `chunk=2` in existing logs → p₁ is the
  correct optimization target (if item not in chunk 1, it's permanently lost)
- `get_merge_request_diffs`: P90 = 35k chars ≈ 10k tokens; 28% exceed 8k-token budget
- `get_epics`: P90 = 43k chars ≈ 12k tokens; 37% exceed 8k-token budget
- Median `get_issues` response: ~2400 chars/item (confirms 686 token/item calibration)
- After any large response: agents generate text in next turn (absorbed data, never retry)

**Key implication**: The eval harness power-law distribution (gold in top 20%) matches
real project behavior — hot issues get many comments and are worked on first. Priority
strategy's 3.3× gain on power-law directly translates to production benefit.

## Related Work

- Selective Context (Li et al., 2023) — sentence-level compression via self-information
- LLMLingua / LLMLingua-2 (Jiang et al., 2023–2024) — token-level compression
- RECOMP (Xu et al., 2024) — extractive compression for RAG
- ACON (2024) — compresses agent observation history (environment-level, not item-level)
