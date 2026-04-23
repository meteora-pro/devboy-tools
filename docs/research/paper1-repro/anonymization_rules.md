# Anonymization Rules — Paper 1 Benchmark

Everything in `paper1/` is local-only. When pulling results into the public
paper (`docs/research/paper-1-trimtree.md`) or the public data directory
(`docs/research/data/`), apply these rules.

---

## Safe to publish (goes into paper / committed data)

- SWE-bench task IDs (already public)
- Public GitHub repo names referenced by SWE-bench
- Problem statements from SWE-bench (public CC license)
- Candidate file paths from public repos
- Aggregate metrics: p₁, p@k, llm_acc, cost numbers
- Prompt templates — after sanitization check
- Model names by their **public identifier** only
  (e.g. `claude-sonnet-4-6`, `gpt-oss:20b`)
- Cache hit rates as percentages

---

## Never publish (stays in `paper1/`)

| Item | Reason |
|------|--------|
| Ollama host IP / hostname | Reveals infra topology |
| Any API key (even redacted) | Credential hygiene |
| `.env` file or its contents | See above |
| Timestamps that correlate with our usage patterns | Could deanon |
| GPU model name if specific to our setup | Reveals infra |
| Internal session IDs | N/A here but blanket rule |
| Our own Claude Code log extracts | Separate anonymization pipeline |
| Latency numbers tied to specific host | Reveals infra |

---

## Mapping — where aggregates land

| Paper1 artifact | Summarize to | Committed location |
|-----------------|--------------|-------------------|
| `strategy_results.parquet` | per-strategy × budget means | `docs/research/data/swe_bench_strategies.csv` |
| `llm_results.parquet` | per-(model, strategy, budget) means | `docs/research/data/swe_bench_llm.csv` |
| `cache_analysis.parquet` | single-row hit-rate summary | Included inline in paper |
| `candidates.parquet` | keep local (too granular) | — |
| `swe_bench_gold.parquet` | keep local (redundant with SWE-bench) | — |

---

## Pre-publish checklist

Before moving any file from `paper1/` to `docs/research/data/`:

- [ ] Grep for Ollama host pattern: `grep -E "(OLLAMA|http.*:1143[0-9]|[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+)"`
- [ ] Grep for API keys: `grep -iE "(sk-|api.key|authorization)"`
- [ ] Confirm only aggregate columns remain (no `session_id`, no paths to
      non-public repos)
- [ ] Round latency / cost numbers to avoid identifier-level precision
- [ ] Run `docs/research/scripts/check_anonymization.py` (if created) on the
      output CSV before `git add`
- [ ] User review per global rule (`feedback_review_before_commit.md`)

---

## If you accidentally commit a secret

1. Rotate the credential immediately (new key at provider console)
2. `git rm` the file, commit, push
3. The secret stays in git history — consider
   `git filter-repo --invert-paths --path docs/research/benchmarks/paper1/`
   if the repo is private; for public repos treat it as burned
4. Audit logs for the leaked key's usage window
