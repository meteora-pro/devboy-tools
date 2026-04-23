#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["matplotlib>=3.8", "pandas>=2.0", "pyarrow>=18.0", "numpy>=1.26"]
# ///
"""
06 — Render plots from aggregated CSVs (from 05).

Inputs:
  artifacts/table_a_strategies.csv
  artifacts/table_a_strategies_by_bucket.csv
  artifacts/table_b_multi_llm.csv              (if E2 ran)
  artifacts/table_b_multi_llm_by_bucket.csv    (if E2 ran)
  artifacts/figure_data_corr.csv               (if E2 ran)

Outputs:
  artifacts/figures/fig1_p1_by_strategy_bucket.png
  artifacts/figures/fig2_llm_acc_vs_algo_p1.png       (if E2)
  artifacts/figures/fig3_llm_acc_by_bucket.png        (if E2)

Style: publication-ready, grayscale-friendly. PNG @ 200 dpi.
"""
from __future__ import annotations

import sys
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np
import pandas as pd


HERE = Path(__file__).resolve().parent.parent
ARTIFACTS = HERE / "artifacts"
FIG_DIR = ARTIFACTS / "figures"
FIG_DIR.mkdir(parents=True, exist_ok=True)

# Publication-friendly style
plt.rcParams.update({
    "font.size": 10,
    "font.family": "sans-serif",
    "axes.titlesize": 11,
    "axes.labelsize": 10,
    "legend.fontsize": 9,
    "xtick.labelsize": 9,
    "ytick.labelsize": 9,
    "figure.dpi": 200,
    "savefig.dpi": 200,
    "savefig.bbox": "tight",
})

STRATEGY_ORDER = ["fifo", "random", "reversed",
                  "priority_kw", "priority_kw_fallback", "priority_all"]
STRATEGY_LABEL = {
    "fifo": "FIFO",
    "random": "Random",
    "reversed": "Reversed",
    "priority_kw": "Priority-KW",
    "priority_kw_fallback": "Priority-KW+",
    "priority_all": "Priority-ALL",
}


# ─── Figure 1: p1 by strategy × budget, split by bucket ──────────────────────

def plot_p1_by_bucket(bucket_csv: Path, out: Path) -> None:
    df = pd.read_csv(bucket_csv)
    buckets = ["small_1-5", "medium_6-20", "large_21+"]
    strategies = [s for s in STRATEGY_ORDER if s in df.strategy.unique()]

    # Take budget=2000 as representative (budget barely matters for median=4 cands)
    budget = 2000
    sub = df[df.budget_tok == budget]

    fig, axes = plt.subplots(1, 3, figsize=(11, 3.5), sharey=True)
    for ax, bucket in zip(axes, buckets):
        row = sub[sub.bucket == bucket]
        xs = np.arange(len(strategies))
        ys = [row[row.strategy == s].p1_mean.values[0] if len(row[row.strategy == s]) else 0
              for s in strategies]
        bars = ax.bar(xs, ys,
                       color=["#888" if s != "priority_kw_fallback" else "#1f77b4"
                              for s in strategies])
        ax.set_xticks(xs)
        ax.set_xticklabels([STRATEGY_LABEL[s] for s in strategies],
                            rotation=35, ha="right")
        ax.set_title(f"n_cand: {bucket.replace('_', ' ')}")
        ax.set_ylim(0, 1.0)
        ax.grid(axis="y", alpha=0.3, linestyle=":")
        # Annotate values
        for b, y in zip(bars, ys):
            ax.text(b.get_x() + b.get_width()/2, y + 0.02, f"{y:.2f}",
                     ha="center", fontsize=8)

    axes[0].set_ylabel(f"Algorithmic p₁ (budget={budget})")
    fig.suptitle("Figure 1. Priority ranking wins in medium / large candidate lists",
                  fontsize=12, y=1.02)
    fig.savefig(out)
    plt.close(fig)
    print(f"Wrote {out}")


# ─── Figure 2: LLM accuracy vs algo_p1 scatter ───────────────────────────────

