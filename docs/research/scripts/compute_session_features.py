#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0"]
# ///
"""
Compute derived per-session features on top of analyze_sessions.py output.

All features are pure algorithmic — no content parsing, no LLM.
Goal: maximize signal from existing parquet tables before any LLM pass.

Input:
  /tmp/claude_analysis/sessions.parquet
  /tmp/claude_analysis/turns.parquet
  /tmp/claude_analysis/compactions.parquet

Output:
  /tmp/claude_analysis/sessions_enriched.parquet — sessions + derived features
"""
import argparse
import sys
from pathlib import Path

import duckdb


# Per-model USD pricing in $/1M tokens (Anthropic published, 2026-Q1).
# Anthropic TTL tiers:
#   cr    = cache_read      (0.1× input)
#   cc_5m = cache_write 5m  (1.25× input)
#   cc_1h = cache_write 1h  (2× input)
# In turns.parquet we have `cache_read`, `ephemeral_5m_tokens`, `ephemeral_1h_tokens`.
# `cache_create` = `ephemeral_5m_tokens + ephemeral_1h_tokens`.
MODEL_PRICING = {
    # Anthropic Claude 4 family (standard 200k + [1m] extended context same rate)
    "claude-opus-4-7":    {"in": 15.00, "out": 75.00, "cr": 1.50,  "cc_5m": 18.75, "cc_1h": 30.00},
    "claude-opus-4-6":    {"in": 15.00, "out": 75.00, "cr": 1.50,  "cc_5m": 18.75, "cc_1h": 30.00},
    "claude-opus-4-5":    {"in": 15.00, "out": 75.00, "cr": 1.50,  "cc_5m": 18.75, "cc_1h": 30.00},
    "claude-sonnet-4-6":  {"in":  3.00, "out": 15.00, "cr": 0.30,  "cc_5m":  3.75, "cc_1h":  6.00},
    "claude-sonnet-4-5":  {"in":  3.00, "out": 15.00, "cr": 0.30,  "cc_5m":  3.75, "cc_1h":  6.00},
    "claude-sonnet-4":    {"in":  3.00, "out": 15.00, "cr": 0.30,  "cc_5m":  3.75, "cc_1h":  6.00},
    "claude-haiku-4-5":   {"in":  1.00, "out":  5.00, "cr": 0.10,  "cc_5m":  1.25, "cc_1h":  2.00},
    # z.ai GLM (approximations — they don't always publish cache TTL split)
    "glm-5.1":            {"in":  0.60, "out":  2.20, "cr": 0.11,  "cc_5m":  0.75, "cc_1h":  1.20},
    "glm-5":              {"in":  0.60, "out":  2.20, "cr": 0.11,  "cc_5m":  0.75, "cc_1h":  1.20},
    "glm-4.7":            {"in":  0.60, "out":  2.20, "cr": 0.11,  "cc_5m":  0.75, "cc_1h":  1.20},
    "glm-4.5":            {"in":  0.60, "out":  2.20, "cr": 0.11,  "cc_5m":  0.75, "cc_1h":  1.20},
    # Moonshot Kimi (approximation based on moonshot-v1-32k base rate)
    "kimi-for-coding":    {"in":  0.60, "out":  2.50, "cr": 0.15,  "cc_5m":  0.75, "cc_1h":  1.20},
    "kimi":               {"in":  0.60, "out":  2.50, "cr": 0.15,  "cc_5m":  0.75, "cc_1h":  1.20},
    # Free/local models — zero cost
    "qwen3-coder":        {"in":  0.00, "out":  0.00, "cr": 0.00,  "cc_5m":  0.00, "cc_1h":  0.00},
    "gpt-oss:20b":        {"in":  0.00, "out":  0.00, "cr": 0.00,  "cc_5m":  0.00, "cc_1h":  0.00},
    "<synthetic>":        {"in":  0.00, "out":  0.00, "cr": 0.00,  "cc_5m":  0.00, "cc_1h":  0.00},
}


def cost_sql(field, column="model"):
    """
    SQL CASE expression: given a model column, return per-token USD price.
    field ∈ {'in','out','cr','cc'}.
    """
    cases = []
    for prefix, rates in MODEL_PRICING.items():
        p = prefix.replace("'", "''")
        cases.append(f"WHEN {column} LIKE '{p}%' THEN {rates[field]}")
    cases_sql = "\n    ".join(cases)
    return f"(CASE {cases_sql} ELSE 3.0 END)"


# Model context-window lookup. Based on known limits as of 2026-04.
# Key is a substring match against top_model (case-sensitive).
# For "1m" variants: Claude Code uses [1m] suffix — we can't see it in the raw
# model name, so we detect via observed context_size_max instead (below).
MODEL_WINDOW_K = {
    "claude-opus-4-7": 200,      # can be 1M via [1m] suffix — detected by context_size
    "claude-opus-4-6": 200,
    "claude-opus-4-5": 200,
    "claude-sonnet-4-6": 200,
    "claude-sonnet-4-5": 200,
    "claude-sonnet-4": 200,
    "claude-haiku-4-5": 200,
    "glm-5.1":           200,
    "glm-5":             128,
    "glm-4.7":           128,
    "glm-4.5":           128,
    "kimi-for-coding":   32,     # moonshot-v1-32k base
    "kimi":              32,
    "qwen3-coder":       128,
    "gpt-oss:20b":       128,
    "<synthetic>":       0,
}


def model_window_k_sql():
    """Generate SQL CASE expression for model → baseline window (in k tokens)."""
    cases = []
    for prefix, k in MODEL_WINDOW_K.items():
        # Escape single quotes
        p = prefix.replace("'", "''")
        cases.append(f"WHEN top_model LIKE '{p}%' THEN {k}")
    return "CASE " + "\n    ".join(cases) + " ELSE 200 END"


