# Plugin naming convention

When the Claude Code / Codex plugin is built, skill names are renamed to drop
the `devboy-` prefix. Plugin namespacing (`/devboy-meteora:<skill>`) already
provides disambiguation, so the prefix becomes redundant.

## Rule

```
plugin_name = source_name.strip_prefix("devboy-").unwrap_or(source_name)
```

Implemented in:

- `scripts/release/build-skills.sh` — applies the rule when generating
  `.claude-plugin/skills/` and `.codex-plugin/skills/`.
- `crates/devboy-skills/src/catalog.rs` — alias resolution so that
  `find("devboy-setup")` and `find("setup")` both return the same skill.

Non-plugin install paths (`devboy onboard`, `devboy skills install`,
manual copy) keep producing the historical filenames in
`~/.claude/skills/`, `~/.codex/skills/`, and `~/.kimi/skills/` to preserve
backward compatibility.

## Full mapping (24 skills)

| Source | Plugin (CC / Codex) | Notes |
|---|---|---|
| `devboy-setup` | `setup` | bootstrap |
| `devboy-repair` | `repair` | bootstrap |
| `devboy-tools-catalog` | `tools-catalog` | bootstrap |
| `devboy-pipeline-tune` | `pipeline-tune` | bootstrap |
| `devboy-create-issue` | `create-issue` | issue tracking |
| `devboy-get-issues` | `get-issues` | issue tracking |
| `devboy-link-issues` | `link-issues` | issue tracking |
| `devboy-solve-issue` | `solve-issue` | issue tracking |
| `devboy-update-issue` | `update-issue` | issue tracking |
| `devboy-fix-review-comments` | `fix-review-comments` | code review |
| `devboy-review-mr` | `review-mr` | code review |
| `devboy-self-review` | `self-review` | code review |
| `analyze-usage` | `analyze-usage` | already without prefix — no rename |
| `devboy-daily-report` | `daily-report` | self-feedback |
| `devboy-knowledge-extract` | `knowledge-extract` | self-feedback |
| `devboy-qa-sweep` | `qa-sweep` | self-feedback |
| `devboy-retro` | `retro` | self-feedback |
| `devboy-run-and-verify` | `run-and-verify` | self-feedback |
| `devboy-meeting-search` | `meeting-search` | meeting notes |
| `devboy-meeting-to-tasks` | `meeting-to-tasks` | meeting notes |
| `devboy-meeting-transcript` | `meeting-transcript` | meeting notes |
| `devboy-chat-search` | `chat-search` | messenger |
| `devboy-chat-summary` | `chat-summary` | messenger |
| `devboy-notify` | `notify` | messenger |

## Adding a new skill

If the skill follows the `devboy-` prefix convention, no extra step is
needed — the rule applies automatically.

If the skill needs an exception (rare), add the override in:

- `scripts/release/build-skills.sh` — manual rename branch
- `crates/devboy-skills/src/catalog.rs` — alias entry

## See also

- ADR-018 §3 — skill naming inside the plugin
- ADR-014 — skills lifecycle (the `history.json` SHA tracking is by source name)
