---
id: ADR-017
title: Agent detection and devboy onboard command
status: proposed
date: 2026-05-01
deciders: ["andreymaznyak"]
tags: ["onboarding", "skills", "ux"]
supersedes: null
superseded_by: null
---

# ADR-017: Agent detection and `devboy onboard` command

## Status

**proposed**

## Context

After installing `devboy`, a user faces the question: *"what next?"*. Today we expect them to know about `devboy skills install <name> --agent <agent>` and pick the right skills manually. This is friction that turns a one-command install into a multi-step research session.

Most users already have an AI coding agent on their machine — Claude Code, GitHub Copilot CLI, Codex CLI, Kimi Code CLI, Cursor, Gemini CLI, or Antigravity. Each leaves deterministic traces in `$HOME/`: config dirs, session storage, history files. We can read those locally (no network), figure out which agent the user actively uses, and skip the question entirely.

Constraints:

- Cross-platform (macOS / Linux / Windows). Cursor's storage path is platform-specific.
- Privacy-respectful — only read metadata (mtimes, file counts), don't slurp content into memory.
- Reuse — the detector should serve `devboy onboard`, `devboy agents list`, and a future `devboy doctor`.
- Extensibility — adding the 8th agent should be one new file, not edits across modules.
- Determinism — same disk state → same primary candidate.

The full path table for the seven agents in MVP scope is documented in issue #217. Highlights:

- **Rich session storage** (events JSONL, easy to count + parse): Claude Code, Copilot CLI ≥1.0, Kimi CLI, Antigravity (Protobuf — count-only).
- **Medium** (only prompts or SQLite KV): Codex CLI, Cursor.
- **Low** (just install marker): Gemini CLI.

## Decision

> **Decision:** Introduce an `AgentDetector` trait + per-agent module under `crates/devboy-core/src/agents/`, and ship `devboy onboard`, `devboy agents list` on top of it. Onboard auto-selects the primary agent by a `freshness × volume` score and installs a curated skill bundle for that agent.

Specifically:

- One file per agent: `agents/{claude,codex,copilot,kimi,cursor,gemini,antigravity}.rs`. Each implements `AgentDetector::{check_installed, count_sessions, last_used, paths_checked}`.
- `detect_all() -> Vec<AgentSnapshot>` runs all detectors, attaches a score, returns sorted.
- Score: `0.6 * freshness + 0.4 * volume` where `freshness = max(0, 1 - days_since_last_used / 14)` and `volume = min(1, log10(sessions+1) / 3)`.
- Primary chosen automatically iff `score_top1 / score_top2 >= 1.5`, otherwise the user is asked.
- Cross-platform paths via the `dirs` crate; binary lookups via `which::which`.
- Skill bundles are TOML manifests under `crates/devboy-core/bundles/<profile>.toml` listing skill IDs (embedded into the binary at build time so the bundle ships with `devboy-core`; see ADR-022).
- `devboy onboard --agent <id> --yes` is the headless mode for CI / dotfiles / Docker.

## Consequences

### Positive

- ✅ One-command onboarding: `devboy onboard` → confirmation → a working setup.
- ✅ Reuse — `agents list` and future `doctor` lean on the same detector, no duplication.
- ✅ Extensible — adding an agent = one file + one line in the registry.
- ✅ Diagnostic transparency — `paths_checked` lets users see *why* an agent wasn't picked.

### Negative

- ❌ Maintenance — agents change their on-disk format; detectors will need occasional updates (e.g. Copilot CLI flipped formats in apr 2026).
- ❌ Some agents store almost no useful state (Gemini CLI), so the score becomes mostly install-presence — not a real activity signal.

### Risks

- ⚠️ False positives on machines with leftover config dirs from agents the user no longer uses → mitigated by score-based ranking with freshness decay (14 days).
- ⚠️ Cursor's SQLite-key namespace can change between versions, breaking the count-sessions heuristic → mitigated by falling back to mtime of `workspaceStorage/`.
- ⚠️ Cross-platform parity drift (especially Windows) → mitigated by fixture-based snapshot tests in `tests/fixtures/agents/` per OS.

## Alternatives Considered

### Alternative 1: Ask the user upfront

**Description:** No detector — `devboy onboard` shows a list of seven agents and asks the user to pick.

**Why rejected:** Most users won't know the canonical name (is it "Claude" or "Claude Code"? "Copilot CLI" or "@github/copilot"?), and we already have all the data on disk. Detector + confirmation prompt is strictly better UX.

### Alternative 2: Single bundled "starter" install, no agent-specific tailoring

**Description:** `devboy onboard` installs the same set of skills for every agent.

**Why rejected:** Skill manifests are agent-specific (different SKILL.md format / activation phrases / tool surface for Claude vs Copilot vs Cursor). A one-size-fits-all bundle either over-installs or under-installs.

### Alternative 3: Online registry of agents (fetch detector definitions from GitHub)

**Description:** Detectors are fetched at runtime from a public registry, so we can add support without releasing a new `devboy` binary.

**Why rejected:** Premature for MVP, adds a network dependency to a local diagnostic, complicates security review. Reconsider once we hit the 10th agent.

## Implementation

- **Issue:** #217
- **PR:** TBD (this branch)
- **Code:** `crates/devboy-core/src/agents/` (new module)
- **Tests:** `tests/fixtures/agents/<agent_id>/` snapshots + score-formula table tests

## References

- Issue #217 — full path table for the seven supported agents
- ADR-012 / 013 / 014 — skills subsystem this onboard command sits on top of
- Kimi CLI session model — `MoonshotAI/kimi-cli/src/kimi_cli/{session,share,metadata}.py`
- Copilot CLI install pattern — `gh.io/copilot-install` (curl-pipe-bash, same shape we use for analyze-usage backend)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-01 | andreymaznyak | Initial draft |
