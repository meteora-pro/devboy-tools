#!/usr/bin/env bash
#
# Verify (and where missing, restore) the symlink-based plugin skill tree.
#
# We do NOT copy or transform skill files. The plugin tree is just a
# directory of symlinks pointing at the real source under /skills/:
#
#   plugins/claude/skills/<source-name>  -> ../../../skills/<category>/<source-name>
#   plugins/codex/skills                 -> ../claude/skills            (single link)
#   plugins/codex/bin/devboy-shim.sh     -> ../../claude/bin/devboy-shim.sh
#
# Claude Code registers each skill under the `name:` field in its
# frontmatter (and dedupes when the same `name:` appears twice), so we
# do NOT create extra short-form alias symlinks like `setup ->
# .../devboy-setup` — Claude would only show one of them. To get
# `/devboy:setup`, the source filename + frontmatter name has to drop
# the `devboy-` prefix, which is a deferred refactor (history.json
# migration, bundles update, embedded loader, existing user installs).
#
# Source of truth: skills/<category>/<name>/SKILL.md.
#
# This script has two responsibilities:
#   1. Detect drift: any source skill that does not have a matching link,
#      or any link with a wrong target, or the Codex top-level link gone.
#   2. Repair: in `write` mode, recreate missing or broken links so that a
#      fresh checkout (or one that lost symlinks for any reason) is fixed
#      with one command.
#
# Usage:
#   scripts/release/build-skills.sh             # restore any missing/broken links
#   scripts/release/build-skills.sh --dry-run   # print expected layout
#   scripts/release/build-skills.sh --check     # exit 1 on drift (used in CI)

set -euo pipefail

MODE=write
case "${1:-}" in
  --dry-run) MODE=dry ;;
  --check)   MODE=check ;;
  "")        MODE=write ;;
  *) echo "Unknown flag: $1" >&2; exit 2 ;;
esac

ROOT="$(git rev-parse --show-toplevel)"
SKILLS_SRC="$ROOT/skills"
CLAUDE_DST="$ROOT/plugins/claude/skills"
CODEX_LINK="$ROOT/plugins/codex/skills"
CODEX_BIN_LINK="$ROOT/plugins/codex/bin/devboy-shim.sh"
EXPECTED_CODEX_SKILLS="../claude/skills"
EXPECTED_CODEX_SHIM="../../claude/bin/devboy-shim.sh"

# Each plugin entry: name, expected symlink target relative to CLAUDE_DST.
# One symlink per source skill, name preserved (see header comment).
discover_skills() {
  while IFS= read -r skill_md; do
    local src_dir src_name cat
    src_dir="$(dirname "$skill_md")"
    src_name="$(basename "$src_dir")"
    cat="$(basename "$(dirname "$src_dir")")"
    printf '%s\t../../../skills/%s/%s\n' "$src_name" "$cat" "$src_name"
  done < <(find "$SKILLS_SRC" -mindepth 3 -maxdepth 3 -name SKILL.md -type f | sort)
}

restore_link() {
  local link="$1"
  local target="$2"
  mkdir -p "$(dirname "$link")"
  if [ -L "$link" ] && [ "$(readlink "$link")" = "$target" ]; then
    return 0
  fi
  rm -rf "$link"
  ln -s "$target" "$link"
  echo "  (re)created $link -> $target"
}

drift_check() {
  local rc=0

  # 1. Every source skill has a matching link in plugins/claude/skills/.
  while IFS=$'\t' read -r name target; do
    local link="$CLAUDE_DST/$name"
    if [ ! -L "$link" ]; then
      echo "Drift: missing symlink $link -> $target" >&2
      rc=1
    elif [ "$(readlink "$link")" != "$target" ]; then
      echo "Drift: $link -> $(readlink "$link"), expected -> $target" >&2
      rc=1
    fi
  done < <(discover_skills)

  # 2. No stale links / extra entries in plugins/claude/skills/.
  if [ -d "$CLAUDE_DST" ]; then
    local expected_names
    expected_names="$(discover_skills | cut -f1 | sort)"
    while IFS= read -r entry; do
      [ -z "$entry" ] && continue
      if ! grep -qx "$entry" <<<"$expected_names"; then
        echo "Drift: unexpected entry $CLAUDE_DST/$entry (no matching skill in /skills/)" >&2
        rc=1
      fi
    done < <(ls -1 "$CLAUDE_DST" 2>/dev/null | sort)
  fi

  # 3. Codex top-level skills link present and correct.
  if [ ! -L "$CODEX_LINK" ]; then
    echo "Drift: $CODEX_LINK is not a symlink (expected -> $EXPECTED_CODEX_SKILLS)" >&2
    rc=1
  elif [ "$(readlink "$CODEX_LINK")" != "$EXPECTED_CODEX_SKILLS" ]; then
    echo "Drift: $CODEX_LINK -> $(readlink "$CODEX_LINK"), expected -> $EXPECTED_CODEX_SKILLS" >&2
    rc=1
  fi

  # 4. Codex shim symlink present and correct.
  if [ ! -L "$CODEX_BIN_LINK" ]; then
    echo "Drift: $CODEX_BIN_LINK is not a symlink (expected -> $EXPECTED_CODEX_SHIM)" >&2
    rc=1
  elif [ "$(readlink "$CODEX_BIN_LINK")" != "$EXPECTED_CODEX_SHIM" ]; then
    echo "Drift: $CODEX_BIN_LINK -> $(readlink "$CODEX_BIN_LINK"), expected -> $EXPECTED_CODEX_SHIM" >&2
    rc=1
  fi

  if [ "$rc" -ne 0 ]; then
    echo "Run scripts/release/build-skills.sh and commit the result." >&2
    exit 1
  fi
  echo "OK: plugin skill links match /skills/."
}

case "$MODE" in
  write)
    mkdir -p "$CLAUDE_DST"
    # Restore per-skill links.
    while IFS=$'\t' read -r name target; do
      restore_link "$CLAUDE_DST/$name" "$target"
    done < <(discover_skills)
    # Prune entries that no longer have a source.
    expected_names="$(discover_skills | cut -f1 | sort)"
    while IFS= read -r entry; do
      [ -z "$entry" ] && continue
      if ! grep -qx "$entry" <<<"$expected_names"; then
        echo "  removed stale entry $CLAUDE_DST/$entry"
        rm -rf "$CLAUDE_DST/$entry"
      fi
    done < <(ls -1 "$CLAUDE_DST" 2>/dev/null | sort)
    # Top-level Codex links.
    restore_link "$CODEX_LINK"     "$EXPECTED_CODEX_SKILLS"
    restore_link "$CODEX_BIN_LINK" "$EXPECTED_CODEX_SHIM"
    cnt=$(discover_skills | wc -l | tr -d ' ')
    echo "OK: $cnt skill symlinks under plugins/claude/skills/ (Codex shares via top-level symlink)."
    ;;
  dry)
    echo "Expected layout:"
    while IFS=$'\t' read -r name target; do
      printf '  plugins/claude/skills/%-30s -> %s\n' "$name" "$target"
    done < <(discover_skills)
    printf '  %-49s -> %s\n' "$CODEX_LINK"     "$EXPECTED_CODEX_SKILLS"
    printf '  %-49s -> %s\n' "$CODEX_BIN_LINK" "$EXPECTED_CODEX_SHIM"
    ;;
  check)
    drift_check
    ;;
esac
