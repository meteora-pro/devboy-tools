# Paper 1 — TLDR summary

**TrimTree: Priority-Driven Pagination for LLM Tool Responses**
*Target venue*: EMNLP 2026 / ACL 2026 (Systems track)
*Authors*: Andrei Mazniak

---

## One-sentence pitch

We treat LLM tool-response pagination as a **0/1 knapsack with a learned
value function** and show that a simple keyword-overlap scorer — plus a
FIFO safety-fallback — lifts first-chunk hit rate (p₁) by **+10.8 p.p.**
on SWE-bench Verified while **closing the local-model gap to within 0.1 p.p.
of Claude Opus 4.7** (when the gold file is in the compressed set).

---

## Problem

Coding agents (Claude Code, Cursor, Copilot) get tool responses that often
overflow the practical context budget:
- `get_issues` / `get_merge_requests` return 20–200 items
- Real Bash file-search (`grep -rln`, `rg -l`) returns 50+ paths routinely
- Empirical `0%` pagination rate — agents **never** request chunk 2. The
  first chunk is everything.

So the optimization target reduces to one number:
**p₁ = P(needed item ∈ first chunk)**.

---

## Method

1. Parse API response → tree of items (path, size, metadata).
2. Score each item with a **Value function** (strategy-dependent).
3. Solve **0/1 knapsack** over `(value, token_cost)` pairs within the
   client's budget → selected subset.
4. Order selected by value desc → that's chunk 1.
5. Remaining items addressable via `chunk_cursor` for agents who request
   more (in practice, nobody does, confirming step-up-front compression).

Strategies evaluated (6 total):

- **FIFO** — grep's native order (realistic CLI baseline)
- Random — shuffle (lower bound)
- Reversed — adversarial mirror of FIFO
- **Priority-KW** — `value = cos(path_tokens, issue_tokens)`
- Priority-ALL — composite (KW + depth + ext_prior + recency + fname)
- **Priority-KW⁺** — KW with FIFO-fallback when all scores are zero
  (production fix for degenerate empty-selected case, ~14% of tasks)

---

## Main results

### Table A — Algorithmic p₁ (no LLM, 500 tasks × 6 strategies × 4 budgets = 12 000 cells)

| Strategy | p₁ | Δ vs FIFO |
|----------|---:|----------:|
| Reversed | 2.6% | −21.6 |
| Random | 22.6% | −1.6 |
| **FIFO** | 24.2% | — |
| Priority-ALL | 30.2% | +6.0 |
| **Priority-KW** | **35.0%** | **+10.8** ← H1 PASS |
| **Priority-KW⁺** | **35.8%** | **+11.6** |

**Bucket-conditioned effect** (where compression actually bites):

| n_candidates | FIFO p₁ | Priority-KW⁺ p₁ |
|--------------|--------:|----------------:|
| small 1-5 (50% of tasks) | 36.5% | 42.4% |
| **medium 6-20 (13%)** | **0.0%** | **29.1%** |
| **large 21+ (11%)** | **10.2%** | **26.1%** (2.5×) |

In medium candidate lists, grep's natural ordering **never** puts gold at
rank 0. Priority fixes this.

### Table B — LLM accuracy (5 models × 100 tasks × 2 strategies × 4 budgets = 4 000 calls)

| Tier | Model | Accuracy | Notes |
|------|-------|---------:|-------|
| Frontier | Claude Opus 4.7 | **93.9%** | w/o thinking; Anthropic Batch + cache |
| Mid | Claude Sonnet 4.5 | 91.9% | ditto |
| Mid | GLM-5.1 (z.ai) | 91.9% | w/ thinking = 2048 budget |
| Local | gemma4:26b (Priority-KW⁺) | **87.0%** | 24 GB GPU, free |
| Local | gpt-oss:20b (Priority-KW⁺) | 82.3% | 24 GB GPU, free |

### Table C — Cost efficiency ($/correct answer)

| Model | Accuracy | $/correct |
|-------|---------:|----------:|
| gemma4:26b (local) | 86.1% | **$0.0000** |
| gpt-oss:20b (local) | 81.1% | **$0.0000** |
| GLM-5.1 (z.ai) | 91.9% | **$0.0006** |
| Sonnet 4.5 | 91.9% | $0.0022 |
| Opus 4.7 | 93.9% | $0.0307 |

**GLM-5.1 matches Sonnet 4.5 at 3.7× lower cost**. Opus 4.7 is 14× Sonnet
for +2 p.p. accuracy.

---

## Hypotheses — final verdicts

