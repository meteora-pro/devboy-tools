# Research pipeline — scripts

Reproducible data-processing for Paper 1 (TrimTree) and the companion papers.
Each script is a standalone **uv single-file** — just `uv run <script>`.

Scripts read their input from `~/.claude/projects/*/**.jsonl` (the standard
Claude Code session log location) and write aggregates into
`/tmp/claude_analysis/` by default. **Nothing is hard-coded to any private
host, user, or project.** All API keys resolve via environment variables.

---

## Pipeline order

```
  ~/.claude/projects/*.jsonl
          │
          ▼
   analyze_sessions.py   ← parses JSONL, anonymizes tool / project names,
          │                 emits sessions / turns / tool_calls / bash_commands
          │                 / human_turns / compactions / meta_events parquets
          │
          ├── detect_workflow_patterns.py         (derives workflow labels)
          ├── extract_loops.py                    (loop-level aggregation)
          ├── enrich_loops.py                     (cost / cache / trigger / success)
          ├── extract_loop_list_events.py         (MCP list-tool gold events)
          ├── extract_bash_list_events.py         (Bash file-search gold events)
          └── compute_session_features.py         (session-level feature roll-up)
                  │
                  ├── mine_correlations.py
                  ├── partial_correlations.py
                  ├── within_bucket_correlations.py
                  └── analyze_features.py         (human-readable tables)

  ── LLM-assisted classification (optional, uses z.ai GLM-4.6 via Anthropic-compatible endpoint) ──
   llm_classify_sessions.py        (classify sessions: intent, success, domain)
   llm_classify_bash_events.py     (classify 4k Bash gold events: category, use_case, priority signal)

  ── Sampling for manual review ──
   sample_bash_gold_cases.py       (curates N events with enough context)
   select_diverse_bash_events.py   (stratified sample across buckets)
   export_llm_review.py            (export classifications for audit)

  ── Session linking ──
   link_sessions.py                (chains parent → subagent sessions)
```

---

## Running the pipeline

```bash
# 0. one-time setup: copy env template for LLM classification
cat > /tmp/claude_analysis/.env.zai <<EOF
ZAI_API_KEY=your_zai_coding_plan_key
ZAI_BASE_URL=https://api.z.ai/api/anthropic
ZAI_MODEL=glm-4.6
EOF

# 1. core ETL — parses ~/.claude/projects JSONL, emits /tmp/claude_analysis/*.parquet
uv run docs/research/scripts/analyze_sessions.py

# 2. workflow patterns + session-level features
uv run docs/research/scripts/detect_workflow_patterns.py
uv run docs/research/scripts/compute_session_features.py

# 3. loop-level pipeline (Paper 1 core)
uv run docs/research/scripts/extract_loops.py
uv run docs/research/scripts/enrich_loops.py

# 4. gold-selection extraction (MCP + Bash)
uv run docs/research/scripts/extract_loop_list_events.py
uv run docs/research/scripts/extract_bash_list_events.py

# 5. LLM classification of Bash gold events (~20 min on z.ai coding endpoint)
uv run docs/research/scripts/llm_classify_bash_events.py \
  --concurrency 10 --checkpoint-every 25

# 6. correlation analyses
uv run docs/research/scripts/mine_correlations.py
uv run docs/research/scripts/partial_correlations.py
uv run docs/research/scripts/within_bucket_correlations.py
```

All outputs land in `/tmp/claude_analysis/*.parquet`. That directory is ephemeral;
re-run the pipeline from step 1 after a reboot.

---

## Anonymization guarantees

The pipeline never emits:
- Full text of user turns (only character counts + intent labels)
- Session UUIDs (only hash prefixes or sequential numbering within aggregates)
- Raw file paths from user's codebases (only extensions, depths, token counts)
- MCP project slugs (hashed by `anonymize_tool_name()` in `analyze_sessions.py`)
- API keys, host IPs, user names, emails

What IS emitted:
- Anonymized aggregates (counts, means, distributions)
- Workflow labels and categories
- Token / cost / latency numbers per session / loop
- Tool-call verb histograms

Before committing any derived CSV/parquet to `docs/research/data/`, run the
pre-publish checklist from `docs/research/benchmarks/paper1/anonymization_rules.md`.

---

## Requirements

- Python ≥ 3.10 with `uv` installed (inline dependency blocks in each script)
- DuckDB, pyarrow, pandas, httpx (pulled automatically by `uv run`)
- For LLM classification: z.ai coding plan API key (Anthropic-compatible
  endpoint) OR Anthropic API key — both configurable via `.env.zai` or env

---

## Provenance

These scripts were iteratively developed against a personal Claude Code log
corpus while preparing Paper 1. Every script is designed to run against
**any** `~/.claude/projects/*.jsonl` — not tied to a specific user. Results in
`paper-1-trimtree.md` come from running this pipeline; other researchers
can reproduce the numbers by running it on their own logs.

See `docs/research/paper-1-trimtree.md` for the research context and
`docs/research/benchmarks/paper1/TASK.md` (local-only) for the public-benchmark
continuation.
