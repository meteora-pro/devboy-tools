---
name: research-pipeline
description: Rebuild the Claude Code agent-usage research dataset (sessions, loops, tool calls, gold-selection events, correlations) from ~/.claude/projects/*.jsonl logs into /tmp/claude_analysis/*.parquet. Use when the user wants to bootstrap analytics on a new machine, refresh after new sessions, validate reproducibility, or onboard the research pipeline after cloning this repo.
allowed-tools: Bash, Read
---

# research-pipeline

Reproducible pipeline for the Paper 1 (TrimTree) and related research. Parses
Claude Code session logs (`~/.claude/projects/**/*.jsonl`) into a family of
Parquet tables under `/tmp/claude_analysis/`, then optionally runs LLM
classification and correlation analyses.

All scripts are anonymizing-by-construction: no raw paths, session UUIDs, or
project slugs leave the pipeline. API keys resolve through environment
variables only.

---

## Preconditions

1. **`uv` installed** — `curl -LsSf https://astral.sh/uv/install.sh | sh`
2. **Repo cloned** — scripts live at `docs/research/scripts/*.py`
3. **JSONL logs present** — `~/.claude/projects/` is populated automatically
   by Claude Code; nothing to do if the user has used Claude Code before
4. **(Optional) z.ai key** for LLM classification:
   ```bash
   mkdir -p /tmp/claude_analysis
   cat > /tmp/claude_analysis/.env.zai <<EOF
   ZAI_API_KEY=<key>
   ZAI_BASE_URL=https://api.z.ai/api/anthropic
   ZAI_MODEL=glm-4.6
   EOF
   ```

## Pipeline DAG

```
 ~/.claude/projects/*.jsonl
         │
         ▼
  1. analyze_sessions.py            — core ETL + anonymizer
         │  emits: sessions, turns, tool_calls, bash_commands,
         │         human_turns, compactions, meta_events (parquet)
         ▼
  2. detect_workflow_patterns.py    — workflow labels
  3. extract_loops.py               — 1 row per agent loop
  4. enrich_loops.py                — cost, cache, trigger, success_proxy
  5. compute_session_features.py    — 1 row per session, 225 features
         │
         ▼
  6. extract_loop_list_events.py    — MCP gold-selection events
  7. extract_bash_list_events.py    — Bash file-search gold events
         │
         ▼
  8. llm_classify_bash_events.py    — (optional, needs z.ai key)
         │
         ▼
  9. mine_correlations.py           — pairwise feature correlations
 10. partial_correlations.py        — controlling for session size
 11. within_bucket_correlations.py  — per-bucket analyses
 12. analyze_features.py            — human-readable summary tables
```

Steps 2-5 depend on step 1. Steps 6-7 depend on 1 and 3. Step 8 depends on 7.
Steps 9-12 depend on 5. Within each group you can parallelize if memory allows.

## Run — canonical sequence

Run from the repo root. Default outputs land in `/tmp/claude_analysis/`.

```bash
mkdir -p /tmp/claude_analysis

# 1. Core ETL (2-5 min for 1 GB of logs)
uv run docs/research/scripts/analyze_sessions.py

# 2-5. Loops + phases + features (1-2 min each)
uv run docs/research/scripts/detect_workflow_patterns.py
uv run docs/research/scripts/extract_loops.py
uv run docs/research/scripts/enrich_loops.py
uv run docs/research/scripts/compute_session_features.py

# 6-7. Gold-selection (Paper 1 core, < 1 min each)
uv run docs/research/scripts/extract_loop_list_events.py
uv run docs/research/scripts/extract_bash_list_events.py

# 8. LLM classification — OPTIONAL (~18 min on z.ai coding plan, ~$1-2)
# Skip if no API key; downstream analyses still work.
uv run docs/research/scripts/llm_classify_bash_events.py \
    --concurrency 10 --checkpoint-every 25

# 9-11. Correlations (seconds each)
uv run docs/research/scripts/mine_correlations.py
uv run docs/research/scripts/partial_correlations.py
uv run docs/research/scripts/within_bucket_correlations.py

# 12. Readable tables
uv run docs/research/scripts/analyze_features.py
```

**Total wall-clock**: 10-20 minutes without LLM step; +~20 min with it.

## Verify the pipeline succeeded

```bash
# List what was produced
ls -la /tmp/claude_analysis/*.parquet

# Row counts per table (sanity check)
for f in /tmp/claude_analysis/*.parquet; do
  echo -n "$(basename $f): "
  uv run --with 'duckdb>=1.0' python3 -c "
import duckdb; print(duckdb.sql(\"SELECT COUNT(*) FROM '$f'\").fetchone()[0])"
done
```

Expected tables after a full run (names, typical row magnitude):

