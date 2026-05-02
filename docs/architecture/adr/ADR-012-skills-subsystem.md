---
id: ADR-012
title: Skills subsystem — procedural recipes on top of the tool bundle
status: proposed
date: 2026-04-17
deciders: ["Andrei Mazniak"]
tags: ["skills", "agents", "cli", "architecture"]
supersedes: null
superseded_by: null
---

# ADR-012: Skills subsystem

## Status

**proposed** — design agreed; implementation pending (tracked as separate GitHub issues).

## Context

`devboy-tools` is a configurable bundle of tools (ADR-002, ADR-007). Agents need more than tools — they need **procedural recipes**: "given this tool set, here is the sequence of calls that solves task X". Today each integrator hand-writes these recipes locally (in `CLAUDE.md`, in internal wikis, in one-off scripts). We want to ship those recipes next to the tools that they call.

Requirements:

1. **Recipes are versioned with the tools they call.** If a tool is renamed, the recipe that calls it ships in the same release.
2. **Agent-agnostic where possible.** Claude Code, Codex, Kimi, Cursor, and the AGENTS.md convention all look for skills at different paths. We want one source of truth that installs into whichever path the user (or their agent) prefers.
3. **CLI-first.** Recipes should invoke tools through `devboy tools call <name>` rather than hardcoding MCP server names — this avoids the MCP tool-list tax (ADR-002), works offline, and keeps the recipes portable across transports.
4. **Extensible source.** We want to ship baseline skills embedded in the binary **today**, with the option to load additional skills from external sources (marketplaces, Langfuse) **later**.

## Decision

> **Decision:** Introduce a `devboy-skills` crate, a `SkillSource` trait with an initial `EmbeddedSkillSource` implementation, and a `devboy skills` CLI command family. Baseline skills live in the OSS repository under `skills/<NN-category>/<name>/SKILL.md` and are compiled into the binary via `rust-embed`.

### Crate layout

```
crates/devboy-skills/
├── Cargo.toml
└── src/
    ├── lib.rs              # public API, re-exports
    ├── source.rs           # `SkillSource` trait
    ├── embedded.rs         # `EmbeddedSkillSource` (rust-embed)
    ├── skill.rs            # `Skill`, `Frontmatter`, parse / validate
    ├── catalog.rs          # category + name index, filtering, search
    └── error.rs
```

### `SkillSource` trait

```rust
#[async_trait]
pub trait SkillSource: Send + Sync {
    fn name(&self) -> &'static str;                    // "embedded", "marketplace", "langfuse"
    async fn list(&self) -> Result<Vec<SkillSummary>>; // name, category, version, description
    async fn load(&self, name: &str) -> Result<Skill>; // full SKILL.md + metadata
}
```

Multiple sources can be layered; the CLI picks the source explicitly (`--source embedded|marketplace|langfuse`) or falls back to a deterministic priority order when unspecified.

### Repository layout for baseline skills

```
skills/
├── 00-self-bootstrap/         # Skills that configure / repair devboy itself
│   ├── setup/
│   │   └── SKILL.md
│   ├── repair/
│   │   └── SKILL.md
│   └── tools-catalog/
│       └── SKILL.md
├── 01-issue-tracking/
│   ├── get-issues/
│   ├── create-issue/
│   ├── update-issue/
│   ├── link-issues/
│   └── solve-issue/
├── 02-code-review/
│   ├── review-mr/
│   ├── fix-review-comments/
│   └── self-review/
├── 03-self-feedback/
│   ├── run-and-verify/
│   ├── daily-report/
│   ├── retro/
│   └── knowledge-extract/
├── 04-meeting-notes/
│   ├── meeting-search/
│   ├── meeting-transcript/
│   └── meeting-to-tasks/
└── 05-messenger/
    ├── chat-search/
    ├── chat-summary/
    └── notify/
```

### SKILL.md frontmatter

The on-disk format is a Markdown file with YAML frontmatter. Fields are deliberately minimal and align with the Agent Skills Standard where overlap exists:

```yaml
---
name: get-issues
description: Fetch and summarise issues from the configured tracker.
category: issue-tracking
version: 1                              # integer; bumped on every change
compatibility: devboy-tools >= 0.18     # semver range for the tool bundle
activation:                             # optional hints for agents that support it
  - "get issues"
  - "list tickets"
  - "show open issues"
tools:                                  # optional — lists tools the skill calls
  - get_issues
  - get_issue
  - add_issue_comment
---

# get-issues

...body of the skill in plain Markdown...
```

