---
name: analyze-usage
description: Analyze higher-level patterns in Claude Code usage. Outputs (a) graphic monthly/weekly digest with metaphors — aquarium of biomes (🐋🦈🐬🐟🦐🦠), archetypes (⚙️🔬🌐🛠📝🔍🏗💬), rhythm, stack palette, DORA radar (CFR + lead time + pushes), friction (compacts/pivots/subagents); (b) per-session parquet bundles for further analysis (biome, archetype, rhythm, growth, milestones, idle gaps, subagent pyramids, compact patterns, parallelism, burst classes, topics). Use when the user asks about weekly/monthly reports, session biomes, productivity profiles, "what kind of work was done", when a session became a whale, DORA metrics, or wants drill-down view of a specific session.
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
- "weekly digest", "monthly report", "отчёт за неделю/месяц/квартал", "графический отчёт", "DORA", "CFR", "lead time" → use `scripts/period_report.py --from --to --period monthly|weekly|both`
- "rhythm", "Mono / Phased / Mixed / Chaos", "ритм сессии", "stack palette", "self-feedback", "TDD ratio", "motivated read" — these come from `lib/classifiers.py` and the corresponding extractors

If the user only wants the base research dataset (sessions, turns, tool_calls,
loops, correlations), use `research-pipeline` instead. This skill **extends**
that dataset.

## Three-tier output

```
outputs/
  raw/        Tier 1 stat parquet, original UUIDs/paths/branches — owner only
  anon/       Tier 1 anonymized — shareable bundle (audit with `--audit-anon`)
  llm/        Tier 2 LLM-augmented features (session names, narratives) — owner only
  reports/    markdown reports written by the agent (Tier 2)
```

- `raw/` keeps original session UUIDs, project paths, file paths, branch
  names, and prompt token frequencies. Owner-only; never share without
  re-running the anonymizer.
- `anon/` follows the anonymization contract below. Hashed session ids
  (`s0001` / `a0001`), hashed project slugs (`p<6hex>`), file ext only,
  no raw paths or branch names, prompt tokens replaced by 6-char hashes
  (reverse dictionary stays in `raw/_token_dict.json`). Auditable via
  `pattern_detector.py --audit-anon`.
- `llm/` holds Tier 2 outputs that depend on the agent (Claude). Tier 1
  extractors stay deterministic; LLM enrichment is layered on top via
  `extract_llm_*.py` extractors that build a queue (`_queue.jsonl`) the
  agent consumes and writes back as parquet. Owner-only by default.
- `reports/` is markdown the agent generates from the parquet tables.
  Reports may include real session names and quotes if the user owns the
  data.

## Tier model

- **Tier 0 — tests**: `tests/run_all.sh` runs 22 unit + smoke tests
  (classifiers, stats, full extractor pipeline on a synthetic fixture).
  Run before shipping a new extractor.
- **Tier 1 — always-on (no API key)**: pure Python extractors using regex
  and statistics. Lives in `scripts/extract_*.py`. Reuses `lib/` helpers.
- **Tier 2 — agent-side LLM**: when the skill is active, the agent itself
  reads parquet + raw jsonl and adds the narrative layer (session naming,
  phase detection, pattern interpretation, drill-down quotes). No external
  LLM API call required.

If you (the agent) need a metric that doesn't exist yet, **write a new
`extract_<name>.py`**, wire it into `pipeline.py`, add a test in
`tests/test_extractors_smoke.py`, run it. The pipeline is designed to
extend without rewrite.

## Run