| File | ~rows | Contents |
|---|---:|---|
| sessions_enriched.parquet | ~3k | per-session, 225 columns |
| loops_enriched.parquet | ~15k | per-agent-loop with cost/cache/outcome |
| turns.parquet | ~700k | every turn (human/agent/tool-result) |
| tool_calls.parquet | ~250k | every tool invocation |
| bash_commands.parquet | ~100k | every Bash command |
| human_turns.parquet | ~20k | user messages with intent labels |
| meta_events.parquet | ~370k | lifecycle events (start/resume/compact) |
| bash_list_events.parquet | ~4k | Bash gold-selection events |
| bash_events_classified.parquet | ~4k | (if step 8 ran) LLM-categorized |
| compactions.parquet | ~900 | context compactions |
| workflow_patterns.parquet | ~25k | phase labels |

## Quick analytics (after pipeline)

A few one-liners to confirm data is real:

```bash
# Total cost, tokens, sessions
uv run --with 'duckdb>=1.0' python3 -c "
import duckdb; D='/tmp/claude_analysis'
print(duckdb.sql(f\"\"\"
  SELECT COUNT(*) n_sessions,
         SUM(agent_loops) n_loops,
         ROUND(SUM(in_tokens_total+out_tokens_total+cache_read_total+cache_create_total)/1e9, 2) tokens_b
  FROM '{D}/sessions_enriched.parquet'
\"\"\").fetchdf())"

# Top models used
uv run --with 'duckdb>=1.0' python3 -c "
import duckdb; D='/tmp/claude_analysis'
print(duckdb.sql(f\"\"\"
  SELECT top_model, COUNT(*) n FROM '{D}/sessions_enriched.parquet'
  WHERE top_model IS NOT NULL GROUP BY 1 ORDER BY n DESC LIMIT 10
\"\"\").fetchdf())"
```

## Known issues (document as you find them)

1. **`classify_file` misclassifies tests as code** — `analyze_sessions.py:505`.
   `.test.ts`, `_test.py`, `*.spec.tsx`, `.feature` files end up in `writes_code`
   instead of `writes_test`. Fix: reorder conditions in `classify_file` so the
   test/spec pattern check runs before the code-extension check. After fix,
   rerun from step 1.

2. **Model pricing in `enrich_loops.py` is a snapshot** — if Anthropic changes
   rates, `cost_usd` becomes approximate. Fix by editing the `MODEL_PRICING`
   dict in `enrich_loops.py` and rerunning step 4.

3. **`/tmp` may be cleared on reboot (macOS)** — if you want durable storage,
   pass `--out-dir ~/claude_analysis` (where supported) or rsync the parquet
   tree to a permanent location after each run.

4. **Sessions with `ts_ms` outside their own `[ts_start_ms, ts_end_ms]`** —
   23 sessions (~6M turns) have this artifact, probably from resume/replay
   semantics. For concurrency analyses, clamp `active_ms BETWEEN session_start
   AND session_end` to avoid inflated counts.

## Portability — running on a new machine

Three scenarios:

### A. Fresh dataset on another machine (different Claude Code account)

```bash
git clone <repo-url> && cd devboy-tools
curl -LsSf https://astral.sh/uv/install.sh | sh
# then follow the canonical sequence above
```

### B. Move this machine's corpus to another

```bash
# Source machine
rsync -avz --exclude='*.tmp' ~/.claude/projects/ target:~/.claude/projects/
# Target machine: clone repo, run canonical sequence
```

### C. Copy pre-computed parquets (no recompute)

```bash
# Source
tar czf /tmp/claude_analysis.tgz -C /tmp claude_analysis/
scp /tmp/claude_analysis.tgz target:/tmp/
# Target
tar xzf /tmp/claude_analysis.tgz -C /tmp/
# Now DuckDB queries work immediately; no uv / git needed
```

## Anonymization guarantees (never emitted)

- Full user-turn text (only character counts + intent labels)
- Session UUIDs (aggregates or stable hash prefixes only)
- Raw file paths from codebases (only extensions, depths, token counts)
- MCP project slugs (hashed via `anonymize_tool_name` in analyze_sessions.py)
- API keys, host IPs, user names, emails

Before copying any derived CSV/parquet into `docs/research/data/` (the
committed public directory), run the pre-publish checklist from
`docs/research/benchmarks/paper1/anonymization_rules.md`.

## When to rerun

- **Step 1 only** — if you just want updated numbers with no feature changes
  (new sessions since last run). Steps 2-11 are fast; running everything
  from 1 takes ~15 min and guarantees consistency.
- **All steps** — after editing a script (e.g. fixing the `classify_file`
  bug); upstream outputs propagate.
- **Step 8 selectively** — re-classify only new bash events by using the
  `--resume` flag so prior classifications are kept.

## Related docs

- `docs/research/scripts/README.md` — DAG + anonymization rules
- `docs/research/paper-1-trimtree.md` — research context and claims
- `docs/research/benchmarks/paper1/TASK.md` (local-only, gitignored) —
  public benchmark continuation plan
