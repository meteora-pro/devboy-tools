# Paper 1 — Reproducibility kit

How to reproduce every number in **Paper 1: TrimTree — Priority-Driven
Pagination for LLM Tool Responses** from a clean machine.

All experiment scripts are in `docs/research/paper1-repro/scripts/`
(prefixed `01` to `07`). The pipeline is deterministic given the same seed
and same inputs; most cost comes from cloud-LLM calls in E2, which are
billed per token.

---

## 1. Hardware used in our runs

| Component | Our setup | Minimum |
|-----------|-----------|---------|
| CPU | AMD Ryzen 5950X (16c) | 8 cores |
| GPU | NVIDIA RTX 3090 (24 GB VRAM) | 24 GB VRAM (for `gemma4:26b`) or cloud-only |
| RAM | 128 GB | 16 GB |
| Disk | 100 GB free (5 GB for SWE-bench repos) | 20 GB |
| OS | Windows 10 + Git Bash + Docker Desktop | Linux / macOS / Windows |
| Python | 3.12.10 | 3.10+ |
| Rust | 1.90 (only needed for the `llm-eval` crate — synthetic baseline) | optional |
| Ollama | 0.21.0 (native Anthropic-compat at `/v1/messages`) | 0.14+ |

**GPU-only vs cloud-only**:
- E2-local runs (`gpt-oss:20b`, `gemma4:26b`) require **24 GB VRAM**. Can be
  skipped — all findings are replicable from cloud LLMs alone.
- E2-cloud runs (Sonnet 4.5, Opus 4.7, GLM-5.1) are **GPU-free**.

---

## 2. Credentials & accounts

Copy `paper1-repro/.env.example` → `paper1-repro/.env` and fill in:

