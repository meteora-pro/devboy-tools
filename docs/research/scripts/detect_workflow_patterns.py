#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0"]
# ///
"""
Detect multi-turn workflow patterns from tool_calls + bash_commands + turns.

All detection is pure SQL/Python — no content parsing beyond anonymized
features already in the parquet tables. No LLM.

Patterns emitted (one row per event into workflow_patterns.parquet):
  - self_review_loop           : agent responds to comments on MR it created
  - transcript_to_ticket       : meeting transcript → create_issue/epic
  - ci_iteration_cycle         : push → error → fix → push
  - investigation_to_merge_arc : discovery → edit → commit → push → create_mr
  - stuck_cycle                : same tool_family+action fails ≥3 times close together
  - premature_done             : agent says "done" but subsequent turn has error

Output schema:
  session, pattern, start_ts_ms, end_ts_ms, duration_sec, span_turns, evidence_json
"""
import argparse
import json
import sys
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import duckdb


def detect_patterns(in_dir):
    con = duckdb.connect()
    for tbl in ("tool_calls", "bash_commands", "turns", "sessions", "meta_events"):
        con.execute(f"CREATE VIEW {tbl} AS SELECT * FROM '{in_dir}/{tbl}.parquet'")

    rows = []

    # ─── transcript_to_ticket ────────────────────────────────────────────
    # get_meeting_transcript → create_issue/create_epic within 20 turns (same session)
    r = con.execute("""
      WITH transcripts AS (
        SELECT session, ts_ms AS t_ms,
          row_number() OVER (PARTITION BY session ORDER BY ts_ms) AS r_idx
        FROM tool_calls WHERE tool_family = 'devboy:meetings' AND tool_action = 'detail'
      ),
      creates AS (
        SELECT session, ts_ms,
          CASE WHEN tool_family = 'devboy:issues' THEN 'issue' ELSE 'epic' END AS kind
        FROM tool_calls
        WHERE tool_action = 'create'
          AND tool_family IN ('devboy:issues', 'devboy:mrs')
      )
      SELECT t.session, t.t_ms AS start_ts, c.ts_ms AS end_ts, c.kind
      FROM transcripts t
      JOIN creates c ON t.session = c.session
        AND c.ts_ms > t.t_ms AND c.ts_ms - t.t_ms < 3600000  -- within 1h
    """).fetchall()
    for session, start_ts, end_ts, kind in r:
        rows.append({
            "session": session, "pattern": "transcript_to_ticket",
            "start_ts_ms": start_ts, "end_ts_ms": end_ts,
            "duration_sec": (end_ts - start_ts) / 1000,
            "evidence_json": json.dumps({"created": kind}),
        })

    # ─── self_review_loop ────────────────────────────────────────────────
    # Pattern: create_merge_request → get_merge_request_discussions(same session, later)
    #          → Edit/Write → git push → add_merge_request_comment
    r = con.execute("""
      WITH mr_creates AS (
        SELECT session, ts_ms AS mr_ts
        FROM tool_calls
        WHERE tool_family = 'devboy:mrs' AND tool_action = 'create'
      ),
      discussions AS (
        SELECT session, ts_ms AS disc_ts
        FROM tool_calls
        WHERE tool_category LIKE '%discussion%' AND tool_action = 'detail'
      ),
      mr_comments AS (
        SELECT session, ts_ms AS comment_ts
        FROM tool_calls
        WHERE tool_family = 'devboy:mrs' AND tool_action = 'comment'
      )
      SELECT mc.session,
        mc.mr_ts AS start_ts,
        MIN(com.comment_ts) AS end_ts,
        COUNT(DISTINCT d.disc_ts) AS review_cycles
      FROM mr_creates mc
      JOIN discussions d ON d.session = mc.session AND d.disc_ts > mc.mr_ts
      JOIN mr_comments com ON com.session = mc.session AND com.comment_ts > d.disc_ts
      GROUP BY mc.session, mc.mr_ts
      HAVING COUNT(DISTINCT d.disc_ts) >= 1
    """).fetchall()
    for session, start_ts, end_ts, cycles in r:
        rows.append({
            "session": session, "pattern": "self_review_loop",
            "start_ts_ms": start_ts, "end_ts_ms": end_ts,
            "duration_sec": (end_ts - start_ts) / 1000 if end_ts else 0,
            "evidence_json": json.dumps({"review_cycles": cycles}),
        })

    # ─── ci_iteration_cycle ──────────────────────────────────────────────
    # Pattern: git push → (api_error OR next git push within 30 min, no commit between)
    r = con.execute("""
      WITH pushes AS (
        SELECT session, ts_ms,
          LEAD(ts_ms) OVER (PARTITION BY session ORDER BY ts_ms) AS next_push_ts
        FROM bash_commands WHERE git_subcategory = 'git_push'
      ),
      commits_between AS (
        SELECT p.session, p.ts_ms, p.next_push_ts,
          COUNT(c.ts_ms) AS commits_between
        FROM pushes p
        LEFT JOIN bash_commands c
          ON c.session = p.session AND c.git_subcategory = 'git_commit'
          AND c.ts_ms > p.ts_ms AND c.ts_ms < p.next_push_ts
        WHERE p.next_push_ts IS NOT NULL
        GROUP BY 1, 2, 3
      )
      SELECT session, ts_ms AS start_ts, next_push_ts AS end_ts,
        commits_between AS fixes
      FROM commits_between
      WHERE next_push_ts - ts_ms < 1800000  -- within 30 min
        AND commits_between >= 1            -- at least 1 fix commit in between
    """).fetchall()
    for session, start_ts, end_ts, fixes in r:
        rows.append({
            "session": session, "pattern": "ci_iteration_cycle",
            "start_ts_ms": start_ts, "end_ts_ms": end_ts,
            "duration_sec": (end_ts - start_ts) / 1000,
            "evidence_json": json.dumps({"fixes_between_pushes": fixes}),
        })

    # ─── investigation_to_merge_arc ──────────────────────────────────────
    # Pattern: discovery tools (Grep/Glob/list calls ≥5) → implementation
    # (Edit/Write ≥3) → git commit → git push → create_merge_request within 1h
    r = con.execute("""
      WITH mr_events AS (
        SELECT session, ts_ms AS mr_ts
        FROM tool_calls
        WHERE tool_family = 'devboy:mrs' AND tool_action = 'create'
      ),
      session_stats AS (
        SELECT m.session, m.mr_ts,
          -- count discovery tools in the 2h before mr_create
          (SELECT COUNT(*) FROM tool_calls tc
           WHERE tc.session = m.session
             AND tc.ts_ms BETWEEN m.mr_ts - 7200000 AND m.mr_ts
             AND tc.workflow_role = 'discovery') AS discovery_n,
          (SELECT COUNT(*) FROM tool_calls tc
           WHERE tc.session = m.session
             AND tc.ts_ms BETWEEN m.mr_ts - 7200000 AND m.mr_ts
             AND tc.workflow_role = 'implementation') AS impl_n,
          (SELECT COUNT(*) FROM bash_commands bc
           WHERE bc.session = m.session
             AND bc.ts_ms BETWEEN m.mr_ts - 7200000 AND m.mr_ts
             AND bc.git_subcategory = 'git_commit') AS commits_n,
          (SELECT COUNT(*) FROM bash_commands bc
           WHERE bc.session = m.session
             AND bc.ts_ms BETWEEN m.mr_ts - 7200000 AND m.mr_ts
             AND bc.git_subcategory = 'git_push') AS pushes_n
        FROM mr_events m
      )
      SELECT session, mr_ts, discovery_n, impl_n, commits_n, pushes_n
      FROM session_stats
      WHERE discovery_n >= 5 AND impl_n >= 3 AND commits_n >= 1 AND pushes_n >= 1
    """).fetchall()
    for session, mr_ts, disc_n, impl_n, commits_n, pushes_n in r:
        rows.append({
            "session": session, "pattern": "investigation_to_merge_arc",
            "start_ts_ms": mr_ts - 7200000, "end_ts_ms": mr_ts,
            "duration_sec": 7200.0,  # 2h window
            "evidence_json": json.dumps({
                "discovery": disc_n, "implementation": impl_n,
                "commits": commits_n, "pushes": pushes_n,
            }),
        })

    # ─── stuck_cycle ─────────────────────────────────────────────────────
    # Same (tool_family, tool_action) fails ≥3 times within 5 min
    r = con.execute("""
      WITH errs AS (
        SELECT session, ts_ms, tool_family, tool_action
        FROM tool_calls WHERE is_error = true
      ),
      clustered AS (
        SELECT session, tool_family, tool_action,
          COUNT(*) AS err_count,
          MIN(ts_ms) AS start_ts, MAX(ts_ms) AS end_ts
        FROM errs
        GROUP BY session, tool_family, tool_action
        HAVING COUNT(*) >= 3 AND MAX(ts_ms) - MIN(ts_ms) < 300000
      )
      SELECT session, start_ts, end_ts, tool_family, tool_action, err_count
      FROM clustered
    """).fetchall()
    for session, start_ts, end_ts, family, action, count in r:
        rows.append({
            "session": session, "pattern": "stuck_cycle",
            "start_ts_ms": start_ts, "end_ts_ms": end_ts,
            "duration_sec": (end_ts - start_ts) / 1000,
            "evidence_json": json.dumps({
                "tool": f"{family}/{action}", "repeats": count,
            }),
        })

    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in-dir", default="/tmp/claude_analysis")
    ap.add_argument("--out", default="/tmp/claude_analysis/workflow_patterns.parquet")
    args = ap.parse_args()

    rows = detect_patterns(args.in_dir)
    print(f"Detected {len(rows)} workflow pattern events", file=sys.stderr)

    if not rows:
        print("No rows to write", file=sys.stderr)
        return

    schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("pattern", pa.string()),
        pa.field("start_ts_ms", pa.int64()),
        pa.field("end_ts_ms", pa.int64()),
        pa.field("duration_sec", pa.float64()),
        pa.field("evidence_json", pa.string()),
    ])
    pq.write_table(pa.Table.from_pylist(rows, schema=schema), args.out,
                   compression="zstd")
    size = Path(args.out).stat().st_size
    print(f"Wrote {len(rows)} rows, {size/1024:.0f} KB → {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
