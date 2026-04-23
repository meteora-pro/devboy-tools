# Paper 1 — Reproducibility pipeline

**Status:** public. Scripts + configs committed to the repo. Only `.env`
(secrets) and `artifacts/` (local outputs, repo clones, 5+ GB) are gitignored.

Purpose: public benchmark for Paper 1 — TrimTree Priority-Driven Pagination —
against SWE-bench Verified, across multiple LLMs. Final tables and plots go
into `../paper-1-trimtree.md`. See also `../paper-1-REPRODUCIBILITY.md` for
step-by-step commands and cost breakdown, and `../paper-1-SUMMARY.md` for
the TLDR.

---

## Quick start

```bash
# 1. Copy env template and fill in secrets (Ollama host IP, API keys)
cp .env.example .env
$EDITOR .env

# 2. Download SWE-bench Verified (500 tasks)
uv run scripts/01_download_swe_bench.py

# 3. Generate candidate lists per task (keyword grep proxy)
uv run scripts/02_generate_candidates.py

# 4. Run pure-algo strategies (FIFO / Random / Reversed / Priority-KW / Priority-ALL)
uv run scripts/03_run_strategies.py

# 5. Multi-LLM comprehension pass (uses Ollama + configured API models)
uv run scripts/04_run_multi_llm.py --models gemma4:26b,gpt-oss:20b

# 6. Aggregate results for paper tables
uv run scripts/05_aggregate_results.py
```

Per-step details in `TASK.md`. Architecture rationale, KV-cache strategy,
batch API design — also in `TASK.md`.

---

## Layout

```
paper1/
├── README.md                  # ← you are here
├── TASK.md                    # full experiment spec (hypotheses, metrics,
│                              #   acceptance criteria, deliverables)
├── .env.example               # template for secrets (Ollama host, API keys)
├── .env                       # actual secrets — never commit, never log
├── anonymization_rules.md     # what goes back to paper vs stays local
├── scripts/
│   ├── 01_download_swe_bench.py
│   ├── 02_generate_candidates.py
│   ├── 03_run_strategies.py   # pure-algo p₁ / p@k / E[chunks]
│   ├── 04_run_multi_llm.py    # LLM comprehension, Anthropic batch + Ollama stream
│   ├── 05_aggregate_results.py
│   └── 06_plots.py            # optional matplotlib/altair plots
├── config/
│   ├── models.yaml            # endpoints + pricing + cache strategies
│   ├── strategies.yaml        # 5 value-assignment strategies
│   └── budgets.yaml           # token budget grid
├── prompts/
│   ├── system_cache_prefix.md # stable portion (goes under cache_control)
│   └── task_variable.md       # per-task wrapper template
└── artifacts/
    ├── swe_bench_gold.parquet
    ├── candidates.parquet
    ├── strategy_results.parquet
    ├── llm_results.parquet
    └── cache_analysis.parquet
```

---

## Hard rules (enforced by gitignore + spec)

1. **Nothing from this folder is ever committed.** If you push it by mistake,
   rotate all secrets immediately.
2. **No host IPs, API keys, or model names in committed paper.** The paper
   refers to models by tier (e.g. "local 20B model") or by public name only.
3. **Long operations must checkpoint.** Per `docs/research/scripts/llm_classify_*.py`
   pattern: flush parquet every N completions, `--resume` support for restart.
4. **Ollama host is remote.** Scripts that call Ollama use `OLLAMA_BASE_URL`
   from `.env`; never hard-code the IP. Verify reachability before long runs.
5. **KV-cache by default for Anthropic calls.** System prompt is split into
   stable (cache_control=ephemeral) + variable per-task. Batch API for ≥ 100
   calls of same shape — 50% cost reduction.

---

## Where results flow back into the paper

After each experiment stage completes, review artifacts → decide what goes
into `paper-1-trimtree.md`:

- **E1 output** (strategy_results.parquet) → Table A: SWE-bench p₁/p@k per strategy × budget
- **E2 output** (llm_results.parquet) → Table B: model × strategy accuracy
- **E3 output** (cache_analysis.parquet) → small table or footnote: cache hit rate confirms claim
- **E4 output** (pareto_budget.parquet) → Figure: $/correct Pareto frontier

The raw artifacts stay here; only anonymized aggregates get copied into
`docs/research/data/` (which IS committed).
