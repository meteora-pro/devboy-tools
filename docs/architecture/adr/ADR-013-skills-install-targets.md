---
id: ADR-013
title: Skill install targets — repo-local default, global and agent-specific overrides
status: proposed
date: 2026-04-17
deciders: ["Andrei Mazniak"]
tags: ["skills", "cli", "install"]
supersedes: null
superseded_by: null
---

# ADR-013: Skill install targets

## Status

**proposed** — design agreed; implementation pending together with ADR-012.

## Context

Skills (ADR-012) can live in multiple places depending on which agent reads them. We need to pick a default that is **predictable, safe to iterate on, and agent-agnostic**, and we need escape hatches for users who want to install globally or tailor to a specific agent.

The landscape of install paths today:

- **Claude Code** — `~/.claude/skills/` (user) and `./.claude/skills/` (project)
- **Codex** — `~/.codex/skills/` (user) and `./.codex/skills/` (project)
- **Cursor / Kimi** — similar per-agent paths
- **AGENTS.md convention** — vendor-neutral `AGENTS.md` at project root describing agent instructions; no standardised subdirectory for skills

## Decision

> **Decision:** Default install target is **repo-local** at `.agents/skills/<name>/` (mirroring the `.devboy.toml` convention). `--global` installs to `~/.agents/skills/<name>/`. `--agent <name>` installs to that agent's conventional path. `--agent all` installs to every detected agent plus the vendor-neutral default.

### Path resolver

```
devboy skills install setup               # → <repo>/.agents/skills/setup/
devboy skills install setup --global      # → ~/.agents/skills/setup/
devboy skills install setup --agent claude     # → ~/.claude/skills/setup/
devboy skills install setup --agent codex      # → ~/.codex/skills/setup/
devboy skills install setup --agent cursor     # → ~/.cursor/skills/setup/
devboy skills install setup --agent kimi       # → ~/.kimi/skills/setup/
devboy skills install setup --agent all        # → all detected agents + ~/.agents/skills/
devboy skills install setup --agent claude --local  # → <repo>/.claude/skills/setup/
```

The `--local` modifier switches agent-specific installs from their global home to their project-local conventional path. `--local` on its own (without `--agent`) is the default and therefore redundant — included only for clarity in scripts.

### Why repo-local by default

`devboy-tools` follows the same default that the rest of the CLI uses — a project under `.devboy.toml` is assumed to be a self-contained unit. Skills installed per-project:

- Don't leak to other repositories the developer works on
- Are easy to rip out (`git clean` / remove the directory)
- Get versioned with the repository if the team commits `.agents/skills/` (opt-in — the directory is added to `.gitignore` by default)
- Let different repositories pin different skill versions

### Behaviour when there is no repository

If `devboy skills install` is invoked outside a git repository (or outside a directory that contains `.devboy.toml`) **and** neither `--global` nor `--agent` is passed, the command **fails with a clear error** that lists the options:

```
error: no git repository / .devboy.toml at the current path

skills are installed repo-locally by default. choose one:
  devboy skills install <name> --global           # install to ~/.agents/skills/
  devboy skills install <name> --agent claude     # install to ~/.claude/skills/
  devboy skills install <name> --agent all        # all detected agents
  cd <your-project> && devboy skills install <name>
```

Explicit is safer than a silent fallback — the user always knows where the skill landed.

### `--agent all` semantics

`--agent all`:

1. Reads the existing `CredentialStore`-style logic to detect which agents are installed on the machine (checks for `~/.claude/`, `~/.codex/`, `~/.cursor/`, `~/.kimi/` directories).
2. Installs to each detected agent's home path.
3. **Also** installs to the vendor-neutral `~/.agents/skills/` — so any agent that respects AGENTS.md-style paths picks it up too.
4. Reports per-target results at the end.

### Co-installation with AGENTS.md

If the repository (or `~/`, for `--global`) contains an `AGENTS.md` file, `devboy skills install` does **not** edit it. `AGENTS.md` describes how to work on the project; skills are separate, discoverable artefacts. We may revisit this if users request an opt-in pointer from `AGENTS.md` into `.agents/skills/`, but the default is hands-off.

## Consequences

### Positive

- ✅ Default is predictable and contained — installing a skill can never silently leak to another project
- ✅ Flag shape is symmetric with the rest of the CLI (repo-local default, `--global` escape hatch)
- ✅ `--agent <name>` gives exact control without the user needing to know the path convention for that agent
- ✅ `--agent all` is the "it just works" flag for users who don't care where it lands as long as every agent sees it
- ✅ Fail-fast when the location is ambiguous — no guesswork

### Negative

- ❌ Team members who want a shared skill set need to agree to commit `.agents/skills/` (off by default — `.gitignore`'d out of the box)
- ❌ `~/.agents/skills/` is a new convention and not yet read by every agent. For now it's mostly useful paired with `--agent all`.

### Risks

- ⚠️ **Path explosion** when `--agent all` runs on a machine with many agents installed. Mitigation: dry-run is always available, and the per-target result list at the end makes it obvious which paths were written.
- ⚠️ **Agent paths change** upstream (e.g. Claude Code starts reading from a different directory). Mitigation: the path table is a small, centralised piece of code that's easy to update when a vendor changes their convention.

## Alternatives Considered

### Alternative 1: Global default (`~/.agents/skills/`)

**Description:** Install to `~/.agents/skills/` by default, with `--local` to scope to a repository.

**Why not chosen:** Less predictable for multi-repo developers. One team's recipes leak into another team's agent conversations. Less consistent with `.devboy.toml` (which is repo-local by default).

### Alternative 2: Agent-specific default (pick Claude Code)

**Description:** Install to `~/.claude/skills/` by default since that's the most common target today.

**Why not chosen:** Makes the CLI feel Claude-centric even though the rest of the tool bundle is agent-agnostic. Fine for the `--agent claude` opt-in but a poor default.

### Alternative 3: Fallback to `$PWD/.agents/skills/` when no repo is present

**Description:** If we can't find a repo root, create `.agents/skills/` in the current working directory anyway.

**Why not chosen:** Too magical — the user wouldn't always realise they've scattered skill directories across random folders. Failing with a clear flag hint is better.

## Implementation

- **Resolver:** new module in `crates/devboy-skills/src/install.rs`
- **Agent detection:** check standard agent home paths (`~/.claude/`, `~/.codex/`, `~/.cursor/`, `~/.kimi/`) — small table, easy to extend
- **CLI:** `devboy skills install` accepts `--global`, `--agent`, `--local`, `--force`, `--dry-run`

Related issues: see ADR-012.

## References

- [ADR-012: Skills subsystem](./ADR-012-skills-subsystem.md)
- [ADR-014: Skills lifecycle](./ADR-014-skills-lifecycle.md)
- [AGENTS.md](https://agents.md/)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-17 | Andrei Mazniak | Initial version |
