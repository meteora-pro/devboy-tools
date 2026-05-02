#!/usr/bin/env bash
#
# Generate plugin skill trees for Claude Code and Codex from /skills/.
#
# Source of truth: skills/<category>/<name>/SKILL.md (and any sibling files).
# Target:
#   plugins/claude/skills/<rule(name)>/SKILL.md
#   plugins/codex/skills/<rule(name)>/SKILL.md
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
CODEX_DST="$ROOT/plugins/codex/skills"

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
    # Rewrite the `name:` field in the frontmatter to match the plugin name.
    if [ "$src_name" != "$plugin_name" ]; then
      sed -i.bak -E "s/^name:[[:space:]]*${src_name}[[:space:]]*$/name: ${plugin_name}/" "$dst_dir/SKILL.md"
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
  local tmp_claude tmp_codex
  tmp_claude="$(mktemp -d)"
  tmp_codex="$(mktemp -d)"
  CLAUDE_DST="$tmp_claude" CODEX_DST="$tmp_codex" generate_to "$tmp_claude"
  CLAUDE_DST="$tmp_claude" CODEX_DST="$tmp_codex" generate_to "$tmp_codex"
  local rc=0
  diff -ruN "$tmp_claude" "$ROOT/plugins/claude/skills" >/dev/null 2>&1 || rc=1
  diff -ruN "$tmp_codex"  "$ROOT/plugins/codex/skills"  >/dev/null 2>&1 || rc=1
  rm -rf "$tmp_claude" "$tmp_codex"
  if [ "$rc" -ne 0 ]; then
    echo "Drift detected: plugin skill tree is out of sync with /skills/." >&2
    echo "Run scripts/release/build-skills.sh and commit the result." >&2
    exit 1
  fi
  echo "OK: plugin skill trees match /skills/."
}

case "$MODE" in
  write)
    generate_to "$CLAUDE_DST"
    generate_to "$CODEX_DST"
    cc_count=$(find "$CLAUDE_DST" -name SKILL.md | wc -l | tr -d ' ')
    cx_count=$(find "$CODEX_DST"  -name SKILL.md | wc -l | tr -d ' ')
    echo "Generated:"
    echo "  Claude Code plugin: $cc_count skills at plugins/claude/skills/"
    echo "  Codex plugin:       $cx_count skills at plugins/codex/skills/"
    ;;
  dry)
    echo "Would generate (rule: drop 'devboy-' prefix):"
    generate_dry
    ;;
  check)
    drift_check
    ;;
esac
