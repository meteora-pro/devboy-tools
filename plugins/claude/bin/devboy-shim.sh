#!/usr/bin/env bash
#
# Locate the `devboy` binary across the install paths the `setup` skill may
# have used:
#   1. ${CLAUDE_PLUGIN_DATA}/bin/devboy — GitHub Release fallback (offline / no npm)
#   2. devboy on $PATH                  — npm install -g, brew, manual cp
#
# Exec into the first one found. Print a hint to /devboy-meteora:setup if both
# are missing.

set -e

if [ -n "${CLAUDE_PLUGIN_DATA:-}" ] && [ -x "$CLAUDE_PLUGIN_DATA/bin/devboy" ]; then
  exec "$CLAUDE_PLUGIN_DATA/bin/devboy" "$@"
fi

if command -v devboy >/dev/null 2>&1; then
  exec devboy "$@"
fi

cat >&2 <<EOF
devboy: binary not found.

The Claude Code plugin "devboy@meteora-devboy" is loaded but the CLI itself
is not installed. Run the bundled bootstrap skill:

  /devboy-meteora:setup

Or install manually:

  npm install -g @devboy-tools/cli

After installation, run /reload-plugins for Claude Code to pick up the MCP
server.
EOF
exit 127
