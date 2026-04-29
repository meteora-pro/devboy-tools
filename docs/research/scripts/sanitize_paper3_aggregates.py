#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=18.0", "pandas>=2.0"]
# ///
"""
Paper 3 — research aggregate sanitiser.

Reads the per-event parquet emitted by `extract_paper3_followups.py`
(which lives in /tmp and is *not* committed) and writes anonymised,
k-anonymised CSV aggregates that are safe to land in `docs/research/data/`.

Anonymity contract for the committed artifacts:

  - No session_hash, no turn_idx, no tool_use_id_hash.
  - No per-event records. Every row is an aggregate over ≥ K_THRESHOLD
    sessions or follow-up occurrences (default K = 5).
  - Tool names already pre-anonymised by the extractor
    (`mcp__<slug>__<verb>` → `mcp__p<hash6>__<verb>`).
  - Response sizes reported only as aggregate percentiles
    (median / p90 / p99) — never as a raw distribution or as `max`,
    which could be matched to a single high-watermark response.
  - "chars" everywhere = string code-point length (`len(text)`),
    not UTF-8 bytes. The KB priors derived from these aggregates
    carry the same semantics; see Paper 2 §Tokenizer-aware metering
    for the cl100k/o200k correction factor.
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

import pyarrow.parquet as pq
import pandas as pd

K_THRESHOLD = 5  # drop edges + tools observed fewer than this many times


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--events",
        type=Path,
        default=Path("/tmp/claude_analysis/paper3_tool_events.parquet"),
    )
    ap.add_argument(
        "--edges",
        type=Path,
        default=Path("/tmp/claude_analysis/paper3_followup_edges.parquet"),
    )
    ap.add_argument(
        "--output-dir",
        type=Path,
        default=Path("docs/research/data"),
    )
    ap.add_argument("--k-threshold", type=int, default=K_THRESHOLD)
    args = ap.parse_args()

    events = pq.read_table(args.events).to_pandas()
    edges = pq.read_table(args.edges).to_pandas()
    args.output_dir.mkdir(parents=True, exist_ok=True)

    # ─── Tool volume + size aggregate ────────────────────────────────
    tool_volume = (
        events.groupby("tool_anon")
        .agg(
            calls=("response_chars", "size"),
            sessions=("session_hash", "nunique"),
            response_chars_median=("response_chars", "median"),
            response_chars_p90=("response_chars", lambda s: int(s.quantile(0.9))),
            response_chars_p99=("response_chars", lambda s: int(s.quantile(0.99))),
            # `max` intentionally absent — it is a per-session
            # high-watermark and the easiest fingerprinting target;
            # see the anonymity contract in this module's docstring.
        )
        .reset_index()
        .sort_values("calls", ascending=False)
    )
    # k-anonymity: drop tools called by fewer than K distinct sessions.
    keep = tool_volume["sessions"] >= args.k_threshold
    dropped = (~keep).sum()
    tool_volume_safe = tool_volume[keep].copy()
    # Cast medians to int — small float drift from pandas could leak
    # bit-level info; integer chars are public signal anyway.
    tool_volume_safe["response_chars_median"] = (
        tool_volume_safe["response_chars_median"].astype(int)
    )

    out_volume = args.output_dir / "paper3_tool_volume.csv"
    tool_volume_safe.to_csv(out_volume, index=False)
    print(
        f"wrote {out_volume} — {len(tool_volume_safe):,} tools "
        f"(dropped {dropped} below k={args.k_threshold})"
    )

    # ─── Follow-up edges aggregate ───────────────────────────────────
    edges_safe = edges[edges["count"] >= args.k_threshold].copy()
    edges_safe = edges_safe.sort_values(["lag", "count"], ascending=[True, False])
    out_edges = args.output_dir / "paper3_followup_edges.csv"
    edges_safe.to_csv(out_edges, index=False)
    print(
        f"wrote {out_edges} — {len(edges_safe):,} edges "
        f"(dropped {len(edges) - len(edges_safe)} below k={args.k_threshold})"
    )

    # ─── Inline preview for the reviewer ─────────────────────────────
    print("\n## Top 25 follow-up edges (lag = 1) after k-anonymity:")
    top = edges_safe[edges_safe["lag"] == 1].head(25)
    print(top.to_string(index=False))

    print("\n## Top 15 tools by volume after k-anonymity:")
    print(
        tool_volume_safe[
            [
                "tool_anon",
                "calls",
                "sessions",
                "response_chars_median",
                "response_chars_p90",
                "response_chars_p99",
            ]
        ].head(15).to_string(index=False)
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
