#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Per-parent subagent pyramid: count + size distribution.

Schema (raw + anon):
  parent_sid, project, subagent_count,
  shark_subs, dolphin_subs, fish_subs, shrimp_subs, plankton_subs,
  median_sub_asst, max_sub_asst
"""
from __future__ import annotations

import argparse
import sys
from datetime import datetime
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session, find_subagents  # noqa: E402
from lib.classifiers import biome_of  # noqa: E402
from lib.stats import median  # noqa: E402
from lib.anonymize import hash_path, SidProjector  # noqa: E402
from lib.io import outputs_dirs, write_parquet  # noqa: E402


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--since", default=None)
    p.add_argument("--outputs", default=None)
    args = p.parse_args(argv)
    since = datetime.fromisoformat(args.since) if args.since else None
    raw_dir, anon_dir = outputs_dirs(Path(args.outputs) if args.outputs else None)
    sid_main = SidProjector("s")
    raw_rows = []
    anon_rows = []
    for jf in find_session_files():
        try:
            sess = load_session(jf, include_subagents=False, since=since)
        except Exception:
            continue
        sub_files = find_subagents(jf)
        sub_asst = []
        for sf in sub_files:
            try:
                sub = load_session(sf, include_subagents=False, since=since)
            except Exception:
                continue
            asst_n = sum(1 for ev in sub.events if ev.type == "assistant")
            sub_asst.append(asst_n)
        if not sub_asst and not sess.events:
            continue
        bins = {"Whale":0, "Shark":0, "Dolphin":0, "Fish":0, "Shrimp":0, "Plankton":0}
        for n in sub_asst:
            bins[biome_of(n)] += 1
        row = {
            "parent_sid": sess.sid,
            "project": sess.project,
            "subagent_count": len(sub_asst),
            "whale_subs": bins["Whale"],
            "shark_subs": bins["Shark"],
            "dolphin_subs": bins["Dolphin"],
            "fish_subs": bins["Fish"],
            "shrimp_subs": bins["Shrimp"],
            "plankton_subs": bins["Plankton"],
            "median_sub_asst": median(sub_asst),
            "max_sub_asst": max(sub_asst) if sub_asst else 0,
        }
        raw_rows.append({**row, "parent_sid": sess.sid, "project": sess.project})
        anon_rows.append({**row, "parent_sid": sid_main.get(sess.sid), "project": hash_path(sess.project)})
    n_raw = write_parquet(raw_rows, raw_dir / "subagent_pyramid.parquet")
    n_anon = write_parquet(anon_rows, anon_dir / "subagent_pyramid.parquet")
    print(f"subagent_pyramid: raw {n_raw}, anon {n_anon}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
