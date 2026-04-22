#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0"]
# ///
"""
Cross-session linkage: detect bug-fix sessions that likely continue prior work.

Algorithm:
  For each bug_fix_session S:
    1. Same project_hash as some prior session P
    2. P ended within N days before S started
    3. P had meaningful work (tool_calls >= 20)
    4. Link strength = f(project match × time proximity × activity signals)

Output: session_links.parquet
  fix_session, prior_session, hours_since, link_score, reason
"""
import argparse
import sys
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import duckdb


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in-dir", default="/tmp/claude_analysis")
    ap.add_argument("--out", default="/tmp/claude_analysis/session_links.parquet")
    ap.add_argument("--window-days", type=int, default=7,
                    help="Max days to look back")
    args = ap.parse_args()

    con = duckdb.connect()
    con.execute(f"""
      CREATE VIEW se AS SELECT * FROM '{args.in_dir}/sessions_enriched.parquet'
    """)

    # For each main bug_fix session, find candidate prior sessions:
    #   - Same project_hash
    #   - prior end ≤ bug_fix start (chronological order)
    #   - Within window_days
    #   - Prior session was substantive (tool_calls >= 20)
    #   - Prior session's top_tool_categories shares at least one MCP family
    window_ms = args.window_days * 86400 * 1000
    rows = con.execute(f"""
      WITH bf AS (
        SELECT session AS fix_session, ts_start_ms AS fix_start,
          project_hash, top_model, first_human_turn_chars
        FROM se WHERE NOT is_subagent AND is_bug_fix_session = 1
          AND ts_start_ms IS NOT NULL AND project_hash != ''
      ),
      priors AS (
        SELECT session AS prior_session, ts_end_ms AS prior_end,
          project_hash, tool_calls_total, top_model,
          mr_create_count, issue_create_count
        FROM se WHERE NOT is_subagent
          AND tool_calls_total >= 20
          AND project_hash != ''
          AND ts_end_ms IS NOT NULL
      )
      SELECT bf.fix_session, bf.fix_start, bf.project_hash,
        priors.prior_session, priors.prior_end,
        (bf.fix_start - priors.prior_end) / 1000 / 3600.0 AS hours_since,
        priors.tool_calls_total AS prior_calls,
        priors.mr_create_count AS prior_mrs,
        priors.issue_create_count AS prior_issues
      FROM bf
      JOIN priors ON bf.project_hash = priors.project_hash
        AND priors.prior_end < bf.fix_start
        AND priors.prior_end > bf.fix_start - {window_ms}
      WHERE bf.fix_session != priors.prior_session
    """).fetchall()

    # For each fix_session, keep top-3 priors by recency
    # Compute link_score = recency_score × activity_score
    from collections import defaultdict
    by_fix = defaultdict(list)
    for fix, fstart, proj, prior, pend, hrs, calls, mrs, issues in rows:
        # Recency: closer = higher (linear decay within window)
        recency = max(0, 1 - hrs / (args.window_days * 24))
        # Activity: log-scale on tool_calls
        activity = min(1, calls / 500)
        # Boost: prior had MR/issue activity
        boost = 0.2 if (mrs > 0 or issues > 0) else 0
        score = round(recency * 0.6 + activity * 0.3 + boost, 3)
        reasons = []
        if hrs < 24: reasons.append("within_24h")
        elif hrs < 72: reasons.append("within_3d")
        else: reasons.append("within_window")
        if mrs > 0: reasons.append("prior_has_mrs")
        if issues > 0: reasons.append("prior_has_issues")
        if calls >= 200: reasons.append("prior_substantial")
        by_fix[fix].append({
            "fix_session": fix,
            "prior_session": prior,
            "hours_since": round(hrs, 2),
            "link_score": score,
            "reason": "+".join(reasons),
            "prior_tool_calls": calls,
        })

    # Keep top-3 priors per fix_session by score
    out = []
    for fix, candidates in by_fix.items():
        candidates.sort(key=lambda x: -x["link_score"])
        for c in candidates[:3]:
            out.append(c)

    print(f"Fix sessions with candidate priors: {len(by_fix)}", file=sys.stderr)
    print(f"Link edges: {len(out)}", file=sys.stderr)

    if not out:
        print("No links found", file=sys.stderr)
        return

    schema = pa.schema([
        pa.field("fix_session", pa.string()),
        pa.field("prior_session", pa.string()),
        pa.field("hours_since", pa.float64()),
        pa.field("link_score", pa.float64()),
        pa.field("reason", pa.string()),
        pa.field("prior_tool_calls", pa.int32()),
    ])
    pq.write_table(pa.Table.from_pylist(out, schema=schema), args.out,
                   compression="zstd")
    size = Path(args.out).stat().st_size
    print(f"Wrote {len(out)} rows, {size/1024:.0f} KB → {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
