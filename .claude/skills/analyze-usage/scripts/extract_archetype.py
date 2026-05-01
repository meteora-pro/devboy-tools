#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Per-session archetype + rhythm classification.

Schema (raw + anon):
  sid, project, edit_count, bash_count, read_count, archetype, rhythm,
  edit_share, bash_share, read_share, total_tools
"""
from __future__ import annotations

import argparse
import sys
from datetime import datetime
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session, find_subagents  # noqa: E402
from lib.classifiers import (  # noqa: E402
    archetype_of, rhythm_of, bash_sub_of,
    ARCHETYPE_EMOJI, RHYTHM_EMOJI,
)
from lib.anonymize import hash_path, SidProjector  # noqa: E402
from lib.io import outputs_dirs, write_parquet  # noqa: E402


def _classify_session(events) -> tuple[int, int, int, list[str]]:
    edit = bash = read = 0
    seq: list[str] = []
    for ev in events:
        if ev.type != "assistant":
            continue
        for tu in ev.tool_uses:
            n = tu.get("name", "")
            if n in ("Edit", "MultiEdit", "Write"):
                edit += 1
                seq.append("Edit")
            elif n in ("Read", "Grep", "Glob", "ToolSearch"):
                read += 1
                seq.append("Read")
            elif n == "Bash":
                bash += 1
                cmd = (tu.get("input", {}) or {}).get("command", "") or ""
                seq.append(f"Bash:{bash_sub_of(cmd)}")
    return edit, bash, read, seq


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--since", default=None)
    p.add_argument("--outputs", default=None)
    args = p.parse_args(argv)

    since = datetime.fromisoformat(args.since) if args.since else None
    raw_dir, anon_dir = outputs_dirs(Path(args.outputs) if args.outputs else None)

    sid_main = SidProjector("s")
    sid_sub = SidProjector("a")
    raw_rows = []
    anon_rows = []

    for jf in find_session_files():
        try:
            sess = load_session(jf, include_subagents=False, since=since)
        except Exception:
            continue
        if not sess.events:
            continue
        e, b, r, seq = _classify_session(sess.events)
        total = e + b + r
        arch = archetype_of(e, b, r)
        rh = rhythm_of(seq)
        raw_rows.append({
            "sid": sess.sid, "project": sess.project,
            "edit_count": e, "bash_count": b, "read_count": r,
            "edit_share": e / total if total else 0.0,
            "bash_share": b / total if total else 0.0,
            "read_share": r / total if total else 0.0,
            "total_tools": total,
            "archetype": arch, "archetype_emoji": ARCHETYPE_EMOJI[arch],
            "rhythm": rh, "rhythm_emoji": RHYTHM_EMOJI[rh],
        })
        anon_rows.append({
            "sid": sid_main.get(sess.sid), "project": hash_path(sess.project),
            "edit_count": e, "bash_count": b, "read_count": r,
            "edit_share": e / total if total else 0.0,
            "bash_share": b / total if total else 0.0,
            "read_share": r / total if total else 0.0,
            "total_tools": total,
            "archetype": arch, "rhythm": rh,
        })
        for sf in find_subagents(jf):
            try:
                sub = load_session(sf, include_subagents=False, since=since)
            except Exception:
                continue
            if not sub.events:
                continue
            e2, b2, r2, seq2 = _classify_session(sub.events)
            total2 = e2 + b2 + r2
            arch2 = archetype_of(e2, b2, r2)
            rh2 = rhythm_of(seq2)
            raw_rows.append({
                "sid": "sub_" + sub.sid, "project": sess.project,
                "edit_count": e2, "bash_count": b2, "read_count": r2,
                "edit_share": e2 / total2 if total2 else 0.0,
                "bash_share": b2 / total2 if total2 else 0.0,
                "read_share": r2 / total2 if total2 else 0.0,
                "total_tools": total2,
                "archetype": arch2, "archetype_emoji": ARCHETYPE_EMOJI[arch2],
                "rhythm": rh2, "rhythm_emoji": RHYTHM_EMOJI[rh2],
            })
            anon_rows.append({
                "sid": sid_sub.get(sub.sid), "project": hash_path(sess.project),
                "edit_count": e2, "bash_count": b2, "read_count": r2,
                "edit_share": e2 / total2 if total2 else 0.0,
                "bash_share": b2 / total2 if total2 else 0.0,
                "read_share": r2 / total2 if total2 else 0.0,
                "total_tools": total2,
                "archetype": arch2, "rhythm": rh2,
            })

    n_raw = write_parquet(raw_rows, raw_dir / "archetype.parquet")
    n_anon = write_parquet(anon_rows, anon_dir / "archetype.parquet")
    print(f"archetype: raw {n_raw}, anon {n_anon}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
