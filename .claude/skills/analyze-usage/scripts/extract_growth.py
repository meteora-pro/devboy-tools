#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Per-session growth metrics + milestone burst-indices.

For each session: prompts cum-curve over its bursts.
- daily_R / daily_LOC (CV / Gini / acceleration on day-buckets)
- milestone_at(R=10, 30, 100, 500) — at which burst index R first reaches threshold
- session-level Gini of LOC across bursts

Outputs:
  growth.parquet      — per-session aggregate metrics
  milestones.parquet  — long-form (sid, threshold, burst_idx)
"""
from __future__ import annotations

import argparse
import sys
from collections import Counter
from datetime import datetime
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session  # noqa: E402
from lib.stats import cv, gini, acceleration, milestone_at  # noqa: E402
from lib.anonymize import hash_path, SidProjector  # noqa: E402
from lib.io import outputs_dirs, write_parquet  # noqa: E402


MILESTONES = (10, 30, 100, 500)


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--since", default=None)
    p.add_argument("--outputs", default=None)
    args = p.parse_args(argv)
    since = datetime.fromisoformat(args.since) if args.since else None
    raw_dir, anon_dir = outputs_dirs(Path(args.outputs) if args.outputs else None)
    sid_main = SidProjector("s")
    growth_raw = []
    growth_anon = []
    miles_raw = []
    miles_anon = []
    for jf in find_session_files():
        try:
            sess = load_session(jf, include_subagents=False, since=since)
        except Exception:
            continue
        if not sess.bursts:
            continue
        # cumulative real prompts (each burst = +1 if not is_notify)
        cum_R = []
        c = 0
        burst_loc = []
        for b in sess.bursts:
            if not b.is_notify:
                c += 1
            cum_R.append(c)
            # LOC per burst (rough): count Edit/Write tools * proxy
            loc = 0
            for tu in b.tool_uses:
                if tu.get("name") in ("Edit", "MultiEdit", "Write"):
                    loc += 1
            burst_loc.append(loc)
        # daily aggregation
        per_day_R: Counter[str] = Counter()
        per_day_LOC: Counter[str] = Counter()
        for b, lo in zip(sess.bursts, burst_loc):
            day = b.start.date().isoformat()
            if not b.is_notify:
                per_day_R[day] += 1
            per_day_LOC[day] += lo
        days = sorted(per_day_R.keys() | per_day_LOC.keys())
        daily_R = [per_day_R.get(d, 0) for d in days]
        daily_LOC = [per_day_LOC.get(d, 0) for d in days]

        row = {
            "sid": sess.sid,
            "project": sess.project,
            "n_bursts": len(sess.bursts),
            "real_prompts": cum_R[-1] if cum_R else 0,
            "n_days": len(days),
            "cv_daily_R": cv(daily_R),
            "gini_daily_R": gini(daily_R),
            "cv_daily_LOC": cv(daily_LOC),
            "gini_daily_LOC": gini(daily_LOC),
            "acceleration_R": acceleration(daily_R),
            "burst_loc_gini": gini(burst_loc),
        }
        growth_raw.append({**row, "sid": sess.sid, "project": sess.project})
        growth_anon.append({**row, "sid": sid_main.get(sess.sid), "project": hash_path(sess.project)})

        for thr in MILESTONES:
            idx = milestone_at(cum_R, thr)
            if idx is None:
                continue
            miles_raw.append({"sid": sess.sid, "project": sess.project, "threshold": thr, "burst_idx": idx})
            miles_anon.append({"sid": sid_main.get(sess.sid), "project": hash_path(sess.project), "threshold": thr, "burst_idx": idx})

    n_g = write_parquet(growth_raw, raw_dir / "growth.parquet")
    write_parquet(growth_anon, anon_dir / "growth.parquet")
    n_m = write_parquet(miles_raw, raw_dir / "milestones.parquet")
    write_parquet(miles_anon, anon_dir / "milestones.parquet")
    print(f"growth: raw {n_g} | milestones: raw {n_m}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
