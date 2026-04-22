#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0", "numpy", "scipy"]
# ///
"""
Partial correlation analysis — compute correlations between features
after removing the session-volume confound.

Method:
  partial_r(X, Y | Z) = corr(residual(X | Z), residual(Y | Z))

where Z is a control variable (typically `total_records` = session size).

This strips "both features scale with session size" effects and reveals
real behavioral correlations.
"""
import argparse
import sys

import duckdb
import numpy as np
from scipy import stats
import pyarrow as pa
import pyarrow.parquet as pq


# Features to include — focus on potentially behavioral
FEATURES = [
    # Volume signals (will mostly vanish after size control)
    "agent_turns", "human_turns", "tool_calls_total",
    "duration_active_sec", "duration_idle_sec", "llm_work_sec",
    # Context/cache
    "context_size_max", "context_size_mean", "cache_hit_rate",
    "mean_context_fill_fraction", "ceiling_approaches",
    "cache_5m_expired_count", "cache_1h_expired_count", "cache_rebuild_tokens",
    "exp_cause_human_afk", "exp_cause_long_tool", "exp_cause_chain",
    # Compactions
    "compaction_count", "compact_pre_avg",
    # Cost
    "cost_usd_total", "cost_per_turn_mean_usd", "cost_per_turn_max_usd",
    # Tools
    "tool_diversity", "longest_same_tool_run",
    "mcp_fraction", "bash_fraction",
    "subagents_in_tree",
    # Git
    "git_commit_count", "git_push_count", "git_merge_count",
    # Workflow
    "mr_create_count", "issue_create_count", "mr_comment_count",
    "transcript_pulls", "ci_checks",
    # Phase iterations
    "research_to_impl_cycles", "verify_to_research_backloops",
    "phase_transitions",
    # Read/Write
    "reads", "writes", "searches",
    "writes_code", "writes_test", "writes_config", "writes_docs",
    # Planning
    "todo_write_count", "task_mgmt_count",
    # Errors
    "tool_error_count", "bash_error_count", "api_error_count",
    # Interrupts
    "interruption_mid_loop_count", "queue_operation_count",
    "resume_with_prompt_count",
    # Human turn
    "course_correct_count", "error_report_count", "instruction_count",
    "detailed_spec_count", "urgent_count",
    "total_human_chars",
    # Web
    "web_research_count", "web_after_error_count", "web_before_impl_count",
    # Workflow patterns
    "self_review_loops", "ci_cycles", "merge_arcs",
    # Rates (already normalized — partial shouldn't affect much)
    "correction_rate",
]

CONTROL = "total_records"


