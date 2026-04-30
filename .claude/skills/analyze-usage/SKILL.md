---
name: analyze-usage
description: Analyze higher-level patterns in Claude Code usage — biome (whale/shark/.../plankton), archetype (constructor/operator/researcher), growth curves, milestones, idle gaps, subagent pyramids, compact patterns, parallelism. Builds on research-pipeline. Use when the user asks about session biomes, productivity profiles, "what kind of work" was done, when a session became a whale, what shaped a long-running project, or wants to see drill-down view of a specific session.
allowed-tools: Bash, Read, Write, Edit, Glob, Grep
---

# analyze-usage

Higher-level usage analytics on top of `research-pipeline`. Extracts **patterns**
that the existing pipeline does not surface directly: biome class, archetype,
growth curves, milestones, gaps, subagent pyramids, compact patterns, and
burst-level RW classes. Output is a pair of parquet bundles plus an
agent-written markdown report.

## When to use

Trigger on any of:

- "biome", "whale", "shark", "creature", "плактон", "кит", "акула"
- "archetype", "constructor", "operator", "researcher"
- "growth", "milestone", "когда сессия стала", "rate of growth"
- "subagent pyramid", "subagent stats", "сабагенты"
- "compact pattern", "context window over time", "автокомпакты"
- "idle gaps", "should I split this session", "split point"
- "drill into session", "view session timeline" + a session id

If the user only wants the base research dataset (sessions, turns, tool_calls,
loops, correlations), use `research-pipeline` instead. This skill **extends**
that dataset.

## Two-layer output

```
outputs/
  raw/        non-anonymized parquet — owner only
  anon/       anonymized parquet — shareable bundle
  reports/    markdown reports written by the agent
```

- `raw/` keeps original session UUIDs, project paths, file paths, branch
  names, and prompt token frequencies. Owner-only; never share without
  re-running the anonymizer.
- `anon/` follows the same anonymity contract as Papers 1-3: hashed session
  ids (s0001 / a0001), hashed project slugs (p7d8fb1), file ext only, no raw
  paths or branch names, prompt tokens replaced by 6-char hashes (sidecar
  dictionary stays in `raw/`).
- `reports/` is markdown the agent generates from the parquet tables. Reports
  may include real session names and quotes if the user owns the data.

## Tier model

- **Tier 1 — always-on (no API key)**: pure Python extractors using regex
  and statistics. Lives in `scripts/extract_*.py`. Reuses `lib/` helpers.
- **Tier 2 — agent-side LLM**: when the skill is active, the agent itself
  reads parquet + raw jsonl and adds the narrative layer (session naming,
  phase detection, pattern interpretation, drill-down quotes). No external
  LLM API call required.

If you (the agent) need a metric that doesn't exist yet, **write a new
`extract_<name>.py`**, wire it into `pipeline.py`, run it. The pipeline is
designed to extend without rewrite.

## Run

```bash
# Full pipeline (assumes research-pipeline has populated /tmp/claude_analysis/)
uv run .claude/skills/analyze-usage/scripts/pipeline.py \
  --since 2026-04-01 \
  --outputs .claude/skills/analyze-usage/outputs/

# Only specific extractors
uv run .claude/skills/analyze-usage/scripts/extract_biome.py
uv run .claude/skills/analyze-usage/scripts/extract_growth.py

# Drill into one session (requires raw jsonl, owner-only)
uv run .claude/skills/analyze-usage/scripts/view_session.py 2c052d83
```

The pipeline expects `~/.claude/projects/*.jsonl` to be present. If
`/tmp/claude_analysis/sessions.parquet` already exists from `research-pipeline`,
the skill reuses it; otherwise it parses jsonl directly.

## Available extractors

