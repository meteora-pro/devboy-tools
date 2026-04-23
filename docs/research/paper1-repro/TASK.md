# Paper 1 — Public Benchmark Experiment Specification

**Owner:** Andrei Mazniak
**Target venue:** EMNLP 2026 / ACL 2026 (Systems track)
**Parent document:** `../paper-1-trimtree.md`

---

## Research question

Does **Priority-TrimTree** (value-weighted 0/1 knapsack over list-tool
responses, with keyword-match as dominant Value signal) outperform **FIFO**
pagination on the canonical Claude-Code "find-the-file" workflow, measured
against a public, reproducible benchmark?

Prior real-world data (`paper-1-trimtree.md` §Bash File-Search Gold-Selection)
shows FIFO is inadequate in 63% of 4,175 Bash gold events. This benchmark
translates that private finding into a publishable result.

---

## Hypotheses

| # | Hypothesis | Success criterion |
|---|------------|-------------------|
| H1 | Priority-KW (weight 0.70) beats FIFO on SWE-bench p₁ | Δp₁ ≥ +0.10 at budget=2k tokens |
| H2 | Adding path_depth + filetype + recency (Priority-ALL) improves further | Δp₁ ≥ +0.03 vs Priority-KW |
| H3 | LLM downstream accuracy tracks algorithmic p₁ across model tiers | Pearson r(algo_p₁, llm_acc) ≥ 0.85 |
| H4 | KV-cache + Batch API reduces Anthropic cost/correct by ≥ 40% vs naive calls | Measured $ delta on Sonnet 4.6 run |
| H5 | A small local 20B model with compressed input matches a frontier model's rate on gold-selection | Gap ≤ 10 p.p. when gold is in the compressed set |

H5 is the strongest commercial-relevance claim: if true, Priority-TrimTree
lets small local models do what used to need Opus.

---

## Datasets

### Primary — SWE-bench Verified (public, 500 tasks)

- Source: `princeton-nlp/SWE-bench_Verified` on HuggingFace
- Per-task fields used: `repo`, `base_commit`, `problem_statement`,
  `patch` (for gold extraction)
- **Gold**: list of files modified in `patch` (parseable from diff headers)

### Candidate-set generation (grep proxy)

For each task, at `base_commit`:

1. Tokenize `problem_statement` → keep identifiers, noun phrases (spaCy /
   simple POS tag)
2. Filter stopwords + single-character tokens
3. Run `grep -rln` over the repo for each surviving keyword (batch-AND of
   top-K keywords). If no hits → fall back to `find` by inferred file type.
4. Deduplicate, truncate to 50 candidates, preserve grep's discovery order
   (so FIFO = filesystem-native order, the realistic baseline).
5. Collect per-candidate metadata: `{path, ext, depth, size, mtime,
   keyword_overlap_score}`.

### Sampling for E2 (LLM comprehension)

- Stratified sample of 100 tasks from the 500, balancing:
  - `n_candidates` bucket (small 3-9 / medium 10-29 / large 30+)
  - `gold_in_fifo_top1` (yes / no) — balance so at least 50% are
    FIFO-failing tasks where Priority's edge matters

---

## Strategies under test

(Defined in `config/strategies.yaml`)

1. **FIFO** — grep's native order (baseline)
2. **Random** — shuffle (lower bound)
3. **Reversed** — reverse FIFO (adversarial baseline)
4. **Priority-KW** — `v(item) = keyword_overlap_score`
5. **Priority-ALL** — weighted:
   ```
   v = 0.70·kw + 0.15·depth + 0.08·ext_prior + 0.04·recency + 0.03·fname
   ```
   (weights from Bash corpus empirics; to be tuned on SWE-bench train subset)

---

## Experiments

### E1 — Algorithmic Priority Hit Rate on SWE-bench

**Scope:** 500 tasks × 5 strategies × 4 budgets = 10,000 measurements.
Pure Python; no LLM.

**Budgets** (token-equivalent; 1 tok ≈ 4 chars): **1k / 2k / 4k / 8k**.

**Measure:**
- `p₁` — gold in first chunk
- `p@k` for k ∈ {3, 5, 10}
- `E[chunks]` — chunks until gold found (infinite if missed)
- `tokens_used` per chunk

**Output:** `artifacts/strategy_results.parquet`
columns: `task_id, strategy, budget_tok, p1, p3, p5, p10, chunks_to_gold,
tokens_used, n_candidates, gold_in_fifo_top1`

**Acceptance:** H1 and H2 directly measurable from this file.

**Timeline:** ~30 min compute (pure algo, 10k iterations).

