#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=18.0", "pandas>=2.0", "numpy>=1.26"]
# ///
"""
05 — Aggregate raw parquet artifacts into tables for the paper.

Inputs:
  artifacts/strategy_results.parquet    (E1 — algorithmic)
  artifacts/llm_results.*.parquet       (E2 — LLM comprehension, one per model)

Outputs (CSV files + stdout summary):
  artifacts/table_a_strategies.csv         — p₁ × strategy × budget
  artifacts/table_a_strategies_by_bucket.csv — same, split by n_candidates bucket
  artifacts/table_b_multi_llm.csv          — accuracy × model × strategy × budget
  artifacts/figure_data_corr.csv           — llm_acc vs algo_p₁ scatter data

Also prints hypothesis checks (H1, H2, H3) to stderr.
"""
from __future__ import annotations

import sys
from pathlib import Path

import numpy as np
import pandas as pd
import pyarrow.parquet as pq


HERE = Path(__file__).resolve().parent.parent
ARTIFACTS = HERE / "artifacts"
STRATEGY_PARQUET = ARTIFACTS / "strategy_results.parquet"

# Pricing per 1M tokens (USD). Source: platform.claude.com/docs/en/about-claude/pricing
# + z.ai coding plan + Ollama (local=free).
MODEL_PRICING = {
    # Anthropic
    "claude-sonnet-4-5":   {"in": 3.00,  "out": 15.00, "cache_r": 0.30,  "cache_w": 3.75,  "batch_discount": 0.5},
    "claude-sonnet-4-6":   {"in": 3.00,  "out": 15.00, "cache_r": 0.30,  "cache_w": 3.75,  "batch_discount": 0.5},
    "claude-opus-4-7":     {"in": 15.00, "out": 75.00, "cache_r": 1.50,  "cache_w": 18.75, "batch_discount": 0.5},
    "claude-haiku-4-5":    {"in": 1.00,  "out": 5.00,  "cache_r": 0.10,  "cache_w": 1.25,  "batch_discount": 0.5},
    # z.ai GLM Coding Plan
    "glm-5.1":             {"in": 0.60,  "out": 2.20,  "cache_r": 0.11,  "cache_w": 0.75,  "batch_discount": 1.0},
    "glm-4.5":             {"in": 0.60,  "out": 2.20,  "cache_r": 0.11,  "cache_w": 0.75,  "batch_discount": 1.0},
    # Ollama local — free
    "gemma4:26b":          {"in": 0.0, "out": 0.0, "cache_r": 0.0, "cache_w": 0.0, "batch_discount": 1.0},
    "gpt-oss:20b":         {"in": 0.0, "out": 0.0, "cache_r": 0.0, "cache_w": 0.0, "batch_discount": 1.0},
}


def load_strategy_results() -> pd.DataFrame:
    if not STRATEGY_PARQUET.exists():
        raise SystemExit(f"Missing {STRATEGY_PARQUET}. Run 03 first.")
    return pq.read_table(STRATEGY_PARQUET).to_pandas()


def load_llm_results() -> pd.DataFrame:
    """Concat all llm_results.<suffix>.parquet into one df."""
    parts = sorted(ARTIFACTS.glob("llm_results.*.parquet"))
    parts = [p for p in parts if "smoketest" not in p.name]
    if not parts:
        print("No llm_results.*.parquet files found — E2 not yet run.",
              file=sys.stderr)
        return pd.DataFrame()
    dfs = [pq.read_table(p).to_pandas() for p in parts]
    df = pd.concat(dfs, ignore_index=True)
    print(f"Loaded {len(df)} LLM rows from {len(parts)} partial file(s): "
          f"{[p.name for p in parts]}", file=sys.stderr)
    return df


def n_cand_bucket(n: int) -> str:
    if n <= 5:
        return "small_1-5"
    elif n <= 20:
        return "medium_6-20"
    else:
        return "large_21+"


def wilson_ci(k: int, n: int, z: float = 1.96) -> tuple[float, float]:
    """Wilson score 95% CI for a binomial proportion."""
    if n == 0:
        return (0.0, 0.0)
    p = k / n
    denom = 1 + z ** 2 / n
    center = (p + z ** 2 / (2 * n)) / denom
    half = z * np.sqrt(p * (1 - p) / n + z ** 2 / (4 * n ** 2)) / denom
    return (max(0.0, center - half), min(1.0, center + half))


# ─── Table A: Strategy × Budget (E1) ─────────────────────────────────────────