ENRICH_SQL = """
WITH
  -- Per-session cache/token aggregates from turns
  tok AS (
    SELECT
      session,
      SUM(in_tokens + cache_read + cache_create) AS prompt_tokens_total,
      CAST(SUM(cache_read) AS DOUBLE) /
        NULLIF(SUM(in_tokens + cache_read + cache_create), 0) AS cache_hit_rate,
      COUNT(*) FILTER (WHERE turn_kind = 'agent' AND context_size_tokens > 150000) AS ceiling_approaches,
      COUNT(*) FILTER (WHERE turn_kind = 'agent' AND context_size_tokens > 500000) AS ctx_over_500k,
      COUNT(*) FILTER (WHERE turn_kind = 'agent' AND context_size_tokens > 900000) AS ctx_over_900k
    FROM turns
    GROUP BY session
  ),
  -- Cost per session (USD) from turns.model pricing.
  -- Use ephemeral_5m_tokens / ephemeral_1h_tokens when available for
  -- correct cache_create tier pricing. Fall back to cc_5m rate for any
  -- cache_create tokens unaccounted for (detail wasn't emitted by API).
  cost_calc AS (
    WITH per_turn_cost AS (
      SELECT session,
        (
          in_tokens * {IN_PRICE}
        + out_tokens * {OUT_PRICE}
        + cache_read * {CR_PRICE}
        + ephemeral_5m_tokens * {CC5M_PRICE}
        + ephemeral_1h_tokens * {CC1H_PRICE}
        + GREATEST(
            cache_create - ephemeral_5m_tokens - ephemeral_1h_tokens, 0
          ) * {CC5M_PRICE}
        ) / 1000000.0 AS turn_cost_usd
      FROM turns
      WHERE turn_kind = 'agent' AND model != ''
    )
    SELECT session,
      ROUND(SUM(turn_cost_usd), 4) AS cost_usd_total,
      ROUND(AVG(turn_cost_usd), 6) AS cost_per_turn_mean_usd,
      ROUND(MAX(turn_cost_usd), 4) AS cost_per_turn_max_usd,
      COUNT(*) AS cost_turn_count
    FROM per_turn_cost
    GROUP BY session
  ),
  -- Detailed cost breakdown per session (split by token type)
  cost_breakdown AS (
    SELECT session,
      ROUND(SUM(in_tokens * {IN_PRICE}) / 1000000.0, 4) AS cost_input_usd,
      ROUND(SUM(out_tokens * {OUT_PRICE}) / 1000000.0, 4) AS cost_output_usd,
      ROUND(SUM(cache_read * {CR_PRICE}) / 1000000.0, 4) AS cost_cache_read_usd,
      ROUND(SUM(ephemeral_5m_tokens * {CC5M_PRICE}) / 1000000.0, 4) AS cost_cache_5m_usd,
      ROUND(SUM(ephemeral_1h_tokens * {CC1H_PRICE}) / 1000000.0, 4) AS cost_cache_1h_usd,
      ROUND(SUM(GREATEST(cache_create - ephemeral_5m_tokens - ephemeral_1h_tokens, 0) * {CC5M_PRICE}) / 1000000.0, 4) AS cost_cache_other_usd
    FROM turns
    WHERE turn_kind = 'agent' AND model != ''
    GROUP BY session
  ),
  -- KV-cache TTL expiration detection.
  -- Claude's ephemeral cache: 5m and 1h TTLs.
  -- Heuristic: if an agent turn comes after a gap exceeding TTL AND it has
  -- non-trivial cache_create relative to context size, the cache expired.
  -- We only count turns where cache_miss_ratio > 0.05 AND cache_create > 2000 tok
  -- (filters out trivial cache-prefix extensions).
  -- Per-turn: classify WHAT caused each cache expiration by looking at prev turn.
  -- prev_kind = turn_kind of the turn immediately before this agent turn.
  -- We combine with prev_tool_category if prev was a tool_result → know which tool.
  turns_with_prev AS (
    SELECT *,
      LAG(turn_kind) OVER (PARTITION BY session ORDER BY ts_ms) AS prev_kind,
      LAG(first_tool_category) OVER (PARTITION BY session ORDER BY ts_ms) AS prev_tool_cat
    FROM turns
  ),
  cache_exp AS (
    SELECT
      session,
      COUNT(*) FILTER (WHERE
        turn_kind = 'agent'
        AND latency_since_prev_ms BETWEEN 300000 AND 3600000   -- 5m–1h
        AND cache_create >= 2000
        AND CAST(cache_create AS DOUBLE) /
            NULLIF(cache_create + cache_read + in_tokens, 0) > 0.05
      ) AS cache_5m_expired_count,
      COUNT(*) FILTER (WHERE
        turn_kind = 'agent'
        AND latency_since_prev_ms > 3600000                    -- >1h
        AND cache_create >= 2000
        AND CAST(cache_create AS DOUBLE) /
            NULLIF(cache_create + cache_read + in_tokens, 0) > 0.05
      ) AS cache_1h_expired_count,
      -- Cause breakdown for 5m+ expirations
      COUNT(*) FILTER (WHERE
        turn_kind = 'agent' AND latency_since_prev_ms > 300000
        AND cache_create >= 2000
        AND prev_kind = 'human'
      ) AS exp_cause_human_afk,
      COUNT(*) FILTER (WHERE
        turn_kind = 'agent' AND latency_since_prev_ms > 300000
        AND cache_create >= 2000
        AND prev_kind = 'tool_result'
      ) AS exp_cause_long_tool,
      COUNT(*) FILTER (WHERE
        turn_kind = 'agent' AND latency_since_prev_ms > 300000
        AND cache_create >= 2000
        AND prev_kind = 'agent'
      ) AS exp_cause_chain,
      -- Tokens paid to rebuild cache after any gap > 5m
      SUM(CASE WHEN turn_kind = 'agent' AND latency_since_prev_ms > 300000
               THEN cache_create ELSE 0 END) AS cache_rebuild_tokens,
      -- Break rebuild tokens by cause
      SUM(CASE WHEN turn_kind = 'agent' AND latency_since_prev_ms > 300000 AND prev_kind = 'human'
               THEN cache_create ELSE 0 END) AS rebuild_cost_human_afk,
      SUM(CASE WHEN turn_kind = 'agent' AND latency_since_prev_ms > 300000 AND prev_kind = 'tool_result'
               THEN cache_create ELSE 0 END) AS rebuild_cost_long_tool,
      -- Gap distribution
      COUNT(*) FILTER (WHERE turn_kind = 'agent' AND latency_since_prev_ms > 300000) AS gaps_over_5m,
      COUNT(*) FILTER (WHERE turn_kind = 'agent' AND latency_since_prev_ms > 3600000) AS gaps_over_1h
    FROM turns_with_prev
    GROUP BY session
  ),
  -- Tool distribution per session
  tools AS (
    SELECT
      session,
      COUNT(DISTINCT first_tool_category) FILTER (WHERE tool_calls_in_turn > 0) AS tool_diversity,
      SUM(CASE WHEN first_tool_category LIKE 'mcp_%' THEN tool_calls_in_turn ELSE 0 END) AS mcp_calls,
      SUM(CASE WHEN first_tool_category = 'builtin:Bash' THEN tool_calls_in_turn ELSE 0 END) AS bash_calls,
      SUM(CASE WHEN first_tool_category LIKE 'builtin:%' THEN tool_calls_in_turn ELSE 0 END) AS builtin_calls,
      SUM(CASE WHEN first_tool_category = 'builtin:subagent' THEN tool_calls_in_turn ELSE 0 END) AS subagent_spawn_calls,
      SUM(tool_calls_in_turn) AS all_tool_calls
    FROM turns
    WHERE turn_kind = 'agent'
    GROUP BY session
  ),
  -- Longest consecutive same tool_category run per session
  tool_runs AS (
    SELECT session, MAX(run_len) AS longest_same_tool_run
    FROM (
      SELECT session, tool_category, COUNT(*) AS run_len
      FROM (
        SELECT session, first_tool_category AS tool_category,
          ROW_NUMBER() OVER (PARTITION BY session ORDER BY ts_ms) -
          ROW_NUMBER() OVER (PARTITION BY session, first_tool_category ORDER BY ts_ms) AS grp
        FROM turns
        WHERE turn_kind = 'agent' AND tool_calls_in_turn > 0 AND first_tool_category != ''
      )
      GROUP BY session, tool_category, grp
    )
    GROUP BY session
  ),
  -- Compaction-derived: pre_tokens min/max, trigger mix
  comp AS (
    SELECT
      session,
      MIN(pre_tokens) AS compact_pre_min,
      MAX(pre_tokens) AS compact_pre_max,
      AVG(pre_tokens) AS compact_pre_avg,
      COUNT(*) FILTER (WHERE trigger='manual') AS compact_manual,
      COUNT(*) FILTER (WHERE trigger='auto') AS compact_auto
    FROM compactions
    GROUP BY session
  ),
  -- Subagent spawn counts
  subs AS (
    SELECT parent_session AS session, COUNT(*) AS subagents_linked_children
    FROM sessions WHERE is_subagent AND parent_session != ''
    GROUP BY parent_session
  ),
  roots AS (
    SELECT root_session AS session, COUNT(*) AS subagents_in_tree
    FROM sessions WHERE is_subagent AND root_session != ''
    GROUP BY root_session
  ),
  -- Git activity roll-up (from bash_commands.parquet)
  git_stats AS (
    SELECT session,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_commit')   AS git_commit_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_push')     AS git_push_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_pull')     AS git_pull_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_fetch')    AS git_fetch_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_merge')    AS git_merge_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_rebase')   AS git_rebase_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_checkout') AS git_checkout_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_status')   AS git_status_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_diff')     AS git_diff_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_log')      AS git_log_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_stash')    AS git_stash_count,
      COUNT(*) FILTER (WHERE git_subcategory = 'git_reset')    AS git_reset_count
    FROM bash_commands GROUP BY session
  ),
  -- Workflow roles from tool_calls (using new taxonomy)
  wf_stats AS (
    SELECT session,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:mrs' AND tool_action = 'create')
        AS mr_create_count,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:issues' AND tool_action = 'create')
        AS issue_create_count,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:issues' AND tool_action = 'comment')
        AS issue_comment_count,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:mrs' AND tool_action = 'comment')
        AS mr_comment_count,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:meetings' AND tool_action = 'detail')
        AS transcript_pulls,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:mrs' AND tool_action = 'detail'
        AND tool_category LIKE '%discussion%')
        AS mr_discussion_reads,
      COUNT(*) FILTER (WHERE tool_family = 'devboy:ci')     AS ci_checks,
      COUNT(*) FILTER (WHERE workflow_role = 'discovery')   AS role_discovery,
      COUNT(*) FILTER (WHERE workflow_role = 'deep_dive')   AS role_deep_dive,
      COUNT(*) FILTER (WHERE workflow_role = 'implementation') AS role_implementation,
      COUNT(*) FILTER (WHERE workflow_role = 'deployment')  AS role_deployment,
      COUNT(*) FILTER (WHERE workflow_role = 'verification') AS role_verification,
      COUNT(*) FILTER (WHERE workflow_role = 'collaboration') AS role_collaboration,
      COUNT(*) FILTER (WHERE workflow_role = 'research_external') AS role_research,
      COUNT(*) FILTER (WHERE is_error) AS tool_error_count_calc
    FROM tool_calls GROUP BY session
  ),
  bash_err AS (
    SELECT session, COUNT(*) AS bash_errors_calc
    FROM bash_commands WHERE is_error GROUP BY session
  ),
  -- TodoWrite / TaskCreate / TaskUpdate tracking — micro-task management
  todo_stats AS (
    SELECT session,
      COUNT(*) FILTER (WHERE tool_name = 'TodoWrite') AS todo_write_count,
      COUNT(*) FILTER (WHERE tool_name IN ('TaskCreate', 'TaskUpdate')) AS task_mgmt_count,
      COUNT(*) FILTER (WHERE tool_name = 'TaskCreate') AS task_create_count,
      COUNT(*) FILTER (WHERE tool_name = 'TaskUpdate') AS task_update_count,
      -- First TodoWrite/Task occurrence (ts_ms) - indicates when user/agent started planning
      MIN(CASE WHEN tool_name IN ('TodoWrite', 'TaskCreate') THEN ts_ms END) AS first_plan_ts_ms
    FROM tool_calls GROUP BY session
  ),
  -- Bug-fix signal from first human turn
  bugfix_signals AS (
    SELECT session,
      MAX(CASE WHEN turn_idx_in_session = 1 AND has_error_report THEN 1 ELSE 0 END) AS first_turn_error,
      MAX(CASE WHEN turn_idx_in_session = 1 AND has_issue_ref THEN 1 ELSE 0 END) AS first_turn_issue_ref,
      MAX(CASE WHEN turn_idx_in_session = 1 AND intent = 'report_error' THEN 1 ELSE 0 END) AS first_turn_is_error_report,
      MAX(CASE WHEN turn_idx_in_session = 1 AND intent IN ('urgent_request', 'course_correct') THEN 1 ELSE 0 END) AS first_turn_urgent_or_correct
    FROM human_turns GROUP BY session
  ),
  -- Human turn signals per session (from human_turns.parquet)
  human_stats AS (
    SELECT session,
      COUNT(*) FILTER (WHERE intent = 'course_correct') AS course_correct_count,
      COUNT(*) FILTER (WHERE intent = 'report_error')   AS error_report_count,
      COUNT(*) FILTER (WHERE intent = 'approve_short')  AS approval_count,
      COUNT(*) FILTER (WHERE intent = 'urgent_request') AS urgent_count,
      COUNT(*) FILTER (WHERE intent = 'detailed_spec')  AS detailed_spec_count,
      COUNT(*) FILTER (WHERE intent = 'instruction')    AS instruction_count,
      COUNT(*) FILTER (WHERE intent = 'quick_question') AS quick_question_count,
      COUNT(*) FILTER (WHERE intent = 'question')       AS question_count,
      COUNT(*) FILTER (WHERE is_mid_loop_interrupt)     AS interrupt_turn_count,
      COUNT(*) FILTER (WHERE has_negation AND chars > 50) AS significant_negation_count,
      COUNT(*) FILTER (WHERE has_error_report)          AS error_report_any_count,
      SUM(chars)                                        AS total_human_chars,
      ROUND(AVG(chars), 0)                              AS avg_human_chars,
      MAX(chars)                                        AS max_human_chars,
      COUNT(*)                                          AS human_turns_analyzed
    FROM human_turns GROUP BY session
  ),
  -- Workflow pattern flags
  -- Phase rollup per session (counts + transitions + iteration count)
  phase_stats AS (
    WITH ordered AS (
      SELECT session, ts_ms, workflow_phase,
        LAG(workflow_phase) OVER (PARTITION BY session ORDER BY ts_ms) AS prev_phase
      FROM tool_calls WHERE workflow_phase != 'other'
    )
    SELECT session,
      COUNT(*) FILTER (WHERE workflow_phase = 'plan')      AS phase_plan_count,
      COUNT(*) FILTER (WHERE workflow_phase = 'research')  AS phase_research_count,
      COUNT(*) FILTER (WHERE workflow_phase = 'implement') AS phase_implement_count,
      COUNT(*) FILTER (WHERE workflow_phase = 'verify')    AS phase_verify_count,
      COUNT(*) FILTER (WHERE workflow_phase = 'deploy')    AS phase_deploy_count,
      COUNT(*) FILTER (WHERE workflow_phase = 'collab')    AS phase_collab_count,
      COUNT(*) FILTER (WHERE workflow_phase = 'execute')   AS phase_execute_count,
      -- Count transitions (where phase changed from previous tool call)
      COUNT(*) FILTER (WHERE prev_phase IS NOT NULL AND prev_phase != workflow_phase)
        AS phase_transitions,
      -- Research→implement transitions (iteration signal)
      COUNT(*) FILTER (WHERE prev_phase = 'research' AND workflow_phase = 'implement')
        AS research_to_implement_count,
      COUNT(*) FILTER (WHERE prev_phase = 'implement' AND workflow_phase = 'verify')
        AS implement_to_verify_count,
      COUNT(*) FILTER (WHERE prev_phase = 'verify' AND workflow_phase = 'research')
        AS verify_to_research_count
    FROM ordered
    GROUP BY session
  ),
  -- Web research features (WebSearch + WebFetch).
  -- Compute session bounds FROM tool_calls only to avoid dup-session JOIN inflation.
  sess_bounds AS (
    SELECT session, MIN(ts_ms) AS start_ms, MAX(ts_ms) AS end_ms
    FROM tool_calls GROUP BY session
  ),
  web_stats AS (
    WITH web_calls AS (
      SELECT tc.session, tc.ts_ms, tc.tool_name,
        CAST(tc.ts_ms - sb.start_ms AS DOUBLE) /
          NULLIF(sb.end_ms - sb.start_ms, 0) AS pos_frac,
        (SELECT COUNT(*) FROM tool_calls p
         WHERE p.session = tc.session
           AND p.ts_ms < tc.ts_ms AND p.ts_ms > tc.ts_ms - 600000
           AND p.is_error = true) AS errors_before,
        (SELECT COUNT(*) FROM tool_calls n
         WHERE n.session = tc.session
           AND n.ts_ms > tc.ts_ms AND n.ts_ms < tc.ts_ms + 600000
           AND n.workflow_phase = 'implement') AS impl_after
      FROM tool_calls tc
      JOIN sess_bounds sb ON tc.session = sb.session
      WHERE tc.tool_family = 'builtin:web'
    )
    SELECT session,
      COUNT(*) AS web_research_count,
      COUNT(*) FILTER (WHERE tool_name = 'WebSearch') AS websearch_count,
      COUNT(*) FILTER (WHERE tool_name = 'WebFetch') AS webfetch_count,
      ROUND(AVG(pos_frac), 3) AS web_pos_mean,
      COUNT(*) FILTER (WHERE pos_frac < 0.25) AS web_early_count,
      COUNT(*) FILTER (WHERE pos_frac >= 0.75) AS web_late_count,
      COUNT(*) FILTER (WHERE errors_before > 0) AS web_after_error_count,
      COUNT(*) FILTER (WHERE impl_after > 0) AS web_before_impl_count
    FROM web_calls GROUP BY session
  ),
  -- Read/Write counters
  rw_stats AS (
    SELECT session,
      COUNT(*) FILTER (WHERE tool_name = 'Read') AS reads,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit', 'NotebookEdit')) AS writes,
      COUNT(*) FILTER (WHERE tool_name IN ('Grep', 'Glob')) AS searches,
      -- Writes by file type
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'code') AS writes_code,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'test') AS writes_test,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'config') AS writes_config,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'docs') AS writes_docs,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'infra') AS writes_infra,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'ui') AS writes_ui,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'sql') AS writes_sql,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'script') AS writes_script,
      COUNT(*) FILTER (WHERE tool_name IN ('Write', 'Edit') AND file_type = 'other') AS writes_other
    FROM tool_calls GROUP BY session
  ),
  wp_stats AS (
    SELECT session,
      COUNT(*) FILTER (WHERE pattern = 'self_review_loop') AS self_review_loops,
      COUNT(*) FILTER (WHERE pattern = 'transcript_to_ticket') AS transcript_tickets,
      COUNT(*) FILTER (WHERE pattern = 'ci_iteration_cycle') AS ci_cycles,
      COUNT(*) FILTER (WHERE pattern = 'investigation_to_merge_arc') AS merge_arcs,
      COUNT(*) FILTER (WHERE pattern = 'stuck_cycle') AS stuck_cycles
    FROM workflow_patterns GROUP BY session
  )
SELECT
  s.*,
  -- Cache efficiency
  ROUND(COALESCE(tok.cache_hit_rate, 0), 4) AS cache_hit_rate,
  tok.prompt_tokens_total,
  tok.ceiling_approaches,
  tok.ctx_over_500k,
  tok.ctx_over_900k,
  -- KV-cache TTL expirations
  COALESCE(cache_exp.cache_5m_expired_count, 0) AS cache_5m_expired_count,
  COALESCE(cache_exp.cache_1h_expired_count, 0) AS cache_1h_expired_count,
  COALESCE(cache_exp.cache_rebuild_tokens, 0) AS cache_rebuild_tokens,
  COALESCE(cache_exp.exp_cause_human_afk, 0) AS exp_cause_human_afk,
  COALESCE(cache_exp.exp_cause_long_tool, 0) AS exp_cause_long_tool,
  COALESCE(cache_exp.exp_cause_chain, 0) AS exp_cause_chain,
  COALESCE(cache_exp.rebuild_cost_human_afk, 0) AS rebuild_cost_human_afk,
  COALESCE(cache_exp.rebuild_cost_long_tool, 0) AS rebuild_cost_long_tool,
  COALESCE(cache_exp.gaps_over_5m, 0) AS agent_gaps_over_5m,
  COALESCE(cache_exp.gaps_over_1h, 0) AS agent_gaps_over_1h,
  -- Tool fingerprint
  COALESCE(tools.tool_diversity, 0) AS tool_diversity,
  COALESCE(tools.mcp_calls, 0) AS mcp_calls,
  COALESCE(tools.bash_calls, 0) AS bash_calls,
  COALESCE(tools.builtin_calls, 0) AS builtin_calls,
  COALESCE(tools.subagent_spawn_calls, 0) AS subagent_spawn_calls,
  CASE WHEN COALESCE(tools.all_tool_calls, 0) > 0
    THEN ROUND(CAST(tools.mcp_calls AS DOUBLE) / tools.all_tool_calls, 4) ELSE 0 END AS mcp_fraction,
  CASE WHEN COALESCE(tools.all_tool_calls, 0) > 0
    THEN ROUND(CAST(tools.bash_calls AS DOUBLE) / tools.all_tool_calls, 4) ELSE 0 END AS bash_fraction,
  -- Patterns
  COALESCE(tool_runs.longest_same_tool_run, 0) AS longest_same_tool_run,
  CASE WHEN COALESCE(tool_runs.longest_same_tool_run, 0) >= 5 THEN 1 ELSE 0 END AS has_tool_loop,
  CASE WHEN s.loop_turns_max >= 50 THEN 1 ELSE 0 END AS has_context_grinding,
  CASE WHEN s.api_error_count > 10 THEN 1 ELSE 0 END AS retry_intensive,
  -- Compaction stats
  comp.compact_pre_min,
  comp.compact_pre_max,
  ROUND(COALESCE(comp.compact_pre_avg, 0)) AS compact_pre_avg,
  COALESCE(comp.compact_manual, 0) AS compact_manual,
  COALESCE(comp.compact_auto, 0) AS compact_auto,
  -- Subagent tree
  COALESCE(subs.subagents_linked_children, 0) AS subagents_linked_children,
  COALESCE(roots.subagents_in_tree, 0) AS subagents_in_tree,
  -- Throughput
  CASE WHEN s.duration_sec > 0
    THEN ROUND(CAST(s.out_tokens_total AS DOUBLE) / (s.duration_sec / 60), 2)
    ELSE 0 END AS out_tokens_per_min,
  CASE WHEN s.agent_turns > 0
    THEN ROUND(CAST(s.human_turns AS DOUBLE) / s.agent_turns, 4)
    ELSE 0 END AS human_agent_ratio,
  -- Derived session classes
  CASE
    WHEN s.is_subagent THEN 'subagent'
    WHEN s.total_records < 10 THEN 'main:tiny'
    WHEN s.total_records < 50 THEN 'main:small'
    WHEN s.total_records < 200 THEN 'main:medium'
    WHEN s.total_records < 1000 THEN 'main:large'
    ELSE 'main:huge'
  END AS size_bucket,
  -- Model native-provider classification
  CASE
    WHEN s.top_model LIKE 'claude-%' THEN 'anthropic'
    WHEN s.top_model LIKE 'glm-%' THEN 'zhipu'
    WHEN s.top_model LIKE 'kimi%' THEN 'moonshot'
    WHEN s.top_model LIKE 'qwen%' THEN 'qwen'
    WHEN s.top_model LIKE 'gpt-oss%' THEN 'local_ollama'
    WHEN s.top_model = '<synthetic>' THEN 'synthetic'
    WHEN s.top_model = '' THEN 'none'
    ELSE 'other'
  END AS provider,
  CASE WHEN s.top_model LIKE 'claude-%' THEN 1 ELSE 0 END AS is_native_anthropic,
  -- Baseline context window for the top model (in k tokens)
  {MODEL_WINDOW_K_SQL} AS model_window_k_baseline,
  -- Detect extended-window usage: the model's [1m] variant.
  -- Signal: context_size_max exceeded the baseline window × 1.2 (safety margin).
  CASE WHEN s.context_size_max > {MODEL_WINDOW_K_SQL} * 1200
       THEN 1 ELSE 0 END AS used_extended_context,
  CASE
    WHEN s.context_size_max < 50000 THEN '1.low <50k'
    WHEN s.context_size_max < 200000 THEN '2.med 50-200k'
    WHEN s.context_size_max < 500000 THEN '3.extended 200-500k'
    WHEN s.context_size_max < 900000 THEN '4.large 500-900k'
    ELSE '5.peak 900k+'
  END AS context_profile,
  -- How close session approached its available window (mean across turns)
  CASE WHEN {MODEL_WINDOW_K_SQL} > 0
       THEN ROUND(CAST(s.context_size_mean AS DOUBLE) /
            (CASE WHEN s.context_size_max > {MODEL_WINDOW_K_SQL} * 1200
                  THEN 1000 * 1000  -- 1M effective
                  ELSE {MODEL_WINDOW_K_SQL} * 1000 END), 4)
       ELSE 0 END AS mean_context_fill_fraction,
  -- Git activity
  COALESCE(git_stats.git_commit_count, 0) AS git_commit_count,
  COALESCE(git_stats.git_push_count, 0) AS git_push_count,
  COALESCE(git_stats.git_pull_count, 0) AS git_pull_count,
  COALESCE(git_stats.git_fetch_count, 0) AS git_fetch_count,
  COALESCE(git_stats.git_merge_count, 0) AS git_merge_count,
  COALESCE(git_stats.git_rebase_count, 0) AS git_rebase_count,
  COALESCE(git_stats.git_checkout_count, 0) AS git_checkout_count,
  COALESCE(git_stats.git_status_count, 0) AS git_status_count,
  COALESCE(git_stats.git_diff_count, 0) AS git_diff_count,
  COALESCE(git_stats.git_log_count, 0) AS git_log_count,
  COALESCE(git_stats.git_stash_count, 0) AS git_stash_count,
  COALESCE(git_stats.git_reset_count, 0) AS git_reset_count,
  -- Workflow roles (from tool_calls)
  COALESCE(wf_stats.mr_create_count, 0) AS mr_create_count,
  COALESCE(wf_stats.issue_create_count, 0) AS issue_create_count,
  COALESCE(wf_stats.issue_comment_count, 0) AS issue_comment_count,
  COALESCE(wf_stats.mr_comment_count, 0) AS mr_comment_count,
  COALESCE(wf_stats.transcript_pulls, 0) AS transcript_pulls,
  COALESCE(wf_stats.mr_discussion_reads, 0) AS mr_discussion_reads,
  COALESCE(wf_stats.ci_checks, 0) AS ci_checks,
  COALESCE(wf_stats.role_discovery, 0) AS role_discovery,
  COALESCE(wf_stats.role_deep_dive, 0) AS role_deep_dive,
  COALESCE(wf_stats.role_implementation, 0) AS role_implementation,
  COALESCE(wf_stats.role_deployment, 0) AS role_deployment,
  COALESCE(wf_stats.role_verification, 0) AS role_verification,
  COALESCE(wf_stats.role_collaboration, 0) AS role_collaboration,
  COALESCE(wf_stats.role_research, 0) AS role_research,
  COALESCE(wf_stats.tool_error_count_calc, 0) AS tool_errors_from_calls,
  COALESCE(bash_err.bash_errors_calc, 0) AS bash_errors_from_cmds,
  -- Cost (USD)
  COALESCE(cost_calc.cost_usd_total, 0) AS cost_usd_total,
  COALESCE(cost_calc.cost_per_turn_mean_usd, 0) AS cost_per_turn_mean_usd,
  COALESCE(cost_calc.cost_per_turn_max_usd, 0) AS cost_per_turn_max_usd,
  COALESCE(cost_breakdown.cost_input_usd, 0) AS cost_input_usd,
  COALESCE(cost_breakdown.cost_output_usd, 0) AS cost_output_usd,
  COALESCE(cost_breakdown.cost_cache_read_usd, 0) AS cost_cache_read_usd,
  COALESCE(cost_breakdown.cost_cache_5m_usd, 0) AS cost_cache_5m_usd,
  COALESCE(cost_breakdown.cost_cache_1h_usd, 0) AS cost_cache_1h_usd,
  COALESCE(cost_breakdown.cost_cache_other_usd, 0) AS cost_cache_other_usd,
  -- TODO / micro-task tracking
  COALESCE(todo_stats.todo_write_count, 0) AS todo_write_count,
  COALESCE(todo_stats.task_mgmt_count, 0)  AS task_mgmt_count,
  COALESCE(todo_stats.task_create_count, 0) AS task_create_count,
  COALESCE(todo_stats.task_update_count, 0) AS task_update_count,
  CASE WHEN COALESCE(todo_stats.todo_write_count, 0) > 0
         OR COALESCE(todo_stats.task_mgmt_count, 0) > 0
       THEN 1 ELSE 0 END AS uses_planning,
  -- Human direction features
  COALESCE(human_stats.course_correct_count, 0) AS course_correct_count,
  COALESCE(human_stats.error_report_count, 0) AS error_report_count,
  COALESCE(human_stats.approval_count, 0) AS approval_count,
  COALESCE(human_stats.urgent_count, 0) AS urgent_count,
  COALESCE(human_stats.detailed_spec_count, 0) AS detailed_spec_count,
  COALESCE(human_stats.instruction_count, 0) AS instruction_count,
  COALESCE(human_stats.interrupt_turn_count, 0) AS interrupt_turn_count,
  COALESCE(human_stats.significant_negation_count, 0) AS significant_negation_count,
  COALESCE(human_stats.total_human_chars, 0) AS total_human_chars,
  COALESCE(human_stats.avg_human_chars, 0) AS avg_human_chars,
  COALESCE(human_stats.max_human_chars, 0) AS max_human_chars,
  COALESCE(human_stats.human_turns_analyzed, 0) AS human_turns_analyzed,
  -- Correction rate: what fraction of human turns are corrections?
  CASE WHEN COALESCE(human_stats.human_turns_analyzed, 0) > 0 THEN
    ROUND(CAST(COALESCE(human_stats.course_correct_count, 0) AS DOUBLE) /
          human_stats.human_turns_analyzed, 4)
  ELSE 0 END AS correction_rate,
  -- Direction signal: went_wrong
  CASE WHEN COALESCE(human_stats.course_correct_count, 0) >= 3
         OR COALESCE(human_stats.significant_negation_count, 0) >= 2
         OR COALESCE(human_stats.interrupt_turn_count, 0) >= 5
       THEN 1 ELSE 0 END AS signal_went_wrong,
  -- Gross errors signal
  CASE WHEN s.api_error_count >= 5
         OR (COALESCE(tools.all_tool_calls, 0) > 10
             AND CAST(COALESCE(wf_stats.tool_error_count_calc, 0) AS DOUBLE) /
                 COALESCE(tools.all_tool_calls, 1) > 0.1)
         OR COALESCE(wp_stats.stuck_cycles, 0) >= 1
       THEN 1 ELSE 0 END AS signal_gross_errors,
  -- Multi-task signal
  CASE WHEN s.resume_with_prompt_count >= 2
         OR COALESCE(human_stats.instruction_count, 0) >= 3
         OR COALESCE(human_stats.detailed_spec_count, 0) >= 2
       THEN 1 ELSE 0 END AS signal_multi_task,
  -- Workflow pattern counts
  COALESCE(wp_stats.self_review_loops, 0) AS self_review_loops,
  COALESCE(wp_stats.transcript_tickets, 0) AS transcript_tickets,
  COALESCE(wp_stats.ci_cycles, 0) AS ci_cycles,
  COALESCE(wp_stats.merge_arcs, 0) AS merge_arcs,
  COALESCE(wp_stats.stuck_cycles, 0) AS stuck_cycles_pattern,
  -- Phase counts + iteration
  COALESCE(phase_stats.phase_plan_count, 0) AS phase_plan_count,
  COALESCE(phase_stats.phase_research_count, 0) AS phase_research_count,
  COALESCE(phase_stats.phase_implement_count, 0) AS phase_implement_count,
  COALESCE(phase_stats.phase_verify_count, 0) AS phase_verify_count,
  COALESCE(phase_stats.phase_deploy_count, 0) AS phase_deploy_count,
  COALESCE(phase_stats.phase_collab_count, 0) AS phase_collab_count,
  COALESCE(phase_stats.phase_transitions, 0) AS phase_transitions,
  COALESCE(phase_stats.research_to_implement_count, 0) AS research_to_impl_cycles,
  COALESCE(phase_stats.implement_to_verify_count, 0) AS impl_to_verify_cycles,
  COALESCE(phase_stats.verify_to_research_count, 0) AS verify_to_research_backloops,
  -- Read/Write split
  COALESCE(rw_stats.reads, 0) AS reads,
  COALESCE(rw_stats.writes, 0) AS writes,
  COALESCE(rw_stats.searches, 0) AS searches,
  CASE WHEN COALESCE(rw_stats.writes, 0) > 0
       THEN ROUND(CAST(rw_stats.reads AS DOUBLE) / rw_stats.writes, 3)
       ELSE NULL END AS read_write_ratio,
  COALESCE(rw_stats.writes_code, 0) AS writes_code,
  COALESCE(rw_stats.writes_test, 0) AS writes_test,
  COALESCE(rw_stats.writes_config, 0) AS writes_config,
  COALESCE(rw_stats.writes_docs, 0) AS writes_docs,
  COALESCE(rw_stats.writes_infra, 0) AS writes_infra,
  COALESCE(rw_stats.writes_ui, 0) AS writes_ui,
  COALESCE(rw_stats.writes_sql, 0) AS writes_sql,
  COALESCE(rw_stats.writes_script, 0) AS writes_script,
  COALESCE(rw_stats.writes_other, 0) AS writes_other,
  -- Bug-fix session signal
  CASE WHEN COALESCE(bugfix_signals.first_turn_error, 0) = 1
         OR COALESCE(bugfix_signals.first_turn_is_error_report, 0) = 1
         OR (COALESCE(bugfix_signals.first_turn_issue_ref, 0) = 1
             AND COALESCE(bugfix_signals.first_turn_urgent_or_correct, 0) = 1)
       THEN 1 ELSE 0 END AS is_bug_fix_session,
  COALESCE(bugfix_signals.first_turn_error, 0) AS first_turn_had_error,
  COALESCE(bugfix_signals.first_turn_issue_ref, 0) AS first_turn_had_issue_ref,
  -- Web research
  COALESCE(web_stats.web_research_count, 0) AS web_research_count,
  COALESCE(web_stats.websearch_count, 0) AS websearch_count,
  COALESCE(web_stats.webfetch_count, 0) AS webfetch_count,
  COALESCE(web_stats.web_pos_mean, 0) AS web_pos_mean,
  COALESCE(web_stats.web_early_count, 0) AS web_early_count,
  COALESCE(web_stats.web_late_count, 0) AS web_late_count,
  COALESCE(web_stats.web_after_error_count, 0) AS web_after_error_count,
  COALESCE(web_stats.web_before_impl_count, 0) AS web_before_impl_count,
  -- ═══════ RATE FEATURES (volume-invariant) ═══════
  -- Per-turn error rate
  CASE WHEN COALESCE(tools.all_tool_calls, 0) > 0
       THEN ROUND(CAST(COALESCE(wf_stats.tool_error_count_calc, 0) AS DOUBLE) /
                  tools.all_tool_calls, 4) ELSE 0 END AS tool_error_rate,
  CASE WHEN COALESCE(tools.bash_calls, 0) > 0
       THEN ROUND(CAST(COALESCE(bash_err.bash_errors_calc, 0) AS DOUBLE) /
                  tools.bash_calls, 4) ELSE 0 END AS bash_error_rate,
  -- Per-agent-turn metrics
  CASE WHEN s.agent_turns > 0
       THEN ROUND(CAST(COALESCE(human_stats.course_correct_count, 0) AS DOUBLE) /
                  s.agent_turns, 4) ELSE 0 END AS correction_per_agent_turn,
  CASE WHEN s.agent_turns > 0
       THEN ROUND(CAST(s.api_error_count AS DOUBLE) / s.agent_turns, 4)
       ELSE 0 END AS api_error_per_agent_turn,
  CASE WHEN s.agent_turns > 0
       THEN ROUND(CAST(COALESCE(tools.all_tool_calls, 0) AS DOUBLE) /
                  s.agent_turns, 3) ELSE 0 END AS tools_per_agent_turn,
  -- Phase ratios
  CASE WHEN COALESCE(phase_stats.phase_implement_count, 0) > 0
       THEN ROUND(CAST(COALESCE(phase_stats.phase_research_count, 0) AS DOUBLE) /
                  phase_stats.phase_implement_count, 3) ELSE NULL
       END AS research_to_impl_ratio,
  CASE WHEN COALESCE(phase_stats.phase_implement_count, 0) > 0
       THEN ROUND(CAST(COALESCE(phase_stats.phase_verify_count, 0) AS DOUBLE) /
                  phase_stats.phase_implement_count, 3) ELSE NULL
       END AS verify_to_impl_ratio,
  -- Web research density
  CASE WHEN s.agent_turns > 0
       THEN ROUND(CAST(COALESCE(web_stats.web_research_count, 0) AS DOUBLE) /
                  s.agent_turns, 4) ELSE 0 END AS web_per_agent_turn,
  CASE WHEN COALESCE(web_stats.web_research_count, 0) > 0
       THEN ROUND(CAST(COALESCE(web_stats.web_after_error_count, 0) AS DOUBLE) /
                  web_stats.web_research_count, 3) ELSE 0
       END AS web_error_triggered_fraction,
  -- Cost efficiency
  CASE WHEN s.agent_turns > 0
       THEN ROUND(COALESCE(cost_calc.cost_usd_total, 0) / s.agent_turns, 4)
       ELSE 0 END AS cost_usd_per_agent_turn,
  CASE WHEN COALESCE(wf_stats.mr_create_count, 0) > 0
       THEN ROUND(COALESCE(cost_calc.cost_usd_total, 0) / wf_stats.mr_create_count, 2)
       ELSE NULL END AS cost_usd_per_mr,
  CASE WHEN COALESCE(git_stats.git_commit_count, 0) > 0
       THEN ROUND(COALESCE(cost_calc.cost_usd_total, 0) / git_stats.git_commit_count, 3)
       ELSE NULL END AS cost_usd_per_commit,
  -- Tool-loop density
  CASE WHEN s.agent_loops > 0
       THEN ROUND(CAST(s.loop_turns_max AS DOUBLE) / s.agent_loops, 3) ELSE 0
       END AS max_loop_vs_avg_loop_ratio,
  CASE WHEN s.tool_calls_total > 0
       THEN ROUND(CAST(COALESCE(tool_runs.longest_same_tool_run, 0) AS DOUBLE) /
                  s.tool_calls_total, 4) ELSE 0
       END AS tool_loop_density,
  -- Read/write specifics
  CASE WHEN COALESCE(rw_stats.writes, 0) > 0
       THEN ROUND(CAST(COALESCE(rw_stats.writes_code, 0) AS DOUBLE) /
                  rw_stats.writes, 3) ELSE NULL
       END AS writes_code_fraction,
  -- Planning density
  CASE WHEN s.agent_turns > 0
       THEN ROUND(CAST(COALESCE(todo_stats.todo_write_count, 0) AS DOUBLE) /
                  s.agent_turns, 4) ELSE 0 END AS planning_per_agent_turn,
  -- Throughput (per hour of active time)
  CASE WHEN s.duration_active_sec > 0
       THEN ROUND(CAST(s.agent_turns AS DOUBLE) /
                  (s.duration_active_sec / 3600.0), 1) ELSE 0
       END AS agent_turns_per_active_hour,
  CASE WHEN s.duration_active_sec > 0
       THEN ROUND(CAST(s.human_turns AS DOUBLE) /
                  (s.duration_active_sec / 3600.0), 2) ELSE 0
       END AS human_turns_per_active_hour,
  -- Interruption density
  CASE WHEN s.agent_turns > 0
       THEN ROUND(CAST(s.interruption_mid_loop_count AS DOUBLE) / s.agent_turns, 4)
       ELSE 0 END AS interruption_per_agent_turn,
  -- MCP ratio
  CASE WHEN COALESCE(tools.all_tool_calls, 0) > 0
       THEN ROUND(CAST(COALESCE(tools.mcp_calls, 0) AS DOUBLE) /
                  tools.all_tool_calls, 4) ELSE 0
       END AS mcp_call_fraction,
  -- Cache rebuild cost as fraction of session cost
  CASE WHEN COALESCE(cost_calc.cost_usd_total, 0) > 0
       THEN ROUND(CAST(COALESCE(cache_exp.cache_rebuild_tokens, 0) * 3.0 AS DOUBLE) / 1000000.0 /
                  cost_calc.cost_usd_total, 4) ELSE 0
       END AS rebuild_cost_fraction_approx
FROM sessions s
LEFT JOIN tok        ON s.session = tok.session
LEFT JOIN tools      ON s.session = tools.session
LEFT JOIN tool_runs  ON s.session = tool_runs.session
LEFT JOIN comp       ON s.session = comp.session
LEFT JOIN subs       ON s.session = subs.session
LEFT JOIN roots      ON s.session = roots.session
LEFT JOIN cache_exp  ON s.session = cache_exp.session
LEFT JOIN git_stats  ON s.session = git_stats.session
LEFT JOIN wf_stats   ON s.session = wf_stats.session
LEFT JOIN bash_err   ON s.session = bash_err.session
LEFT JOIN cost_calc  ON s.session = cost_calc.session
LEFT JOIN cost_breakdown ON s.session = cost_breakdown.session
LEFT JOIN todo_stats ON s.session = todo_stats.session
LEFT JOIN human_stats ON s.session = human_stats.session
LEFT JOIN wp_stats   ON s.session = wp_stats.session
LEFT JOIN phase_stats ON s.session = phase_stats.session
LEFT JOIN rw_stats   ON s.session = rw_stats.session
LEFT JOIN bugfix_signals ON s.session = bugfix_signals.session
LEFT JOIN web_stats ON s.session = web_stats.session
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in-dir", default="/tmp/claude_analysis")
    ap.add_argument("--out", default="/tmp/claude_analysis/sessions_enriched.parquet")
    args = ap.parse_args()

    in_dir = Path(args.in_dir)
    con = duckdb.connect()
    for tbl in ("sessions", "turns", "compactions", "bash_commands", "tool_calls",
                "human_turns", "workflow_patterns"):
        con.execute(f"CREATE VIEW {tbl} AS SELECT * FROM '{in_dir}/{tbl}.parquet'")

    n = con.execute(f"SELECT COUNT(*) FROM sessions").fetchone()[0]
    print(f"Input: {n} sessions", file=sys.stderr)

    sql = ENRICH_SQL
    sql = sql.replace("{MODEL_WINDOW_K_SQL}", model_window_k_sql())
    sql = sql.replace("{IN_PRICE}", cost_sql("in"))
    sql = sql.replace("{OUT_PRICE}", cost_sql("out"))
    sql = sql.replace("{CR_PRICE}", cost_sql("cr"))
    sql = sql.replace("{CC5M_PRICE}", cost_sql("cc_5m"))
    sql = sql.replace("{CC1H_PRICE}", cost_sql("cc_1h"))
    con.execute(f"""
        COPY ({sql}) TO '{args.out}' (FORMAT PARQUET, COMPRESSION ZSTD)
    """)
    n_out = con.execute(f"SELECT COUNT(*) FROM '{args.out}'").fetchone()[0]
    size = Path(args.out).stat().st_size
    print(f"Wrote {n_out} rows, {size/1024:.0f} KB → {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