---

### E2 — Multi-LLM comprehension on compressed listings

**Scope:** 100 sampled tasks × 2 strategies (FIFO vs Priority-ALL) × 4 budgets
× N models = up to 3,200 LLM calls per model.

**Per call:**
- Input: compressed candidate list (per strategy × budget) + issue text
- Output: JSON `{"chosen_file": "path/to/file.py"}`
- Judge: string-compare `chosen_file` with gold list (exact or suffix-match)

**Models** (resolved from `config/models.yaml` + `.env`):

| Tier | Model | Endpoint | Via |
|------|-------|----------|-----|
| Local | `gpt-oss:20b` | `$OLLAMA_BASE_URL` | Ollama streaming |
| Local | `gemma4:26b` | `$OLLAMA_BASE_URL` | Ollama streaming |
| Local | `qwen3-coder:*` (size-fitting for 3090) | `$OLLAMA_BASE_URL` | Ollama streaming |
| API frontier | `claude-opus-4-7` | Anthropic | **Batch API + cache_control** |
| API midrange | `claude-sonnet-4-6` | Anthropic | **Batch API + cache_control** |
| API cheap | `claude-haiku-4-5` | Anthropic | **Batch API + cache_control** |
| API alt (optional) | `glm-4.6` | z.ai coding endpoint (Anthropic-compat) | sync with cache |
| API alt (optional) | `kimi-for-coding` | Moonshot | sync |

**Hard rule:** the final model list is chosen by the operator at run time
via `--models` flag; the script never assumes a model is present. If an
endpoint is unreachable, the script skips that tier and logs it.

**Output:** `artifacts/llm_results.parquet`
columns: `task_id, model, strategy, budget_tok, chosen_file, is_correct,
latency_ms, input_tokens, output_tokens, cache_read, cache_write, cost_usd,
error`

**Checkpointing** (MANDATORY per `feedback_checkpoint_long_operations.md`):
- Flush every 25 completions
- `--resume` support: skip already-processed `(task_id, model, strategy, budget_tok)` tuples

**Acceptance:**
- H3: Pearson r(algo_p₁, llm_acc) computed per model in aggregation.
- H5: compare `llm_acc[local_20B, Priority]` vs `llm_acc[opus, FIFO]` at
  same budget. Gap ≤ 10 p.p. confirms H5.

**Timeline:**
- Local models: ~1-2h per model (depends on 3090 throughput)
- Anthropic Batch API: submit → poll (up to 24h but usually < 1h)
- Expected total compute: overnight for full grid

---

### E3 — KV-cache efficiency study

**Scope:** subset of E2, specifically Anthropic calls (Opus/Sonnet/Haiku).

**Two passes over same 100 tasks × 2 strategies:**
1. `cache_control: ephemeral` on stable system prefix (default)
2. No cache_control (control condition)

**Measure:**
- Cache hit rate per call (from Anthropic response headers)
- Actual billed tokens (input, cache_read, cache_create)
- Cost per task end-to-end

**Output:** `artifacts/cache_analysis.parquet`
columns: `task_id, model, strategy, budget_tok, cache_condition, cache_hit_rate,
tokens_input, tokens_cache_read, tokens_cache_create, cost_usd_with_cache,
cost_usd_without_cache`

**Acceptance:** H4 verified if `mean(cost_with_cache) / mean(cost_without) ≤ 0.60`
on Sonnet.

**Timeline:** ~30 min with Batch API.

---

### E4 — Budget sweep & Pareto frontier (OPTIONAL, if E1-E3 leave time)

**Scope:** critical models × 8 budgets ∈ {500, 1k, 1.5k, 2k, 3k, 4k, 6k, 8k}.

**Deliverable:** $/correct Pareto curve showing where Priority dominates
FIFO and where budget headroom makes strategy irrelevant.

**Output:** `artifacts/pareto_budget.parquet`

---

## KV-cache strategy (critical for H4)

Anthropic billing: input = 1×, cache_read = 0.1×, cache_write 5m = 1.25×,
cache_write 1h = 2×. To minimize cost we want *cache_read*, not *cache_write*.

### Prompt structure

```
[STABLE — wrapped in cache_control: ephemeral]
=== SYSTEM PROMPT ===
You are a code navigation assistant. Given an issue and a list of
candidate files from a codebase search, return the single file most
likely to need modification.

=== OUTPUT SCHEMA ===
Return strict JSON:
{"chosen_file": "<path>", "confidence": 0.0-1.0, "reasoning": "..."}

=== SCORING RUBRIC ===
(3-5 short rules on how to weight keyword match / path depth / ext)

=== SWE-BENCH FORMAT NOTES ===
(1 paragraph on how candidate lists look)

[VARIABLE — per task, NOT cached]
=== TASK ===
Issue: <problem_statement text>

Candidate files:
  1. path/to/a.py
  2. path/to/b.py
  ...
```

