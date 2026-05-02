# Plugin skill layout

The Claude Code and Codex plugins share **the same source files** as
`devboy onboard` — there is no separate plugin tree. `plugins/claude/skills/`
is a directory of **symlinks** pointing at the real skill directories
under `/skills/<category>/<source-name>/`. The Codex plugin reuses the
same tree via a single top-level symlink:

```
plugins/claude/skills/devboy-setup    -> ../../../skills/00-self-bootstrap/devboy-setup
plugins/claude/skills/devboy-get-issues -> ../../../skills/01-issue-tracking/devboy-get-issues
…  (24 entries, one per source skill)

plugins/codex/skills                  -> ../claude/skills
plugins/codex/bin/devboy-shim.sh      -> ../../claude/bin/devboy-shim.sh
```

Skill names are **identical everywhere** — `devboy-setup`,
`devboy-get-issues`, …, `analyze-usage` (the only one without a
`devboy-` prefix). Inside Claude Code the plugin namespacing prepends
`devboy:`, so users invoke skills as
`/devboy:devboy-setup`. Verbose, but the trade-off bought us
zero file duplication: editing `skills/00-self-bootstrap/devboy-setup/SKILL.md`
updates the plugin in the same edit.

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

1. Create `skills/<category>/<name>/SKILL.md` with frontmatter
   (`name:` matches the directory name).
2. Run `scripts/release/build-skills.sh` to (re)create the matching
   symlink.
3. Commit both — the source files and the new symlink under
   `plugins/claude/skills/`.

Removing a skill:

1. `git rm -r skills/<category>/<name>/`.
2. Run `scripts/release/build-skills.sh` — the orphan symlink is
   pruned automatically.
3. Commit.

## Backward-compat alias in `Catalog::get()`

`crates/devboy-skills/src/catalog.rs` retains a small alias rule:
`Catalog::get("devboy-setup")` and `Catalog::get("setup")` both
resolve to the same `SkillSummary`. This is only useful if a future
change ever drops the prefix from source filenames; today the alias
returns the same entry whichever form the caller uses.

## See also

- ADR-018 §3 — skill naming inside the plugin
- ADR-014 — skills lifecycle (the `history.json` SHA tracking is by source name)
