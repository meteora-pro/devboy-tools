# Skills

`devboy-tools` is not only a tool bundle — it also ships a catalogue of **skills**: small Markdown recipes that tell an AI agent how to use those tools to accomplish common tasks. Skills are the second half of the "configurable tool bundle" story (see [ADR-012](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-012-skills-subsystem.md)).

A skill is a single `SKILL.md` file with a short YAML frontmatter and a Markdown body. Agents that honour the Agent Skills Standard pick them up from a well-known directory; the CLI puts them there for you.

## Why skills

Tools are verbs — `get_issues`, `create_merge_request`, `send_message`. Skills are short recipes that chain verbs into procedures: "to review a merge request, first fetch the diff, then look up open discussions, then post inline comments with this checklist". Without skills, every agent learns those patterns from scratch on every new project. With skills, the pattern ships next to the tools it calls and every agent picks it up for free.

Skills in `devboy-tools` follow a few conventions:

- **English only** — no locale-specific copy in the baseline catalogue.
- **CLI-first tool invocation** — skills call `devboy tools call <name>` rather than referring to a specific MCP server. That keeps recipes portable across agents and transports.
- **Short bodies** — roughly one page each. A skill is a recipe, not a framework.
- **Minimal frontmatter** — `name`, `description`, `category`, `version` are required; `compatibility`, `activation`, `tools` are recommended; unknown fields are preserved.

## The shipped catalogue

The baseline catalogue is compiled into the binary and falls into six categories. Every skill is listed with `devboy skills list`; every skill's full body is printed with `devboy skills show <name>`.

| Category | What the skills cover |
|----------|-----------------------|
| `self-bootstrap` | Configure and repair `devboy-tools` itself. |
| `issue-tracking` | Fetch, create, update, link, and solve issues across GitHub / GitLab / ClickUp / Jira. |
| `code-review` | Review a merge request, apply reviewer feedback, dry-run a review pass of your own MR. |
| `self-feedback` | Run and verify a command, produce a daily report, do a retrospective over the last N days, extract the lesson from a fix. |
| `meeting-notes` | Search meeting notes, fetch a transcript, turn action items into issues. |
| `messenger` | Search chat history, summarise a channel over a time window, send a structured notification. |

## Quick start

Install the self-bootstrap skills into the current project:

```bash
devboy skills list --category self-bootstrap
devboy skills install --category self-bootstrap
```

Install every shipped skill globally so Claude Code / Codex / Cursor / Kimi pick them up regardless of which repository you're in:

```bash
devboy skills install --all --agent all
```

Preview a change without touching disk:

```bash
devboy skills install devboy-review-mr --agent claude --dry-run
```

Upgrade previously-installed skills after a `devboy upgrade` — with no arguments the subcommand upgrades every skill recorded in the local manifest; pass explicit names to scope it to a subset:

```bash
devboy skills upgrade                       # every installed skill
devboy skills upgrade devboy-review-mr      # just one
```

## Where skills go

Install targets follow the rules in [ADR-013](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-013-skills-install-targets.md):

- **Default** — repo-local at `<repo>/.agents/skills/<skill>/`. Mirrors the `.devboy.toml` convention.
- `--global` — `~/.agents/skills/<skill>/`.
- `--agent claude|codex|cursor|kimi` — that agent's conventional path (e.g. `~/.claude/skills/`).
- `--agent all` — every detected agent, plus the vendor-neutral `~/.agents/skills/`.
- `--local` in combination with `--agent X` puts the install inside the repo instead of the home directory.

If you run `devboy skills install` outside a git repository and without `--global` or `--agent`, the command fails with a clear error listing the flags you might have meant — no silent fallbacks.

## Upgrade safety

Every install target keeps a `.manifest.json` recording the SHA256 of each installed file. When you `upgrade`:

- **Unchanged** files (same hash as the current shipped version) are skipped silently.
- **Historical-safe** files (hash matches a version we previously shipped) are auto-upgraded to the current version.
- **User-modified** files (hash matches nothing we ever shipped) are left alone with a warning. Pass `--force` to overwrite.

This means you can edit installed skills to taste and later upgrade the rest of the catalogue without losing your edits. See [ADR-014](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-014-skills-lifecycle.md) for the full lifecycle.

## Session traces

Skills in the `self-feedback` category read **session traces** — append-only JSONL files at `<target>/.devboy/sessions/<YYYY-MM-DD>/<skill>/<session_id>/trace.jsonl` that record what another skill did. Each `trace begin` gets its own `<session_id>` subdirectory with a sibling `meta.json`, so concurrent runs of the same skill on the same day never overwrite one another. Any caller can write into that format via the `devboy trace` CLI:

```bash
result=$(devboy trace begin --skill my-skill)
SESSION_DIR=$(echo "$result" | jq -r .session_dir)
SESSION_ID=$(echo "$result" | jq -r .session_id)

devboy trace event --session-dir "$SESSION_DIR" --session-id "$SESSION_ID" \
  --skill my-skill --phase tool_call \
  --payload '{"tool":"get_issues","args":{"limit":20}}'

devboy trace end --session-dir "$SESSION_DIR" --session-id "$SESSION_ID" \
  --skill my-skill --outcome success --summary "pulled 20 issues"
```

A central redaction pass strips known credential shapes and env-var-valued secrets before anything lands on disk. Set `DEVBOY_TRACE_REDACTION=off` to disable it for local debugging only. The format is described in [ADR-015](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-015-skills-session-traces.md).

## Adding your own skill

Skills are plain Markdown. To add one to a project, drop a `SKILL.md` under `<repo>/.agents/skills/<name>/` with a frontmatter block that looks like this:

```yaml
---
name: my-skill
description: One-sentence summary ending with a period.
category: self-bootstrap            # one of the six categories
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "phrase agents can use to trigger me"
tools:
  - get_issues
  - create_merge_request
---

# my-skill

...body goes here, roughly one page...
```

Agents that honour the standard will discover the skill automatically. `devboy skills list` also picks it up if you point a custom source at the directory (coming in a future release — the initial cut only lists the embedded catalogue).

To contribute a skill upstream, open a pull request that adds `skills/<NN-category>/<name>/SKILL.md` in the `devboy-tools` repository.
