#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Per-session biome classification.

Schema (raw):
  sid, project, real_prompts, asst_count, biome, biome_emoji,
  first_ts, last_ts, jsonl_path

Schema (anon):
  sid (s0001/a0001), project (p<6hex>), real_prompts, asst_count, biome,
  first_iso_week, last_iso_week
"""

from __future__ import annotations

import argparse
import sys
from datetime import datetime
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session, find_subagents  # noqa: E402
from lib.classifiers import biome_of, BIOME_EMOJI  # noqa: E402
from lib.anonymize import hash_path, SidProjector  # noqa: E402
from lib.io import outputs_dirs, write_parquet  # noqa: E402


def _iso_week(ts: datetime | None) -> str:
    if ts is None:
        return ""
    iy, iw, _ = ts.isocalendar()
    return f"{iy}-W{iw:02d}"


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--since", default=None, help="YYYY-MM-DD")
    p.add_argument("--outputs", default=None)
    args = p.parse_args(argv)

    since = datetime.fromisoformat(args.since) if args.since else None
    raw_dir, anon_dir = outputs_dirs(Path(args.outputs) if args.outputs else None)

    sid_proj = SidProjector("s")
    sub_proj = SidProjector("a")

    raw_rows: list[dict] = []
    anon_rows: list[dict] = []

    files = find_session_files()
    for jf in files:
        try:
            sess = load_session(jf, include_subagents=False, since=since)
        except Exception:
            continue
        if not sess.events:
            continue
        real = sum(1 for ev in sess.events if ev.type == "user" and ev.user_kind == "real_human")
        asst = sum(1 for ev in sess.events if ev.type == "assistant")
        b = biome_of(real)
        first_ts = sess.events[0].ts
        last_ts = sess.events[-1].ts
        raw_rows.append({
            "sid": sess.sid,
            "project": sess.project,
            "real_prompts": real,
            "asst_count": asst,
            "biome": b,
            "biome_emoji": BIOME_EMOJI[b],
            "first_ts": first_ts.isoformat() if first_ts else None,
            "last_ts": last_ts.isoformat() if last_ts else None,
            "jsonl_path": str(jf),
        })
        anon_rows.append({
            "sid": sid_proj.get(sess.sid),
            "project": hash_path(sess.project),
            "real_prompts": real,
            "asst_count": asst,
            "biome": b,
            "first_iso_week": _iso_week(first_ts),
            "last_iso_week": _iso_week(last_ts),
        })
        # subagents
        for sf in find_subagents(jf):
            try:
                sub = load_session(sf, include_subagents=False, since=since)
            except Exception:
                continue
            if not sub.events:
                continue
            asst_s = sum(1 for ev in sub.events if ev.type == "assistant")
            # subagent biome thresholds differ; reuse asst not real
            b_sub = biome_of(asst_s)  # threshold-by-asst proxy
            raw_rows.append({
                "sid": "sub_" + sub.sid,
                "project": sess.project,
                "real_prompts": 0,
                "asst_count": asst_s,
                "biome": b_sub,
                "biome_emoji": BIOME_EMOJI[b_sub],
                "first_ts": sub.events[0].ts.isoformat() if sub.events[0].ts else None,
                "last_ts": sub.events[-1].ts.isoformat() if sub.events[-1].ts else None,
                "jsonl_path": str(sf),
            })
            anon_rows.append({
                "sid": sub_proj.get(sub.sid),
                "project": hash_path(sess.project),
                "real_prompts": 0,
                "asst_count": asst_s,
                "biome": b_sub,
                "first_iso_week": _iso_week(sub.events[0].ts),
                "last_iso_week": _iso_week(sub.events[-1].ts),
            })

    n_raw = write_parquet(raw_rows, raw_dir / "biome.parquet")
    n_anon = write_parquet(anon_rows, anon_dir / "biome.parquet")
    sid_proj.dump(raw_dir / "_sid_main.json")
    sub_proj.dump(raw_dir / "_sid_sub.json")
    print(f"biome: raw {n_raw} rows, anon {n_anon} rows", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
