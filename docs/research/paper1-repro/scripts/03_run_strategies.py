#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=18.0", "numpy>=1.26"]
# ///
"""
03 — Run the 5 strategies on each task × budget combination.

Input:  artifacts/candidates.parquet (from 02)
Output: artifacts/strategy_results.parquet

Pure algorithmic. No LLM. No network. ~10k measurements.

Strategies:
  fifo, random, reversed, priority_kw, priority_all

Per (task, strategy, budget):
  1. Compute value per candidate.
  2. Per-candidate cost = 30 + len(path) // 4  (path token estimate).
  3. 0/1 knapsack DP → selected subset within budget.
  4. Ordered by value desc → that's "chunk 1".
  5. Compute p1 / p@k / chunks_to_gold / tokens_used.
"""
from __future__ import annotations

import math
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np
import pyarrow as pa
import pyarrow.parquet as pq


HERE = Path(__file__).resolve().parent.parent
ARTIFACTS = HERE / "artifacts"
IN_PARQUET = ARTIFACTS / "candidates.parquet"
OUT_PARQUET = ARTIFACTS / "strategy_results.parquet"

BUDGETS = [1000, 2000, 4000, 8000]
STRATEGIES = ["fifo", "random", "reversed", "priority_kw",
              "priority_kw_fallback", "priority_all"]
TOKENS_PER_CAND_BASE = 30  # rendered row overhead

# Filetype prior — how "likely is this file the target" by extension.
EXT_PRIOR = {
    "py": 1.00, "pyx": 0.95, "pyi": 0.70,
    "rs": 1.00, "ts": 1.00, "tsx": 1.00, "js": 0.90, "go": 1.00,
    "java": 1.00, "cpp": 1.00, "c": 0.95, "h": 0.80, "hpp": 0.80,
    "md": 0.30, "txt": 0.20, "rst": 0.25,
    "json": 0.50, "yaml": 0.50, "yml": 0.50, "toml": 0.40,
    "lock": 0.10,
}


