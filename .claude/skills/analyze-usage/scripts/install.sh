#!/usr/bin/env bash
# install.sh — fetch the analyze-usage skill into ~/.claude/skills/ without
# cloning the whole devboy-tools repo.
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/meteora-pro/devboy-tools/main/.claude/skills/analyze-usage/scripts/install.sh | bash
#
# Or with a pinned version:
#   REF=v0.22.0 curl ... | bash

set -e

REPO="${REPO:-meteora-pro/devboy-tools}"
REF="${REF:-main}"
DEST="${DEST:-$HOME/.claude/skills/analyze-usage}"

echo "→ Installing analyze-usage skill"
echo "  repo: $REPO @ $REF"
echo "  dest: $DEST"

if ! command -v git >/dev/null 2>&1; then
    echo "✗ git is required" >&2
    exit 1
fi
if ! command -v uv >/dev/null 2>&1; then
    echo "⚠  uv is required to run the skill — install: https://docs.astral.sh/uv/" >&2
fi

# Sparse-checkout the subdir only
TMP=$(mktemp -d)
trap "rm -rf $TMP" EXIT

git clone --depth 1 --filter=blob:none --sparse \
    --branch "$REF" \
    "https://github.com/${REPO}.git" "$TMP/repo" >/dev/null 2>&1

cd "$TMP/repo"
git sparse-checkout set ".claude/skills/analyze-usage" >/dev/null 2>&1

# Move into place
mkdir -p "$(dirname "$DEST")"
if [ -d "$DEST" ]; then
    echo "⚠  $DEST already exists — backing up to ${DEST}.backup-$(date +%s)"
    mv "$DEST" "${DEST}.backup-$(date +%s)"
fi
mv "$TMP/repo/.claude/skills/analyze-usage" "$DEST"

# Make CLI executable
chmod +x "$DEST/bin/analyze-usage" 2>/dev/null || true

echo
echo "✓ installed to $DEST"
echo
echo "Add to your PATH (zsh/bash):"
echo "  echo 'export PATH=\"$DEST/bin:\$PATH\"' >> ~/.zshrc"
echo
echo "Or run directly:"
echo "  $DEST/bin/analyze-usage --help"
