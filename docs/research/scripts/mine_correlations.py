#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0", "numpy", "scipy"]
# ///
"""
Correlation mining pass on sessions_enriched.parquet.

Computes pairwise correlations between features and ranks them by effect size.
Separates trivial/tautological pairs from potentially interesting ones.

Stats used:
  continuous × continuous: Pearson r (with p-value via t-test)
  binary × continuous: point-biserial correlation (special case of Pearson)
  categorical × continuous: ANOVA eta-squared
  categorical × categorical: Cramer's V

Output: markdown report to stdout + `correlations.csv` / `correlations.parquet`
"""
import argparse
import sys
from pathlib import Path

import duckdb
import numpy as np
from scipy import stats
import pyarrow as pa
import pyarrow.parquet as pq


# Feature groups — curated from sessions_enriched schema
CONTINUOUS = [
    # Volume
    "total_records", "human_turns", "agent_turns", "tool_calls_total",
    "agent_loops", "loop_turns_max", "loop_calls_max",
    "duration_sec", "duration_active_sec", "duration_idle_sec",
    "llm_work_sec", "resume_count", "longest_gap_sec",
    # Context
    "context_size_max", "context_size_mean", "ceiling_approaches",
    "ctx_over_500k", "ctx_over_900k", "mean_context_fill_fraction",
    # Tokens / cache
    "cache_read_total", "cache_create_total", "cache_hit_rate",
    "cache_5m_expired_count", "cache_1h_expired_count", "cache_rebuild_tokens",
    "exp_cause_human_afk", "exp_cause_long_tool", "exp_cause_chain",
    # Compactions
    "compaction_count", "compact_pre_avg",
    # Cost
    "cost_usd_total", "cost_per_turn_mean_usd", "cost_per_turn_max_usd",
    "cost_input_usd", "cost_output_usd", "cost_cache_read_usd",
    "cost_cache_5m_usd", "cost_cache_1h_usd",
    # Tool mix
    "tool_diversity", "longest_same_tool_run",
    "mcp_calls", "bash_calls", "mcp_fraction", "bash_fraction",
    "subagents_in_tree",
    # Git
    "git_commit_count", "git_push_count", "git_merge_count", "git_checkout_count",
    "git_status_count", "git_diff_count", "git_log_count",
    # Workflow counts
    "mr_create_count", "issue_create_count", "issue_comment_count",
    "mr_comment_count", "transcript_pulls", "mr_discussion_reads", "ci_checks",
    "role_discovery", "role_deep_dive", "role_implementation",
    "role_deployment", "role_verification", "role_collaboration", "role_research",
    # Phases / iterations
    "phase_plan_count", "phase_research_count", "phase_implement_count",
    "phase_verify_count", "phase_deploy_count", "phase_collab_count",
    "phase_transitions", "research_to_impl_cycles", "impl_to_verify_cycles",
    "verify_to_research_backloops",
    # Read/Write
    "reads", "writes", "searches",
    "writes_code", "writes_test", "writes_config", "writes_docs",
    "writes_infra", "writes_ui",
    # Planning
    "todo_write_count", "task_mgmt_count",
    # Errors
    "tool_error_count", "bash_error_count", "api_error_count",
    # Interruptions + resumes
    "interruption_mid_loop_count", "queue_operation_count",
    "resume_with_prompt_count", "resume_continuation_count",
    # Human turn features
    "course_correct_count", "error_report_count", "approval_count",
    "urgent_count", "detailed_spec_count", "instruction_count",
    "significant_negation_count", "total_human_chars",
    "avg_human_chars", "max_human_chars",
    # Web research
    "web_research_count", "websearch_count", "webfetch_count",
    "web_early_count", "web_late_count", "web_after_error_count",
    "web_before_impl_count",
    # Workflow patterns
    "self_review_loops", "transcript_tickets", "ci_cycles",
    "merge_arcs", "stuck_cycles_pattern",
    # Rates/derived
    "correction_rate",
]