def partial_correlation(x, y, z):
    """
    Partial correlation of X and Y controlling for Z.
    Returns (r, p, n).
    """
    mask = np.isfinite(x) & np.isfinite(y) & np.isfinite(z)
    x, y, z = x[mask], y[mask], z[mask]
    n = len(x)
    if n < 20 or np.std(z) == 0 or np.std(x) == 0 or np.std(y) == 0:
        return 0.0, 1.0, n

    # Use log(1+z) to handle session-size skew
    z_log = np.log1p(z)
    if np.std(z_log) == 0:
        return 0.0, 1.0, n

    # Residuals via OLS: r_x = x - (a + b*z_log); r_y = y - (c + d*z_log)
    # Simple implementation
    z_mean = z_log.mean()
    x_mean = x.mean()
    y_mean = y.mean()
    zc = z_log - z_mean
    zz = (zc ** 2).sum()
    if zz == 0:
        return 0.0, 1.0, n
    bx = (zc * (x - x_mean)).sum() / zz
    by = (zc * (y - y_mean)).sum() / zz
    rx = (x - x_mean) - bx * zc
    ry = (y - y_mean) - by * zc
    if np.std(rx) == 0 or np.std(ry) == 0:
        return 0.0, 1.0, n
    r = np.corrcoef(rx, ry)[0, 1]
    if not np.isfinite(r):
        return 0.0, 1.0, n
    # t-test with df = n - 3 (2 for control adjustment)
    t = r * np.sqrt((n - 3) / max(1e-10, 1 - r * r))
    p = 2 * (1 - stats.t.cdf(abs(t), df=n - 3))
    return float(r), float(p), n


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp",
                    default="/tmp/claude_analysis/sessions_enriched.parquet")
    ap.add_argument("--out",
                    default="/tmp/claude_analysis/partial_correlations.parquet")
    ap.add_argument("--min-effect", type=float, default=0.2)
    ap.add_argument("--max-p", type=float, default=0.01)
    args = ap.parse_args()

    con = duckdb.connect()
    con.execute(f"CREATE VIEW se_raw AS SELECT * FROM '{args.inp}'")
    con.execute("""
      CREATE VIEW se AS
      SELECT * FROM (
        SELECT *, ROW_NUMBER() OVER (PARTITION BY session ORDER BY total_records DESC) AS rn
        FROM se_raw WHERE NOT is_subagent
      ) WHERE rn = 1
    """)
    n_sess = con.execute("SELECT COUNT(*) FROM se").fetchone()[0]
    print(f"# Partial correlations on {n_sess} unique main sessions",
          file=sys.stderr)
    print(f"# Control: log1p({CONTROL})", file=sys.stderr)

    # Pull all features
    available = {r[0] for r in con.execute("DESCRIBE se").fetchall()}
    feats = [f for f in FEATURES if f in available]
    if CONTROL not in available:
        print(f"ERROR: control variable {CONTROL} not found", file=sys.stderr)
        sys.exit(1)

    cols = feats + [CONTROL]
    rows = con.execute(f"SELECT {', '.join(cols)} FROM se").fetchall()
    M = {c: np.array([float(row[i]) if row[i] is not None else np.nan
                      for row in rows]) for i, c in enumerate(cols)}
    z = M[CONTROL]

    results = []
    n_pairs = len(feats) * (len(feats) - 1) // 2
    print(f"# Computing {n_pairs} pairs …", file=sys.stderr)

    for i, a in enumerate(feats):
        for b in feats[i + 1:]:
            r, p, n = partial_correlation(M[a], M[b], z)
            if abs(r) >= args.min_effect and p < args.max_p:
                # Also compute raw Pearson for comparison
                mask = np.isfinite(M[a]) & np.isfinite(M[b])
                if mask.sum() >= 10 and np.std(M[a][mask]) > 0 and np.std(M[b][mask]) > 0:
                    r_raw = float(np.corrcoef(M[a][mask], M[b][mask])[0, 1])
                else:
                    r_raw = 0.0
                results.append({
                    "feature_a": a, "feature_b": b,
                    "partial_r": round(r, 4),
                    "raw_r": round(r_raw, 4),
                    "diff": round(r - r_raw, 4),
                    "p_value": round(p, 6), "n": n,
                })

    # Sort by |partial_r|
    results.sort(key=lambda x: -abs(x["partial_r"]))

    # Separate by type
    print(f"\n# Partial correlations report\n")
    print(f"Sessions: {n_sess} (deduped main)")
    print(f"Control: log1p({CONTROL})")
    print(f"Threshold: |partial_r| >= {args.min_effect}, p < {args.max_p}")
    print(f"Total significant partial correlations: {len(results)}\n")

    def mdtable(title, rs, max_rows=30):
        print(f"## {title}\n")
        if not rs:
            print("_(none)_\n"); return
        print("| feature_a | feature_b | partial_r | raw_r | Δ (partial−raw) | p_value | n |")
        print("|-----------|-----------|----------:|------:|----------------:|--------:|--:|")
        for r in rs[:max_rows]:
            diff = r["diff"]
            sign = "↑" if diff > 0.1 else ("↓" if diff < -0.1 else " ")
            print(f"| {r['feature_a']} | {r['feature_b']} | {r['partial_r']:+.3f} | "
                  f"{r['raw_r']:+.3f} | {diff:+.3f} {sign} | {r['p_value']:.4g} | {r['n']} |")
        print()

    mdtable("Top partial correlations (all)", results, 30)

    # Pairs where partial >> raw (size was HIDING a correlation)
    hidden = sorted([r for r in results if r["partial_r"] - r["raw_r"] > 0.15],
                    key=lambda x: -(x["partial_r"] - x["raw_r"]))
    mdtable("Correlations REVEALED by controlling for size (partial > raw)",
            hidden, 20)

    # Pairs where partial << raw (size was CAUSING apparent correlation)
    illusory = sorted([r for r in results if r["raw_r"] - r["partial_r"] > 0.15],
                     key=lambda x: -(x["raw_r"] - x["partial_r"]))
    mdtable("Correlations EXPLAINED AWAY by size (raw > partial)",
            illusory, 20)

    # Pairs where sign flipped
    flipped = [r for r in results if np.sign(r["partial_r"]) != np.sign(r["raw_r"])
               and abs(r["raw_r"]) > 0.1]
    mdtable("Sign-flipped pairs (direction reversed after size control)", flipped, 15)

    # Write parquet
    if results:
        schema = pa.schema([
            pa.field("feature_a", pa.string()),
            pa.field("feature_b", pa.string()),
            pa.field("partial_r", pa.float64()),
            pa.field("raw_r", pa.float64()),
            pa.field("diff", pa.float64()),
            pa.field("p_value", pa.float64()),
            pa.field("n", pa.int32()),
        ])
        pq.write_table(pa.Table.from_pylist(results, schema=schema),
                       args.out, compression="zstd")
        print(f"\n_Wrote {len(results)} rows → {args.out}_", file=sys.stderr)


if __name__ == "__main__":
    main()
