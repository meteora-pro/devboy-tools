# Glossary — analyze-usage concepts

Each concept links to the source file where it's computed. If a definition
disagrees with the code, the code wins — open an issue.

---

## 🌊 Biome — session size class

A session's size class derived from its real-human prompt count.

| Biome | Real prompts | Emoji |
|-------|-------------:|:-----:|
| Plankton | 0–2 | 🦠 |
| Shrimp | 3–9 | 🦐 |
| Fish | 10–29 | 🐟 |
| Dolphin | 30–99 | 🐬 |
| Shark | 100–499 | 🦈 |
| Whale | ≥ 500 | 🐋 |

Real-human prompt = a `type=user` event whose first text block is **not**
prefixed by a system marker (`<command-name>`, `<system-reminder>`,
`<user-prompt-submit-hook>`, `<task-notification>`, `Caveat:`,
`<local-command-stdout>`, `[Request interrupted by user`, or
`This session is being continued from a previous conversation`).

**Code:**
- [`lib/classifiers.py:biome_of`](lib/classifiers.py)
- [`lib/parsers.py:_classify_user_kind`](lib/parsers.py)
- [`scripts/extract_biome.py`](scripts/extract_biome.py)

---

## 🎭 Archetype — what the session does

8-class classification (Variant B 45/25 thresholds) based on Edit / Bash /
Read tool-call shares.

| Archetype | Rule | Emoji |
|-----------|------|:-----:|
| Constructor | Edit ≥ 45% | 🏗 |
| Operator    | Bash ≥ 45% | ⚙️ |
| Researcher  | Read ≥ 45% | 🔬 |
| Polymath    | All three ≥ 25% | 🌐 |
| Builder     | Edit ≥ 25% AND Bash ≥ 25% (and not Polymath) | 🛠 |
| Scholar     | Edit ≥ 25% AND Read ≥ 25% (and not Polymath) | 📝 |
| Inspector   | Bash ≥ 25% AND Read ≥ 25% (and not Polymath) | 🔍 |
| Discusser   | Total tools < 5, or no rule matches | 💬 |

**Code:**
- [`lib/classifiers.py:archetype_of`](lib/classifiers.py)
- [`scripts/extract_archetype.py`](scripts/extract_archetype.py)

History: started with Constructor / Operator / Researcher; expanded after
Cycle 7 of the hypothesis loop revealed `Mixed` archetype was a catch-all.

---

## 🎵 Rhythm — temporal tool-mix shape

Classification of the dominant-tool sequence over 10 windows.

| Rhythm | Rule | Emoji |
|--------|------|:-----:|
| Mono   | Top tool dominates ≥ 80% of windows | 🎼 |
| Phased | ≤ 4 transitions AND ≤ 3 unique tops | 📊 |
| Mixed  | Anything else | 🎲 |
| Chaos  | ≥ 6 transitions AND ≥ 4 unique tops | 🌪 |
| —      | Fewer than 10 events total | · |

The "tool" label uses extended palette: `Edit`, `Read`, `Bash:git`,
`Bash:test`, `Bash:build`, `Bash:run`, `Bash:device`, `Bash:deploy`,
`Bash:format`, `Bash:net`, `Bash:playwright_cli`, `Bash:shell`,
`Bash:other`. Without sub-categories Chaos is impossible to reach with
only 3 distinct tool families — see Cycle 20 → Cycle 21 fix.

**Code:**
- [`lib/classifiers.py:rhythm_of`](lib/classifiers.py)
- [`lib/classifiers.py:bash_sub_of`](lib/classifiers.py)

---

## 🎨 Stack — code-base layer

File-path classification into:

| Stack | Examples |
|-------|----------|
| frontend  | `*.tsx`, `*.css`, `apps/dashboard-ui/`, `*-web/` |
| backend   | `*.rs`, `*.py`, `*.go`, `*.sql`, `apps/*-api/`, `services/*` |
| infra     | `*.tf`, `Dockerfile`, `*.sh`, `/k8s/`, `/charts/`, `deckhouse` |
| docs      | `*.md`, `*.mdx`, `*.rst` |
| config    | `*.yaml` (non-K8s), `*.json`, `*.toml` |
| other     | lockfiles, binaries, unknown |