def token_cost(path: str) -> int:
    """Approx tokens to render one candidate row in a list."""
    return TOKENS_PER_CAND_BASE + max(1, len(path) // 4)


# ─── Value functions ──────────────────────────────────────────────────────────

def value_fifo(cands: list[dict]) -> np.ndarray:
    n = len(cands)
    if n == 0:
        return np.empty(0, dtype=np.float64)
    return np.array([1.0 - (i / n) for i in range(n)], dtype=np.float64)


def value_random(cands: list[dict], seed: int = 42) -> np.ndarray:
    rng = np.random.default_rng(seed)
    return rng.random(len(cands))


def value_reversed(cands: list[dict]) -> np.ndarray:
    n = len(cands)
    if n == 0:
        return np.empty(0, dtype=np.float64)
    return np.array([i / max(n - 1, 1) for i in range(n)], dtype=np.float64)


def value_priority_kw(cands: list[dict]) -> np.ndarray:
    return np.array([c["keyword_overlap_score"] for c in cands], dtype=np.float64)


def value_priority_kw_fallback(cands: list[dict]) -> np.ndarray:
    """Priority-KW with graceful fallback for zero-signal candidate sets.

    - If all keyword_overlap_score == 0 → behave as FIFO (original discovery order).
    - Otherwise → keyword_score + small FIFO epsilon (breaks ties toward earlier).

    This prevents the degenerate knapsack-returns-empty-set case when no candidate
    path shares tokens with the issue text.
    """
    kws = [c["keyword_overlap_score"] for c in cands]
    if not kws or max(kws) == 0:
        return value_fifo(cands)
    n = len(cands)
    eps = 1e-6
    return np.array(
        [kw + eps * (1.0 - i / max(n - 1, 1)) for i, kw in enumerate(kws)],
        dtype=np.float64,
    )


def value_priority_all(cands: list[dict]) -> np.ndarray:
    W_KW, W_DEPTH, W_EXT, W_REC, W_FNAME = 0.70, 0.15, 0.08, 0.04, 0.03
    # Recency signal — we don't have commit time delta here; use current mtime
    # relative to newest file in the candidate set as a proxy.
    if not cands:
        return np.empty(0, dtype=np.float64)
    mtimes = np.array([c["mtime_epoch"] for c in cands], dtype=np.float64)
    max_m = mtimes.max() if mtimes.size else 0.0
    vals = np.zeros(len(cands), dtype=np.float64)
    for i, c in enumerate(cands):
        kw = float(c["keyword_overlap_score"])
        depth = 1.0 / (1.0 + float(c["depth"]))
        ext_prior = EXT_PRIOR.get(c["ext"], 0.5)
        age_days = max(0.0, (max_m - float(c["mtime_epoch"])) / 86400.0)
        rec = 1.0 / (1.0 + age_days / 30.0)  # half-life 30 days
        # Filename match: ext-stripped filename shares a token with path segments
        # that look like identifiers (proxy for "this file is named after the issue topic")
        fname_match = 1.0 if "_" in c["path"].rsplit("/", 1)[-1] else 0.0
        vals[i] = W_KW * kw + W_DEPTH * depth + W_EXT * ext_prior + \
                  W_REC * rec + W_FNAME * fname_match
    return vals


VALUE_FNS = {
    "fifo": value_fifo,
    "random": value_random,
    "reversed": value_reversed,
    "priority_kw": value_priority_kw,
    "priority_kw_fallback": value_priority_kw_fallback,
    "priority_all": value_priority_all,
}


# ─── Knapsack ─────────────────────────────────────────────────────────────────

def knapsack_01(values: np.ndarray, costs: np.ndarray, budget: int) -> list[int]:
    """Exact 0/1 knapsack DP. Returns indices of selected items.

    O(n · budget). Fine for n ≤ 50 and budget ≤ 8000.
    """
    n = len(values)
    if n == 0 or budget <= 0:
        return []
    # dp[c] = best value using any prefix of items with total cost ≤ c
    dp = np.zeros(budget + 1, dtype=np.float64)
    keep = np.zeros((n, budget + 1), dtype=bool)
    for i in range(n):
        ci = int(costs[i])
        if ci > budget:
            continue
        vi = float(values[i])
        # Iterate c from high to low so each item used at most once.
        # Only check c in [ci, budget].
        new_dp = dp.copy()
        for c in range(ci, budget + 1):
            alt = dp[c - ci] + vi
            if alt > dp[c]:
                new_dp[c] = alt
                keep[i, c] = True
        dp = new_dp
    # Backtrack
    selected: list[int] = []
    c = budget
    for i in range(n - 1, -1, -1):
        if c < 0:
            break
        if keep[i, c]:
            selected.append(i)
            c -= int(costs[i])
    selected.reverse()
    return selected


# ─── Per-task evaluation ──────────────────────────────────────────────────────

def evaluate_task(cands: list[dict], strategy: str, budget: int,
                  gold_set: set[str]) -> dict:
    """Return metrics dict for one (task, strategy, budget) cell."""
    n = len(cands)
    if n == 0:
        return dict(p1=0, p3=0, p5=0, p10=0,
                    chunks_to_gold=-1, tokens_used=0, n_candidates=0,
                    n_selected=0, n_chunks_needed=0)

    values = VALUE_FNS[strategy](cands)
    costs = np.array([token_cost(c["path"]) for c in cands], dtype=np.int64)

    selected_indices = knapsack_01(values, costs, budget)
    # Order selected by value desc (chunk 1 = top-ranked)
    selected_indices.sort(key=lambda i: -values[i])

    selected_paths = [cands[i]["path"] for i in selected_indices]
    n_selected = len(selected_indices)
    tokens_used = int(sum(costs[i] for i in selected_indices))

    def hit_in(sub: list[str]) -> int:
        return int(any(p in gold_set for p in sub))

    p1 = hit_in(selected_paths[:1])
    p3 = hit_in(selected_paths[:3])
    p5 = hit_in(selected_paths[:5])
    p10 = hit_in(selected_paths[:10])

    # chunks_to_gold: in which chunk does the gold first appear?
    # Use a greedy simulation: each chunk is top-ranked subset of *remaining*
    # items within budget. Skip items already emitted.
    emitted: set[int] = set()
    chunks_to_gold = -1
    chunk_idx = 0
    n_chunks_needed = 0
    while len(emitted) < n:
        chunk_idx += 1
        # Remaining items by value desc
        remaining = [i for i in range(n) if i not in emitted]
        remaining.sort(key=lambda i: -values[i])
        budget_left = budget
        this_chunk: list[int] = []
        for i in remaining:
            ci = int(costs[i])
            if ci <= budget_left:
                this_chunk.append(i)
                budget_left -= ci
        if not this_chunk:
            break
        # First chunk to contain gold?
        if chunks_to_gold == -1 and any(cands[i]["path"] in gold_set for i in this_chunk):
            chunks_to_gold = chunk_idx
        emitted.update(this_chunk)
        n_chunks_needed = chunk_idx
        if chunks_to_gold != -1 and chunk_idx > chunks_to_gold + 2:
            # No need to keep iterating once we've seen gold + a couple more
            break

    return dict(
        p1=p1, p3=p3, p5=p5, p10=p10,
        chunks_to_gold=chunks_to_gold,
        tokens_used=tokens_used,
        n_candidates=n,
        n_selected=n_selected,
        n_chunks_needed=n_chunks_needed,
    )


# ─── Main ─────────────────────────────────────────────────────────────────────

def main() -> int:
    if not IN_PARQUET.exists():
        print(f"Missing {IN_PARQUET}. Run 02_generate_candidates.py first.",
              file=sys.stderr)
        return 1

    tbl = pq.read_table(IN_PARQUET).to_pydict()
    # Group rows by task_id, preserving rank order
    by_task: dict[str, list[dict]] = defaultdict(list)
    gold_by_task: dict[str, set[str]] = defaultdict(set)

    n_rows = len(tbl["task_id"])
    for i in range(n_rows):
        tid = tbl["task_id"][i]
        row = dict(
            path=tbl["path"][i],
            rank=tbl["rank"][i],
            ext=tbl["ext"][i],
            depth=tbl["depth"][i],
            size_bytes=tbl["size_bytes"][i],
            mtime_epoch=tbl["mtime_epoch"][i],
            keyword_overlap_score=tbl["keyword_overlap_score"][i],
        )
        by_task[tid].append(row)
        if tbl["is_gold"][i]:
            gold_by_task[tid].add(tbl["path"][i])

    # Sort each task's candidates by rank (FIFO order)
    for tid in by_task:
        by_task[tid].sort(key=lambda r: r["rank"])

    # Gold files may exist but not appear in candidates (if grep missed them).
    # Load the gold parquet to get the full gold list per task.
    GOLD_PARQUET = ARTIFACTS / "swe_bench_gold.parquet"
    if GOLD_PARQUET.exists():
        gt = pq.read_table(GOLD_PARQUET).to_pydict()
        for i in range(len(gt["task_id"])):
            tid = gt["task_id"][i]
            if tid not in gold_by_task:
                gold_by_task[tid] = set()
            for g in gt["gold_files"][i]:
                gold_by_task[tid].add(g)

    all_tasks = sorted(by_task.keys())
    print(f"Tasks with candidates: {len(all_tasks)}", file=sys.stderr)
    print(f"Strategies: {STRATEGIES}", file=sys.stderr)
    print(f"Budgets: {BUDGETS}", file=sys.stderr)
    print(f"Total cells: {len(all_tasks) * len(STRATEGIES) * len(BUDGETS)}",
          file=sys.stderr)

    out_rows: list[dict] = []
    for tidx, tid in enumerate(all_tasks, 1):
        cands = by_task[tid]
        gold_set = gold_by_task[tid]

        # Precompute: did gold appear at rank 0 under FIFO?
        gold_in_fifo_top1 = bool(cands and cands[0]["path"] in gold_set)

        for strategy in STRATEGIES:
            for budget in BUDGETS:
                m = evaluate_task(cands, strategy, budget, gold_set)
                out_rows.append(dict(
                    task_id=tid,
                    strategy=strategy,
                    budget_tok=budget,
                    gold_in_fifo_top1=gold_in_fifo_top1,
                    **m,
                ))
        if tidx % 50 == 0 or tidx == len(all_tasks):
            print(f"[{tidx}/{len(all_tasks)}] tasks processed", file=sys.stderr)

    schema = pa.schema([
        ("task_id", pa.string()),
        ("strategy", pa.string()),
        ("budget_tok", pa.int32()),
        ("p1", pa.int32()),
        ("p3", pa.int32()),
        ("p5", pa.int32()),
        ("p10", pa.int32()),
        ("chunks_to_gold", pa.int32()),
        ("tokens_used", pa.int32()),
        ("n_candidates", pa.int32()),
        ("n_selected", pa.int32()),
        ("n_chunks_needed", pa.int32()),
        ("gold_in_fifo_top1", pa.bool_()),
    ])
    table = pa.Table.from_pylist(out_rows, schema=schema)
    pq.write_table(table, OUT_PARQUET, compression="zstd")

    # ─── Summary tables to stderr ─────────────────────────────────────────
    print(file=sys.stderr)
    print(f"Wrote {OUT_PARQUET}  rows={len(out_rows)}", file=sys.stderr)
    print(file=sys.stderr)
    print("=== p1 by strategy × budget (% of tasks) ===", file=sys.stderr)
    print(f"{'strategy':<14}  " + "  ".join(f"b={b:>5}" for b in BUDGETS),
          file=sys.stderr)
    by_cell: dict[tuple[str, int], list[int]] = defaultdict(list)
    for r in out_rows:
        by_cell[(r["strategy"], r["budget_tok"])].append(r["p1"])
    for s in STRATEGIES:
        line = f"{s:<14}  "
        for b in BUDGETS:
            vals = by_cell[(s, b)]
            pct = sum(vals) / len(vals) * 100 if vals else 0.0
            line += f"{pct:>6.1f}%  "
        print(line, file=sys.stderr)

    # Priority vs FIFO delta
    print(file=sys.stderr)
    print("=== Priority-KW Δ vs FIFO (percentage points) ===", file=sys.stderr)
    for b in BUDGETS:
        fifo_p = np.mean(by_cell[("fifo", b)]) if by_cell[("fifo", b)] else 0
        prio_p = np.mean(by_cell[("priority_kw", b)]) if by_cell[("priority_kw", b)] else 0
        delta = (prio_p - fifo_p) * 100
        print(f"  b={b:>5}: FIFO={fifo_p*100:5.1f}%  "
              f"Priority-KW={prio_p*100:5.1f}%  Δ=+{delta:+5.1f} p.p.",
              file=sys.stderr)

    print(file=sys.stderr)
    print("=== Priority-ALL Δ vs Priority-KW (percentage points) ===",
          file=sys.stderr)
    for b in BUDGETS:
        kw_p = np.mean(by_cell[("priority_kw", b)]) if by_cell[("priority_kw", b)] else 0
        all_p = np.mean(by_cell[("priority_all", b)]) if by_cell[("priority_all", b)] else 0
        delta = (all_p - kw_p) * 100
        print(f"  b={b:>5}: Priority-KW={kw_p*100:5.1f}%  "
              f"Priority-ALL={all_p*100:5.1f}%  Δ=+{delta:+5.1f} p.p.",
              file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())
