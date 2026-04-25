---
name: devboy-pipeline-tune
description: Analyse the user's Claude Code (or other agent) logs and auto-configure the layered-pipeline compression profiles for their tools, models, and workflow.
category: self-bootstrap
version: 1
compatibility: devboy-tools >= 0.19
activation:
  - "tune devboy pipeline"
  - "configure compression"
  - "analyse claude logs"
  - "set up pipeline profiles"
  - "what compression settings should I use"
tools:
  - tune
  - show
  - doctor
---

# devboy-pipeline-tune

Adapt the layered-pipeline (Paper 2 / `crates/plugins/format-pipeline`) to **this** user. The pipeline has four profile axes — tokenizer, LLM, agent/session, data/endpoint — and a horizontal hint policy. Defaults are conservative; this skill mines the user's existing agent logs to pick a tuned profile that matches their actual tool and model mix.

## When to use

- The user has been running Claude Code (or another agent) for a while and asks: *"can we make tool responses cheaper?"* or *"why are my contexts so big?"*.
- A new project is being set up and the user wants pipeline defaults personalised before turning telemetry on.
- The user reports that one specific tool (e.g. `mcp__gitlab__get_issues`) is dominating the context.
- After a major LLM switch (e.g. moving from GLM-4 to Claude Sonnet) — the tokenizer profile changes and so should the encoder choices.

## What the user gets

After running this skill the user has a `~/.config/devboy/pipeline_config.toml` with:

- **`profiles.llm.active`** pinned to their dominant model (if ≥80% share);
- **`profiles.agent.active`** pinned to one of `default` / `file_search_heavy` / `marathon_refactor` based on session length, read-share, and compaction count;
- **`profiles.data.variants`** extended with placeholder entries for every observed `mcp__*` endpoint, ready for them to set `preferred_format`;
- **`hints`** policy left at safe defaults: `schema_explainer` is **off** (confirmed 0 lift in the 2026-04-25 evaluation), `inline_format_hint` is on **only** for local Ollama models.

## Procedure

### 0. Sanity check

```bash
command -v devboy >/dev/null || { echo "install devboy first"; exit 1; }
ls ~/.claude/projects/ >/dev/null 2>&1 || \
  echo "no Claude logs at ~/.claude/projects — pass --input-dir <PATH> instead"
```

### 1. Dry-run analysis (the user reads the proposal first)

```bash
devboy tune from-claude-logs --dry-run
```

The command:

1. Recurses `~/.claude/projects/<project>/*.jsonl` and parses every line.
2. Counts model ids, tool invocations (read-class vs other), `mcp__*` endpoint hits, sessions, and `/compact` events.
3. Prints a summary, the proposed `profiles.llm.active`, `profiles.agent.active`, and the new data-profile variants — without touching disk.

Read the summary aloud to the user before applying:

- **`# events`** — total parsed (more than ~5 000 means a confident fit).
- **`# model distribution`** — verify the dominant model is what they intend to keep using.
- **`# top mcp endpoints`** — these are candidates for per-domain templates.

### 2. Apply

If the user agrees, drop `--dry-run`:

```bash
devboy tune from-claude-logs
```

Output ends with `# wrote → ~/.config/devboy/pipeline_config.toml`. The file is human-readable TOML — encourage the user to commit a project-local copy if they want it under VCS.

### 3. Verify

```bash
devboy tune show | head -80
```

Look for these markers:

- `[profiles.tokenizer]` has all three variants (`anthropic_class`, `openai_o200k`, `ollama_bpe`).
- `[profiles.llm]` has `active = "<their_model>"`.
- `[profiles.agent]` has `active = "<inferred_variant>"`.
- `[hints.types.schema_explainer]` has `enabled = false`.

If the active LLM is not in the variants list, the active value will fall back to `"auto"` — explain to the user that they can hand-add their model:

```toml
[profiles.llm.variants."their-model-name"]
tokenizer = "anthropic_class"   # or openai_o200k / ollama_bpe
prefer_explicit_keys = true
context_window = 100000
max_inline_nested = 128
```

### 4. Per-tool refinement (optional)

For every `mcp__*` endpoint that landed in `profiles.data.variants` without a `preferred_format`, ask the user what shape that tool returns:

| User says | Set `preferred_format` to |
|---|---|
| "list of issues / PRs / records" | `csv_from_md` |
| "log lines or pipeline output" | `pipeline_deep_mckp` |
| "code diff" | `mr_diff_fence` |
| "single configuration object" | `kv` |
| "free text / prose" | leave unset |

Edit the TOML directly and rerun `devboy tune show` to confirm.

### 5. Watch for regressions

After the first session with the new config, compare token usage in `devboy doctor` (or the user's billing dashboard). If the LLM accuracy drops on a specific endpoint, the most likely cause is an over-aggressive `preferred_format` — revert that endpoint to no preference and retry.

## Anti-patterns

- **Don't skip the dry-run on the first run.** The auto-detector is rule-based, not magical; the user should sanity-check the inferred LLM and agent variants before pinning.
- **Don't force `profiles.llm.active = "claude-sonnet-4.6"` if the user's actual dominant model is something else.** The tokenizer profile drives encoder choice; mismatching it will produce token estimates that are wrong by ~2× on Anthropic-class tokenizers.
- **Don't enable `schema_explainer`.** It was confirmed to add 0 percentage points of accuracy lift in the 2026-04-25 evaluation. If a user asks for it, point them at §"Encoder Bug Postmortem" in `paper-2-mckp-format-adaptive.md`.
- **Don't run `from-claude-logs` against a directory containing other tools' logs without `--project`.** The aggregator does not anonymise across project boundaries; mixing projects produces a noisy fit.

## Failure modes

| Symptom | Cause | Fix |
|---|---|---|
| `claude logs directory not found` | `~/.claude/projects` doesn't exist on this machine | Pass `--input-dir <PATH>` to wherever the user keeps their agent logs. |
| `no jsonl events parsed — check the path` | Path exists but contains no `.jsonl`, or the format is not a Claude Code log | Verify with `ls -R <PATH> | head` and ensure files end in `.jsonl`. |
| `profiles.llm.active = "auto"` after the run | Dominant model didn't reach 80% share, or it isn't in the built-in variants | Either accept the auto-resolution, or hand-add the model to `profiles.llm.variants` (see step 3). |
| Wrong `profiles.agent.active` | The classifier saw an atypical session window | Override manually: edit `pipeline_config.toml` and set `profiles.agent.active = "default"` (or whatever fits). The next tune run respects the explicit value. |

## Cross-references

- The full algorithm and validation are in `docs/research/paper-2-mckp-format-adaptive.md` §"Configuration Extensibility".
- Per-axis defaults live in `crates/plugins/format-pipeline/src/adaptive_config.rs`.
- The CLI implementation is `crates/plugins/format-pipeline/src/bin/tune.rs`; subcommand `from-claude-logs`.