def build_table_a(df: pd.DataFrame) -> pd.DataFrame:
    agg = (df.groupby(["strategy", "budget_tok"])
             .agg(p1_mean=("p1", "mean"),
                  p3_mean=("p3", "mean"),
                  p5_mean=("p5", "mean"),
                  p10_mean=("p10", "mean"),
                  tokens_used_mean=("tokens_used", "mean"),
                  n_tasks=("p1", "count"))
             .reset_index())
    # CIs
    ci_lo, ci_hi = [], []
    for _, r in agg.iterrows():
        k = int(round(r["p1_mean"] * r["n_tasks"]))
        lo, hi = wilson_ci(k, int(r["n_tasks"]))
        ci_lo.append(lo)
        ci_hi.append(hi)
    agg["p1_ci_lo"] = ci_lo
    agg["p1_ci_hi"] = ci_hi
    return agg


def build_table_a_by_bucket(df: pd.DataFrame) -> pd.DataFrame:
    df = df.copy()
    df["bucket"] = df["n_candidates"].apply(n_cand_bucket)
    return (df.groupby(["bucket", "strategy", "budget_tok"])
              .agg(p1_mean=("p1", "mean"),
                   n_tasks=("p1", "count"))
              .reset_index())


# ─── Table B: Model × Strategy × Budget (E2) ──────────────────────────────────

def build_table_b(llm_df: pd.DataFrame) -> pd.DataFrame:
    if llm_df.empty:
        return pd.DataFrame()
    agg = (llm_df.groupby(["model", "strategy", "budget_tok"])
                 .agg(acc_mean=("is_correct", "mean"),
                      acc_n=("is_correct", "count"),
                      parse_err=("error",
                                  lambda s: (s.str.len() > 0).mean()),
                      latency_mean_ms=("latency_ms", "mean"),
                      tokens_prompt_mean=("tokens_prompt", "mean"),
                      tokens_comp_mean=("tokens_completion", "mean"))
                 .reset_index())
    ci_lo, ci_hi = [], []
    for _, r in agg.iterrows():
        k = int(round(r["acc_mean"] * r["acc_n"]))
        lo, hi = wilson_ci(k, int(r["acc_n"]))
        ci_lo.append(lo)
        ci_hi.append(hi)
    agg["acc_ci_lo"] = ci_lo
    agg["acc_ci_hi"] = ci_hi
    return agg


def build_table_c_cost(llm_df: pd.DataFrame) -> pd.DataFrame:
    """Total cost and cost/correct per model, applying per-model pricing.

    Handles two schemas: tokens_prompt+tokens_completion (04 Ollama/z.ai) and
    tokens_prompt_fresh+tokens_cache_read+tokens_cache_create+tokens_completion (07).
    """
    if llm_df.empty:
        return pd.DataFrame()
    rows = []
    for model, g in llm_df.groupby("model"):
        pricing = MODEL_PRICING.get(model)
        if pricing is None:
            print(f"[warn] no pricing entry for {model}, skipping cost", file=sys.stderr)
            continue

        if "tokens_prompt_fresh" in g.columns:
            fresh = g["tokens_prompt_fresh"].sum()
            cache_r = g["tokens_cache_read"].sum()
            cache_w = g["tokens_cache_create"].sum()
        else:
            # 04 schema stores total prompt — treat as fresh
            fresh = g["tokens_prompt"].sum() if "tokens_prompt" in g.columns else 0
            cache_r = 0
            cache_w = 0

        completion = g["tokens_completion"].sum() if "tokens_completion" in g.columns else 0
        cost = pricing["batch_discount"] * (
            fresh * pricing["in"] / 1e6
            + cache_r * pricing["cache_r"] / 1e6
            + cache_w * pricing["cache_w"] / 1e6
            + completion * pricing["out"] / 1e6
        )
        correct = int(g["is_correct"].sum())
        n = len(g)
        rows.append(dict(
            model=model,
            n_calls=n,
            accuracy=correct / max(n, 1),
            total_cost_usd=round(cost, 4),
            cost_per_call_usd=round(cost / max(n, 1), 6),
            cost_per_correct_usd=round(cost / max(correct, 1), 6) if correct > 0 else float("inf"),
            input_fresh=int(fresh),
            cache_read=int(cache_r),
            cache_write=int(cache_w),
            output=int(completion),
            cache_hit_rate=(cache_r / max(cache_r + cache_w + fresh, 1)),
        ))
    return pd.DataFrame(rows).sort_values("cost_per_correct_usd")


def build_table_b_by_bucket(llm_df: pd.DataFrame) -> pd.DataFrame:
    if llm_df.empty:
        return pd.DataFrame()
    df = llm_df.copy()
    df["bucket"] = df["n_candidates"].apply(n_cand_bucket)
    return (df.groupby(["bucket", "model", "strategy", "budget_tok"])
              .agg(acc_mean=("is_correct", "mean"),
                   n=("is_correct", "count"))
              .reset_index())


