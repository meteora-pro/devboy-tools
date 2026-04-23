# Paper 1 — SWE-bench TrimTree reproduction pipeline
#
# Run `make help` for target list. `make repro` runs the full pipeline
# assuming `.env` is filled in and Ollama is reachable.
#
# Cost-controlled targets: `data`, `e1`, `e2-local`, `e2-cloud-glm`,
# `e2-cloud-anthropic`, `aggregate` can be run independently.

PY := python -m uv run
SCRIPTS := scripts
ARTIFACTS := artifacts

.DEFAULT_GOAL := help

.PHONY: help
help:
	@echo "Paper 1 reproduction targets:"
	@echo ""
	@echo "  setup             — verify .env + Ollama connectivity"
	@echo "  data              — download SWE-bench + generate candidates (stages 01, 02)"
	@echo "  e1                — algorithmic strategy eval (stage 03)"
	@echo "  e2-local          — LLM eval on local gpt-oss + gemma (stage 04, ~4 hrs, needs 24GB GPU)"
	@echo "  e2-cloud-glm      — LLM eval on z.ai glm-5.1 (stage 04 zai, ~3.5 hrs, ~\$$0.5)"
	@echo "  e2-cloud-sonnet   — Anthropic Batch Sonnet 4.5 (stage 07, ~2 min, ~\$$1.6)"
	@echo "  e2-cloud-opus     — Anthropic Batch Opus 4.7 (stage 07, ~2 min, ~\$$23)"
	@echo "  aggregate         — tables + plots (stages 05, 06)"
	@echo "  clean             — remove artifacts/ (keeps repos_cache, swe_bench_cache)"
	@echo "  clean-all         — remove everything including repo clones"
	@echo ""
	@echo "  repro             — everything above, sequentially"
	@echo "  repro-no-opus     — same but skip Opus (saves \$$23)"
	@echo "  repro-cloud-only  — skip local models (no GPU required)"

# ─── Setup check ─────────────────────────────────────────────────────────────

.PHONY: setup
setup:
	@test -f .env || (echo "Missing .env — copy .env.example and fill in keys" && exit 1)
	@grep -q "^ANTHROPIC_API_KEY=sk-ant-" .env || echo "WARN: ANTHROPIC_API_KEY not set"
	@grep -q "^ZAI_API_KEY=" .env || echo "WARN: ZAI_API_KEY not set"
	@echo "Environment OK."

# ─── Stage 1 — data ──────────────────────────────────────────────────────────

$(ARTIFACTS)/swe_bench_gold.parquet: $(SCRIPTS)/01_download_swe_bench.py
	$(PY) $<

$(ARTIFACTS)/candidates.parquet: $(SCRIPTS)/02_generate_candidates.py $(ARTIFACTS)/swe_bench_gold.parquet
	$(PY) $<

.PHONY: data
data: $(ARTIFACTS)/candidates.parquet
	@echo "Data stage complete."

# ─── Stage 2 — E1 algorithmic ────────────────────────────────────────────────

$(ARTIFACTS)/strategy_results.parquet: $(SCRIPTS)/03_run_strategies.py $(ARTIFACTS)/candidates.parquet
	$(PY) $<

.PHONY: e1
e1: $(ARTIFACTS)/strategy_results.parquet
	@echo "E1 complete. See artifacts/strategy_results.parquet."

# ─── Stage 3 — E2 local ──────────────────────────────────────────────────────

.PHONY: e2-local-gptoss
e2-local-gptoss: setup data
	$(PY) $(SCRIPTS)/04_run_multi_llm.py --model gpt-oss:20b \
	    --strategies fifo,priority_kw,priority_kw_fallback \
	    --tasks 100 --budgets 1000,2000,4000,8000 --think-level high \
	    --output-partial gptoss_full

.PHONY: e2-local-gemma
e2-local-gemma: setup data
	-curl -s -X POST http://localhost:11343/api/generate \
	    -H 'Content-Type: application/json' \
	    -d '{"model":"gpt-oss:20b","keep_alive":0}' > /dev/null
	$(PY) $(SCRIPTS)/04_run_multi_llm.py --model gemma4:26b \
	    --strategies fifo,priority_kw,priority_kw_fallback \
	    --tasks 100 --budgets 1000,2000,4000,8000 --think-level high \
	    --output-partial gemma_full

.PHONY: e2-local
e2-local: e2-local-gptoss e2-local-gemma
	@echo "E2 local complete."

# ─── Stage 4 — E2 cloud ──────────────────────────────────────────────────────

.PHONY: e2-cloud-glm
e2-cloud-glm: setup data
	$(PY) $(SCRIPTS)/04_run_multi_llm.py --provider zai --model glm-5.1 \
	    --strategies fifo,priority_kw --budgets 1000,2000,4000,8000 \
	    --tasks 100 --thinking-budget 2048 --concurrency 1 \
	    --output-partial glm5_1

.PHONY: e2-cloud-sonnet
e2-cloud-sonnet: setup data
	$(PY) $(SCRIPTS)/07_anthropic_batch.py --model claude-sonnet-4-5 \
	    --strategies fifo,priority_kw_fallback --budgets 1000,2000,4000,8000 \
	    --tasks 100 --output-partial sonnet45

.PHONY: e2-cloud-opus
e2-cloud-opus: setup data
	$(PY) $(SCRIPTS)/07_anthropic_batch.py --model claude-opus-4-7 \
	    --strategies fifo,priority_kw_fallback --budgets 1000,2000,4000,8000 \
	    --tasks 100 --output-partial opus47

.PHONY: e2-cloud
e2-cloud: e2-cloud-glm e2-cloud-sonnet e2-cloud-opus
	@echo "E2 cloud complete."

# ─── Stage 5 — aggregate + plots ─────────────────────────────────────────────

.PHONY: aggregate
aggregate:
	$(PY) $(SCRIPTS)/05_aggregate_results.py
	$(PY) $(SCRIPTS)/06_plots.py
	@echo "Aggregation + plots complete. See artifacts/figures/"

# ─── Orchestration ──────────────────────────────────────────────────────────

.PHONY: repro
repro: setup data e1 e2-local e2-cloud aggregate
	@echo "Full reproduction complete."

.PHONY: repro-no-opus
repro-no-opus: setup data e1 e2-local e2-cloud-glm e2-cloud-sonnet aggregate
	@echo "Reproduction (no Opus) complete."

.PHONY: repro-cloud-only
repro-cloud-only: setup data e1 e2-cloud aggregate
	@echo "Cloud-only reproduction complete."

# ─── Cleanup ────────────────────────────────────────────────────────────────

.PHONY: clean
clean:
	rm -rf $(ARTIFACTS)/figures $(ARTIFACTS)/*.parquet $(ARTIFACTS)/*.csv \
	       $(ARTIFACTS)/batch_id.*.txt $(ARTIFACTS)/.*.log 2>/dev/null || true
	@echo "Cleaned artifacts. Repos cache and SWE-bench cache preserved."

.PHONY: clean-all
clean-all: clean
	rm -rf $(ARTIFACTS)/repos_cache $(ARTIFACTS)/swe_bench_cache 2>/dev/null || true
	@echo "Cleaned everything."
