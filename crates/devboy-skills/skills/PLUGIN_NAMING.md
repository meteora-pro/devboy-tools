# Plugin skill layout

The Claude Code and Codex plugins share **the same source files** as
`devboy onboard` — there is no separate plugin tree. `plugins/claude/skills/`
is a directory of **symlinks** pointing at the real skill directories
under `/crates/devboy-skills/skills/<category>/<source-name>/`. The Codex
plugin reuses the same tree via a single top-level symlink:

```
plugins/claude/skills/setup       -> ../../../crates/devboy-skills/skills/00-self-bootstrap/setup
plugins/claude/skills/get-issues  -> ../../../crates/devboy-skills/skills/01-issue-tracking/get-issues
…  (24 entries, one per source skill)

plugins/codex/skills                  -> ../claude/skills
plugins/codex/bin/devboy-shim.sh      -> ../../claude/bin/devboy-shim.sh
```

Skill names are **identical everywhere** — source folder name, plugin
folder name, and the `name:` field in the frontmatter all match
(`setup`, `get-issues`, …, `analyze-usage`). Inside Claude Code the
plugin namespacing prepends `devboy:` (the plugin name from
`plugin.json`), so users invoke skills as `/devboy:setup`,
`/devboy:get-issues`, … Editing
`crates/devboy-skills/skills/00-self-bootstrap/setup/SKILL.md` updates
the plugin in the same edit — zero file duplication.

## Why no rename rule

The previous design generated copies of every SKILL.md under
`plugins/claude/skills/<short-name>/` with the `devboy-` prefix
stripped from the frontmatter `name:` field. That created 48 files on
disk, two of every skill, and forced contributors to either edit both
copies or remember to run a generator before committing. The symlink
layout removes the rename and the generator step.

## Maintaining the layout

Use `scripts/release/build-skills.sh` for both maintenance and
validation:

| Command | What it does |
|---|---|
| `scripts/release/build-skills.sh`           | Restores any missing or wrong-target symlinks; prunes stale entries. Idempotent — safe to run any time. |
| `scripts/release/build-skills.sh --dry-run` | Prints the expected layout without touching the filesystem. |
| `scripts/release/build-skills.sh --check`   | Exits non-zero on any drift — used as a CI gate. |

Adding a new skill:

1. Create `crates/devboy-skills/skills/<category>/<name>/SKILL.md`
   with frontmatter (`name:` matches the directory name).
2. Run `scripts/release/build-skills.sh` to (re)create the matching
   symlink.
3. Commit both — the source files and the new symlink under
   `plugins/claude/skills/`.

Removing a skill:

1. `git rm -r crates/devboy-skills/skills/<category>/<name>/`.
2. Run `scripts/release/build-skills.sh` — the orphan symlink is
   pruned automatically.
3. Commit.

## Legacy name compatibility

Source skill files were renamed in 0.25 to drop the `devboy-` prefix.
Older callers (scripts, dotfiles, AGENTS.md cheat-sheets) that still
ask for `devboy-setup` keep working: `Catalog::get("devboy-setup")`
returns the same entry as `Catalog::get("setup")`. The alias rule
lives in `canonical_skill_name()` in
`crates/devboy-skills/src/catalog.rs` and will stay until at least
0.27 to give external scripts time to update.

## See also

- ADR-018 §3 — skill naming inside the plugin
- ADR-014 — skills lifecycle (the `history.json` SHA tracking is by source name)
