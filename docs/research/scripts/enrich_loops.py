#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0"]
# ///
"""
Enrich loops.parquet with derived per-loop features:

- Cost: cost_usd (from per-turn tokens × per-model pricing)
- Cache: cache_read_total, cache_create_total, cache_hit_rate
- Trigger: first_loop / after_resume / after_correction / continuation
- Success proxy: based on loop_outcome + next human turn intent
- Next-human timing: time from loop_end to next human turn (if any)

Output: /tmp/claude_analysis/loops_enriched.parquet
"""
import argparse
import sys
from pathlib import Path

import duckdb


# Pricing per million tokens (as in compute_session_features.py)
MODEL_PRICING = {
    "claude-opus-4-7":    {"in": 15.00, "out": 75.00, "cr": 1.50,  "cc_5m": 18.75, "cc_1h": 30.00},
    "claude-opus-4-6":    {"in": 15.00, "out": 75.00, "cr": 1.50,  "cc_5m": 18.75, "cc_1h": 30.00},
    "claude-opus-4-5":    {"in": 15.00, "out": 75.00, "cr": 1.50,  "cc_5m": 18.75, "cc_1h": 30.00},
    "claude-sonnet-4-6":  {"in":  3.00, "out": 15.00, "cr": 0.30,  "cc_5m":  3.75, "cc_1h":  6.00},
    "claude-sonnet-4-5":  {"in":  3.00, "out": 15.00, "cr": 0.30,  "cc_5m":  3.75, "cc_1h":  6.00},
    "claude-sonnet-4":    {"in":  3.00, "out": 15.00, "cr": 0.30,  "cc_5m":  3.75, "cc_1h":  6.00},
    "claude-haiku-4-5":   {"in":  1.00, "out":  5.00, "cr": 0.10,  "cc_5m":  1.25, "cc_1h":  2.00},
    "glm-5.1":            {"in":  0.60, "out":  2.20, "cr": 0.11,  "cc_5m":  0.75, "cc_1h":  1.20},
    "glm-5":              {"in":  0.60, "out":  2.20, "cr": 0.11,  "cc_5m":  0.75, "cc_1h":  1.20},
    "glm-4.7":            {"in":  0.60, "out":  2.20, "cr": 0.11,  "cc_5m":  0.75, "cc_1h":  1.20},
    "glm-4.6":            {"in":  0.60, "out":  2.20, "cr": 0.11,  "cc_5m":  0.75, "cc_1h":  1.20},
    "glm-4.5":            {"in":  0.60, "out":  2.20, "cr": 0.11,  "cc_5m":  0.75, "cc_1h":  1.20},
    "kimi-for-coding":    {"in":  0.60, "out":  2.50, "cr": 0.15,  "cc_5m":  0.75, "cc_1h":  1.20},
    "kimi":               {"in":  0.60, "out":  2.50, "cr": 0.15,  "cc_5m":  0.75, "cc_1h":  1.20},
    "qwen3-coder":        {"in":  0.00, "out":  0.00, "cr": 0.00,  "cc_5m":  0.00, "cc_1h":  0.00},
    "gpt-oss:20b":        {"in":  0.00, "out":  0.00, "cr": 0.00,  "cc_5m":  0.00, "cc_1h":  0.00},
    "<synthetic>":        {"in":  0.00, "out":  0.00, "cr": 0.00,  "cc_5m":  0.00, "cc_1h":  0.00},
}


def cost_sql(field, column="model"):
    cases = []
    for prefix, rates in MODEL_PRICING.items():
        p = prefix.replace("'", "''")
        cases.append(f"WHEN {column} LIKE '{p}%' THEN {rates[field]}")
    cases_sql = "\n    ".join(cases)
    return f"(CASE {cases_sql} ELSE 3.0 END)"


