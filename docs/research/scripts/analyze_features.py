#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0", "numpy"]
# ///
"""
EDA on sessions_enriched.parquet — correlations, outliers, distributions.

No LLM. Pure DuckDB + numpy. Goal: extract hypotheses and identify outlier
sessions worth deep-diving with LLM in the next pass.

Output: markdown report to stdout.
"""
import argparse
import sys
from pathlib import Path

import duckdb
import numpy as np


# Features to cross-correlate
NUMERIC_FEATURES = [
    "total_records", "human_turns", "agent_turns", "tool_calls_total",
    "agent_loops", "loop_turns_max", "loop_calls_max",
    "context_size_max", "context_size_mean",
    "cache_read_total", "cache_create_total", "cache_hit_rate",
    "compaction_count", "compact_pre_avg",
    "api_error_count", "subagents_in_tree",
    "tool_diversity", "longest_same_tool_run",
    "mcp_calls", "bash_calls", "mcp_fraction", "bash_fraction",
    "out_tokens_per_min", "human_agent_ratio",
    "duration_sec", "ceiling_approaches",
]


def md_table(rows, headers):
    w = [max(len(h), max((len(str(r[i])) for r in rows), default=0)) for i, h in enumerate(headers)]
    lines = []
    lines.append("| " + " | ".join(h.ljust(w[i]) for i, h in enumerate(headers)) + " |")
    lines.append("|" + "|".join("-" * (w[i] + 2) for i in range(len(headers))) + "|")
    for r in rows:
        lines.append("| " + " | ".join(str(v).ljust(w[i]) for i, v in enumerate(r)) + " |")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--path", default="/tmp/claude_analysis/sessions_enriched.parquet")
    ap.add_argument("--scope", default="main",
                    choices=["all", "main", "subagent"],
                    help="Analyze which subset")
    args = ap.parse_args()

    con = duckdb.connect()
    con.execute(f"CREATE VIEW s AS SELECT * FROM '{args.path}'")
    where = {
        "main": "WHERE NOT is_subagent",
        "subagent": "WHERE is_subagent",
        "all": ""
    }[args.scope]
    con.execute(f"CREATE VIEW ss AS SELECT * FROM s {where}")
    n = con.execute("SELECT COUNT(*) FROM ss").fetchone()[0]

    print(f"# Session-feature EDA — scope: {args.scope} ({n} sessions)\n")

    # ─── 1. Size bucket breakdown ─────────────────────────────────────────
    print("## 1. Size bucket distribution\n")
    rows = con.execute("""
      SELECT size_bucket, COUNT(*) AS n,
        ROUND(AVG(duration_sec)/3600, 1) AS avg_hours,
        ROUND(AVG(tool_calls_total), 0) AS avg_calls,
        ROUND(AVG(cache_hit_rate), 3) AS avg_cache_hit,
        ROUND(AVG(compaction_count), 2) AS avg_compact,
        ROUND(AVG(context_size_max)) AS ctx_max_mean
      FROM ss GROUP BY 1 ORDER BY 1
    """).fetchall()
    print(md_table(rows, ["size_bucket", "n", "avg_hours", "avg_calls",
                          "cache_hit", "compact", "ctx_max_mean"]))

    # ─── 2. Correlation matrix for numeric features (Pearson) ────────────
    print("\n## 2. Feature correlations (Pearson, filtered |r| ≥ 0.3)\n")
    feats = NUMERIC_FEATURES
    # Build value matrix
    cols_sql = ", ".join(f"COALESCE({f}, 0) AS {f}" for f in feats)
    df = con.execute(f"SELECT {cols_sql} FROM ss").fetchall()
    if df:
        M = np.array(df, dtype=float)
        # Correlation matrix
        with np.errstate(invalid='ignore', divide='ignore'):
            corr = np.corrcoef(M, rowvar=False)
        corr = np.nan_to_num(corr, nan=0)
        pairs = []
        for i in range(len(feats)):
            for j in range(i + 1, len(feats)):
                r = corr[i, j]
                if abs(r) >= 0.3:
                    pairs.append((feats[i], feats[j], round(r, 3)))
        pairs.sort(key=lambda x: -abs(x[2]))
        # Print top 30
        print(md_table(pairs[:30], ["feature_A", "feature_B", "pearson_r"]))

    # ─── 3. Outliers by feature (top-10 and bottom-10 on z-score) ────────
    print("\n## 3. Interesting outliers\n")
    rows = con.execute("""
      SELECT session, size_bucket, total_records, tool_calls_total,
        cache_hit_rate, compaction_count, context_size_max,
        longest_same_tool_run, subagents_in_tree, api_error_count
      FROM ss
      ORDER BY tool_calls_total DESC LIMIT 10
    """).fetchall()
    print("### 3a. Heaviest sessions (by tool calls)\n")
    print(md_table(rows, ["session", "bucket", "records", "calls", "cache_hit",
                          "compact", "ctx_max", "longest_run", "sub_tree", "errors"]))

    print("\n### 3b. Lowest cache hit rate (≤ 0.7, by volume ≥ 20 tool calls)\n")
    rows = con.execute("""
      SELECT session, size_bucket, tool_calls_total, cache_hit_rate,
        out_tokens_total, top_model, compaction_count
      FROM ss
      WHERE tool_calls_total >= 20 AND cache_hit_rate <= 0.7
      ORDER BY cache_hit_rate ASC LIMIT 10
    """).fetchall()
    print(md_table(rows, ["session", "bucket", "calls", "cache_hit",
                          "out_tok", "model", "compact"]))

    print("\n### 3c. Tool-loop sessions (longest same-tool run)\n")
    rows = con.execute("""
      SELECT session, size_bucket, tool_calls_total,
        longest_same_tool_run, tool_diversity, api_error_count, top_model
      FROM ss
      WHERE longest_same_tool_run >= 15
      ORDER BY longest_same_tool_run DESC LIMIT 10
    """).fetchall()
    print(md_table(rows, ["session", "bucket", "calls", "longest_run",
                          "diversity", "errors", "model"]))

    print("\n### 3d. High-retry sessions (api_error_count ≥ 30)\n")
    rows = con.execute("""
      SELECT session, size_bucket, tool_calls_total, api_error_count,
        duration_sec / 3600 AS hours, top_model, claude_code_version
      FROM ss
      WHERE api_error_count >= 30
      ORDER BY api_error_count DESC LIMIT 10
    """).fetchall()
    print(md_table(rows, ["session", "bucket", "calls", "errors", "hours", "model", "cc_ver"]))

    print("\n### 3e. Compaction anomalies\n")
    rows = con.execute("""
      SELECT session, size_bucket,
        compaction_count, compact_auto, compact_manual,
        compact_pre_min, compact_pre_avg, compact_pre_max
      FROM ss
      WHERE compaction_count >= 10
      ORDER BY compaction_count DESC LIMIT 10
    """).fetchall()
    print(md_table(rows, ["session", "bucket", "total", "auto", "manual",
                          "pre_min", "pre_avg", "pre_max"]))

    # ─── 4. Distribution percentiles ──────────────────────────────────────
    print("\n## 4. Feature percentiles (main sessions only, non-zero values)\n")
    for f in ["tool_calls_total", "context_size_max", "cache_hit_rate",
              "longest_same_tool_run", "compaction_count", "subagents_in_tree",
              "ceiling_approaches", "mcp_fraction"]:
        r = con.execute(f"""
          SELECT
            ROUND(quantile_cont({f}, 0.5), 3) AS p50,
            ROUND(quantile_cont({f}, 0.9), 3) AS p90,
            ROUND(quantile_cont({f}, 0.99), 3) AS p99,
            MAX({f}) AS max
          FROM ss WHERE {f} IS NOT NULL AND {f} > 0
        """).fetchone()
        if r:
            print(f"- **{f}**: p50={r[0]}  p90={r[1]}  p99={r[2]}  max={r[3]}")

    # ─── 5. Claude Code version evolution  ────────────────────────────────
    print("\n## 5. Behavior change by Claude Code version band\n")
    rows = con.execute("""
      WITH ver_band AS (
        SELECT *,
          CASE
            WHEN claude_code_version LIKE '2.0.%' THEN '2.0.x (Dec-Jan)'
            WHEN claude_code_version LIKE '2.1.1' OR claude_code_version LIKE '2.1.2'
              OR claude_code_version LIKE '2.1.3' OR claude_code_version LIKE '2.1.4'
              OR claude_code_version LIKE '2.1.5' OR claude_code_version LIKE '2.1.6'
              OR claude_code_version LIKE '2.1.7' OR claude_code_version LIKE '2.1.8'
              OR claude_code_version LIKE '2.1.9' OR claude_code_version LIKE '2.1.12' THEN '2.1.0-12 (Jan-Feb)'
            WHEN substr(claude_code_version, 1, 4) = '2.1.'
              AND CAST(substr(claude_code_version, 5) AS INT) BETWEEN 30 AND 70 THEN '2.1.30-70 (Feb-Mar)'
            WHEN substr(claude_code_version, 1, 4) = '2.1.'
              AND CAST(substr(claude_code_version, 5) AS INT) >= 70 THEN '2.1.70+ (Mar-Apr)'
            ELSE claude_code_version
          END AS ver_band
        FROM ss WHERE claude_code_version != ''
      )
      SELECT ver_band, COUNT(*) AS n,
        ROUND(AVG(tool_calls_total), 1) AS avg_calls,
        ROUND(AVG(compaction_count), 2) AS avg_compact,
        ROUND(AVG(cache_hit_rate), 3) AS cache_hit,
        ROUND(AVG(loop_turns_max), 1) AS max_loop_turns,
        ROUND(AVG(subagents_in_tree), 1) AS subs
      FROM ver_band GROUP BY 1 ORDER BY 1
    """).fetchall()
    print(md_table(rows, ["cc_version_band", "n", "calls", "compact",
                          "cache_hit", "max_loop", "subs"]))

    # ─── 6. Model behavior comparison ─────────────────────────────────────
    print("\n## 6. Behavior by top model\n")
    rows = con.execute("""
      SELECT top_model, COUNT(*) AS n,
        ROUND(AVG(tool_calls_total), 1) AS avg_calls,
        ROUND(AVG(compaction_count), 2) AS avg_compact,
        ROUND(AVG(cache_hit_rate), 3) AS cache_hit,
        ROUND(AVG(loop_turns_max), 1) AS max_loop,
        ROUND(AVG(out_tokens_per_min), 1) AS out_tok_per_min,
        ROUND(AVG(api_error_count), 1) AS errors
      FROM ss WHERE top_model != ''
      GROUP BY 1 HAVING COUNT(*) >= 10
      ORDER BY n DESC
    """).fetchall()
    print(md_table(rows, ["model", "n", "calls", "compact", "cache_hit",
                          "max_loop", "out_per_min", "errors"]))

    # ─── 7. Hypothesis seeds  ─────────────────────────────────────────────
    print("\n## 7. Hypothesis seeds for LLM follow-up\n")
    # 7a. Sessions with huge subagent tree but low tool_calls in root
    rows = con.execute("""
      SELECT session, total_records, tool_calls_total,
        subagents_in_tree, bash_fraction, top_model
      FROM ss WHERE NOT is_subagent AND subagents_in_tree >= 30
        AND tool_calls_total < subagents_in_tree * 5
      ORDER BY subagents_in_tree DESC LIMIT 10
    """).fetchall()
    if rows:
        print("### H1. Sessions that delegated heavily (root ≪ subagent work)\n")
        print(md_table(rows, ["session", "records", "calls", "subs", "bash_frac", "model"]))

    # 7b. Sessions that hit ceiling often but without compaction (likely lost work)
    rows = con.execute("""
      SELECT session, total_records, context_size_max, ceiling_approaches,
        compaction_count, top_model, claude_code_version
      FROM ss
      WHERE ceiling_approaches >= 10 AND compaction_count = 0
      ORDER BY ceiling_approaches DESC LIMIT 10
    """).fetchall()
    if rows:
        print("\n### H2. Ceiling approaches WITHOUT compaction (lost context?)\n")
        print(md_table(rows, ["session", "records", "ctx_max", "ceilings",
                              "compact", "model", "cc_ver"]))

    # 7c. Low cache hit sessions — candidate for new cache-prefix optimization
    rows = con.execute("""
      SELECT session, size_bucket, tool_calls_total, cache_hit_rate,
        out_tokens_total, top_model
      FROM ss WHERE tool_calls_total >= 50 AND cache_hit_rate < 0.9
      ORDER BY cache_hit_rate ASC LIMIT 10
    """).fetchall()
    if rows:
        print("\n### H3. Sessions with unexpectedly low cache hit rate\n")
        print(md_table(rows, ["session", "bucket", "calls", "cache_hit",
                              "out_tok", "model"]))


if __name__ == "__main__":
    main()
