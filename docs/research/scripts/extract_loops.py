#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0"]
# ///
"""
Build per-loop aggregates — one row per (session, loop_id).

A loop starts at a human turn (or session start) and ends at next human turn
(or session end). All agent/tool_result turns in between belong to that loop.

Captures: volume, phase distribution, cost, output ("what agent delivered"),
and Paper-1-specific signals for list/enrich patterns.

Output: /tmp/claude_analysis/loops.parquet
"""
import argparse
import sys
from pathlib import Path

import duckdb
import pyarrow as pa


LOOPS_SQL = """
WITH
  -- Turn-level rollup per (session, loop_id)
  turn_agg AS (
    SELECT session, loop_id,
      MIN(ts_ms) AS loop_start_ms,
      MAX(ts_ms) AS loop_end_ms,
      COUNT(*) FILTER (WHERE turn_kind = 'agent') AS agent_turns,
      COUNT(*) FILTER (WHERE turn_kind = 'tool_result') AS tool_results,
      COUNT(*) FILTER (WHERE turn_kind = 'human') AS human_turns_in_loop,
      SUM(tool_calls_in_turn) AS tool_calls,
      SUM(in_tokens + cache_read + cache_create) AS prompt_tokens,
      SUM(out_tokens) AS output_tokens,
      MAX(context_size_tokens) AS ctx_max_in_loop,
      AVG(context_size_tokens) FILTER (WHERE turn_kind = 'agent' AND context_size_tokens > 0) AS ctx_mean_in_loop
    FROM turns
    GROUP BY session, loop_id
  ),
  -- Tool call phase/family/action rollup per loop
  tool_agg AS (
    SELECT session, loop_id,
      COUNT(*) AS tool_call_count,
      COUNT(DISTINCT tool_category) AS tool_diversity,
      -- Phase counts
      COUNT(*) FILTER (WHERE workflow_phase = 'plan') AS phase_plan,
      COUNT(*) FILTER (WHERE workflow_phase = 'research') AS phase_research,
      COUNT(*) FILTER (WHERE workflow_phase = 'implement') AS phase_implement,
      COUNT(*) FILTER (WHERE workflow_phase = 'verify') AS phase_verify,
      COUNT(*) FILTER (WHERE workflow_phase = 'deploy') AS phase_deploy,
      COUNT(*) FILTER (WHERE workflow_phase = 'collab') AS phase_collab,
      -- File writes
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit', 'NotebookEdit')) AS writes,
      COUNT(DISTINCT CASE WHEN tool_name IN ('Write', 'Edit', 'NotebookEdit')
                          THEN file_ext || '|' || tool_name ELSE NULL END) AS _placeholder,
      -- Read/search
      COUNT(*) FILTER (WHERE tool_name = 'Read') AS reads,
      COUNT(*) FILTER (WHERE tool_name IN ('Grep', 'Glob')) AS searches,
      -- MCP patterns
      COUNT(*) FILTER (WHERE tool_family = 'devboy:issues' AND tool_action = 'list') AS mcp_list_issues,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:mrs' AND tool_action = 'list') AS mcp_list_mrs,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:issues' AND tool_action = 'detail') AS mcp_detail_issues,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:mrs' AND tool_action = 'detail') AS mcp_detail_mrs,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:mrs' AND tool_action = 'create') AS mcp_create_mr,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:issues' AND tool_action = 'create') AS mcp_create_issue,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:mrs' AND tool_action = 'comment') AS mcp_comment_mr,
      -- Subagent
      COUNT(*) FILTER (WHERE tool_name IN ('Agent', 'Task')) AS subagent_spawns,
      -- Web research
      COUNT(*) FILTER (WHERE tool_name IN ('WebSearch', 'WebFetch')) AS web_research,
      -- Errors
      COUNT(*) FILTER (WHERE is_error) AS tool_errors,
      -- Total output volume
      SUM(output_chars) AS total_output_chars
    FROM tool_calls
    GROUP BY session, loop_id
  ),
  -- File-write detail per loop (writes per unique file_ext × tool_name as proxy)
  write_files AS (
    SELECT session, loop_id,
      COUNT(DISTINCT file_ext) FILTER (WHERE tool_name IN ('Write', 'Edit')) AS unique_ext_touched,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'code') AS writes_code,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'test') AS writes_test,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'config') AS writes_config,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'docs') AS writes_docs,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'infra') AS writes_infra
    FROM tool_calls
    GROUP BY session, loop_id
  ),
  -- Git activity per loop
  git_agg AS (
    SELECT session, loop_id_approx AS loop_id,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_commit') AS git_commits,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_push') AS git_pushes,
      COUNT(*) FILTER (WHERE git_subcategory IN ('git_pull', 'git_fetch')) AS git_syncs,
      COUNT(*) AS bash_count,
      COUNT(*) FILTER (WHERE is_error) AS bash_errors
    FROM (
      -- Approximate loop_id by joining bash to nearest preceding agent turn's loop_id
      SELECT bc.session, bc.git_subcategory, bc.is_error,
        (SELECT loop_id FROM turns t
         WHERE t.session = bc.session AND t.ts_ms <= bc.ts_ms
         ORDER BY t.ts_ms DESC LIMIT 1) AS loop_id_approx
      FROM bash_commands bc
    )
    GROUP BY session, loop_id_approx
  ),
  -- First human turn of loop — from human_turns
  first_human AS (
    SELECT ht.session, t.loop_id,
      FIRST(ht.chars ORDER BY ht.ts_ms) AS first_human_chars,
      FIRST(ht.intent ORDER BY ht.ts_ms) AS first_human_intent,
      FIRST(ht.length_bucket ORDER BY ht.ts_ms) AS first_human_bucket,
      BOOL_OR(ht.has_question) AS human_has_question,
      BOOL_OR(ht.has_error_report) AS human_has_error,
      BOOL_OR(ht.has_issue_ref) AS human_has_issue_ref,
      BOOL_OR(ht.has_code_block) AS human_has_code_block,
      BOOL_OR(ht.has_negation) AS human_has_negation,
      BOOL_OR(ht.has_urgency) AS human_has_urgency
    FROM human_turns ht
    LEFT JOIN turns t ON t.session = ht.session AND t.ts_ms = ht.ts_ms
    WHERE t.loop_id IS NOT NULL
    GROUP BY ht.session, t.loop_id
  )
SELECT
  ta.session,
  ta.loop_id,
  ta.loop_start_ms,
  ta.loop_end_ms,
  (ta.loop_end_ms - ta.loop_start_ms) / 1000 AS duration_sec,
  ta.agent_turns,
  ta.tool_results,
  COALESCE(tc.tool_call_count, 0) AS tool_call_count,
  COALESCE(tc.tool_diversity, 0) AS tool_diversity,
  -- Phases
  COALESCE(tc.phase_plan, 0) AS phase_plan,
  COALESCE(tc.phase_research, 0) AS phase_research,
  COALESCE(tc.phase_implement, 0) AS phase_implement,
  COALESCE(tc.phase_verify, 0) AS phase_verify,
  COALESCE(tc.phase_deploy, 0) AS phase_deploy,
  COALESCE(tc.phase_collab, 0) AS phase_collab,
  -- Read/Write
  COALESCE(tc.reads, 0) AS reads,
  COALESCE(tc.writes, 0) AS writes,
  COALESCE(tc.searches, 0) AS searches,
  COALESCE(wf.unique_ext_touched, 0) AS unique_ext_touched,
  COALESCE(wf.writes_code, 0) AS writes_code,
  COALESCE(wf.writes_test, 0) AS writes_test,
  COALESCE(wf.writes_config, 0) AS writes_config,
  COALESCE(wf.writes_docs, 0) AS writes_docs,
  COALESCE(wf.writes_infra, 0) AS writes_infra,
  -- MCP
  COALESCE(tc.mcp_list_issues, 0) AS mcp_list_issues,
  COALESCE(tc.mcp_list_mrs, 0) AS mcp_list_mrs,
  COALESCE(tc.mcp_detail_issues, 0) AS mcp_detail_issues,
  COALESCE(tc.mcp_detail_mrs, 0) AS mcp_detail_mrs,
  COALESCE(tc.mcp_create_mr, 0) AS mcp_create_mr,
  COALESCE(tc.mcp_create_issue, 0) AS mcp_create_issue,
  COALESCE(tc.mcp_comment_mr, 0) AS mcp_comment_mr,
  COALESCE(tc.subagent_spawns, 0) AS subagent_spawns,
  COALESCE(tc.web_research, 0) AS web_research,
  COALESCE(tc.tool_errors, 0) AS tool_errors,
  COALESCE(tc.total_output_chars, 0) AS total_output_chars,
  -- Git
  COALESCE(ga.git_commits, 0) AS git_commits,
  COALESCE(ga.git_pushes, 0) AS git_pushes,
  COALESCE(ga.git_syncs, 0) AS git_syncs,
  COALESCE(ga.bash_count, 0) AS bash_count,
  COALESCE(ga.bash_errors, 0) AS bash_errors,
  -- Token/cache
  ta.prompt_tokens,
  ta.output_tokens,
  COALESCE(ta.ctx_max_in_loop, 0) AS ctx_max_in_loop,
  COALESCE(ta.ctx_mean_in_loop, 0) AS ctx_mean_in_loop,
  -- Human
  COALESCE(fh.first_human_chars, 0) AS first_human_chars,
  COALESCE(fh.first_human_intent, '') AS first_human_intent,
  COALESCE(fh.first_human_bucket, '') AS first_human_bucket,
  COALESCE(fh.human_has_question, false) AS human_has_question,
  COALESCE(fh.human_has_error, false) AS human_has_error,
  COALESCE(fh.human_has_issue_ref, false) AS human_has_issue_ref,
  COALESCE(fh.human_has_code_block, false) AS human_has_code_block,
  COALESCE(fh.human_has_negation, false) AS human_has_negation,
  COALESCE(fh.human_has_urgency, false) AS human_has_urgency,
  -- Derived outcome signal
  CASE
    WHEN COALESCE(wf.writes_code + wf.writes_test + wf.writes_config + wf.writes_docs + wf.writes_infra, 0) >= 1
         AND COALESCE(ga.git_commits, 0) >= 1
      THEN 'target_write_committed'
    WHEN COALESCE(tc.writes, 0) >= 1
      THEN 'target_write_uncommitted'
    WHEN COALESCE(tc.mcp_create_mr, 0) >= 1 OR COALESCE(tc.mcp_create_issue, 0) >= 1
      THEN 'target_create_entity'
    WHEN COALESCE(tc.mcp_comment_mr, 0) >= 1
      THEN 'target_collab'
    WHEN ta.agent_turns > 0 AND COALESCE(tc.tool_call_count, 0) = 0
      THEN 'target_answer_text'
    WHEN ta.agent_turns > 0 AND COALESCE(tc.tool_call_count, 0) <= 3
         AND COALESCE(tc.reads, 0) + COALESCE(tc.searches, 0) > 0
      THEN 'target_answer_research'
    ELSE 'no_target'
  END AS loop_outcome
FROM turn_agg ta
LEFT JOIN tool_agg tc USING (session, loop_id)
LEFT JOIN write_files wf USING (session, loop_id)
LEFT JOIN git_agg ga USING (session, loop_id)
LEFT JOIN first_human fh USING (session, loop_id)
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in-dir", default="/tmp/claude_analysis")
    ap.add_argument("--out", default="/tmp/claude_analysis/loops.parquet")
    args = ap.parse_args()

    in_dir = Path(args.in_dir)
    con = duckdb.connect()
    for tbl in ("turns", "tool_calls", "bash_commands", "human_turns"):
        con.execute(f"CREATE VIEW {tbl} AS SELECT * FROM '{in_dir}/{tbl}.parquet'")

    print(f"# Building loops.parquet from {in_dir}", file=sys.stderr)
    con.execute(f"COPY ({LOOPS_SQL}) TO '{args.out}' (FORMAT PARQUET, COMPRESSION ZSTD)")
    n = con.execute(f"SELECT COUNT(*) FROM '{args.out}'").fetchone()[0]
    size = Path(args.out).stat().st_size
    print(f"# Wrote {n} loops, {size/1024:.0f} KB → {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
