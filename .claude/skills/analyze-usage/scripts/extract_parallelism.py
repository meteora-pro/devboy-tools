#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Hourly concurrent session count by date/hour bin.

Schema:
  parallelism.parquet — one row per (date, hour) with year, month, day,
  hour, iso_date, concurrent_sessions.
"""
from __future__ import annotations

import argparse
import sys
from collections import defaultdict
from datetime import datetime
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session  # noqa: E402
from lib.io import outputs_dirs, write_parquet  # noqa: E402


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--since", default=None)
    p.add_argument("--outputs", default=None)
    args = p.parse_args(argv)
    since = datetime.fromisoformat(args.since) if args.since else None
    raw_dir, anon_dir = outputs_dirs(Path(args.outputs) if args.outputs else None)

    # For each session: list of (year, month, day, hour) bins where it had ≥1 event
    hour_bin_to_sessions: dict[tuple, set[str]] = defaultdict(set)
    for jf in find_session_files():
        try:
            sess = load_session(jf, include_subagents=False, since=since)
        except Exception:
            continue
        for ev in sess.events:
            t = ev.ts
            key = (t.year, t.month, t.day, t.hour)
            hour_bin_to_sessions[key].add(sess.sid)

    rows = []
    for (y, m, d, h), sids in sorted(hour_bin_to_sessions.items()):
        rows.append({
            "year": y, "month": m, "day": d, "hour": h,
            "iso_date": f"{y:04d}-{m:02d}-{d:02d}",
            "concurrent_sessions": len(sids),
        })

    n = write_parquet(rows, raw_dir / "parallelism.parquet")
    write_parquet(rows, anon_dir / "parallelism.parquet")  # already aggregated, safe to anon-mirror
    print(f"parallelism: {n} hour-bins", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
