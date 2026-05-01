#!/usr/bin/env bash
# Run all tests for analyze-usage skill.
set -e
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

echo "━━ test_classifiers"
uv run --quiet --with pyarrow test_classifiers.py

echo
echo "━━ test_stats"
uv run --quiet test_stats.py

echo
echo "━━ test_extractors_smoke"
uv run --quiet --with pyarrow test_extractors_smoke.py

echo
echo "━━ test_audit_anon"
uv run --quiet --with pyarrow test_audit_anon.py

echo
echo "✓ all tests passed"