```bash
# Graphic period digest (monthly + weekly, with метафоры — aquarium, palette, DORA radar)
uv run .claude/skills/analyze-usage/scripts/period_report.py \
  --from 2026-02-01 --to 2026-04-30 --period both

# Just one month at a time
uv run .claude/skills/analyze-usage/scripts/period_report.py \
  --from 2026-04-01 --to 2026-04-30 --period monthly

# Just last week (auto-saves a copy)
uv run .claude/skills/analyze-usage/scripts/period_report.py \
  --from 2026-04-23 --to 2026-04-30 --period weekly --out /tmp/last_week.txt

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

The pipeline expects `~/.claude/projects/*.jsonl` to be present. Override
the location by setting `CLAUDE_PROJECTS_ROOT=/some/dir/projects` (used by
the smoke tests; useful for sandboxed runs on someone else's logs).

If `/tmp/claude_analysis/sessions.parquet` already exists from
`research-pipeline`, the skill reuses it; otherwise it parses jsonl
directly.

## Available extractors

| Script                          | Output (raw + anon)              | What it computes |
|---------------------------------|----------------------------------|------------------|
| `extract_biome.py`              | `biome.parquet`                  | Whale / Shark / Dolphin / Fish / Shrimp / Plankton per session (main + subagents) |
| `extract_archetype.py`          | `archetype.parquet`              | 8-class archetype (Variant B 45/25): Constructor / Operator / Researcher / Builder / Scholar / Inspector / Polymath / Discusser, plus rhythm class (Mono / Phased / Mixed / Chaos) |
| `extract_growth.py`             | `growth.parquet`, `milestones.parquet` | Per-session: n_bursts, daily R/LOC, CV, Gini, acceleration_R; milestone burst-index for thresholds 10/30/100/500 |
| `extract_outputs.py`            | `outputs.parquet`                | Commits by type (feat/fix/refactor/chore/docs/test/ci) with review-fix vs prod-fix split, pushes, PR/MR/issue creation, comments, files edited/written, lines added/removed, unique branches/issues |
| `extract_subagents.py`          | `subagent_pyramid.parquet`       | Per-parent distribution of subagent sizes (whale/shark/.../plankton bins) |
| `extract_compacts.py`           | `compact_pattern.parquet`        | Per-session compact frequency, real_prompts before first compact, max context size, compact_per_asst |
| `extract_gaps.py`               | `idle_gaps.parquet`              | Gaps >8h with suggested split points (long-form: one row per gap) |
| `extract_topics.py`             | `topic_signatures.parquet`       | BoW per session (top-20 tokens). raw keeps tokens, anon hashes them; reverse dictionary in `raw/_token_dict.json` |
| `extract_burst_classes.py`      | `burst_classes.parquet`          | Per-session counts: pure_qa / read_only / bash_only / write_only / write_heavy / chain_run / mixed |
| `extract_parallelism.py`        | `parallelism.parquet`            | Hour-bin concurrent session counts (date+hour → cnt) |

| Other tool                      | Purpose |
|---------------------------------|---------|
| `period_report.py`              | Graphic monthly/weekly digest with metaphors (aquarium, palette, DORA radar, friction). Reads jsonl directly, no parquet needed. Args: `--from --to --period monthly\|weekly\|both --format text\|markdown\|html\|all [--out FILE \| --out-dir DIR] [--open]`. With `--open` auto-opens HTML in browser. |
| `extract_llm_session_names.py`  | Tier 2 stub: enqueues Whale/Shark/Dolphin sessions for narrative summarization by the agent. Writes `outputs/llm/_queue.jsonl` (and an empty `session_names.parquet` stub). Agent consumes the queue and appends rows. |
| `bin/analyze-usage`             | Standalone CLI wrapper. Subcommands: `period`, `pipeline`, `session SID`, `audit`, `patterns`, `report`, `test`. Add to PATH for non-agent usage. |
| `pipeline.py`                   | Orchestrator — runs all 10 extractors **sequentially** (~4–5 min on a full corpus). Args: `--since --outputs --only <names>` |
| `view_session.py SID`           | Drill-down view of one session by sid prefix (raw jsonl required). Prints biome/archetype/rhythm + first 5 prompts + bash subcategory mix |
| `pattern_detector.py`           | Default: heuristic flags from `raw/` parquets: `needs_split` (≥2 idle gaps), `pure_constructor` (Constructor+Phased), `feature_with_debug` (Inspector+compacts≥2), `pipeline_rescue` (compacts≥3 + LOC<500), `ghost_session` (Plankton+0 tools). With `--audit-anon`: walks `anon/` parquets and asserts no raw UUIDs / abs paths / bash commands / branches / mcp slugs leak — exits 1 on any leak. |
| `report_generator.py`           | Markdown report skeleton from parquet tables → `outputs/reports/<date>.md` |

## Library helpers

`lib/` holds the shared building blocks. Extractors should import from there
rather than duplicate logic.

| Module             | Provides |
|--------------------|----------|
| `lib/parsers.py`   | `find_session_files(root=None)`, `find_subagents(jsonl)`, `parse_session(jsonl, since, until)` → Iterator[Event], `build_bursts(events, sid, project)` → list[Burst], `load_session(jsonl) → ParsedSession`, `split_chain(cmd)`. Honors `CLAUDE_PROJECTS_ROOT` env var. |
| `lib/classifiers.py` | Pure-function classifiers: `biome_of(real_prompts)`, `archetype_of(edit, bash, read)` (Variant B 45/25), `rhythm_of(event_seq)` (Mono / Phased / Mixed / Chaos / "—"), `stack_of(file_path)` (frontend / backend / infra / docs / config / other), `bash_sub_of(cmd)` (git / test / build / run / device / deploy / format / net / playwright_cli / shell / other), `commit_type_of(first_line)`, `is_review_fix(body)`. Plus emoji dicts: `BIOME_EMOJI`, `ARCHETYPE_EMOJI`, `RHYTHM_EMOJI`. |
| `lib/stats.py`     | `cv(xs)`, `gini(xs)`, `shannon_entropy(counter)`, `milestone_at(cum, threshold)`, `acceleration(daily_xs)`, `pearson(xs, ys)`, `percentile(xs, p)`, `median(xs)`, `power_law_fit(xs, ys)` (returns slope/intercept/r/n). |
| `lib/anonymize.py` | `hash_path(s)` → `p<6hex>`, `hash_token(w)`, `hash_branch(name)`, `hash_mcp_slug(name)`, `SidProjector(prefix)` (sequential `s0001` / `a0001` mapping), `file_ext(path)`, `k_anon_filter(rows, group_key)` (drops groups with N<5), `write_token_sidecar(dict, raw_dir)`. |
| `lib/io.py`        | `outputs_dirs(base=None)` → `(raw, anon)`, `outputs_dirs_v2(base=None)` → `(raw, anon, llm)` (auto-mkdir), `write_parquet(rows, path)` (snappy, returns row count, empty marker if no rows). |
| `lib/render.py`    | Period-report renderers in three formats: `render_period_terminal/markdown/html_section(label, agg)`, `render_html_doc(title, sections)`, `render_weekly_table_*`, `render_trends_*`. Pure data → string. Used by `period_report.py`. |

## Tests

`tests/run_all.sh` runs four test suites (~15 seconds total, 28 tests):

| Suite | Coverage | Count |
|-------|----------|-------|
| `tests/test_classifiers.py` | biome thresholds, archetype Variant B (pure / hybrid / too-few-tools), rhythm Mono / Chaos / too-short, stack classification (all extensions), bash sub regex coverage, conventional commit type, review-fix detection | 11 |
| `tests/test_stats.py`       | cv (empty + known), gini (extremes), Shannon entropy, milestone_at, pearson (perfect / zero), percentile / median, power_law_fit (`y=2x²` recovers slope=2), acceleration (const / accel / decel) | 10 |
| `tests/test_extractors_smoke.py` | Builds a synthetic `~/.claude/projects/` fixture with 3 controlled sessions (Operator-Fish, Constructor with feat+fix+review-fix+push, ghost). Sets `CLAUDE_PROJECTS_ROOT`, runs all 10 extractors, asserts schemas + specific values (e.g. session2 must have `feat=1, fix=2, review_fix=1, prod_fix=1, pushes=1, archetype=Constructor`). | 1 (full pipeline) |
| `tests/test_audit_anon.py`  | Builds anon parquets with synthetic content; asserts `pattern_detector.py --audit-anon` returns 0 on clean fixture and 1 on each kind of leak (raw UUID, abs path, raw bash, raw branch in nested list, bad sid format). | 6 |

```bash
bash .claude/skills/analyze-usage/tests/run_all.sh
```

When you add an extractor, add a corresponding assertion to
`test_extractors_smoke.py` so future refactors stay honest.

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

The `outputs/anon/` directory is auditable via `pattern_detector.py
--audit-anon`: it walks every parquet, recursively scans every string-
typed column (including nested lists/dicts), and exits 1 if it finds
raw UUIDs, absolute paths, bash commands, branch names, or un-hashed
MCP slugs. Run after every pipeline regeneration before sharing.

## Extending the pipeline

To add a new metric `foo`:

1. Create `scripts/extract_foo.py` using `lib/parsers.py` to load sessions
   and `lib/classifiers.py` / `lib/stats.py` / `lib/anonymize.py` /
   `lib/io.py` for the rest. Follow the shape of existing extractors
   (CLI: `--since YYYY-MM-DD`, `--outputs DIR`).
2. Emit two parquet files: `outputs/raw/foo.parquet` (full data) and
   `outputs/anon/foo.parquet` (anonymized via `SidProjector` + `hash_path`
   + token hashes).
3. Append `extract_foo` to the `EXTRACTORS` list in `scripts/pipeline.py`.
4. Add a row to the `Available extractors` table above.
5. Add an assertion in `tests/test_extractors_smoke.py` checking the new
   schema + at least one value on the synthetic fixture.
6. Run `bash tests/run_all.sh` — must stay 22+/22+ green.

Do not assume only humans extend this — the agent itself adds extractors
when the user asks "what about <new angle>" and the existing tables don't
cover it.