**Code:** [`lib/classifiers.py:stack_of`](lib/classifiers.py)

---

## 🔧 Bash subcategory — what the shell command does

Single-command classification:

| Sub | Examples |
|-----|----------|
| git    | `git ...`, `gh ...`, `glab ...` |
| test   | `cargo test`, `pytest`, `jest`, `playwright` |
| build  | `cargo build`, `cargo check`, `tsc`, `webpack` |
| run    | `cargo run`, `npm run`, `pnpm dev`, `python ...`, `node ...` |
| device | `adb`, `simctl`, `fastboot` |
| deploy | `kubectl`, `helm`, `terraform`, `werf`, `docker run/build/compose` |
| format | `prettier`, `rustfmt`, `eslint`, `ruff`, `clippy` |
| net    | `curl`, `wget`, `ping`, `nc` |
| playwright_cli | `playwright-cli ...` (manual UI testing tool) |
| shell  | `cd`, `ls`, `cat`, `mv`, `cp`, `find`, `grep`, … |
| other  | everything else |

**Code:** [`lib/classifiers.py:bash_sub_of`](lib/classifiers.py)

---

## 🚀 DORA — DevOps Research and Assessment metrics

Adapted from Forsgren et al. *Accelerate*. Computed from git commit
messages parsed out of bash command logs.

### Conventional commit types

`feat`, `fix`, `refactor`, `chore`, `docs`, `test`, `ci`, `perf`, `style`,
`build`, `revert`. Anything else → `uncategorized`.

### True CFR (Change Failure Rate)

```
true_CFR = prod_fix / feat
prod_fix = total_fix − review_fix
review_fix = fix-commit whose message contains "review", "комментар",
             "copilot", "artslob", "cr ", "nitpick", "address review",
             "по ревью", "after review"
```

Levels (DORA 2024):
- 🟢 Elite: < 0.30
- 🟡 High: 0.30–1.0
- 🟠 Medium: 1.0–3.0
- 🔴 Low: > 3.0

### Lead time (proxy)

Time from `git checkout -b feat/...` to first `git push` of that branch.

### Deploy frequency (proxy)

Count of `git push` events per day.

**Code:**
- [`lib/classifiers.py:commit_type_of`, `is_review_fix`](lib/classifiers.py)
- [`scripts/extract_outputs.py`](scripts/extract_outputs.py)
- [`scripts/period_report.py`](scripts/period_report.py) (header `🚀 DORA`)

---

## ⚡ Friction — context cost markers

| Concept | Definition | Source |
|---------|-----------|--------|
| **Compact** | `isCompactSummary: true` event in jsonl. Indicates Claude condensed context to fit the window. | [`lib/parsers.py:_is_compact`](lib/parsers.py), [`extract_compacts.py`](scripts/extract_compacts.py) |
| **Pivot** | A user message starting with `[Request interrupted by user`. User killed the agent mid-action. | [`scripts/period_report.py:parse_all`](scripts/period_report.py) |
| **Subagent spawn** | A `Task` or `Agent` tool_use call. | [`extract_subagents.py`](scripts/extract_subagents.py) |
| **Idle gap** | Wallclock gap > 8h between consecutive bursts. Suggested split point. | [`extract_gaps.py`](scripts/extract_gaps.py) |

---

## ⏱️ Wallclock vs Active duration

```
wallclock = last_ts − first_ts
active    = sum of intervals ≤ 30 min between consecutive events
ratio     = active / wallclock  (typically 7–23%)
```

**Code:** [`scripts/period_report.py:active_minutes`](scripts/period_report.py)

---

## 🌱 Burst classes — per-prompt work shape

