#!/usr/bin/env bash
#
# Locate the `devboy` binary across the install paths the `setup` skill
# may have used. Works in both Claude Code plugin context
# (`$CLAUDE_PLUGIN_DATA`) and Codex plugin context (`$CODEX_PLUGIN_DATA`).
#
# Lookup order:
#   1. ${CLAUDE_PLUGIN_DATA:-$CODEX_PLUGIN_DATA}/bin/devboy
#                                                — GitHub Release fallback
#                                                  (offline / no npm)
#   2. devboy on $PATH                          — npm install -g, brew, manual cp
#
# Exec into the first one found. Print a hint to /devboy-meteora:setup if
# both are missing.

set -e

PLUGIN_DATA="${CLAUDE_PLUGIN_DATA:-${CODEX_PLUGIN_DATA:-}}"

if [ -n "$PLUGIN_DATA" ] && [ -x "$PLUGIN_DATA/bin/devboy" ]; then
  exec "$PLUGIN_DATA/bin/devboy" "$@"
fi

if command -v devboy >/dev/null 2>&1; then
  exec devboy "$@"
fi

cat >&2 <<EOF
devboy: binary not found.

The plugin "devboy@meteora-devboy" is loaded but the CLI itself is not
installed. Run the bundled bootstrap skill:

  /devboy-meteora:setup

Or install manually:

  npm install -g @devboy-tools/cli

After installation, run /reload-plugins (Claude Code) or restart your
Codex session for the MCP server to bind.
EOF
exit 127