# ─── Correlation scatter data ────────────────────────────────────────────────

def corr_data(strategy_df: pd.DataFrame, llm_df: pd.DataFrame) -> pd.DataFrame:
    """Join per-(task, strategy, budget) algo_p1 with LLM is_correct across models."""
    if llm_df.empty:
        return pd.DataFrame()
    left = strategy_df[["task_id", "strategy", "budget_tok", "p1", "n_candidates"]]
    joined = llm_df.merge(left, on=["task_id", "strategy", "budget_tok"],
                           how="inner", suffixes=("", "_algo"))
    return joined[["task_id", "model", "strategy", "budget_tok",
                    "p1", "is_correct", "n_candidates", "n_candidates_algo"]]


def pearson_per_model(corr_df: pd.DataFrame) -> pd.DataFrame:
    rows = []
    for model, g in corr_df.groupby("model"):
        if len(g) < 3:
            continue
        # Pearson on per-cell means (task collapse)
        per_cell = (g.groupby(["strategy", "budget_tok"])
                    .agg(algo=("p1", "mean"), llm=("is_correct", "mean"))
                    .reset_index())
        if len(per_cell) < 3:
            continue
        r = np.corrcoef(per_cell["algo"], per_cell["llm"])[0, 1]
        rows.append(dict(model=model, r_cellmeans=float(r), n_cells=len(per_cell)))

        # Pearson on raw 0/1 vs 0/1
        r_raw = np.corrcoef(g["p1"].astype(float), g["is_correct"].astype(float))[0, 1]
        rows[-1]["r_raw"] = float(r_raw)
        rows[-1]["n_rows"] = len(g)
    return pd.DataFrame(rows)


# ─── Hypothesis checks ───────────────────────────────────────────────────────

def check_h1(strategy_df: pd.DataFrame) -> None:
    """H1: Priority-KW > FIFO on SWE-bench p₁ at budget=2k, Δ ≥ +0.10."""
    row_kw = strategy_df.query("strategy == 'priority_kw' and budget_tok == 2000")
    row_fifo = strategy_df.query("strategy == 'fifo' and budget_tok == 2000")
    if row_kw.empty or row_fifo.empty:
        print("[H1] SKIP — missing rows", file=sys.stderr)
        return
    p_kw = row_kw["p1"].mean()
    p_fifo = row_fifo["p1"].mean()
    delta = p_kw - p_fifo
    verdict = "PASS" if delta >= 0.10 else "FAIL"
    print(f"[H1 priority_kw_vs_fifo_2k]  Δp₁ = {delta:+.3f}  "
          f"(priority_kw={p_kw:.3f}, fifo={p_fifo:.3f}; threshold +0.100) → {verdict}",
          file=sys.stderr)


def check_h2(strategy_df: pd.DataFrame) -> None:
    """H2: Priority-ALL > Priority-KW, Δ ≥ +0.03 at same budget."""
    for b in sorted(strategy_df["budget_tok"].unique()):
        kw = strategy_df.query("strategy == 'priority_kw' and budget_tok == @b")
        al = strategy_df.query("strategy == 'priority_all' and budget_tok == @b")
        if kw.empty or al.empty:
            continue
        delta = al["p1"].mean() - kw["p1"].mean()
        verdict = "PASS" if delta >= 0.03 else "FAIL"
        print(f"[H2 priority_all_vs_kw b={b}]  Δp₁ = {delta:+.3f}  "
              f"(threshold +0.030) → {verdict}",
              file=sys.stderr)


def check_h3(corr_df: pd.DataFrame) -> None:
    """H3: Pearson r(algo_p₁, llm_acc) ≥ 0.85 per model (cell-means)."""
    if corr_df.empty:
        print("[H3] SKIP — no LLM data yet", file=sys.stderr)
        return
    rows = pearson_per_model(corr_df)
    if rows.empty:
        print("[H3] SKIP — not enough data for correlation", file=sys.stderr)
        return
    for _, r in rows.iterrows():
        verdict = "PASS" if r["r_cellmeans"] >= 0.85 else "FAIL"
        print(f"[H3 corr_algo_vs_llm  {r['model']}]  "
              f"r_cellmeans = {r['r_cellmeans']:+.3f}  "
              f"(n_cells={int(r['n_cells'])}; threshold 0.850) → {verdict}",
              file=sys.stderr)


