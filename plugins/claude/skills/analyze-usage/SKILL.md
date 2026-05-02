---
name: analyze-usage
description: Graphic monthly/weekly digest of Claude Code session patterns — biome aquarium (whale/shark/dolphin/fish/shrimp/plankton), 8-class archetype, rhythm, stack palette, DORA radar (CFR + lead time + pushes), friction (compacts/pivots/subagents). Backend installs on first run via curl.
category: self-feedback
version: 1
compatibility: devboy-tools >= 0.22
activation:
  - "weekly digest"
  - "monthly report"
  - "отчёт за неделю"
  - "отчёт за месяц"
  - "графический отчёт"
  - "DORA"
  - "CFR"
  - "biome"
  - "archetype"
  - "когда сессия стала китом"
  - "drill into session"
tools:
  - trace
---

# analyze-usage

This is a **thin baseline skill** that delegates the heavy lifting to a
sibling Python backend (`bin/analyze-usage` and `lib/`/`scripts/`)
auto-installed on first use into `~/.claude/skills/analyze-usage/`. The
backend reads `~/.claude/projects/*.jsonl` directly — there is nothing
to push, post or upload.

The output is a **graphic period digest** with metaphors:

- 🌊 **Aquarium** of biomes: 🐋 Whale (R≥500) · 🦈 Shark (100-499) ·
  🐬 Dolphin (30-99) · 🐟 Fish (10-29) · 🦐 Shrimp (3-9) · 🦠 Plankton (0-2)
- 🎭 **8 archetypes** (Variant B 45/25 thresholds): 🏗 Constructor /
  ⚙️ Operator / 🔬 Researcher / 🛠 Builder / 📝 Scholar / 🔍 Inspector /
  🌐 Polymath / 💬 Discusser
- 🎵 **Rhythm**: 🎼 Mono · 📊 Phased · 🎲 Mixed · 🌪 Chaos
- 🎨 **Stack palette** (LOC by frontend/backend/infra/docs/config)
- 🚀 **DORA radar**: pushes, PR/MR, feat, fix (review-fix vs prod-fix
  split), True CFR with Elite/High/Medium/Low classification
- ⚡ **Friction**: compacts · pivots · subagent spawns

## When to use

- *"Сделай отчёт за неделю / месяц / квартал"*
- *"Какие у меня DORA метрики?"*, *"что с CFR?"*
- *"Покажи биомы за апрель"*, *"когда сессия стала китом?"*
- *"Drill into session 2c052d83"* — глубокий single-session breakdown
- Quarterly review of how Claude Code time was actually spent

## Procedure

### 1. Ensure backend is installed

The Python backend is **not** embedded in the `devboy` binary (it would
bloat the release). Check whether it exists; if not, fetch it.

```bash
SKILL_DIR="$HOME/.claude/skills/analyze-usage"
if [ ! -x "$SKILL_DIR/bin/analyze-usage" ]; then
    echo "Installing analyze-usage backend (~1MB sparse checkout)..."
    curl -sSL https://raw.githubusercontent.com/meteora-pro/devboy-tools/main/.claude/skills/analyze-usage/scripts/install.sh | bash
fi
```

