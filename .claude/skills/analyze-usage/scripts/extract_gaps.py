#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Idle-gap detection: per-session list of gaps >8h with suggested split point.

Schema (raw + anon):
  sid, project, gap_idx, gap_start, gap_end, gap_hours, before_burst_idx,
  after_burst_idx
"""
from __future__ import annotations

import argparse
import sys
from datetime import datetime, timedelta
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session  # noqa: E402
from lib.anonymize import hash_path, SidProjector  # noqa: E402
from lib.io import outputs_dirs, write_parquet  # noqa: E402


GAP_HOURS = 8


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--since", default=None)
    p.add_argument("--outputs", default=None)
    p.add_argument("--threshold-hours", type=int, default=GAP_HOURS)
    args = p.parse_args(argv)
    since = datetime.fromisoformat(args.since) if args.since else None
    raw_dir, anon_dir = outputs_dirs(Path(args.outputs) if args.outputs else None)
    sid_main = SidProjector("s")
    raw_rows = []
    anon_rows = []
    threshold = timedelta(hours=args.threshold_hours)
    for jf in find_session_files():
        try:
            sess = load_session(jf, include_subagents=False, since=since)
        except Exception:
            continue
        if len(sess.bursts) < 2:
            continue
        for i in range(1, len(sess.bursts)):
            gap = sess.bursts[i].start - sess.bursts[i - 1].end
            if gap < threshold:
                continue
            raw_rows.append({
                "sid": sess.sid, "project": sess.project,
                "gap_idx": i,
                "gap_start": sess.bursts[i - 1].end.isoformat(),
                "gap_end": sess.bursts[i].start.isoformat(),
                "gap_hours": gap.total_seconds() / 3600,
                "before_burst_idx": i - 1,
                "after_burst_idx": i,
            })
            anon_rows.append({
                "sid": sid_main.get(sess.sid), "project": hash_path(sess.project),
                "gap_idx": i,
                "gap_hours": gap.total_seconds() / 3600,
                "before_burst_idx": i - 1,
                "after_burst_idx": i,
            })
    n_raw = write_parquet(raw_rows, raw_dir / "idle_gaps.parquet")
    n_anon = write_parquet(anon_rows, anon_dir / "idle_gaps.parquet")
    print(f"idle_gaps: raw {n_raw}, anon {n_anon}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