def check_h5(llm_df: pd.DataFrame) -> None:
    """H5: when gold_in_selected, gap(local, frontier) ≤ 10 p.p.

    Currently we have only local models (gpt-oss, gemma4). This check will
    be meaningful once frontier models (Claude Opus/Sonnet) are run.
    """
    if llm_df.empty:
        return
    models = sorted(llm_df["model"].unique())
    frontier = {m for m in models if "claude" in m.lower()}
    local = {m for m in models if m not in frontier}
    if not frontier or not local:
        print(f"[H5] SKIP — need both frontier and local tiers. "
              f"Found: {sorted(models)}", file=sys.stderr)
        return
    sub = llm_df[llm_df["gold_in_selected"]]
    accs = sub.groupby("model")["is_correct"].mean()
    max_frontier = max(accs[m] for m in frontier if m in accs.index)
    max_local = max(accs[m] for m in local if m in accs.index)
    gap = (max_frontier - max_local) * 100
    verdict = "PASS" if gap <= 10.0 else "FAIL"
    print(f"[H5 local_vs_frontier_gap]  gap = {gap:+.1f} p.p.  "
          f"(frontier_max={max_frontier:.3f}, local_max={max_local:.3f}; "
          f"threshold 10.0) → {verdict}",
          file=sys.stderr)


# ─── Pretty print ────────────────────────────────────────────────────────────

def print_table_a(df: pd.DataFrame) -> None:
    print("\n=== Table A: Strategy × Budget (SWE-bench, algorithmic) ===",
          file=sys.stderr)
    piv = df.pivot_table(index="strategy", columns="budget_tok",
                         values="p1_mean").round(3)
    print(piv.to_string(), file=sys.stderr)


def print_table_b(df: pd.DataFrame) -> None:
    if df.empty:
        return
    print("\n=== Table B: Model × Strategy × Budget (LLM accuracy) ===",
          file=sys.stderr)
    for model in sorted(df["model"].unique()):
        print(f"\n--- {model} ---", file=sys.stderr)
        sub = df[df["model"] == model]
        piv = sub.pivot_table(index="strategy", columns="budget_tok",
                              values="acc_mean").round(3)
        print(piv.to_string(), file=sys.stderr)


def print_bucket_summary(df: pd.DataFrame, is_llm: bool) -> None:
    if df.empty:
        return
    label = "LLM accuracy" if is_llm else "algorithmic p₁"
    print(f"\n=== {label} by n_candidates bucket × strategy ===",
          file=sys.stderr)
    val_col = "acc_mean" if is_llm else "p1_mean"
    idx_cols = ["bucket"] + (["model"] if is_llm else [])
    piv = df.pivot_table(index=idx_cols + ["strategy"],
                         columns="budget_tok", values=val_col).round(3)
    print(piv.to_string(), file=sys.stderr)


# ─── Main ────────────────────────────────────────────────────────────────────

def main() -> int:
    strategy_df = load_strategy_results()
    llm_df = load_llm_results()

    # Table A
    table_a = build_table_a(strategy_df)
    table_a.to_csv(ARTIFACTS / "table_a_strategies.csv", index=False)

    table_a_bucket = build_table_a_by_bucket(strategy_df)
    table_a_bucket.to_csv(ARTIFACTS / "table_a_strategies_by_bucket.csv", index=False)

    print_table_a(table_a)
    print_bucket_summary(table_a_bucket, is_llm=False)

    # Table B (if LLM data exists)
    table_b = build_table_b(llm_df)
    if not table_b.empty:
        table_b.to_csv(ARTIFACTS / "table_b_multi_llm.csv", index=False)
        print_table_b(table_b)

        table_b_bucket = build_table_b_by_bucket(llm_df)
        table_b_bucket.to_csv(ARTIFACTS / "table_b_multi_llm_by_bucket.csv",
                              index=False)
        print_bucket_summary(table_b_bucket, is_llm=True)

        corr_df = corr_data(strategy_df, llm_df)
        corr_df.to_csv(ARTIFACTS / "figure_data_corr.csv", index=False)

        # Table C — cost & efficiency
        table_c = build_table_c_cost(llm_df)
        if not table_c.empty:
            table_c.to_csv(ARTIFACTS / "table_c_cost.csv", index=False)
            print("\n=== Table C: Cost × accuracy (sorted by $/correct) ===",
                  file=sys.stderr)
            show_cols = ["model", "n_calls", "accuracy", "total_cost_usd",
                         "cost_per_correct_usd", "cache_hit_rate"]
            print(table_c[show_cols].round(4).to_string(index=False), file=sys.stderr)

    # Hypothesis checks
    print("\n=== Hypothesis checks ===", file=sys.stderr)
    check_h1(strategy_df)
    check_h2(strategy_df)
    check_h3(corr_df if not llm_df.empty else pd.DataFrame())
    check_h5(llm_df)

    print(file=sys.stderr)
    print("Outputs written to artifacts/", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
