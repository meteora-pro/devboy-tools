---
id: ADR-014
title: Skills lifecycle — manifest-based install, upgrade, and collision detection
status: proposed
date: 2026-04-17
deciders: ["Andrei Mazniak"]
tags: ["skills", "cli", "lifecycle", "install"]
supersedes: null
superseded_by: null
---

# ADR-014: Skills lifecycle

## Status

**proposed** — design agreed; implementation pending together with ADR-012.

## Context

Installed skills accumulate on disk across several install targets (ADR-013). Two things can happen to any installed skill between now and its next upgrade:

1. **We ship a new version** of it (embedded in a new binary release).
2. **The user edits it** locally to tailor it to their project.

The upgrade command must tell these two apart. Overwriting user edits silently is a bug; refusing to upgrade because the user trivially has our **previous** shipped version would make upgrades painful.

## Decision

> **Decision:** Every install/upgrade writes a **per-location manifest** at `<target>/.manifest.json` recording the SHA256 of each installed skill plus the version it came from. The binary ships an **embedded history** of every previous shipped SHA256 for every skill. On upgrade, we compare the on-disk hash against (a) the current shipped hash, (b) the embedded history of previous shipped hashes, (c) everything else = user modification.

### Three states per installed skill

```
on-disk SHA256 =
  current shipped hash       → unchanged, skip
  any previous shipped hash  → safe to overwrite (user has our older version)
  anything else              → user-modified, refuse without --force
```

### Manifest shape — `<target>/.manifest.json`

```json
{
  "version": 1,
  "installed_from": "devboy-tools 0.18.0",
  "skills": {
    "devboy-setup": {
      "version": 3,
      "installed_at": "2026-04-17T12:34:56Z",
      "source": "embedded",
      "files": {
        "SKILL.md": {
          "sha256": "3f2b...",
          "size": 2438
        }
      }
    }
  }
}
```

One manifest per install target (`.agents/skills/.manifest.json`, `~/.agents/skills/.manifest.json`, `~/.claude/skills/.manifest.json`, …). This keeps install/upgrade/remove as **local** operations without global state to reconcile.

### Embedded history manifest

Inside the binary, `devboy-skills` ships (via `rust-embed`) a file describing every historical shipped SHA256 for every skill:

```json
{
  "devboy-setup": {
    "current": { "version": 3, "sha256": "3f2b..." },
    "history": [
      { "version": 1, "sha256": "abcd..." },
      { "version": 2, "sha256": "ef01..." }
    ]
  }
}
```

A skill is "ours" if its hash appears in `current.sha256` or anywhere in `history`. Everything else is user-modified.

### `devboy skills install` flow

```
for each (skill, target):
  if skill not present → write file, update manifest, count as "installed"
  if skill present:
    compute sha256 of on-disk file
    if sha256 == current shipped sha256 → skip, count as "unchanged"
    if sha256 in historical hashes for this skill → overwrite, count as "upgraded"
    else (user modification):
      if --force → overwrite, count as "overwritten-with-force"
      else → skip, count as "skipped-user-modified"
  update manifest entry
```

Final report:

```
installed       : 3
unchanged       : 2
upgraded        : 1   (was devboy-setup v2 → now v3)
skipped (user)  : 1   (devboy-retro has local edits; pass --force to overwrite)
failed          : 0

exit code: 0   (some skills skipped, not an error)
```

Exit code is non-zero only if **every** install attempt fails — otherwise partial failures are warnings.

### `devboy skills upgrade` flow

Effectively the same flow, but only operates on skills already recorded in the manifest, and looks at every install target the user selects (same `--global` / `--agent` / `--local` flags as install).

### `--dry-run`

Every install / upgrade command supports `--dry-run`, which prints the per-skill plan (installed / unchanged / upgraded / user-modified / would-force) without touching the filesystem.

### `--force`

Overwrites user-modified skills. Always paired with an explicit warning line per file:

```
forcing overwrite: .agents/skills/devboy-retro/SKILL.md (user-modified, last edit 2026-04-15)
```

### `devboy skills remove`

Deletes the skill directory and removes the manifest entry. No hash check — if the user asked to remove it, remove it. A `--dry-run` is still useful for previewing the list.

### Manifest recovery

If `.manifest.json` is missing or corrupted, the install / upgrade commands **reconstruct** it from on-disk state: for each skill directory at the target, compute SHA256, match against the embedded history, and emit the best guess. A warning is printed and the user is told to re-run `upgrade --dry-run` to review.

## Consequences

### Positive

- ✅ User edits survive upgrade by default
- ✅ Genuine "stale version" upgrades are silent and painless
- ✅ Full history of shipped versions gives us retroactive reasoning — we can still recognise a skill a user installed three releases ago
- ✅ Per-location manifest keeps operations atomic and scoped
- ✅ Offline / deterministic — embedded history ships in the binary, no network calls during install / upgrade
- ✅ `--dry-run` makes every run safe to preview

### Negative

- ❌ Binary size grows with every release by the sum of (file hashes + a few bytes of metadata). In practice this is tens of kilobytes per release — negligible compared to the binary itself.
- ❌ A user who deletes the entire `.manifest.json` file and has edited skills loses the "did the user edit this?" signal. Mitigation: we can still detect it by comparing to embedded history, and `--force` is still required to overwrite.

### Risks

- ⚠️ **Hash churn** — cosmetic changes (whitespace, formatting) bump the hash and trigger "upgraded" status even when the content is effectively identical. Mitigation: avoid gratuitous formatting changes in skills; use a markdown linter to keep whitespace stable.
- ⚠️ **False-positive "user-modified"** — if a user imports the skill file from an older devboy version we've since purged from history (e.g. many major versions ago). Mitigation: we don't plan to prune history; it's just hashes, basically free.

## Alternatives Considered

### Alternative 1: Global per-user manifest

**Description:** Single `~/.devboy/skills-manifest.json` recording every install everywhere.

**Why not chosen:** Introduces shared state that has to be reconciled when install targets live on different filesystems or when the user manipulates install directories directly (clone to another machine, `scp`, etc.). Per-location manifests are simpler and let install targets move independently.

### Alternative 2: No history, always `--force` required for re-install

**Description:** Only track current-version hash; any mismatch is treated as user-modified.

**Why not chosen:** Makes upgrading across two releases painful — the user would always need `--force` even when their on-disk file is our own two-releases-old skill they never touched. Friction kills adoption.

### Alternative 3: Network-fetched history

**Description:** Query an online service for the history of shipped hashes when checking on-disk files.

**Why not chosen:** Breaks offline use, introduces a network dependency during `install`, and a new thing to keep up. Embedding is cheap and offline.

### Alternative 4: Git-style three-way merge

**Description:** Track the base version in the manifest and attempt a three-way merge between "our old", "our new", and "user edits" on upgrade.

**Why not chosen:** Overkill for markdown recipes that are rarely more than a page long. The "skip or --force" model is honest about what it can and cannot do automatically, and users who want to keep custom edits while accepting upstream changes can do their own merge with git or a diff tool.

## Implementation

- **Manifest I/O:** new module `crates/devboy-skills/src/manifest.rs` (load / save / reconstruct)
- **Hash comparator:** small comparator in `crates/devboy-skills/src/install.rs` that uses the manifest history from `EmbeddedSkillSource::history()`
- **CLI:** `--force` and `--dry-run` flags on `install`, `upgrade`, `remove`

Related issues: see ADR-012.

## References

- [ADR-012: Skills subsystem](./ADR-012-skills-subsystem.md)
- [ADR-013: Skills install targets](./ADR-013-skills-install-targets.md)
- [`rust-embed`](https://docs.rs/rust-embed/)
- [chezmoi's template-vs-modified detection](https://www.chezmoi.io/) — inspiration for the three-state model

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-17 | Andrei Mazniak | Initial version |