Stable portion ~ 2000 tokens. Variable portion ~ 500-1500 tokens per task.
With 100 tasks at 86% cache hit rate (our measured baseline on GLM
classification), expected cost reduction: 55-65%.

### Batch API

- Submit 100-1600 requests per batch (task × strategy × budget grid)
- All requests share the stable system prefix → cache_control is effective
  even cross-request within a batch
- 50% discount on top of cache savings

### Measuring cache hit rate

From each Anthropic response:
- `usage.cache_read_input_tokens` — counted as 0.1×
- `usage.cache_creation_input_tokens` — counted as 1.25× (5m) or 2× (1h)
- `usage.input_tokens` — fresh, counted as 1×

Hit rate := `cache_read / (cache_read + cache_create + input_tokens)`.

---

## Paper deliverables from this benchmark

After all experiments complete, the following go into
`paper-1-trimtree.md` (in a new "Public Benchmark Results" section):

### Table A — SWE-bench algorithmic p₁

| Strategy | 1k tok | 2k tok | 4k tok | 8k tok | E[chunks] @ 4k |
|----------|-------:|-------:|-------:|-------:|---------------:|
| Random   |   —    |   —    |   —    |   —    |       —        |
| FIFO     |   —    |   —    |   —    |   —    |       —        |
| Reversed |   —    |   —    |   —    |   —    |       —        |
| Priority-KW | **—** | **—** | **—** | **—** |     —        |
| Priority-ALL| **—** | **—** | **—** | **—** |     —        |

### Table B — Multi-LLM accuracy at budget=2k

| Model | FIFO acc | Priority acc | Δ (p.p.) | corr(algo_p₁, llm_acc) |
|-------|---------:|-------------:|---------:|------------------------:|
| gpt-oss:20b (local) | — | — | — | — |
| gemma4:26b (local) | — | — | — | — |
| claude-haiku-4-5   | — | — | — | — |
| claude-sonnet-4-6  | — | — | — | — |
| claude-opus-4-7    | — | — | — | — |

### Table C — Cost efficiency

| Model | $/correct FIFO | $/correct Priority | Savings | Cache hit rate |
|-------|---------------:|-------------------:|--------:|---------------:|
| ...

### Figure 1 — p₁ by strategy, per n_candidates bucket
Bar chart, 5 strategies × 3 size buckets.

### Figure 2 — LLM acc vs algo_p₁ scatter
One point per (model, strategy, budget). Regression line confirms H3.

### Figure 3 — $/correct Pareto (optional, from E4)
Scatter of cost vs accuracy, Pareto front highlighted.

---

## Anonymization / what goes public

SWE-bench is public. Everything derived from its tasks is publishable
(task IDs, repo names, problem statements, file paths).

**Stays local (never in paper, never in committed data):**
- Ollama host IP
- API keys
- Any metadata about our JSON logs
- Per-session identifiers

**Aggregates only → `docs/research/data/`:**
- Table A/B/C row data (CSV)
- Per-task scatter-plot points (task_id + metrics, no internal tags)

**Prompts published in appendix:**
- The stable system prompt (sanitized, no internal references)
- Example variable portion on one public task

---

## Timeline (realistic)

| Phase | Effort | Wall time |
|-------|--------|-----------|
| Env + SWE-bench download + candidate generation | 2 h coding | 1 h compute |
| E1 (pure algo) | 1 h coding | 30 min |
| E2 prompt design + Anthropic Batch scaffolding | 2-3 h | — |
| E2 run (local models, sequential) | — | overnight |
| E2 run (Anthropic batch) | — | 1-24 h |
| E3 cache study | 1 h | 30 min |
| E4 Pareto (optional) | 1 h | 1 h |
| Aggregation + plots + paper writeup | 3-4 h | — |
| **Total** | ~10-12 h coding | ~1-2 days wall-clock |

---

## Out of scope

- Fine-tuning any model
- Training a learned Value function (Paper 2 territory)
- Non-English SWE-bench variants
- Tool responses other than file-search (MCP issues = separate Paper 1 MCP section)
- Multi-file gold (SWE-bench patches sometimes touch 2-3 files; we measure
  p₁ against any-of-gold, record `p_any` as secondary metric)
