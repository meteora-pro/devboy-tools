#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Per-session compact pattern: how many compacts, max context size before each.

Schema (raw + anon):
  sid, project, compacts, real_prompts_before_first_compact,
  context_size_max, asst_count, compact_per_asst
"""
from __future__ import annotations

import argparse
import sys
from datetime import datetime
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session  # noqa: E402
from lib.anonymize import hash_path, SidProjector  # noqa: E402
from lib.io import outputs_dirs, write_parquet  # noqa: E402


def _stats(events) -> dict:
    compacts = 0
    real_before = 0
    real_seen = 0
    seen_first = False
    ctx_max = 0
    asst = 0
    for ev in events:
        if ev.type == "compact":
            compacts += 1
            if not seen_first:
                real_before = real_seen
                seen_first = True
        elif ev.type == "user" and ev.user_kind == "real_human":
            real_seen += 1
        elif ev.type == "assistant":
            asst += 1
            ctx = ev.in_tokens + ev.cache_read + ev.cache_create
            if ctx > ctx_max:
                ctx_max = ctx
    return {
        "compacts": compacts,
        "real_prompts_before_first_compact": real_before,
        "context_size_max": ctx_max,
        "asst_count": asst,
    }


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
        if not sess.events:
            continue
        s = _stats(sess.events)
        cpa = s["compacts"] / s["asst_count"] if s["asst_count"] else 0.0
        raw_rows.append({
            "sid": sess.sid, "project": sess.project,
            **s, "compact_per_asst": cpa,
        })
        anon_rows.append({
            "sid": sid_main.get(sess.sid), "project": hash_path(sess.project),
            **s, "compact_per_asst": cpa,
        })
    n_raw = write_parquet(raw_rows, raw_dir / "compact_pattern.parquet")
    n_anon = write_parquet(anon_rows, anon_dir / "compact_pattern.parquet")
    print(f"compact_pattern: raw {n_raw}, anon {n_anon}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