BINARY = [
    "is_subagent", "used_extended_context",
    "signal_went_wrong", "signal_gross_errors", "signal_multi_task",
    "uses_planning", "is_bug_fix_session",
    "first_turn_had_error", "first_turn_had_issue_ref",
    "has_tool_loop", "has_context_grinding", "retry_intensive",
]

CATEGORICAL = [
    "size_bucket", "provider", "context_profile",
    "top_model",
]

# Trivial clusters — pairs within one cluster are definitionally correlated
TRIVIAL_CLUSTERS = [
    # Pure volume cluster — all correlate because they scale with session size
    {"total_records", "human_turns", "agent_turns", "tool_calls_total",
     "agent_loops", "cache_read_total", "cache_create_total",
     "duration_sec", "duration_active_sec", "duration_idle_sec",
     "llm_work_sec", "longest_gap_sec", "resume_count",
     "resume_with_prompt_count", "resume_continuation_count",
     "reads", "writes", "searches",
     "role_discovery", "role_deep_dive", "role_implementation",
     "role_deployment", "role_verification", "role_collaboration",
     "role_research",
     "phase_plan_count", "phase_research_count", "phase_implement_count",
     "phase_verify_count", "phase_deploy_count", "phase_collab_count",
     "phase_transitions", "research_to_impl_cycles", "impl_to_verify_cycles",
     "verify_to_research_backloops",
     "interruption_mid_loop_count", "queue_operation_count",
     "tool_error_count", "bash_error_count", "api_error_count",
     "mcp_calls", "bash_calls", "builtin_calls",
     "subagents_in_tree", "course_correct_count", "error_report_count",
     "instruction_count", "approval_count", "urgent_count",
     "detailed_spec_count", "significant_negation_count",
     "total_human_chars",
     "ci_checks", "mr_create_count", "issue_create_count",
     "mr_comment_count", "issue_comment_count", "transcript_pulls",
     "mr_discussion_reads", "todo_write_count", "task_mgmt_count",
     "task_create_count", "task_update_count",
     "self_review_loops", "transcript_tickets", "ci_cycles",
     "merge_arcs", "stuck_cycles_pattern",
     "ceiling_approaches", "ctx_over_500k", "ctx_over_900k",
     "cost_usd_total", "cost_input_usd", "cost_output_usd",
     "cost_cache_read_usd", "cost_cache_5m_usd", "cost_cache_1h_usd",
     "cache_5m_expired_count", "cache_1h_expired_count",
     "cache_rebuild_tokens", "exp_cause_human_afk",
     "exp_cause_long_tool", "exp_cause_chain",
     "compaction_count",
     "web_research_count", "websearch_count", "webfetch_count",
     "web_early_count", "web_late_count", "web_after_error_count",
     "web_before_impl_count",
     "git_commit_count", "git_push_count", "git_merge_count",
     "git_checkout_count", "git_status_count", "git_diff_count",
     "git_log_count", "git_fetch_count", "git_pull_count",
     "writes_code", "writes_test", "writes_config", "writes_docs",
     "writes_infra", "writes_ui", "writes_sql", "writes_script",
     "writes_other",
     "longest_same_tool_run", "loop_turns_max", "loop_calls_max",
     "context_size_max", "context_size_mean"},
    # Cost cluster — all cost_* derived from same tokens
    {"cost_usd_total", "cost_input_usd", "cost_output_usd",
     "cost_cache_read_usd", "cost_cache_5m_usd", "cost_cache_1h_usd",
     "cost_per_turn_mean_usd", "cost_per_turn_max_usd"},
    # Context size cluster
    {"context_size_max", "context_size_mean", "ctx_over_500k", "ctx_over_900k",
     "ceiling_approaches", "mean_context_fill_fraction"},
    # Git cluster
    {"git_commit_count", "git_push_count", "git_status_count", "git_diff_count",
     "git_log_count", "git_fetch_count", "git_pull_count", "git_merge_count",
     "git_checkout_count", "git_stash_count", "git_reset_count"},
    # Role/phase definitional — same concept named twice
    {"role_implementation", "phase_implement_count", "writes"},
    {"role_deep_dive", "phase_research_count", "reads",
     "role_discovery", "searches"},
    {"role_verification", "phase_verify_count", "ci_checks"},
    {"role_deployment", "phase_deploy_count"},
    {"role_collaboration", "phase_collab_count"},
    # Resume counters
    {"resume_count", "resume_with_prompt_count", "resume_continuation_count",
     "longest_gap_sec", "duration_idle_sec"},
    # Planning cluster
    {"todo_write_count", "task_mgmt_count", "task_create_count", "task_update_count",
     "phase_plan_count"},
    # Tool loop / context grinding definitional
    {"has_tool_loop", "longest_same_tool_run", "loop_turns_max", "loop_calls_max",
     "has_context_grinding"},
    # Negation/correction signals
    {"course_correct_count", "significant_negation_count"},
    # Bug-fix signals (by construction)
    {"is_bug_fix_session", "first_turn_had_error", "first_turn_had_issue_ref",
     "error_report_count"},
    # Multi-task signals
    {"signal_multi_task", "instruction_count", "detailed_spec_count",
     "resume_with_prompt_count"},
    # Wrong direction signals
    {"signal_went_wrong", "course_correct_count", "significant_negation_count",
     "interruption_mid_loop_count"},
    # Gross errors signals
    {"signal_gross_errors", "tool_error_count", "bash_error_count",
     "api_error_count", "stuck_cycles_pattern", "retry_intensive"},
    # Web research (same family)
    {"web_research_count", "websearch_count", "webfetch_count",
     "web_early_count", "web_after_error_count", "web_before_impl_count"},
    # Compaction cluster
    {"compaction_count", "compact_pre_avg", "cache_1h_expired_count",
     "cache_5m_expired_count", "cache_rebuild_tokens"},
    # Workflow pattern counts all scale with session size
    {"self_review_loops", "transcript_tickets", "ci_cycles", "merge_arcs",
     "stuck_cycles_pattern"},
    # Size bucket × categorical proxies
    {"size_bucket", "total_records", "tool_calls_total", "agent_turns",
     "human_turns"},
    # Context profile × size
    {"context_profile", "context_size_max", "used_extended_context"},
]