Only `name`, `description`, `category`, and `version` are required. `compatibility`, `activation`, and `tools` are recommended. Anything else is preserved verbatim so agents with custom fields are not broken.

### CLI surface

```
devboy skills list [--category <id>] [--source embedded|...]
devboy skills show <name> [--source ...]
devboy skills install <name...> [--category <id>] [--all]
                                [--global] [--agent claude|codex|kimi|cursor|all]
                                [--force] [--dry-run]
devboy skills upgrade [<name...>] [--all] [--force] [--dry-run]
devboy skills remove <name...>
```

Install targets (repo-local default, `--global`, `--agent`) and the upgrade / collision logic are covered in ADR-013 and ADR-014 respectively.

### Tool invocation convention

Skills invoke tools through the CLI:

```bash
devboy tools call get_issues '{"state": "open", "limit": 20}'
```

This is the primary transport. The MCP-server transport remains available for agents that prefer it — a skill that wants to take advantage of direct MCP calls can document both paths, but the CLI form is always present as a fallback and is what makes skills portable across agents that do not speak MCP.

## Consequences

### Positive

- ✅ Skills version with the tool bundle — breaking tool changes and the skill update that follows them ship together
- ✅ One canonical source of truth per skill; install where you want via `--agent` / `--global` / repo-local
- ✅ CLI invocation avoids the MCP tool-list tax — agents only load the tools the skill actually needs
- ✅ `SkillSource` trait leaves the door open for a marketplace and a Langfuse source without redesigning the subsystem
- ✅ Baseline skills ship embedded — no network required on first use

### Negative

- ❌ Baseline skills are tied to the binary release cadence. External sources (future work) will solve that for community skills.
- ❌ Keeping SKILL.md frontmatter + body in sync with tool signatures is ongoing maintenance. Linting (`devboy skills lint`) is future work.

### Risks

- ⚠️ **Skill drift** — skills that call a tool that later disappears or changes signature will break. Mitigation: `compatibility` field in frontmatter gates install/upgrade, and `devboy skills upgrade` detects skills whose required tool version no longer matches the installed bundle.
- ⚠️ **Scope creep** — skills becoming mini-programs instead of recipes. Mitigation: each SKILL.md body stays short and focused on the happy path; anything more complex belongs in the user's own prompt chain, not in a shipped skill.

## Alternatives Considered

### Alternative 1: Ship skills as a separate npm package

**Description:** Publish `@devboy-tools/skills` as a standalone npm package, installable via `npm install -g`.

**Why not chosen:** Detaches the skill version from the tool version, reintroducing exactly the drift problem skills are supposed to help with. Can still be added on top of the embedded source later if we want decoupled cadence.

### Alternative 2: Ship only tool docs, let agents synthesise skills

**Description:** Keep doing what we do today — agents read tool descriptions and figure out how to call them.

**Why not chosen:** Agents are consistently good at "call a tool" and consistently bad at "chain tools to solve a multi-step workflow". Skills are the smallest artefact that fixes this.

### Alternative 3: Skills as MCP tools

**Description:** Model each skill as an MCP tool whose implementation is a script orchestrating other tools.

**Why not chosen:** MCP tools are bound to an MCP connection; skills are not. The CLI transport gives skills portability across agents that do not speak MCP (or speak it with a different server-name convention), which is the main reason we chose CLI-first.

## Implementation

Tracking issues:

- **Issue A (epic):** "Skills subsystem: bring procedural recipes into the tool bundle" — umbrella
- **Issue B:** `feat(skills): add devboy-skills crate with embedded SkillSource`
- **Issue C:** `feat(cli): add devboy skills {list,install,show,remove,upgrade} commands`
- **Issue D:** `feat(skills): category 0 — self-bootstrap (setup, repair, tools-catalog)`

See ADR-013 for install targets, ADR-014 for the install / upgrade lifecycle, ADR-015 for the session-trace format that the self-feedback category depends on, and ADR-016 (deferred) for skill language adaptation.

## References

- [Agent Skills Standard](https://agentskills.io/specification)
- [AGENTS.md](https://agents.md/)
- [`rust-embed`](https://docs.rs/rust-embed/)
- [ADR-002: Rust-based architecture](./ADR-002-rust-architecture.md)
- [ADR-007: Plugin architecture](./ADR-007-plugin-architecture.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-17 | Andrei Mazniak | Initial version |
