#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Pipeline orchestrator: runs all extractors and writes raw + anon parquet bundles.

Usage:
  pipeline.py --since 2026-04-01 --outputs .claude/skills/analyze-usage/outputs/
"""
from __future__ import annotations

import argparse
import importlib
import sys
import time
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

EXTRACTORS = [
    "extract_biome",
    "extract_archetype",
    "extract_outputs",
    "extract_compacts",
    "extract_growth",
    "extract_subagents",
    "extract_gaps",
    "extract_burst_classes",
    "extract_parallelism",
    "extract_topics",
]


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--since", default=None, help="YYYY-MM-DD")
    p.add_argument("--outputs", default=None)
    p.add_argument("--only", nargs="*", default=None,
                   help="Run only listed extractors (without 'extract_' prefix)")
    args = p.parse_args(argv)

    sys.path.insert(0, str(_HERE))  # find sibling extract_*.py
    targets = EXTRACTORS
    if args.only:
        wanted = {f"extract_{x}" if not x.startswith("extract_") else x for x in args.only}
        targets = [t for t in EXTRACTORS if t in wanted]

    summary: list[tuple[str, float, int]] = []
    for name in targets:
        t0 = time.time()
        print(f"━━ {name} …", file=sys.stderr)
        mod = importlib.import_module(name)
        sub_argv = []
        if args.since:
            sub_argv += ["--since", args.since]
        if args.outputs:
            sub_argv += ["--outputs", args.outputs]
        rc = mod.main(sub_argv)
        elapsed = time.time() - t0
        summary.append((name, elapsed, rc))

    print("\n━━ summary ━━", file=sys.stderr)
    for name, elapsed, rc in summary:
        mark = "✓" if rc == 0 else "✗"
        print(f"  {mark}  {name:<28s} {elapsed:>6.1f}s", file=sys.stderr)
    return 0 if all(rc == 0 for _, _, rc in summary) else 1


if __name__ == "__main__":
    raise SystemExit(main())