| Env var | Source | Required for |
|---------|--------|-------------|
| `ANTHROPIC_API_KEY` | [console.anthropic.com](https://console.anthropic.com) → API Keys | E2 Sonnet / Opus (script 07) + E3 cache study |
| `ZAI_API_KEY` | [z.ai](https://z.ai/subscribe) GLM Coding Plan | E2 GLM-5.1 / GLM-4.5 (script 04 `--provider zai`) |
| `OLLAMA_NATIVE_URL` | your Ollama host | E2 local models (default `http://localhost:11343`) |
| `HF_TOKEN` *(optional)* | [huggingface.co/settings/tokens](https://huggingface.co/settings/tokens) | Faster SWE-bench download (script 01) |

**Subscriptions / plans**:
- **Anthropic** — prepaid API credits. Do **not** use OAuth subscription
  tokens (Messages API rejects them). Minimum $25 buys the full E2 frontier
  tier (Opus 4.7 is the dominant cost; Sonnet 4.5 is ~$2).
- **z.ai GLM Coding Plan** — $3/month gives access to `glm-5.1` and
  `glm-5-turbo`. For our volume (800 calls), actual billed cost was $0.44 —
  well within the plan allowance.
- **Ollama** — no subscription; local inference free. Our models were pinned:
  ```
  gemma4:26b@sha256:5571076f3d70…
  gpt-oss:20b@sha256:17052f91a42e…
  ```

### Do NOT do

- Do **not** embed API keys in any committed file. `.env` is git-ignored by
  default in our repo (root `.gitignore`).
- Do **not** share your `ANTHROPIC_API_KEY`. After reproducing, rotate the
  key via the Anthropic console.

---

## 3. Actual costs of our run

Measured on 2026-04-22 / 2026-04-23.

| Experiment | Provider | Model | Calls | Duration | Cost |
|-----------|----------|-------|------:|---------:|-----:|
| E1 (algorithmic) | — | — | 12 000 | 2 min (CPU) | $0.00 |
| E2 local — gpt-oss base + fallback | Ollama | gpt-oss:20b | 1 200 | ~115 min | $0.00 (electricity) |
| E2 local — gemma4 base + fallback | Ollama | gemma4:26b | 1 200 | ~137 min | $0.00 |
| E2 cloud — GLM-5.1 (thinking=2048) | z.ai Anthropic-compat | glm-5.1 | 800 | 210 min | **$0.44** |
| E2 cloud — Sonnet batch | Anthropic Batch API | claude-sonnet-4-5 | 800 | **2 min** | **$1.59** |
| E2 cloud — Opus batch | Anthropic Batch API | claude-opus-4-7 | 800 | ~2 min | **$23.05** |
| **Total paid cost** | | | 2 400 cloud calls | ~7 hrs wall | **≈ $25** |

**Cost budget recommendation**: allocate **$30** for a full reproduction
with all 5 models. Skipping Opus brings it down to **$5**.

---

## 4. Reproduction steps (bash on any Unix-like; Git Bash on Windows)

### Setup (one-time, ~10 min)

```bash
# 1. Clone the repo
git clone https://github.com/meteora-pro/devboy-tools.git
cd devboy-tools

# 2. Install uv (Python script runner with PEP 723 inline deps)
python -m pip install --user uv
# Add ~/.local/bin or equivalent to PATH if not already

# 3. Configure credentials
cp docs/research/paper1-repro/.env.example \
   docs/research/paper1-repro/.env
$EDITOR docs/research/paper1-repro/.env  # fill in keys

# 4. (local only) Start Ollama and pull pinned models
#    Your Ollama server — default http://localhost:11434 or http://host:11343
ollama pull gemma4:26b
ollama pull gpt-oss:20b
ollama show gemma4:26b | grep digest   # verify matches our pinned sha
```

### Stage 1 — data preparation (~60 min, deterministic)

```bash
cd docs/research/paper1-repro

# Download SWE-bench Verified (500 tasks, ~30 sec)
python -m uv run scripts/01_download_swe_bench.py
# → artifacts/swe_bench_gold.parquet (500 rows, 292 KB)

# Clone 12 Python repos + grep candidates per task (~60 min)
# ~5 GB disk for artifacts/repos_cache/
python -m uv run scripts/02_generate_candidates.py
# → artifacts/candidates.parquet (8 645 rows)
```

### Stage 2 — algorithmic E1 (~2 min CPU)

```bash
python -m uv run scripts/03_run_strategies.py
# → artifacts/strategy_results.parquet (12 000 rows)
# Prints Table A to stderr
# H1 PASS and H2 FAIL reversed are visible here.
```

### Stage 3 — LLM E2, local (each ~40-90 min; skip if no GPU)

```bash
# Sequential — unload between models to avoid VRAM swap
python -m uv run scripts/04_run_multi_llm.py --model gpt-oss:20b \
    --strategies fifo,priority_kw,priority_kw_fallback \
    --tasks 100 --budgets 1000,2000,4000,8000 --think-level high \
    --output-partial gptoss_full

curl -X POST http://localhost:11343/api/generate \
    -H 'Content-Type: application/json' \
    -d '{"model":"gpt-oss:20b","keep_alive":0}'

python -m uv run scripts/04_run_multi_llm.py --model gemma4:26b \
    --strategies fifo,priority_kw,priority_kw_fallback \
    --tasks 100 --budgets 1000,2000,4000,8000 --think-level high \
    --output-partial gemma_full
```

### Stage 4 — LLM E2, cloud GLM-5.1 (~3.5 hrs wall)

```bash
python -m uv run scripts/04_run_multi_llm.py --provider zai \
    --model glm-5.1 --tasks 100 \
    --strategies fifo,priority_kw --budgets 1000,2000,4000,8000 \
    --thinking-budget 2048 --concurrency 1 \
    --output-partial glm5_1
# Total ~210 min; actual cost $0.44 (on z.ai Coding Plan)
```

### Stage 5 — LLM E2, Anthropic Batch (~5 min + $)

```bash
# Sonnet 4.5, batch + cache_control (~$1.60, 2 min)
python -m uv run scripts/07_anthropic_batch.py \
    --model claude-sonnet-4-5 --tasks 100 \
    --strategies fifo,priority_kw_fallback --budgets 1000,2000,4000,8000 \
    --output-partial sonnet45

# Opus 4.7 (~$23, 2 min)
python -m uv run scripts/07_anthropic_batch.py \
    --model claude-opus-4-7 --tasks 100 \
    --strategies fifo,priority_kw_fallback --budgets 1000,2000,4000,8000 \
    --output-partial opus47
```

### Stage 6 — aggregate + plots (~15 sec)

```bash
python -m uv run scripts/05_aggregate_results.py
# Writes Tables A/B/C CSV + prints H1..H5 check results to stderr

python -m uv run scripts/06_plots.py
# Renders artifacts/figures/fig1..3.png
```

---

## 5. Expected artifacts

Each stage produces deterministic outputs. Compare hash of yours to ours
(listed in `paper1-repro/artifact_digests.json` if you want strict byte
match — otherwise row counts + mean metrics are sufficient).

| Stage | File | Rows | Key metric |
|-------|------|-----:|------------|
| 01 | `swe_bench_gold.parquet` | 500 | 12 unique repos, 86% single-gold |
| 02 | `candidates.parquet` | ~8 600 | 55.8% tasks have gold in candidates |
| 03 | `strategy_results.parquet` | 12 000 | Priority-KW⁺ p₁ = 35.8% |
| 04/07 | `llm_results.<model>.parquet` | 400-1 200 | see Table B in paper |
| 05 | `table_a_strategies.csv` + `table_b_*` + `table_c_cost.csv` | — | final numbers |
| 06 | `figures/fig1..3.png` | — | 200 dpi publication plots |

---

## 6. Public aggregates (in this repo, under `docs/research/data/`)

All aggregates below are **anonymized** — no raw LLM responses, no repo
paths, no API keys. Safe to commit.

| File | Description | Rows |
|------|-------------|-----:|
| `swe_bench_gold.parquet` | SWE-bench Verified gold files per task | 500 |
| `swe_bench_strategy_results.parquet` | E1 full matrix | 12 000 |
| `swe_bench_strategies.csv` | Table A — p₁ × strategy × budget | 24 |
| `swe_bench_strategies_by_bucket.csv` | A by n_candidates bucket | 72 |
| `swe_bench_llm.csv` | Table B — accuracy × model × strategy × budget | 80 |
| `swe_bench_llm_by_bucket.csv` | B by n_candidates bucket | 240 |
| `swe_bench_cost.csv` | Table C — $/correct across tiers | 5 |
| `swe_bench_corr.csv` | Raw cells for correlation analysis (Fig 2) | ~6 400 |
| `llm_results/llm_results.<model>.parquet` | Per-model raw E2 data | — |
| `ablation_results.csv` | Synthetic ablation (separate Rust harness) | — |
| `gold_selection_real.csv` | Anonymized real-world Bash gold events | — |

Use these CSV/parquet directly to reproduce any table or plot without
running the pipeline.

---

## 7. Interactive analysis

See `docs/research/notebooks/paper1_analysis.ipynb` for a pre-built Jupyter
notebook with:
- Loading all aggregates
- Recreating Tables A/B/C
- Hypothesis check functions (H1…H5) with your own threshold
- Example cells for your own hypotheses (scan budget sweet spot, test
  per-repo generalization, compute confidence intervals, …)

---

## 8. What is NOT included in this repo

Per `.gitignore` rules (`docs/research/data/__personal__/` and
`docs/research/benchmarks/paper*/`):
- Raw Claude Code JSONL logs (PII)
- Repo clones in `artifacts/repos_cache/` (you regenerate via script 02)
- `.env` with real keys
- Our host IPs

If you want the external-validation corpora (Corpus B 689 sessions, Corpus A
221 sessions) used in our Cross-Validation section: contact the authors.
Those were contributed under private agreement.

---

## 9. Troubleshooting

**Cache `cache_creation_input_tokens = 0` on Sonnet 4.6 or Opus 4.5**: known
rollout issue on these specific models. Use `claude-sonnet-4-5` or
`claude-opus-4-7`, which we verified.

**Ollama rate error on concurrent calls**: local Ollama is single-model per
host by default. Use `--concurrency 1` for local, up to 3-5 for cloud
providers (z.ai rate-limits ~6 concurrent at Coding Plan tier).

**Windows `link.exe` conflict with Git Bash coreutil `link`**: build inside a
Docker container — `rust:1.90-slim` works out of the box. See the
`devboy-tools` root README for the exact docker command used.

**SWE-bench HuggingFace download slow**: set `HF_TOKEN` env var to skip
unauthenticated rate limits.

---

## 10. Versions pinned at time of paper

```text
Python          3.12.10
uv              0.11.7
Rust            1.90.0
Ollama          0.21.0
Anthropic API   2023-06-01
Claude models   claude-sonnet-4-5, claude-opus-4-7
z.ai endpoint   https://api.z.ai/api/anthropic (GLM Coding Plan)
SWE-bench       princeton-nlp/SWE-bench_Verified (HF revision "main")
```