ENRICH_SQL = """
WITH
  -- Per-turn cost (from turns with model + token fields)
  turn_cost AS (
    SELECT session, loop_id,
      SUM(
        (in_tokens * {IN_PRICE}
       + out_tokens * {OUT_PRICE}
       + cache_read * {CR_PRICE}
       + ephemeral_5m_tokens * {CC5M_PRICE}
       + ephemeral_1h_tokens * {CC1H_PRICE}
       + GREATEST(cache_create - ephemeral_5m_tokens - ephemeral_1h_tokens, 0) * {CC5M_PRICE}
        ) / 1000000.0
      ) AS cost_usd,
      SUM(cache_read) AS cache_read_total,
      SUM(cache_create) AS cache_create_total,
      SUM(in_tokens) AS in_tokens_total,
      SUM(out_tokens) AS out_tokens_total
    FROM turns
    WHERE turn_kind = 'agent' AND model != ''
    GROUP BY session, loop_id
  ),
  -- Next-human intent/time after loop end
  next_human AS (
    WITH loops_ts AS (
      SELECT session, loop_id, loop_end_ms FROM loops
    ),
    matches AS (
      SELECT l.session, l.loop_id,
        h.ts_ms AS next_human_ts,
        h.intent AS next_human_intent,
        h.chars AS next_human_chars,
        ROW_NUMBER() OVER (
          PARTITION BY l.session, l.loop_id
          ORDER BY h.ts_ms
        ) AS rn
      FROM loops_ts l
      JOIN human_turns h
        ON h.session = l.session
       AND h.ts_ms > l.loop_end_ms
       AND h.ts_ms < l.loop_end_ms + 604800000  -- within 7 days
    )
    SELECT session, loop_id,
      (next_human_ts - (SELECT loop_end_ms FROM loops_ts WHERE session=matches.session AND loop_id=matches.loop_id))/1000 AS lag_to_next_human_sec,
      next_human_intent,
      next_human_chars
    FROM matches WHERE rn = 1
  ),
  -- Previous loop's next_human intent → determines if THIS loop was a correction trigger
  prev_loop_next AS (
    SELECT
      l.session, l.loop_id,
      (SELECT nh.next_human_intent FROM next_human nh
       WHERE nh.session = l.session AND nh.loop_id = l.loop_id - 1) AS triggering_intent
    FROM loops l
  )
SELECT
  l.*,
  COALESCE(tc.cost_usd, 0) AS cost_usd,
  COALESCE(tc.cache_read_total, 0) AS cache_read_total,
  COALESCE(tc.cache_create_total, 0) AS cache_create_total,
  COALESCE(tc.in_tokens_total, 0) AS in_tokens_total,
  COALESCE(tc.out_tokens_total, 0) AS out_tokens_total,
  -- Cache hit rate per loop
  CASE
    WHEN COALESCE(tc.cache_read_total + tc.cache_create_total + tc.in_tokens_total, 0) > 0
    THEN ROUND(CAST(tc.cache_read_total AS DOUBLE) /
               (tc.cache_read_total + tc.cache_create_total + tc.in_tokens_total), 4)
    ELSE 0 END AS cache_hit_rate,
  -- Next human turn (success proxy)
  nh.lag_to_next_human_sec,
  COALESCE(nh.next_human_intent, '') AS next_human_intent,
  COALESCE(nh.next_human_chars, 0) AS next_human_chars,
  -- Trigger classification
  CASE
    WHEN l.loop_id = 0 THEN 'session_start'
    WHEN plp.triggering_intent = 'course_correct' THEN 'after_correction'
    WHEN plp.triggering_intent = 'report_error' THEN 'after_error_report'
    WHEN plp.triggering_intent = 'approve_short' THEN 'after_approval'
    WHEN plp.triggering_intent = 'instruction' THEN 'new_instruction'
    WHEN plp.triggering_intent = 'short_prompt' THEN 'quick_continuation'
    WHEN plp.triggering_intent IN ('question', 'quick_question') THEN 'question_followup'
    WHEN plp.triggering_intent = 'detailed_spec' THEN 'detailed_spec_task'
    ELSE 'continuation'
  END AS trigger_type,
  -- Success proxy
  CASE
    -- Hard success signals
    WHEN l.loop_outcome = 'target_write_committed' THEN 'confirmed_success'
    WHEN l.loop_outcome = 'target_create_entity' THEN 'confirmed_success'
    -- Redirected — next human was a correction
    WHEN nh.next_human_intent = 'course_correct' THEN 'redirected'
    WHEN nh.next_human_intent = 'report_error' THEN 'error_reported'
    -- Partial: wrote but didn't commit
    WHEN l.loop_outcome = 'target_write_uncommitted' THEN 'partial_write'
    -- Text answer, no correction follow-up → probably ok
    WHEN l.loop_outcome IN ('target_answer_text', 'target_answer_research')
         AND (nh.next_human_intent IS NULL OR nh.next_human_intent NOT IN ('course_correct', 'report_error'))
      THEN 'likely_answer_ok'
    -- Collab (MR comments)
    WHEN l.loop_outcome = 'target_collab' THEN 'collab_response'
    -- Nothing concrete happened
    WHEN l.loop_outcome = 'no_target' THEN 'no_outcome'
    ELSE 'unclear'
  END AS success_proxy
FROM loops l
LEFT JOIN turn_cost tc USING (session, loop_id)
LEFT JOIN next_human nh USING (session, loop_id)
LEFT JOIN prev_loop_next plp USING (session, loop_id)
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in-dir", default="/tmp/claude_analysis")
    ap.add_argument("--out", default="/tmp/claude_analysis/loops_enriched.parquet")
    args = ap.parse_args()

    in_dir = Path(args.in_dir)
    con = duckdb.connect()
    for tbl in ("loops", "turns", "human_turns"):
        con.execute(f"CREATE VIEW {tbl} AS SELECT * FROM '{in_dir}/{tbl}.parquet'")

    sql = ENRICH_SQL
    sql = sql.replace("{IN_PRICE}", cost_sql("in"))
    sql = sql.replace("{OUT_PRICE}", cost_sql("out"))
    sql = sql.replace("{CR_PRICE}", cost_sql("cr"))
    sql = sql.replace("{CC5M_PRICE}", cost_sql("cc_5m"))
    sql = sql.replace("{CC1H_PRICE}", cost_sql("cc_1h"))

    n_in = con.execute("SELECT COUNT(*) FROM loops").fetchone()[0]
    print(f"# Enriching {n_in} loops with cost / cache / trigger / success_proxy",
          file=sys.stderr)

    con.execute(f"COPY ({sql}) TO '{args.out}' (FORMAT PARQUET, COMPRESSION ZSTD)")
    n_out = con.execute(f"SELECT COUNT(*) FROM '{args.out}'").fetchone()[0]
    size = Path(args.out).stat().st_size
    print(f"# Wrote {n_out} rows, {size/1024:.0f} KB → {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
