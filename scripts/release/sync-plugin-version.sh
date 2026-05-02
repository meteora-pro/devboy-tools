#!/usr/bin/env bash
#
# Synchronise the plugin manifests with the Cargo workspace version.
#
# Reads `[workspace.package].version` from the root Cargo.toml and writes
# it into:
#   - plugins/claude/.claude-plugin/plugin.json    # version
#   - plugins/codex/.codex-plugin/plugin.json      # version
#   - .claude-plugin/marketplace.json              # plugins[name=devboy].version
#
# Run locally before tagging a release, or via CI on tag pushes.
#
# Usage:
#   scripts/release/sync-plugin-version.sh          # rewrite manifests in place
#   scripts/release/sync-plugin-version.sh --check  # exit 1 if any file is out of sync

set -euo pipefail

MODE=write
case "${1:-}" in
  --check) MODE=check ;;
  "")      MODE=write ;;
  *) echo "Unknown flag: $1" >&2; exit 2 ;;
esac

ROOT="$(git rev-parse --show-toplevel)"
CARGO_TOML="$ROOT/Cargo.toml"
CC_MANIFEST="$ROOT/plugins/claude/.claude-plugin/plugin.json"
CX_MANIFEST="$ROOT/plugins/codex/.codex-plugin/plugin.json"
MARKETPLACE="$ROOT/.claude-plugin/marketplace.json"

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required (brew install jq / apt install jq)" >&2
  exit 1
fi

# Extract `version = "x.y.z"` from the [workspace.package] table.
CARGO_VERSION="$(awk '
  /^\[workspace\.package\]/ {in_ws=1; next}
  /^\[/ && in_ws {in_ws=0}
  in_ws && /^[[:space:]]*version[[:space:]]*=/ {
    match($0, /"[^"]*"/);
    print substr($0, RSTART+1, RLENGTH-2);
    exit
  }
' "$CARGO_TOML")"

if [ -z "$CARGO_VERSION" ]; then
  echo "Cannot find [workspace.package].version in $CARGO_TOML" >&2
  exit 1
fi

echo "Cargo workspace version: $CARGO_VERSION"

write_json_version() {
  local path="$1"
  local jq_filter="$2"
  local tmp
  tmp="$(mktemp)"
  jq --indent 2 --arg v "$CARGO_VERSION" "$jq_filter" "$path" >"$tmp"
  mv "$tmp" "$path"
}

check_json_version() {
  local path="$1"
  local jq_query="$2"
  local got
  got="$(jq -r "$jq_query" "$path")"
  if [ "$got" != "$CARGO_VERSION" ]; then
    echo "  drift: $path has $got, expected $CARGO_VERSION" >&2
    return 1
  fi
  echo "  ok: $path = $got"
}

case "$MODE" in
  write)
    write_json_version "$CC_MANIFEST"  '.version = $v'
    write_json_version "$CX_MANIFEST"  '.version = $v'
    write_json_version "$MARKETPLACE"  '.plugins |= map(if .name == "devboy" then .version = $v else . end)'
    echo "Updated:"
    echo "  $CC_MANIFEST"
    echo "  $CX_MANIFEST"
    echo "  $MARKETPLACE"
    ;;
  check)
    rc=0
    check_json_version "$CC_MANIFEST"  '.version'                                               || rc=1
    check_json_version "$CX_MANIFEST"  '.version'                                               || rc=1
    check_json_version "$MARKETPLACE"  '.plugins[] | select(.name=="devboy") | .version'        || rc=1
    if [ $rc -ne 0 ]; then
      echo "Run scripts/release/sync-plugin-version.sh and commit the result." >&2
      exit 1
    fi
    echo "OK: plugin manifests match Cargo workspace version $CARGO_VERSION."
    ;;
esac