Each burst (one human prompt + the agent's response) is classified into:

| Class | Rule |
|-------|------|
| pure_qa | 0 tool calls |
| read_only | only Read/Grep/Glob/ToolSearch |
| write_only | only Edit/MultiEdit/Write |
| bash_only | only Bash |
| chain_run | Bash with chained commands (≥3 split parts) |
| write_heavy | ≥ 50% Edit/Write of all tools |
| mixed | everything else |

**Code:** [`scripts/extract_burst_classes.py`](scripts/extract_burst_classes.py)

---

## 📈 Growth, milestones, distribution stats

| Concept | Definition |
|---------|-----------|
| `cv` | Coefficient of variation = std/mean |
| `gini` | Gini coefficient (0 = uniform, 1 = max concentration) |
| `shannon_entropy` | Diversity in bits (`log₂`) |
| `milestone_at(thr)` | Burst index where cumulative R first reaches `thr` |
| `acceleration` | Slope(2nd half) − slope(1st half) of daily series |
| `power_law_fit` | Log-log linear fit `y ≈ a · xᵏ`. Returns `slope`, `intercept_log`, `scale=a`, `r`, `n` |

**Code:** [`lib/stats.py`](lib/stats.py)

Empirical findings from the hypothesis loop (Cycle 16, 34, 35):
- `R ∝ rank^3.4` across biomes (r = 0.969)
- `+LOC ∝ rank^5.7` (r = 0.982)
- `compacts ≈ 0.23 × R^0.71` (r = 0.984, sublinear)
- `pivots ≈ 0.031 × R^1.21` (r = 0.999, slightly superlinear)

---

## 🔐 Anonymization

Anonymized parquet under `outputs/anon/` follows this contract:

| Raw field | Anon replacement |
|-----------|-----------------|
| session_id (UUID) | `s0001` (main) / `a0001` (subagent) — sequential |
| project_path | `p<6hex>` SHA-1 truncation |
| file_path | dropped, only `file_ext` + stack class kept |
| bash_command | dropped, only `bash_sub` + structural flags kept |
| prompt_text | dropped, only `chars` + `intent_flags` + `token_hash` BoW |
| branch_name | dropped, only `b<6hex>` kept |
| tool_use_id | dropped |
| MCP slug `mcp__<server>__<verb>` | `mcp__<6hex>__<verb>` (server hashed, verb intact) |
| Aggregate with N < 5 sessions | suppressed (K=5 threshold) |

The reverse-lookup dictionary lives in `outputs/raw/_token_dict.json` and
never gets copied to `anon/`. Audit with `analyze-usage audit`.

**Code:**
- [`lib/anonymize.py`](lib/anonymize.py)
- [`scripts/pattern_detector.py:audit_anon`](scripts/pattern_detector.py)

---

## 📊 Three-tier output

```
outputs/
├── raw/      Tier 1: pure-stat parquet, original identifiers
├── anon/     Tier 1 anonymized: same schema, hashed identifiers
└── llm/      Tier 2: LLM-augmented features (session names, narratives)
```

| Tier | Reproducible offline? | Anonymizable? | Code path |
|------|----------------------|---------------|-----------|
| 1 (stats) | ✓ | ✓ → `anon/` | `scripts/extract_*.py` |
| 2 (LLM)   | ✗ | partial      | `scripts/extract_llm_*.py` (queues; agent fills) |

**Code:**
- [`lib/io.py:outputs_dirs_v2`](lib/io.py)
- [`scripts/extract_llm_session_names.py`](scripts/extract_llm_session_names.py) — Tier 2 example

---

## Patterns detected

Heuristic flags from `pattern_detector.py`:

| Flag | Rule |
|------|------|
| `needs_split` | ≥ 2 idle gaps in the session |
| `pure_constructor` | archetype = Constructor AND rhythm = Phased |
| `feature_with_debug` | archetype = Inspector AND compacts ≥ 2 |
| `pipeline_rescue` | compacts ≥ 3 AND lines_added < 500 AND commits > 0 |
| `ghost_session` | biome = Plankton AND total_tools = 0 |

**Code:** [`scripts/pattern_detector.py`](scripts/pattern_detector.py)

---

## See also

- **[SKILL.md](SKILL.md)** — full architecture, extractor table, extension guide
- **[README.md](README.md)** — quick-start for users (standalone CLI + agent flow)
- **Test suite** — [`tests/run_all.sh`](tests/run_all.sh) — 28 unit + smoke tests
