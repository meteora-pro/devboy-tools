#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0", "numpy", "scipy"]
# ///
"""
Within-bucket correlation analysis.

For each size_bucket (tiny/small/medium/large/huge), compute correlations
on rate-based features. Identifies patterns that apply at specific scales
vs universal patterns.

Uses rate features added in compute_session_features.py to avoid volume-
confound entirely.
"""
import argparse
import sys

import duckdb
import numpy as np
from scipy import stats


# Focus on rate-based features — volume-invariant by construction
RATE_FEATURES = [
    "cache_hit_rate", "mean_context_fill_fraction",
    "tool_error_rate", "bash_error_rate",
    "correction_per_agent_turn", "api_error_per_agent_turn",
    "tools_per_agent_turn",
    "research_to_impl_ratio", "verify_to_impl_ratio",
    "web_per_agent_turn", "web_error_triggered_fraction",
    "cost_usd_per_agent_turn",
    "tool_loop_density", "max_loop_vs_avg_loop_ratio",
    "writes_code_fraction",
    "planning_per_agent_turn",
    "agent_turns_per_active_hour", "human_turns_per_active_hour",
    "interruption_per_agent_turn",
    "mcp_call_fraction", "bash_fraction",
    "correction_rate", "rebuild_cost_fraction_approx",
]

BINARY_FEATURES = [
    "used_extended_context", "is_bug_fix_session", "uses_planning",
    "has_tool_loop", "has_context_grinding",
    "signal_went_wrong", "signal_gross_errors", "signal_multi_task",
]


def pearson(x, y, min_n=10):
    mask = np.isfinite(x) & np.isfinite(y)
    x, y = x[mask], y[mask]
    n = len(x)
    if n < min_n or np.std(x) == 0 or np.std(y) == 0:
        return None, None, n
    r = float(np.corrcoef(x, y)[0, 1])
    if not np.isfinite(r):
        return None, None, n
    t = r * np.sqrt((n - 2) / max(1e-10, 1 - r * r))
    p = 2 * (1 - stats.t.cdf(abs(t), df=n - 2))
    return r, float(p), n


def bucket_analysis(con, bucket, rate_cols, bin_cols, min_effect, max_p):
    where = f"size_bucket = '{bucket}'" if bucket != "all" else "1=1"
    df = con.execute(f"""
      SELECT {', '.join(rate_cols + bin_cols)}
      FROM sd WHERE {where}
    """).fetchall()
    n = len(df)
    if n < 20:
        return n, []
    M = {c: np.array([float(row[i]) if row[i] is not None else np.nan
                      for row in df]) for i, c in enumerate(rate_cols + bin_cols)}
    rows = []
    # Rate × rate
    for i, a in enumerate(rate_cols):
        for b in rate_cols[i + 1:]:
            r, p, nn = pearson(M[a], M[b])
            if r is not None and abs(r) >= min_effect and p < max_p:
                rows.append({"a": a, "b": b, "r": round(r, 3),
                             "p": round(p, 6), "n": nn, "kind": "rate×rate"})
    # Binary × rate
    for bc in bin_cols:
        for rc in rate_cols:
            r, p, nn = pearson(M[bc], M[rc])
            if r is not None and abs(r) >= min_effect and p < max_p:
                rows.append({"a": bc, "b": rc, "r": round(r, 3),
                             "p": round(p, 6), "n": nn, "kind": "bin×rate"})
    rows.sort(key=lambda x: -abs(x["r"]))
    return n, rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp",
                    default="/tmp/claude_analysis/sessions_enriched.parquet")
    ap.add_argument("--min-effect", type=float, default=0.2)
    ap.add_argument("--max-p", type=float, default=0.01)
    ap.add_argument("--top", type=int, default=15)
    args = ap.parse_args()

    con = duckdb.connect()
    con.execute(f"CREATE VIEW se AS SELECT * FROM '{args.inp}'")
    con.execute("""
      CREATE VIEW sd AS SELECT * FROM (
        SELECT *, ROW_NUMBER() OVER (PARTITION BY session ORDER BY total_records DESC) AS rn
        FROM se WHERE NOT is_subagent
      ) WHERE rn = 1
    """)
    available = {r[0] for r in con.execute("DESCRIBE sd").fetchall()}
    rate_cols = [c for c in RATE_FEATURES if c in available]
    bin_cols = [c for c in BINARY_FEATURES if c in available]

    print(f"# Within-bucket correlation analysis")
    print(f"Features: {len(rate_cols)} rate + {len(bin_cols)} binary")
    print(f"Threshold: |r| >= {args.min_effect}, p < {args.max_p}\n")

    buckets = ["all", "main:tiny", "main:small", "main:medium",
               "main:large", "main:huge"]
    all_bucket_results = {}

    for b in buckets:
        n, rows = bucket_analysis(con, b, rate_cols, bin_cols,
                                  args.min_effect, args.max_p)
        all_bucket_results[b] = (n, rows)
        print(f"\n## {b}  (n={n})\n")
        if n < 20:
            print("_(too few sessions for stable correlation)_\n")
            continue
        print(f"Found {len(rows)} significant pairs. Top-{args.top}:\n")
        print("| feature_a | feature_b | r | p | n | kind |")
        print("|-----------|-----------|--:|--:|--:|------|")
        for r in rows[:args.top]:
            print(f"| {r['a']} | {r['b']} | {r['r']:+.3f} | {r['p']:.4g} | {r['n']} | {r['kind']} |")

    # Cross-bucket summary: which correlations are STABLE (present in all buckets)
    # vs bucket-specific
    print("\n\n## Cross-bucket stability\n")
    print("Pairs that appear in ≥3 buckets vs bucket-specific pairs:\n")
    pair_counts = {}
    for b, (_, rows) in all_bucket_results.items():
        if b == "all": continue
        for r in rows:
            key = (r["a"], r["b"])
            if key not in pair_counts:
                pair_counts[key] = {}
            pair_counts[key][b] = r["r"]

    stable = [(k, v) for k, v in pair_counts.items() if len(v) >= 3]
    specific = [(k, v) for k, v in pair_counts.items() if len(v) == 1]

    print(f"### Stable across ≥3 buckets ({len(stable)} pairs)\n")
    # Sort by min absolute effect across buckets (want consistently strong)
    stable.sort(key=lambda x: -min(abs(v) for v in x[1].values()))
    print("| feature_a | feature_b | tiny | small | medium | large | huge |")
    print("|-----------|-----------|-----:|------:|-------:|------:|-----:|")
    for (a, b), by_bucket in stable[:20]:
        cells = []
        for bkt in ("main:tiny", "main:small", "main:medium", "main:large", "main:huge"):
            v = by_bucket.get(bkt)
            cells.append(f"{v:+.2f}" if v is not None else "—")
        print(f"| {a} | {b} | " + " | ".join(cells) + " |")

    print(f"\n### Bucket-specific (only in one bucket, {len(specific)} pairs) — top-15 by strength\n")
    specific.sort(key=lambda x: -max(abs(v) for v in x[1].values()))
    print("| feature_a | feature_b | bucket | r |")
    print("|-----------|-----------|--------|--:|")
    for (a, b), by_bucket in specific[:15]:
        bkt, r = next(iter(by_bucket.items()))
        print(f"| {a} | {b} | {bkt} | {r:+.2f} |")


if __name__ == "__main__":
    main()
