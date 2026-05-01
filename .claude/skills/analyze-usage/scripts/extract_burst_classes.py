#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Per-session burst-class counts: pure_qa / read_only / bash_only / mixed /
write_heavy / write_only / chain_run.

Per burst we look at its tool-uses and bucket the burst into one of 7 classes:
  - pure_qa     no tools at all
  - read_only   only Read/Grep/Glob/ToolSearch
  - bash_only   only Bash
  - write_only  only Edit/MultiEdit/Write
  - write_heavy ≥50% Edit/Write of tools
  - chain_run   only Bash with chained commands (>=3 split parts)
  - mixed       all the rest
"""
from __future__ import annotations

import argparse
import sys
from collections import Counter
from datetime import datetime
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session, split_chain  # noqa: E402
from lib.anonymize import hash_path, SidProjector  # noqa: E402
from lib.io import outputs_dirs, write_parquet  # noqa: E402


def _classify_burst(b) -> str:
    if not b.tool_uses:
        return "pure_qa"
    cats = Counter()
    chain_lengths = []
    for tu in b.tool_uses:
        n = tu.get("name", "")
        if n in ("Edit", "MultiEdit", "Write"):
            cats["write"] += 1
        elif n in ("Read", "Grep", "Glob", "ToolSearch"):
            cats["read"] += 1
        elif n == "Bash":
            cats["bash"] += 1
            cmd = (tu.get("input", {}) or {}).get("command", "") or ""
            chain_lengths.append(len(split_chain(cmd)))
        else:
            cats["other"] += 1
    total = sum(cats.values())
    if cats["read"] == total:
        return "read_only"
    if cats["write"] == total:
        return "write_only"
    if cats["bash"] == total:
        if chain_lengths and max(chain_lengths) >= 3:
            return "chain_run"
        return "bash_only"
    if cats["write"] >= total / 2:
        return "write_heavy"
    return "mixed"


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
    classes = ("pure_qa","read_only","bash_only","write_only","write_heavy","chain_run","mixed")
    for jf in find_session_files():
        try:
            sess = load_session(jf, include_subagents=False, since=since)
        except Exception:
            continue
        cnt = Counter()
        for b in sess.bursts:
            cnt[_classify_burst(b)] += 1
        if not cnt:
            continue
        row = {"sid": sess.sid, "project": sess.project,
               "burst_total": sum(cnt.values()), **{c: cnt.get(c, 0) for c in classes}}
        raw_rows.append(row)
        anon_rows.append({**row, "sid": sid_main.get(sess.sid), "project": hash_path(sess.project)})
    n_raw = write_parquet(raw_rows, raw_dir / "burst_classes.parquet")
    n_anon = write_parquet(anon_rows, anon_dir / "burst_classes.parquet")
    print(f"burst_classes: raw {n_raw}, anon {n_anon}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
