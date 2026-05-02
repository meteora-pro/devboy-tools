#!/usr/bin/env bash
#
# Generate the Claude Code plugin skill tree from /skills/.
#
# Source of truth: skills/<category>/<name>/SKILL.md (and any sibling files).
# Target:           plugins/claude/skills/<rule(name)>/SKILL.md
#
# The Codex plugin reuses the same tree via a relative symlink committed
# to the repo (`plugins/codex/skills -> ../claude/skills`). We do NOT
# generate a separate Codex tree — both plugins read the identical files.
# `--check` validates the symlink as well.
#
# Plugin root layout follows the Claude Code spec: only plugin.json lives in
# .claude-plugin/; skills/ sit next to it at the plugin root. Multi-plugin
# repos use plugins/<name>/ subdirectories (anthropics/claude-plugins-official
# pattern).
#
# Renaming rule: drop the "devboy-" prefix where present (see PLUGIN_NAMING.md).
# The frontmatter `name:` field is rewritten to match the new directory name.
#
# Usage:
#   scripts/release/build-skills.sh             # regenerate
#   scripts/release/build-skills.sh --dry-run   # print actions without writing
#   scripts/release/build-skills.sh --check     # exit 1 if generated tree differs from on-disk
#
# CI runs the script, then `--check` to detect drift.

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
EXPECTED_CODEX_TARGET="../claude/skills"

# Renaming rule. Override here if a skill needs a non-default mapping.
plugin_name_for() {
  local src_name="$1"
  echo "${src_name#devboy-}"
}

generate_to() {
  local dst_root="$1"
  rm -rf "$dst_root"
  mkdir -p "$dst_root"
  while IFS= read -r skill_md; do
    local src_dir src_name plugin_name dst_dir
    src_dir="$(dirname "$skill_md")"
    src_name="$(basename "$src_dir")"
    plugin_name="$(plugin_name_for "$src_name")"
    dst_dir="$dst_root/$plugin_name"
    mkdir -p "$dst_dir"
    # Copy every file in the skill directory (SKILL.md + helpers/templates).
    cp -R "$src_dir"/. "$dst_dir/"
    # Rewrite the `name:` field in the frontmatter and any matching `# H1`
    # body header to match the plugin name. Without the body rewrite the
    # rendered skill would read e.g. `name: setup` but `# devboy-setup`,
    # which is internally inconsistent.
    if [ "$src_name" != "$plugin_name" ]; then
      sed -i.bak -E "
        s/^name:[[:space:]]*${src_name}[[:space:]]*$/name: ${plugin_name}/
        s/^#[[:space:]]+${src_name}[[:space:]]*$/# ${plugin_name}/
      " "$dst_dir/SKILL.md"
      rm -f "$dst_dir/SKILL.md.bak"
    fi
  done < <(find "$SKILLS_SRC" -mindepth 3 -maxdepth 3 -name SKILL.md -type f | sort)
}

generate_dry() {
  while IFS= read -r skill_md; do
    local src_dir src_name plugin_name
    src_dir="$(dirname "$skill_md")"
    src_name="$(basename "$src_dir")"
    plugin_name="$(plugin_name_for "$src_name")"
    if [ "$src_name" = "$plugin_name" ]; then
      printf '  %-32s (no rename)\n' "$src_name"
    else
      printf '  %-32s -> %s\n' "$src_name" "$plugin_name"
    fi
  done < <(find "$SKILLS_SRC" -mindepth 3 -maxdepth 3 -name SKILL.md -type f | sort)
}

drift_check() {
  local rc=0

  # 1. Claude skill tree is in sync with /skills/.
  local tmp
  tmp="$(mktemp -d)"
  generate_to "$tmp"
  if ! diff -ruN "$tmp" "$CLAUDE_DST" >/dev/null 2>&1; then
    rc=1
  fi
  rm -rf "$tmp"

  # 2. Codex skills/ is a symlink pointing at ../claude/skills (relative).
  if [ ! -L "$CODEX_LINK" ]; then
    echo "Drift: $CODEX_LINK is not a symlink (expected -> $EXPECTED_CODEX_TARGET)" >&2
    rc=1
  else
    local actual
    actual="$(readlink "$CODEX_LINK")"
    if [ "$actual" != "$EXPECTED_CODEX_TARGET" ]; then
      echo "Drift: $CODEX_LINK -> $actual, expected -> $EXPECTED_CODEX_TARGET" >&2
      rc=1
    fi
  fi

  if [ "$rc" -ne 0 ]; then
    echo "Run scripts/release/build-skills.sh and commit the result." >&2
    exit 1
  fi
  echo "OK: plugin skill tree matches /skills/ and Codex symlink is correct."
}

case "$MODE" in
  write)
    generate_to "$CLAUDE_DST"
    cc_count=$(find "$CLAUDE_DST" -name SKILL.md | wc -l | tr -d ' ')
    echo "Generated:"
    echo "  Claude Code plugin: $cc_count skills at plugins/claude/skills/"
    # Re-establish the Codex symlink if it is missing OR pointing at the
    # wrong target (e.g. became absolute after a manual move, or got
    # replaced by a directory on a clean Windows checkout).
    mkdir -p "$(dirname "$CODEX_LINK")"
    if [ ! -L "$CODEX_LINK" ] || [ "$(readlink "$CODEX_LINK")" != "$EXPECTED_CODEX_TARGET" ]; then
      rm -rf "$CODEX_LINK"
      ln -s "$EXPECTED_CODEX_TARGET" "$CODEX_LINK"
      echo "  Codex plugin:       (re)created symlink $CODEX_LINK -> $EXPECTED_CODEX_TARGET"
    else
      echo "  Codex plugin:       symlink → $(readlink "$CODEX_LINK") (shared with Claude)"
    fi
    ;;
  dry)
    echo "Would generate (rule: drop 'devboy-' prefix):"
    generate_dry
    ;;
  check)
    drift_check
    ;;
esac