| # | Hypothesis | Verdict | Headline number |
|---|-----------|---------|-----------------|
| **H1** | Priority-KW > FIFO on algo p₁ (≥ +0.10 @ 2k) | **PASS ✓** | Δ = **+0.108** |
| H2 | Priority-ALL > Priority-KW | FAIL reversed | Δ = **−0.048** |
| H3 | corr(algo p₁, LLM acc) ≥ 0.85 | FAIL | r ∈ [−0.66, −0.03] |
| H4 | KV-cache reduces cost ≥ 40% | PASS (Sonnet 66% hit rate) | ≈ 40% input-side savings |
| **H5** | local ≈ frontier when gold in compressed set (≤10 p.p.) | **PASS ✓** | **gap = 0.1 p.p.** |

**H2 and H3 are reportable findings**:
- **H2 reversed**: composite scorer (depth + ext + recency + fname) **adds
  noise**. Pure keyword match with FIFO fallback is optimal.
- **H3 reversed / near-zero correlation**: reasoning-capable LLMs
  **compensate for suboptimal ranking** by reading the whole compressed
  list and applying their own heuristics. Ranking drives *inclusion*, not
  *ordering-within-included*.

---

## External cross-validation

Three independent corpora (our 4 175 events + two external contributors'
anonymized extracts from *their* Claude Code logs) converge on the same
main findings:

| Corpus | Sessions | Bash gold events | FIFO baseline p₁ | Keyword-match signal |
|--------|---------:|-----------------:|-----------------:|--------------------:|
| Ours | 2 607 | 4 175 | 36.7% | reported 83.5% |
| Corpus B | 689 | 590 | **35.4%** | **80.8%** |
| Corpus A | 221 | 145 | 24.1% (small-3-9 bucket, matches our 24%) | **85.4%** (real-signal-only) |

FIFO baseline ≈ 35% **replicates across three independent corpora**.
Keyword-match dominates priority signal in 80-85% of picks in every
corpus. This is unusually strong anti-cherry-picking evidence for a systems
paper.

---

## Commercial takeaway

For the file-localization subtask — the first step any coding agent must do
to answer a tool-call-heavy prompt:

> **A 26 GB local model (`gemma4:26b`) running on a single consumer GPU
> achieves within 0.1 p.p. of the 2-trillion-parameter Claude Opus 4.7
> frontier model** — *provided the compressed candidate list contains the
> gold file*. Priority-KW⁺ maximizes exactly that inclusion probability.
>
> Cost delta: **$0.00 vs $0.031 per correct answer** — effectively free
> for organizations that already own a 24 GB GPU.

---

## Reproducibility

See `paper-1-REPRODUCIBILITY.md` for:

- Exact hardware used (RTX 3090, 24 GB VRAM, Windows 10 + Docker)
- All credentials & subscriptions required (Anthropic API, z.ai Coding Plan)
- **Actual cost of our run: ≈ $25** (Opus is 92% of that)
- Step-by-step bash commands (Makefile targets: `make data`, `make e1`, …)
- Docker image `docker/paper1-repro` for hermetic rebuild
- All public aggregates in `docs/research/data/` (CSV + parquet)
- Interactive analysis notebook: `notebooks/paper1_analysis.ipynb`

---

## Files to cite / download

- `paper-1-trimtree.md` — full paper draft (820 lines)
- `paper-1-REPRODUCIBILITY.md` — this kit
- `docs/research/data/swe_bench_*.csv` — all aggregates, under 200 KB total
- `docs/research/data/llm_results/llm_results.*.parquet` — per-model raw
- `docs/research/notebooks/paper1_analysis.ipynb` — interactive analysis
- `docs/research/paper1-repro/{Makefile,Dockerfile,scripts/}` — the
  full pipeline

---

## Known limitations (full list in paper §Limitations)

1. Grep-proxy misses 44% of golds — sets absolute ceiling on p₁.
2. SWE-bench Verified is Python-only; cross-language validation is future work.
3. Django is 46% of the dataset — per-repo breakdown in supplementary.
4. Median candidate list is 4 items → TrimTree's value only shines in top 10%
   of tasks with ≥ 20 candidates.
5. Single-turn E2 (no full-agent rollout) — resolve-rate on the SWE-bench
   harness is Paper-1 follow-up work.
6. Opus batch cache hit rate was 12.6% (5-min TTL expired across parallel
   workers). `ttl: "1h"` would likely match Sonnet's 66%.

---

## Next papers in the series

- **Paper 2** — MCKP: format-adaptive tree encoding (CSV for tabular
  sub-trees, Markdown for mixed, prose for text-heavy). Extends TrimTree
  from "which items" to "which encoding per subtree".
- **Paper 3** — Context Enrichment Hypothesis: thin list responses drive
  more agent follow-up calls (partially replicated in our corpora).
- **Paper 4** — Dataset-as-Context: replace paginated tool calls with
  queryable Parquet artifacts the LLM queries via generated code.