def is_trivial_pair(a, b):
    """True if a and b belong to the same trivial cluster."""
    for cluster in TRIVIAL_CLUSTERS:
        if a in cluster and b in cluster:
            return True
    return False


def pearson_with_p(x, y):
    """Pearson r + 2-sided p-value. Returns (r, p, n)."""
    mask = np.isfinite(x) & np.isfinite(y)
    x, y = x[mask], y[mask]
    n = len(x)
    if n < 10 or np.std(x) == 0 or np.std(y) == 0:
        return 0.0, 1.0, n
    r = np.corrcoef(x, y)[0, 1]
    if not np.isfinite(r):
        return 0.0, 1.0, n
    # t-test for Pearson
    t = r * np.sqrt((n - 2) / max(1e-10, 1 - r * r))
    p = 2 * (1 - stats.t.cdf(abs(t), df=n - 2))
    return float(r), float(p), n


def eta_squared(groups, values):
    """ANOVA eta-squared for categorical × continuous."""
    groups = np.asarray(groups)
    values = np.asarray(values)
    mask = np.isfinite(values)
    groups, values = groups[mask], values[mask]
    if len(values) < 20:
        return 0.0, 1.0, len(values)
    cats = np.unique(groups)
    if len(cats) < 2:
        return 0.0, 1.0, len(values)
    grand = values.mean()
    ss_total = np.sum((values - grand) ** 2)
    ss_between = 0.0
    buckets = []
    for c in cats:
        subset = values[groups == c]
        if len(subset) < 2: continue
        ss_between += len(subset) * (subset.mean() - grand) ** 2
        buckets.append(subset)
    if ss_total == 0 or len(buckets) < 2:
        return 0.0, 1.0, len(values)
    eta2 = ss_between / ss_total
    try:
        f_val, p_val = stats.f_oneway(*buckets)
        return float(eta2), float(p_val), len(values)
    except Exception:
        return float(eta2), 1.0, len(values)