The installer does a `git sparse-checkout` of `.claude/skills/analyze-usage/`
only — no full repo clone, no Cargo build. Requires `uv` for running
Python (https://docs.astral.sh/uv/).

The installer is idempotent: re-running it just refreshes to the latest
`main` (or pin via `REF=v0.22.0 curl ... | bash`).

### 2. Begin a trace for this report

```bash
result=$(devboy trace begin --skill analyze-usage)
SESSION_DIR=$(echo "$result" | jq -r .session_dir)
SESSION_ID=$(echo "$result" | jq -r .session_id)
```

This skill is traceable — `devboy-retro` will see when it ran and how
long it took.

### 3. Resolve the period

Parse the user's intent into ISO dates and a granularity:

| User says | `--from` | `--to` | `--period` |
|-----------|----------|--------|------------|
| "за прошлую неделю" / "last week" | Monday of previous ISO week | Sunday of previous ISO week | `weekly` |
| "за этот месяц" / "this month" | 1st of current month | today | `monthly` |
| "за апрель" / "April" | first day of April | last day of April | `monthly` |
| "за квартал" / "Q1" / "за 3 месяца" | start month | end month | `both` |
| explicit dates given | use them verbatim | | as requested |

If the user did not specify a format, default to `text` for terminal
output. Prefer `html` with `--open` when the user explicitly asks for a
"graphic" / "красивый" / "в браузере" report.

### 4. Run the period report

```bash
"$SKILL_DIR/bin/analyze-usage" period \
    --from "$FROM" --to "$TO" --period "$PERIOD" \
    --format "$FORMAT" \
    ${OUT:+--out "$OUT"} \
    ${OUT_DIR:+--out-dir "$OUT_DIR"} \
    ${OPEN:+--open}
```

Available `--format` values:

| Format     | Output destination                            |
|------------|----------------------------------------------|
| `text`     | stdout (default)                             |
| `markdown` | use with `--out FILE.md` for Slack/GitHub    |
| `html`     | use with `--out FILE.html` (or auto `/tmp/`) |
| `all`      | writes `.txt`+`.md`+`.html` into `--out-dir` |

For long quarters wallclock-heavy reports, prefer `--out-dir /tmp/<label>`
and `--format all` so the user has all three artefacts at once.

### 5. Drill-down on a specific session (if requested)

If the user names a session ID prefix (e.g. *"посмотри сессию 2c052d83"*):

```bash
"$SKILL_DIR/bin/analyze-usage" session 2c052d83
```

Output: biome / archetype / rhythm / first 5 prompts / bash subcategory mix
+ subagent count.

### 6. (Optional) Generate parquet bundles for further analysis

Only if the user wants raw data for ad-hoc queries:

```bash
"$SKILL_DIR/bin/analyze-usage" pipeline --since 2026-04-01
```

This runs all 10 extractors sequentially (~4-5 minutes on a full
corpus) and writes:

- `outputs/raw/*.parquet`  — original UUIDs/paths/branches (owner-only)
- `outputs/anon/*.parquet` — anonymized version, shareable
- `outputs/llm/*.parquet`  — Tier 2 LLM-augmented (filled by the agent
  in step 7)

After regenerating, **always audit anon before sharing**:

```bash
"$SKILL_DIR/bin/analyze-usage" audit
# exits 1 if any raw UUID / abs path / bash command / branch / mcp slug
# leaks into anon parquet
```

### 7. Tier 2: LLM-augmented narratives (if the user wants them)

Tier 1 (extractors) is pure stat — deterministic, anonymizable. Tier 2
narratives require the agent itself to read the raw jsonl and write
back a summary. The skill never calls an external LLM.

To enqueue Whales/Sharks/Dolphins for narrative:

```bash
"$SKILL_DIR/bin/analyze-usage" llm-queue
# writes outputs/llm/_queue.jsonl with one row per session
```

Then iterate the queue, read each session's jsonl, summarize (≤200
words: name, phases, what was created, pivots, finale), and append rows
to `outputs/llm/session_names.parquet`. The full schema is documented
in `extract_llm_session_names.py`.

### 8. End the trace

```bash
devboy trace end \
    --session-dir "$SESSION_DIR" --session-id "$SESSION_ID" \
    --skill analyze-usage \
    --outcome "$OUTCOME" \
    --summary "<period>: <N> sessions, <M> +LOC, CFR <X>"
```

## Success criteria

- The user receives a digest in the requested format (default: terminal text).
- Numbers reconcile: `+LOC = sum across sessions`, `True CFR = prod_fix /
  feat`, biome counts add up to total session count.
- HTML output renders standalone — no external CSS/JS, opens offline.
- `audit` exits 0 after every `pipeline` regeneration before any sharing.
- Tier 2 narratives, if requested, are appended (not overwriting) to
  `outputs/llm/session_names.parquet`.

## Guardrails

- **Never share `outputs/raw/`** — it contains real UUIDs, file paths,
  branch names, prompt tokens. Only `outputs/anon/` is shareable, and
  only after `audit` passes.
- **Tier 1 must remain deterministic.** If you find yourself wanting an
  LLM call inside an `extract_*.py`, that's a Tier 2 feature — put it in
  `extract_llm_*.py` instead.
- **Don't write to `~/.claude/projects/`** — that's the source data,
  read-only.
- The backend is `uv run`-based. If `uv` is missing, the installer warns
  but still copies the files; the user must install `uv` separately
  (https://docs.astral.sh/uv/).

## Non-goals

- This skill does **not** post the report anywhere. Pipe output into
  `devboy-notify` (category 5) if you need delivery.
- It does **not** call external LLM APIs. Tier 2 enrichment runs
  agent-side.
- It does **not** modify session jsonls. Source data stays untouched.
- It does **not** analyse long-term trends — that's `devboy-retro`.

## Concepts reference

A full glossary (biome thresholds, archetype rules, rhythm classifier,
stack heuristics, bash subcategory regexes, DORA proxy formulas,
scaling-law findings) lives next to the backend at
`~/.claude/skills/analyze-usage/GLOSSARY.md` after install.

The architecture (extractor list, library API, anonymization contract,
extension guide for new metrics) is in
`~/.claude/skills/analyze-usage/SKILL.md`.

## Examples

```bash
# Quick weekly digest:
analyze-usage period --from 2026-04-23 --to 2026-04-30 --period weekly

# Quarter, all formats, open HTML in browser:
analyze-usage period --from 2026-02-01 --to 2026-04-30 --period both \
    --format all --out-dir /tmp/q1_2026 --open

# Drill into one Whale:
analyze-usage session 2c052d83

# Generate parquet bundles + audit:
analyze-usage pipeline --since 2026-04-01
analyze-usage audit
```