def plot_corr_scatter(corr_csv: Path, out: Path) -> None:
    if not corr_csv.exists():
        print(f"  skip: missing {corr_csv}")
        return
    df = pd.read_csv(corr_csv)
    if df.empty:
        return

    # Aggregate to cell means per (model, strategy, budget)
    agg = (df.groupby(["model", "strategy", "budget_tok"])
             .agg(algo=("p1", "mean"), llm=("is_correct", "mean"))
             .reset_index())

    fig, ax = plt.subplots(figsize=(6, 5))
    markers = {"fifo": "o", "priority_kw": "s", "priority_kw_fallback": "^"}
    colors = {}
    for i, m in enumerate(sorted(agg.model.unique())):
        colors[m] = f"C{i}"

    for model in agg.model.unique():
        for strategy in agg.strategy.unique():
            sub = agg[(agg.model == model) & (agg.strategy == strategy)]
            if sub.empty:
                continue
            ax.scatter(sub.algo, sub.llm,
                        marker=markers.get(strategy, "x"),
                        color=colors[model],
                        s=55,
                        label=f"{model} / {STRATEGY_LABEL.get(strategy, strategy)}",
                        alpha=0.75,
                        edgecolors="black", linewidth=0.4)

    # Diagonal: llm = algo
    lims = [0, 1]
    ax.plot(lims, lims, color="gray", linestyle=":", alpha=0.6, label="y = x")
    ax.set_xlim(0, 1)
    ax.set_ylim(0, 1)
    ax.set_xlabel("Algorithmic p₁ (ranking)")
    ax.set_ylabel("LLM accuracy (comprehension)")
    ax.set_title("Figure 2. LLM comprehension vs algorithmic ranking\n"
                 "(each point = one (model, strategy, budget) cell)")
    ax.grid(True, alpha=0.3, linestyle=":")
    ax.legend(loc="lower right", fontsize=7, ncols=2, frameon=True)

    # Pearson r in corner
    r_overall = np.corrcoef(agg["algo"], agg["llm"])[0, 1]
    ax.text(0.02, 0.97, f"Pearson r = {r_overall:+.2f}",
             transform=ax.transAxes, va="top", fontsize=9,
             bbox=dict(boxstyle="round", facecolor="white", alpha=0.85))

    fig.savefig(out)
    plt.close(fig)
    print(f"Wrote {out}")


# ─── Figure 3: LLM accuracy by bucket × strategy ─────────────────────────────

def plot_llm_by_bucket(b_bucket_csv: Path, out: Path) -> None:
    if not b_bucket_csv.exists():
        print(f"  skip: missing {b_bucket_csv}")
        return
    df = pd.read_csv(b_bucket_csv)
    if df.empty:
        return
    buckets = ["small_1-5", "medium_6-20", "large_21+"]
    budget = 2000
    sub = df[df.budget_tok == budget]
    models = sorted(sub.model.unique())
    strategies = [s for s in STRATEGY_ORDER if s in sub.strategy.unique()]

    fig, axes = plt.subplots(1, len(models), figsize=(4 * len(models), 4),
                              sharey=True)
    if len(models) == 1:
        axes = [axes]

    width = 0.8 / len(strategies)
    for ax, model in zip(axes, models):
        m_data = sub[sub.model == model]
        x = np.arange(len(buckets))
        for i, s in enumerate(strategies):
            ys = []
            for b in buckets:
                r = m_data[(m_data.bucket == b) & (m_data.strategy == s)]
                ys.append(r.acc_mean.values[0] if len(r) else 0)
            offset = (i - (len(strategies) - 1) / 2) * width
            ax.bar(x + offset, ys, width, label=STRATEGY_LABEL.get(s, s))
        ax.set_xticks(x)
        ax.set_xticklabels([b.replace("_", " ") for b in buckets])
        ax.set_title(model)
        ax.set_ylim(0, 1.05)
        ax.grid(axis="y", alpha=0.3, linestyle=":")

    axes[0].set_ylabel(f"LLM accuracy (budget={budget})")
    axes[0].legend(fontsize=8, loc="lower left")
    fig.suptitle("Figure 3. LLM comprehension accuracy by candidate-list size",
                  fontsize=12, y=1.02)
    fig.savefig(out)
    plt.close(fig)
    print(f"Wrote {out}")


# ─── Main ────────────────────────────────────────────────────────────────────

def main() -> int:
    p_bucket = ARTIFACTS / "table_a_strategies_by_bucket.csv"
    p_corr = ARTIFACTS / "figure_data_corr.csv"
    p_bucket_llm = ARTIFACTS / "table_b_multi_llm_by_bucket.csv"

    if p_bucket.exists():
        plot_p1_by_bucket(p_bucket, FIG_DIR / "fig1_p1_by_strategy_bucket.png")
    else:
        print(f"Missing {p_bucket} — run 05 first", file=sys.stderr)

    plot_corr_scatter(p_corr, FIG_DIR / "fig2_llm_acc_vs_algo_p1.png")
    plot_llm_by_bucket(p_bucket_llm, FIG_DIR / "fig3_llm_acc_by_bucket.png")

    return 0


if __name__ == "__main__":
    sys.exit(main())