| Script                          | Output (raw + anon)              | What it computes |
|---------------------------------|----------------------------------|------------------|
| `extract_biome.py`              | `biome.parquet`                  | Whale/Shark/Dolphin/Fish/Shrimp/Plankton class per session |
| `extract_archetype.py`          | `archetype.parquet`              | Constructor/Operator/Researcher classification |
| `extract_growth.py`             | `growth.parquet`, `milestones.parquet` | Daily rates, CV, Gini, acceleration; burst-index when each milestone was reached |
| `extract_outputs.py`            | `outputs.parquet`                | Commits, pushes, PR/MR, issues, files, branches, TODOs |
| `extract_subagents.py`          | `subagent_pyramid.parquet`       | Per-parent distribution of subagent sizes |
| `extract_compacts.py`           | `compact_pattern.parquet`        | Per-session compact frequency, context size before each |
| `extract_gaps.py`               | `idle_gaps.parquet`              | Gaps >8h with suggested split points |
| `extract_topics.py`             | `topic_signatures.parquet`       | BoW per session, hashed for anon bundle |
| `extract_burst_classes.py`      | `burst_classes.parquet`          | pure_qa / read_only / bash_only / mixed / write_heavy / write_only counts per session |
| `extract_parallelism.py`        | `parallelism.parquet`            | Per-minute concurrent session counts, hour-of-day map |

| Other tool                      | Purpose |
|---------------------------------|---------|
| `pipeline.py`                   | Orchestrator — runs all extractors, parallel where safe |
| `view_session.py SID`           | Drill-down view of one session (raw jsonl required) |
| `pattern_detector.py`           | Heuristic auto-detection (needs-split, concentrated, constructor) |
| `report_generator.py`           | Markdown report skeleton from parquet tables |

## Library helpers

`lib/` holds the shared building blocks. Extractors should import from there
rather than duplicate logic.

| Module             | Provides |
|--------------------|----------|
| `lib/parsers.py`   | `parse_session(jsonl_path)`, `find_session_files(root)`, `find_subagents(parent_dir, sid)`, `build_bursts(events)`, `split_chain(cmd)` |
| `lib/classifiers.py` | `classify_bash_part(cmd)` → read/write/build/test/env/neutral; `classify_burst(b)` → 7 burst classes; `biome_of(real_prompts)` → emoji + name; `archetype_of(stats)` → constructor/operator/researcher |
| `lib/stats.py`     | `cv(xs)`, `gini(xs)`, `shannon_entropy(counter)`, `milestone_at(cum_R)`, `acceleration(daily_xs)` |
| `lib/anonymize.py` | `hash_path(p)`, `hash_token(w)`, `anonymize_df(df, schema)`, `write_token_sidecar(dict, raw_dir)` |

## Anonymization contract

When emitting to `outputs/anon/`:

- `session_id` → sequential `s0001` (main) or `a0001` (subagent)
- `project_path` → `p<6-hex>` SHA-1 truncation
- `file_path` → drop, keep `file_ext` and `file_type` only
- `bash_command` → drop, keep `category` + structural flags only
- `prompt_text` → drop, keep `chars`, `intent_flags`, and `token_hash` BoW
- `branch_name` → drop, keep `branch_hash` only
- `tool_use_id` → drop
- `mcp_server_slug` → hash, keep verb intact
- Any aggregate with N<5 sessions in the bucket → suppress (K=5 threshold)

The `outputs/anon/` directory must be auditable: see `pattern_detector.py
--audit-anon` (planned) which walks every parquet and asserts no
non-hashed strings leak.

## Extending the pipeline

To add a new metric `foo`:

1. Create `scripts/extract_foo.py` reading from `lib/parsers.py` outputs.
2. Emit two parquet files: `outputs/raw/foo.parquet` and the anonymized
   variant in `outputs/anon/foo.parquet` (use `lib/anonymize.py`).
3. Append `extract_foo` to the orchestrator list in `pipeline.py`.
4. Add a row to the `Available extractors` table above.
5. Add an entry to `docs/METRICS.md` describing the schema and intent.

Do not assume only humans extend this — the agent itself adds extractors when
the user asks "what about <new angle>" and the existing tables don't cover it.