def cramers_v(a, b):
    """Cramer's V for categorical × categorical."""
    a = np.asarray(a)
    b = np.asarray(b)
    ct = np.zeros((len(np.unique(a)), len(np.unique(b))))
    a_vals = {v: i for i, v in enumerate(np.unique(a))}
    b_vals = {v: i for i, v in enumerate(np.unique(b))}
    for av, bv in zip(a, b):
        ct[a_vals[av], b_vals[bv]] += 1
    n = ct.sum()
    if n < 20 or ct.shape[0] < 2 or ct.shape[1] < 2:
        return 0.0, 1.0, int(n)
    try:
        chi2, p, _, _ = stats.chi2_contingency(ct)
        v = float(np.sqrt(chi2 / (n * (min(ct.shape) - 1))))
        return v, float(p), int(n)
    except Exception:
        return 0.0, 1.0, int(n)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp", default="/tmp/claude_analysis/sessions_enriched.parquet")
    ap.add_argument("--out", default="/tmp/claude_analysis/correlations.parquet")
    ap.add_argument("--scope", default="main", choices=["all", "main", "subagent"])
    ap.add_argument("--min-effect", type=float, default=0.15)
    ap.add_argument("--max-p", type=float, default=0.01)
    args = ap.parse_args()

    con = duckdb.connect()
    con.execute(f"CREATE VIEW se_raw AS SELECT * FROM '{args.inp}'")

    # Dedupe by session_label — keep row with largest total_records
    where = {
        "main": "WHERE NOT is_subagent",
        "subagent": "WHERE is_subagent",
        "all": "",
    }[args.scope]
    con.execute(f"""
        CREATE VIEW se AS
        SELECT * FROM (
          SELECT *, ROW_NUMBER() OVER (PARTITION BY session ORDER BY total_records DESC) AS rn
          FROM se_raw {where}
        ) WHERE rn = 1
    """)
    n_sessions = con.execute("SELECT COUNT(*) FROM se").fetchone()[0]
    print(f"# Correlation mining: {n_sessions} unique sessions (scope={args.scope})",
          file=sys.stderr)

    # Pull data into numpy arrays
    available = set(con.execute("DESCRIBE se").fetchall())
    col_names = {r[0] for r in available}

    def safe(name):
        return name if name in col_names else None

    cont = [c for c in CONTINUOUS if safe(c)]
    bin_cols = [c for c in BINARY if safe(c)]
    cat_cols = [c for c in CATEGORICAL if safe(c)]

    # Fetch arrays
    select = cont + bin_cols + cat_cols
    df = con.execute(f"SELECT {', '.join(select)} FROM se").fetchall()
    M = {c: np.array([row[i] for row in df]) for i, c in enumerate(select)}

    # Convert numeric where needed
    for c in cont + bin_cols:
        try:
            M[c] = np.array([float(x) if x is not None else np.nan for x in M[c]])
        except Exception:
            pass

    results = []

    # 1. Continuous × Continuous
    print("# Computing continuous × continuous ...", file=sys.stderr)
    for i, a in enumerate(cont):
        for b in cont[i + 1:]:
            r, p, n = pearson_with_p(M[a], M[b])
            if abs(r) >= args.min_effect and p < args.max_p:
                results.append({
                    "feature_a": a, "feature_b": b, "kind": "pearson",
                    "effect": round(r, 4), "p_value": round(p, 6), "n": n,
                    "trivial": is_trivial_pair(a, b),
                })

    # 2. Binary × Continuous (point-biserial = Pearson)
    print("# Computing binary × continuous ...", file=sys.stderr)
    for bin_c in bin_cols:
        for cont_c in cont:
            r, p, n = pearson_with_p(M[bin_c], M[cont_c])
            if abs(r) >= args.min_effect and p < args.max_p:
                results.append({
                    "feature_a": bin_c, "feature_b": cont_c, "kind": "point_biserial",
                    "effect": round(r, 4), "p_value": round(p, 6), "n": n,
                    "trivial": is_trivial_pair(bin_c, cont_c),
                })

    # 3. Categorical × Continuous (eta-squared)
    print("# Computing categorical × continuous ...", file=sys.stderr)
    for cat_c in cat_cols:
        for cont_c in cont:
            eta2, p, n = eta_squared(M[cat_c], M[cont_c])
            if eta2 >= args.min_effect ** 2 and p < args.max_p:
                results.append({
                    "feature_a": cat_c, "feature_b": cont_c, "kind": "eta_squared",
                    "effect": round(np.sqrt(eta2), 4), "p_value": round(p, 6),
                    "n": n, "trivial": is_trivial_pair(cat_c, cont_c),
                })

    # 4. Categorical × Binary (Cramer's V)
    print("# Computing categorical × binary ...", file=sys.stderr)
    for cat_c in cat_cols:
        for bin_c in bin_cols:
            v, p, n = cramers_v(M[cat_c], M[bin_c])
            if v >= args.min_effect and p < args.max_p:
                results.append({
                    "feature_a": cat_c, "feature_b": bin_c, "kind": "cramers_v",
                    "effect": round(v, 4), "p_value": round(p, 6), "n": n,
                    "trivial": is_trivial_pair(cat_c, bin_c),
                })

    # 5. Binary × Binary (Cramer's V)
    print("# Computing binary × binary ...", file=sys.stderr)
    for i, a in enumerate(bin_cols):
        for b in bin_cols[i + 1:]:
            v, p, n = cramers_v(M[a], M[b])
            if v >= args.min_effect and p < args.max_p:
                results.append({
                    "feature_a": a, "feature_b": b, "kind": "cramers_v_bin",
                    "effect": round(v, 4), "p_value": round(p, 6), "n": n,
                    "trivial": is_trivial_pair(a, b),
                })

    results.sort(key=lambda x: -abs(x["effect"]))

    # Markdown report to stdout
    def mdtable(title, rows, max_rows=20):
        print(f"\n## {title}\n")
        if not rows:
            print("_(none)_\n")
            return
        print("| feature_a | feature_b | kind | effect | p_value | n |")
        print("|-----------|-----------|------|-------:|--------:|--:|")
        for r in rows[:max_rows]:
            print(f"| {r['feature_a']} | {r['feature_b']} | {r['kind']} | {r['effect']:+.3f} | {r['p_value']:.4g} | {r['n']} |")

    non_trivial = [r for r in results if not r["trivial"]]
    trivial = [r for r in results if r["trivial"]]

    print(f"# Correlation mining report\n")
    print(f"Sessions analyzed: {n_sessions} (scope={args.scope}, deduped)")
    print(f"Total significant pairs: {len(results)} "
          f"({len(non_trivial)} non-trivial, {len(trivial)} trivial)\n")
    print(f"Thresholds: |effect| ≥ {args.min_effect}, p < {args.max_p}")

    mdtable("Top-30 non-trivial correlations", non_trivial, 30)
    mdtable("Top-10 continuous × continuous (non-trivial)",
            [r for r in non_trivial if r["kind"] == "pearson"], 10)
    mdtable("Top-20 binary × continuous (how flags predict metrics)",
            [r for r in non_trivial if r["kind"] == "point_biserial"], 20)
    mdtable("Top-15 categorical × continuous (ANOVA)",
            [r for r in non_trivial if r["kind"] == "eta_squared"], 15)
    mdtable("Top-10 binary × binary (signal pairs)",
            [r for r in non_trivial if r["kind"] == "cramers_v_bin"], 10)
    mdtable("Top-10 trivial (tautological — to exclude)",
            trivial, 10)

    # Write parquet
    if results:
        schema = pa.schema([
            pa.field("feature_a", pa.string()),
            pa.field("feature_b", pa.string()),
            pa.field("kind", pa.string()),
            pa.field("effect", pa.float64()),
            pa.field("p_value", pa.float64()),
            pa.field("n", pa.int32()),
            pa.field("trivial", pa.bool_()),
        ])
        pq.write_table(pa.Table.from_pylist(results, schema=schema), args.out,
                       compression="zstd")
        print(f"\n_Wrote {len(results)} rows to {args.out}_", file=sys.stderr)


if __name__ == "__main__":
    main()
