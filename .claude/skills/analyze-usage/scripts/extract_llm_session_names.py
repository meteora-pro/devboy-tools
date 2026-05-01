#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Tier-2 (agent-side LLM) extractor: per-session narrative names.

Reads `outputs/raw/biome.parquet` to know which sessions exist, then for
each Whale/Shark/Dolphin spawns the agent (via stdin protocol) with the
session jsonl path and asks for: name (≤8 words), one-paragraph summary,
phase list, pivot count.

This script does **not** call an LLM directly — it emits a JSONL queue
to `outputs/llm/_queue.jsonl` that the agent consumes when it runs the
skill. Each queue entry has:
  {sid, jsonl_path, why_selected, status: "pending"}

When the agent reads the queue and writes back narrative, it appends to
`outputs/llm/session_names.parquet` with schema:
  sid, project, name, summary, phases (list[str]), pivots, generator,
  generated_at

Why split: Tier 1 must be reproducible offline (pure stat). Tier 2
narrative depends on the agent — keeping it as a separate parquet
ensures Tier 1 stays deterministic and shareable, while Tier 2 enriches
when the agent is involved.
"""
from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime
from pathlib import Path

import pyarrow.parquet as pq

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.io import outputs_dirs_v2  # noqa: E402


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--outputs", default=None)
    p.add_argument("--biomes", nargs="+", default=["Whale", "Shark", "Dolphin"],
                   help="Biome classes to enqueue for narrative")
    args = p.parse_args(argv)
    raw, _, llm = outputs_dirs_v2(Path(args.outputs) if args.outputs else None)

    biome_path = raw / "biome.parquet"
    if not biome_path.exists():
        print(f"missing {biome_path} — run pipeline.py first", file=sys.stderr)
        return 1

    rows = pq.read_table(biome_path).to_pylist()
    queue: list[dict] = []
    for r in rows:
        if r["biome"] not in args.biomes:
            continue
        queue.append({
            "sid": r["sid"],
            "project": r["project"],
            "biome": r["biome"],
            "jsonl_path": r.get("jsonl_path", ""),
            "real_prompts": r["real_prompts"],
            "status": "pending",
            "queued_at": datetime.now().isoformat(timespec="seconds"),
        })

    queue_path = llm / "_queue.jsonl"
    queue_path.write_text("\n".join(json.dumps(q, ensure_ascii=False) for q in queue))
    print(f"queue: {len(queue)} sessions → {queue_path}", file=sys.stderr)

    # Stub the parquet so consumers know it exists (empty until agent fills)
    if not (llm / "session_names.parquet").exists():
        from lib.io import write_parquet
        write_parquet([], llm / "session_names.parquet")
        print(f"  empty stub → {llm}/session_names.parquet", file=sys.stderr)

    print(
        f"\nNext step: agent reads {queue_path}, summarises each session, "
        f"appends rows to {llm}/session_names.parquet.",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
