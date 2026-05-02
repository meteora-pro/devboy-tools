---
id: ADR-018
title: Distribution as Claude Code and Codex plugins with agent-driven bootstrap
status: proposed
date: 2026-05-02
deciders: ["andreymaznyak"]
tags: ["distribution", "plugins", "onboarding", "ux"]
supersedes: null
superseded_by: null
---

# ADR-018: Distribution as Claude Code and Codex plugins with agent-driven bootstrap

## Status

**proposed**

## Context

Today `devboy` is installed in two steps: `npm install -g @devboy-tools/cli` puts the binary on `PATH`, then `devboy onboard` (ADR-017) detects the user's primary AI agent and installs a curated skill bundle into the agent-specific directory. This works on a clean machine, but it has friction:

- the user has to know about npm and remember the second command;
- there is no canonical install path *inside* Claude Code itself — no slash-command, no marketplace listing;
- the [Codex Plugin Marketplace](https://www.codex-marketplace.com/) launched with 20+ third-party plugins; we have no distribution answer for Codex CLI users;
- updates repeat both steps (`npm i -g` followed by `onboard`).

The two ecosystems we care about most (Claude Code and Codex) now ship a first-class plugin format that bundles skills, agents, hooks, and MCP server registrations under a single manifest with versioning, atomic install, and namespaced commands. Three other agents we support do not have a plugin format, but two of them (OpenCode, Kimi CLI) auto-read `~/.claude/skills/`, so a Claude-Code-shaped plugin gives them coverage for free. The remaining four (Copilot CLI, Cursor, Gemini CLI, Antigravity) keep using `devboy onboard`.

A naive way to package us as a Claude Code plugin would be to bundle five pre-built Rust binaries (~125 MB total) inside `.claude-plugin/bin/`. This is heavyweight and duplicates GitHub Releases. A second option — requiring users to pre-install via npm before installing the plugin — defeats the "one-click" goal that motivates packaging in the first place.

There is a third option, which leverages the fact that we are shipping to *agents*, not to humans: **the plugin carries skills, and the agent runs them to install the binary**. We already have `setup` and `repair` skills in `skills/00-self-bootstrap/`. Once Claude Code loads the plugin, the agent itself can be instructed by the skill to check `command -v devboy`, run `npm install -g @devboy-tools/cli` (or fall back to a GitHub Release binary (unsigned today; users can verify against `sha256sums.txt` if needed)), and verify with `devboy doctor`. No bundled binary, no pre-install requirement, and any breakage is debuggable in the same agent session.

Constraints:

- A plugin should add zero blocking installation steps for the user beyond the plugin install command itself.
- One git repository (`meteora-pro/devboy-tools`) must remain the single source of truth for skills, plugin manifests, and the Cargo workspace — we will not maintain a separate `devboy-claude-plugin` repo.
- Skills already live in `/skills/` (master) and are embedded into the binary at compile time as an offline fallback. Adding a third copy would invite drift.
- npm distribution must keep working — it is the primary path for CI, dotfiles, and Docker (where there is no interactive agent to drive the bootstrap).
- `devboy onboard` (ADR-017) must keep working unchanged for the four agents without a plugin format, and must not double-install skills into `~/.claude/skills/` when a Claude Code plugin already provides them.

This ADR is **distinct from ADR-007**, which uses the word *plugin* for an internal Rust extension trait (API providers, format pipeline, enricher). Here, *plugin* refers to the Claude Code / Codex plugin manifest format. The two concepts do not overlap.

## Decision

> **Decision:** Ship `devboy` as a Claude Code plugin and a Codex plugin from the same `meteora-pro/devboy-tools` repository, with no bundled binary. The plugin carries only skills, manifests, and an MCP-server registration; the agent installs the `devboy` binary on first use by running the `setup` skill. `devboy onboard` remains the canonical install path for the four agents that lack a plugin format and learns to detect — and skip — installations that the plugin already provided.

Specifically:

### 1. Plugin distribution model — agent-driven bootstrap

- Plugins ship **no compiled binary**. The `.claude-plugin/` and `.codex-plugin/` manifests reference `devboy mcp serve` from `$PATH`.
- The plugin's `skills/` is a directory of symlinks onto `/skills/<category>/<name>/`, including `setup` and `repair`. On first activation Claude Code surfaces these to the user; the user runs `/devboy:setup` (or accepts an auto-trigger).
- `setup` instructs the agent to:
  1. Check `command -v devboy`. If present, jump to step 4.
  2. Try `npm install -g @devboy-tools/cli`. If npm is missing, fall back to fetching the platform binary from the latest GitHub Release into `${CLAUDE_PLUGIN_DATA}/bin/devboy` and adding it to `$PATH` for the session. The release asset is not currently signed; users in security-sensitive environments can verify the binary against the `sha256sums.txt` published alongside the release.
  3. Run `devboy onboard --agent claude --yes` (inside Codex: `--agent codex`).
  4. Run `devboy doctor` and report.
- If anything goes wrong, the agent invokes `repair` in the same session — no out-of-band debugging.

### 2. Single repository, single source of truth

- Both manifests live in `meteora-pro/devboy-tools` alongside the Cargo workspace:
  - `plugins/claude/.claude-plugin/plugin.json`
  - `.claude-plugin/marketplace.json` (lists this repo as a single-plugin marketplace; `source.path = "plugins/claude"`)
  - `plugins/codex/.codex-plugin/plugin.json`
- Skills remain at `/skills/` as the single source. The Claude plugin's `skills/` directory is a directory of symlinks; the Codex plugin's `skills/` is a single top-level symlink onto the Claude tree. No copies, no generated files.
- Embedded skills inside the Rust binary continue to be generated from `/skills/` at compile time via `rust-embed` and serve as the offline fallback when the binary is run on a machine with no network access (e.g., for `devboy onboard` from a Dockerfile cache).

### 3. Skill naming and storage inside the plugin

- Source skill directories under `/skills/<category>/<name>/` are named without a prefix (`setup`, `get-issues`, …, `analyze-usage`). The plugin tree under `plugins/claude/skills/<name>/` is a directory of **symlinks** pointing at those sources. There is no rename and no generated copy — editing `skills/00-self-bootstrap/setup/SKILL.md` updates the plugin in the same edit.
- The Codex plugin reuses the same tree via a single top-level symlink (`plugins/codex/skills -> ../claude/skills`).
- Plugin namespacing prefixes skills with the **plugin name** (`plugin.json#name = "devboy"`), not the marketplace name. Inside Claude Code skills are invoked as `/devboy:setup`, `/devboy:get-issues`, …, `/devboy:analyze-usage`.
- The skills catalogue keeps a backward-compat alias in `Catalog::get()` so that callers still passing the legacy `devboy-` form (older scripts, dotfiles, AGENTS.md cheat-sheets) keep resolving: `Catalog::get("devboy-setup")` returns the same `SkillSummary` as `Catalog::get("setup")`. The alias lives in `crates/devboy-skills/src/catalog.rs`'s `canonical_skill_name()`.

### 4. Versioning synchronised with the Cargo workspace tag

- `Cargo.toml#workspace.package.version` remains the single semver source of truth.
- A CI step (`scripts/release/sync-plugin-version.sh`) injects the value into `.claude-plugin/plugin.json#version` and updates the `sha` for this plugin in `marketplace.json` on every tag push.
- The release workflow does **three** publishes per tag, in order: `cargo publish` (where applicable), `npm publish`, and a `marketplace.json` SHA bump committed to `main`. All three must succeed for a release to be considered complete; partial releases are rolled back via `git revert`.

### 5. Onboard de-duplication

- `devboy onboard` reads `~/.claude/settings.json#enabledPlugins` and `~/.claude.json` to detect a plugin install of `devboy@meteora-devboy`. When found, the Claude target is treated as **already provisioned** and skill installation into `~/.claude/skills/` is skipped (a one-line note is printed: `claude: skipped — already provided by Claude Code plugin meteora-devboy/devboy@<version>`).
- The same logic applies to Codex (`~/.codex/settings.json#enabledPlugins` once Codex CLI exposes it).
- For agents without a plugin format the behaviour is unchanged.

### 6. Hooks are out of scope for v1

- The `hooks/hooks.json` slot stays empty. Cross-agent hook formats are non-portable (Claude `settings.json` ≠ Codex `hooks/hooks.json` ≠ Kimi `[[hooks]]` in TOML ≠ OpenCode JS plugin handlers), and no current devboy feature requires them. We will revisit when there is a concrete user request that hooks (and not skills) is the right primitive for.

### 7. Slash commands are not shipped

- We deliberately do not ship a `commands/` directory. Custom slash commands are deprecated in Codex CLI and not first-class in Kimi CLI. Skills are the only artifact universally supported across the four plugin-aware or skill-aware agents (Claude, OpenCode, Codex, Kimi).

### 8. Codex plugin in the same repo, separate manifest

- `.codex-plugin/plugin.json` references the same `/skills/` master.
- Existing files under `agents/codex/*.md` are converted mechanically to TOML at the `.codex-plugin/agents/*.toml` location during build.
- After the first release we list the plugin on [codex-marketplace.com](https://www.codex-marketplace.com/).

## Consequences

### Positive

- ✅ One-click install for Claude Code: `/plugin install devboy@meteora-devboy` — every other step happens inside the agent.
- ✅ No 125 MB of bundled binaries; we reuse the existing GitHub Releases pipeline.
- ✅ Cross-agent coverage — three of seven supported agents (Claude Code, OpenCode, Kimi CLI) are reached by one Claude-Code-shaped plugin; Codex gets a second manifest in the same repo.
- ✅ Single source of truth — skills, manifests, and Cargo workspace coexist in one repo, version-locked to a git tag.
- ✅ Self-healing — if anything goes wrong during bootstrap, the user is already in an agent session and `repair` runs in the same context.
- ✅ Compatible with `devboy onboard` — it remains the entry point for Cursor, Gemini, Copilot CLI, and Antigravity, and dedupes when a plugin already covers the Claude target.

### Negative

- ❌ The release pipeline grows from one publish step to three (cargo / npm / marketplace SHA). Each adds a CI failure mode.
- ❌ The `setup` skill becomes load-bearing for first-run UX. A regression in the skill (e.g., npm install path changes) breaks new installs even when the binary itself is fine.
- ❌ Plugins-as-skills means the agent must run a privileged install command (`npm install -g`) without explicit user approval beyond the initial plugin install. We rely on the user reading the plugin manifest before installing it.
- ❌ MCP server is not active until the user runs `/reload-plugins` or restarts Claude Code after `setup` has installed the binary. The skill must instruct this explicitly.

### Risks

- ⚠️ **Source-of-truth drift between `/skills/`, embedded Rust skills, and the plugin tree.** Largely mitigated by the symlink layout — the plugin tree references the same files; the only thing that can drift is a new skill being added to `/skills/` without the matching symlink under `plugins/claude/skills/`. `scripts/release/build-skills.sh --check` catches that in CI; the embedded copy is regenerated from `/skills/` at compile time by `rust-embed`.
- ⚠️ **Marketplace plugin cache TTL** — Claude Code keeps orphaned plugin versions for 7 days, so dev iterations could pick up stale code. Mitigation: use `claude --plugin-dir .` against the working tree during development, only test the marketplace path on tag releases.
- ⚠️ **Codex plugin format is new and may change.** Mitigation: keep `.codex-plugin/` and `.claude-plugin/` decoupled; if the Codex format breaks, the Claude side keeps shipping.
- ⚠️ **`npm install -g` permission failures** on machines without sudo / with restrictive global npm prefix. Mitigation: `setup` falls back to the GitHub Release binary (unsigned today; users can verify against `sha256sums.txt` if needed) in `${CLAUDE_PLUGIN_DATA}/bin/`, which does not require root.
- ⚠️ **Naming collision with ADR-007.** The word *plugin* is now overloaded inside this project. Mitigation: README and ADR-018 explicitly disambiguate; ADR-007's title remains "Plugin architecture (Rust)" and ADR-018's tags do not include the bare `plugins` tag.

## Alternatives Considered

### Alternative 1: Bundle pre-built Rust binaries inside the plugin

**Description:** Include cross-compiled binaries for darwin-arm64, darwin-x64, linux-x64, linux-arm64, win32-x64 under `.claude-plugin/bin/`. The plugin's MCP entry points to the bundled binary directly, and the user never sees `npm install`.

**Why rejected:** Plugin payload grows by ~125 MB; the marketplace caches every commit-SHA version for 7 days, multiplying disk usage on user machines; the bundled binaries duplicate the existing artefacts on GitHub Releases; once we add signing/notarisation it would have to happen twice.

### Alternative 2: Plugin requires `devboy` pre-installed via npm

**Description:** The plugin manifest assumes `devboy` is already on `PATH` and only carries skills + MCP config. A README install line tells the user to `npm install -g @devboy-tools/cli` first.

**Why rejected:** Defeats the one-click goal that motivates the plugin. Two-step install indistinguishable from today's flow modulo the marketplace listing.

### Alternative 3: Separate `devboy-claude-plugin` repository

**Description:** Keep the Cargo workspace in `meteora-pro/devboy-tools` and create a thin `meteora-pro/devboy-claude-plugin` that mirrors `/skills/` and ships only the manifest.

**Why rejected:** Two repos in lock-step is more drift surface, more CI to maintain, and forces every contributor who adds a skill to coordinate two PRs. Marketplaces are happy to host plugins from sub-paths of a repo via the `source.path` field, so a single repo works.

### Alternative 4: Replace `devboy onboard` with the plugin entirely

**Description:** Drop `devboy onboard` once the plugin ships; assume every supported agent will eventually have a plugin format.

**Why rejected:** Today only Claude Code and Codex have plugin manifests. Cursor / Gemini / Copilot CLI / Antigravity do not, and we cannot ship a plugin to them. `devboy onboard` covers them and stays the simpler universal install for advanced flows (CI, dotfiles, headless Docker) where running a plugin from inside an agent session is impossible.

### Alternative 5: Ship hooks for lint/format on first cut

**Description:** Bundle a `hooks/hooks.json` that runs `cargo fmt`/`prettier` after writes, mirroring `affaan-m/everything-claude-code`.

**Why rejected:** Hook formats are not portable across the four plugin-aware agents, doubling the maintenance surface for a feature that no current devboy capability needs. Easier to add later when there is a concrete request.

## Implementation

- **Issues:** #226 (this work) — single-PR rollout
- **Predecessors:** ADR-017 (`devboy onboard`), ADR-013 (skill install targets), ADR-014 (skills lifecycle)
- **Disambiguation:** ADR-007 (Rust internal plugin architecture) — a different concept, despite the shared word
- **Code targets:** `.claude-plugin/`, `.codex-plugin/`, `scripts/release/sync-plugin-version.sh`, `crates/devboy-skills/src/install.rs` (dedup logic), `skills/00-self-bootstrap/{setup,repair}/SKILL.md` (rename + bootstrap rewrites)
- **Docs:** `docs/guide/integrations/claude-code-plugin.md`, `docs/guide/integrations/codex-plugin.md`, README "Install via Claude Code Plugin" section

## References

- [Claude Code Plugins](https://code.claude.com/docs/en/plugins)
- [Plugin marketplaces](https://code.claude.com/docs/en/plugin-marketplaces)
- [Plugins reference](https://code.claude.com/docs/en/plugins-reference)
- [Codex Plugin Build Guide](https://developers.openai.com/codex/plugins/build)
- [Codex Plugin Marketplace](https://www.codex-marketplace.com/)
- [anthropics/claude-plugins-official](https://github.com/anthropics/claude-plugins-official) — reference structure
- [OpenCode Skills](https://opencode.ai/docs/skills/) — reads `~/.claude/skills/` directly
- [Kimi CLI Skills](https://moonshotai.github.io/kimi-cli/en/customization/skills.html) — reads `~/.claude/skills/` and `~/.codex/skills/`

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-02 | andreymaznyak | Initial draft |
