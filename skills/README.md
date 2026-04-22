# Baseline skills

This directory holds the **baseline catalogue of skills** shipped embedded in the `devboy-tools` binary. Skills are procedural recipes on top of the existing tool bundle — "given these tools, here is how to accomplish task X".

The design is described in [`ADR-012`](../docs/architecture/adr/ADR-012-skills-subsystem.md), the install-target rules in [`ADR-013`](../docs/architecture/adr/ADR-013-skills-install-targets.md), and the upgrade / collision lifecycle in [`ADR-014`](../docs/architecture/adr/ADR-014-skills-lifecycle.md).

## Layout

```
skills/
├── 00-self-bootstrap/
├── 01-issue-tracking/
├── 02-code-review/
├── 03-self-feedback/
├── 04-meeting-notes/
└── 05-messenger/
```

Each skill lives in its own directory containing a single `SKILL.md` file with YAML frontmatter and Markdown body:

```
skills/<NN-category>/<skill-name>/SKILL.md
```

## Frontmatter

Required:

- `name` — matches the directory name
- `description` — one-line summary (used by `devboy skills list`)
- `category` — one of `self-bootstrap`, `issue-tracking`, `code-review`, `self-feedback`, `meeting-notes`, `messenger`
- `version` — integer; bump on every change

Recommended:

- `compatibility` — semver range against the tool bundle, e.g. `devboy-tools >= 0.18`
- `activation` — list of trigger phrases for agents that support activation
- `tools` — list of tool names the skill calls (used by future compatibility checks)

Unknown fields are preserved verbatim so agent-specific extensions are not stripped.

## Conventions

- **English only.** See [`ADR-016`](../docs/architecture/adr/ADR-016-skills-language-adaptation.md) for why, and when we might revisit.
- **CLI-first tool invocation.** Skills invoke tools through `devboy tools call <name>` rather than referencing a specific MCP server name. This keeps recipes portable across agents and transports.
- **Keep bodies short.** A SKILL.md is a recipe, not a framework. If a skill grows past a single page, split it.

## Installing baseline skills

End users install skills through the CLI (see [`ADR-013`](../docs/architecture/adr/ADR-013-skills-install-targets.md)):

```bash
devboy skills list
devboy skills install devboy-setup
devboy skills install --category self-bootstrap
devboy skills install --all --agent claude
```

## Adding a new baseline skill

1. Pick the category and name
2. Create `skills/<NN-category>/<name>/SKILL.md` with valid frontmatter
3. Add unit-test coverage if the skill introduces new frontmatter fields the parser should recognise
4. Submit a PR — reviewers check that the skill obeys the conventions above
