# analyze-usage

Higher-level analytics on top of Claude Code session logs.
Generates **graphic monthly/weekly digests** with metaphors (aquarium of
biomes, archetype bars, DORA radar) and **per-session parquet bundles** for
deeper analysis.

Two ways to use it:

1. **Standalone CLI** — run `bin/analyze-usage` directly from your terminal,
   no agent needed.
2. **Through an agent** — Claude (or any agent that loads `.claude/skills/`)
   triggers it on phrases like *"weekly digest"*, *"DORA"*, *"когда сессия
   стала китом"*, *"drill into session 2c052d83"*.

---

## Install (without cloning the whole repo)

```bash
# Sparse-checkout just this skill into ~/.claude/skills/analyze-usage/:
curl -sSL https://raw.githubusercontent.com/meteora-pro/devboy-tools/main/.claude/skills/analyze-usage/scripts/install.sh | bash

# …then add to PATH:
echo 'export PATH="$HOME/.claude/skills/analyze-usage/bin:$PATH"' >> ~/.zshrc
exec zsh
analyze-usage --help
```

If you already cloned `devboy-tools`, the skill is already at
`.claude/skills/analyze-usage/` — skip the install step.

Requirements: `uv` (https://docs.astral.sh/uv/) for running Python scripts,
`git` for the install.

## Quick start (standalone)

```bash
# Add to PATH (once):
export PATH="$PWD/.claude/skills/analyze-usage/bin:$PATH"

# Last week, html, auto-open in browser:
analyze-usage period --from 2026-04-23 --to 2026-04-30 --format html --open

# Whole quarter, all 3 formats (text + markdown + html) into a directory:
analyze-usage period --from 2026-02-01 --to 2026-04-30 --period both \
    --format all --out-dir /tmp/q1_2026

# Drill into one session:
analyze-usage session 2c052d83

# Help:
analyze-usage --help
```

---

## Output formats

| Format     | When to use                                            |
|------------|--------------------------------------------------------|
| `text`     | Quick glance in terminal (default if `--out` omitted)  |
| `markdown` | Paste into Slack / GitHub / docs                       |
| `html`     | Pretty page with biome colors, progress bars, tables   |
| `all`      | Generates `report.{txt,md,html}` in `--out-dir`        |

`--open` auto-opens the HTML in your default browser (only with
`--format html` or `--format all`).

---

## Output directories

```
.claude/skills/analyze-usage/outputs/
├── raw/      ← Tier 1 stat extraction with original UUIDs/paths/branches.
│              Owner-only. Keeps reverse dictionaries (_token_dict.json,
│              _sid_main.json, _sid_sub.json).
├── anon/     ← Tier 1 anonymized version. Sharable. Audit with
│              `analyze-usage audit` after every regeneration.
└── llm/      ← Tier 2 LLM-augmented features (session names, narratives).
              Generated when the agent runs the skill. Empty until then.
```

Run `analyze-usage pipeline` to populate `raw/` + `anon/`. ~4–5 min for a
full corpus, sequential.

---

## Three-tier model

| Tier | What                                | Reproducible offline? | Anonymizable? |
|------|-------------------------------------|----------------------|---------------|
| **0**| `tests/` — unit + smoke tests        | ✓ (deterministic)    | n/a           |
| **1**| `scripts/extract_*.py` — pure stat   | ✓                    | ✓ → `anon/`   |
| **2**| LLM-augmented features (narratives) | ✗ (needs agent)      | partial       |

Tier 1 must remain deterministic — same input → same parquet.
Tier 2 is enriched by the agent (Claude reads `outputs/llm/_queue.jsonl`,
summarizes each session, writes narrative back to `outputs/llm/*.parquet`).

---

## Subcommands

```
analyze-usage period      Graphic digest (monthly/weekly, text/md/html/all)
analyze-usage pipeline    Run all 10 extractors → parquet
analyze-usage session SID Drill-down on one session
analyze-usage audit       Verify outputs/anon/ has no leaks
analyze-usage patterns    Heuristic flags from raw parquet
analyze-usage report      Markdown report from existing parquet
analyze-usage test        Run all tests
```

---

## Environment

| Variable                | Default                  | Purpose                            |
|-------------------------|--------------------------|------------------------------------|
| `CLAUDE_PROJECTS_ROOT`  | `~/.claude/projects`     | Where session jsonls live          |

Set `CLAUDE_PROJECTS_ROOT` to point at a different folder (sandbox testing,
analyzing someone else's logs after they share with you, etc).

---

## Through the agent

When you ask Claude things like:

- "сделай отчёт за последний месяц"
- "weekly digest with html"
- "drill into session 2c052d83"
- "какие у меня DORA метрики?"
- "посмотри на наши биомы"

…the skill loads automatically. Claude will run `period_report.py` with
the right args, parse the parquet, and produce a narrative. For long-form
analysis (Tier 2) Claude consumes `outputs/llm/_queue.jsonl`, reads the
relevant jsonls, and appends narrative summaries to `outputs/llm/*.parquet`.

---

## Architecture

See [SKILL.md](./SKILL.md) for the full table of extractors, library API,
anonymization contract, and extension guide.
