#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["datasets>=2.19", "pyarrow>=18.0", "python-dotenv>=1.0"]
# ///
"""
01 — Download SWE-bench Verified (500 tasks) and write per-task gold files.

For each task:
- task_id
- repo (e.g. "astropy/astropy")
- base_commit (SHA)
- problem_statement
- gold_files: list[str] (parsed from `patch` diff headers)
- n_gold: int

Output: artifacts/swe_bench_gold.parquet

Next step: 02_generate_candidates.py — checks out each repo at base_commit
and runs the keyword grep proxy.

Gold extraction rule (diff headers):
- `+++ b/<path>` → target file (works for modify, add, rename)
- `+++ /dev/null` → file deleted; skip (we want find-the-file targets)
"""
from __future__ import annotations

import os
import re
import statistics
import sys
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
from datasets import load_dataset
from dotenv import load_dotenv


HERE = Path(__file__).resolve().parent.parent
ARTIFACTS = HERE / "artifacts"
OUT_PATH = ARTIFACTS / "swe_bench_gold.parquet"

PLUS_HEADER = re.compile(r"^\+\+\+ b/(.+?)\s*$", re.MULTILINE)


def extract_gold_files(patch: str) -> list[str]:
    """Extract modified / added file paths from a unified diff patch."""
    if not patch:
        return []
    files = set()
    for m in PLUS_HEADER.finditer(patch):
        path = m.group(1).strip()
        if path and path != "/dev/null":
            files.add(path)
    return sorted(files)


def pct(vals: list[int], q: float) -> float:
    if not vals:
        return 0.0
    xs = sorted(vals)
    idx = min(len(xs) - 1, max(0, int(q * (len(xs) - 1))))
    return float(xs[idx])


def main() -> int:
    load_dotenv(HERE / ".env")
    hf_token = os.environ.get("HF_TOKEN") or None

    # Explicit cache dir can overflow Windows MAX_PATH (260 chars) because
    # HF concatenates the full path into the lock-file name. Default HF cache
    # (~/.cache/huggingface) is short enough.
    cache_dir = os.environ.get("SWE_BENCH_CACHE_DIR") or None
    ARTIFACTS.mkdir(parents=True, exist_ok=True)

    print(f"Loading princeton-nlp/SWE-bench_Verified "
          f"(cache: {cache_dir or 'default HF cache'})…",
          file=sys.stderr)
    ds = load_dataset(
        "princeton-nlp/SWE-bench_Verified",
        split="test",
        cache_dir=cache_dir,
        token=hf_token,
    )

    rows = []
    unique_repos: set[str] = set()
    n_gold_values: list[int] = []
    empty_patch = 0

    for task in ds:
        gold = extract_gold_files(task["patch"])
        if not gold:
            empty_patch += 1
        rows.append({
            "task_id": task["instance_id"],
            "repo": task["repo"],
            "base_commit": task["base_commit"],
            "problem_statement": task["problem_statement"],
            "gold_files": gold,
            "n_gold": len(gold),
        })
        unique_repos.add(task["repo"])
        n_gold_values.append(len(gold))

    if len(rows) != 500:
        print(f"WARNING: expected 500 rows, got {len(rows)}", file=sys.stderr)

    if empty_patch:
        print(f"WARNING: {empty_patch} task(s) yielded empty gold_files — "
              "investigate patch format",
              file=sys.stderr)

    table = pa.Table.from_pylist(
        rows,
        schema=pa.schema([
            ("task_id", pa.string()),
            ("repo", pa.string()),
            ("base_commit", pa.string()),
            ("problem_statement", pa.large_string()),
            ("gold_files", pa.list_(pa.string())),
            ("n_gold", pa.int32()),
        ]),
    )
    pq.write_table(table, OUT_PATH, compression="zstd")

    # Sanity stats to stderr
    mean_g = statistics.mean(n_gold_values)
    median_g = statistics.median(n_gold_values)
    n_with_one = sum(1 for v in n_gold_values if v == 1)
    n_with_three_plus = sum(1 for v in n_gold_values if v >= 3)

    print(file=sys.stderr)
    print(f"Wrote {OUT_PATH}", file=sys.stderr)
    print(f"Rows:            {len(rows)}", file=sys.stderr)
    print(f"Unique repos:    {len(unique_repos)}", file=sys.stderr)
    print(f"n_gold min/p25/median/p75/p90/max: "
          f"{min(n_gold_values)}/{pct(n_gold_values, 0.25):.0f}/"
          f"{median_g:.0f}/{pct(n_gold_values, 0.75):.0f}/"
          f"{pct(n_gold_values, 0.90):.0f}/{max(n_gold_values)}",
          file=sys.stderr)
    print(f"mean(n_gold):    {mean_g:.2f}", file=sys.stderr)
    print(f"n_gold == 1:     {n_with_one} ({n_with_one / len(rows) * 100:.1f}%)",
          file=sys.stderr)
    print(f"n_gold ≥ 3:      {n_with_three_plus} "
          f"({n_with_three_plus / len(rows) * 100:.1f}%)",
          file=sys.stderr)

    # Top repos by task count
    from collections import Counter
    repo_counts = Counter(r["repo"] for r in rows)
    print(file=sys.stderr)
    print("Top repos by task count:", file=sys.stderr)
    for repo, cnt in repo_counts.most_common(15):
        print(f"  {cnt:4d}  {repo}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())
