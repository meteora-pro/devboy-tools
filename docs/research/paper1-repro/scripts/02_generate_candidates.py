#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=18.0", "python-dotenv>=1.0"]
# ///
"""
02 — Generate candidate file lists per SWE-bench task (grep proxy).

For each task from `swe_bench_gold.parquet`:
  1. Ensure the repo is cloned at `artifacts/repos_cache/<owner>__<name>/`
  2. Checkout the task's `base_commit`
  3. Tokenize `problem_statement` → keyword list (identifiers, noun-like tokens)
  4. Run `rg -l` — AND of top keywords first; fall back to OR, then to find-by-ext
  5. Truncate to 50 candidates, preserve discovery order (FIFO baseline)
  6. Collect per-file metadata: ext, depth, size, mtime, keyword_overlap_score

Output: artifacts/candidates.parquet with one row per (task, candidate):
  task_id, rank, path, ext, depth, size_bytes, mtime_epoch,
  keyword_overlap_score, is_gold

Tasks grouped by repo and sorted by base_commit to minimize checkout churn.

Next step: 03_run_strategies.py — apply value strategies + knapsack, measure p₁.
"""
from __future__ import annotations

import math
import os
import re
import subprocess
import sys
import time
from collections import Counter
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
from dotenv import load_dotenv


HERE = Path(__file__).resolve().parent.parent
ARTIFACTS = HERE / "artifacts"
GOLD_PARQUET = ARTIFACTS / "swe_bench_gold.parquet"
OUT_PARQUET = ARTIFACTS / "candidates.parquet"

# Limits
MAX_CANDIDATES_PER_TASK = 50
MAX_KEYWORDS = 10
MIN_KEYWORD_LEN = 3
FILE_TYPES = ["py", "pyx", "pyi"]  # SWE-bench Verified is Python-only.

STOPWORDS = {
    "the", "and", "for", "with", "this", "that", "from", "have", "has", "had",
    "are", "was", "were", "been", "being", "not", "but", "can", "could", "would",
    "should", "will", "may", "might", "shall", "must", "when", "where", "which",
    "what", "who", "why", "how", "all", "any", "some", "one", "two", "three",
    "our", "your", "their", "his", "her", "its", "them", "they", "you",
    "me", "my", "we", "us", "it",
    "is", "be", "do", "does", "did", "get", "got", "set", "use", "used", "using",
    "also", "so", "if", "then", "else", "or", "as", "by", "up", "down", "out",
    "over", "under", "about", "into", "than", "like", "just", "only", "same",
    "such", "each", "other", "most", "more", "very", "much", "many", "few",
    "no", "yes", "now", "here", "there", "own", "too", "even", "back",
    "make", "makes", "made", "take", "takes", "took", "come", "came", "see",
    "saw", "seen", "look", "looks", "find", "found", "want", "wanted", "need",
    "know", "knows", "known", "try", "tried", "work", "works", "run",
    "way", "ways", "case", "cases", "call", "calls", "called", "time", "times",
    "file", "files", "line", "lines", "code", "value", "values", "function",
    "method", "methods", "class", "classes", "object", "objects", "test", "tests",
    # Issue-report boilerplate
    "issue", "issues", "bug", "bugs", "fix", "fixes", "fixed", "fail", "failed",
    "fails", "error", "errors", "exception", "raise", "raises", "raised",
    "expected", "actual", "actually", "example", "reproduce", "reproducible",
    "reproduction", "description", "steps", "step", "reported", "report",
    "traceback", "assert", "attached", "console", "output", "please", "thanks",
    "above", "below", "version", "versions",
}

IDENT_RX = re.compile(r"\b[A-Za-z_][A-Za-z0-9_]{2,}\b")
CAMEL_RX = re.compile(r"[A-Z][a-z]+[A-Z]")
SNAKE_RX = re.compile(r"_")
DOT_RX = re.compile(r"\.")


def extract_keywords(statement: str, max_kw: int = MAX_KEYWORDS) -> list[str]:
    """Pick up to `max_kw` most-distinctive keywords from an issue statement."""
    if not statement:
        return []
    counts: Counter[str] = Counter()
    for raw in IDENT_RX.findall(statement):
        t = raw.strip()
        if len(t) < MIN_KEYWORD_LEN:
            continue
        if t.lower() in STOPWORDS:
            continue
        counts[t] += 1

    def score(term: str, count: int) -> float:
        s = len(term) * (1.0 + math.log1p(count))
        if CAMEL_RX.search(term):
            s *= 1.5
        if SNAKE_RX.search(term):
            s *= 1.3
        if DOT_RX.search(term):
            s *= 1.2
        return s

    ranked = sorted(counts.items(), key=lambda kv: -score(kv[0], kv[1]))
    keywords: list[str] = []
    seen_lower: set[str] = set()
    for term, _ in ranked:
        lw = term.lower()
        if lw in seen_lower:
            continue
        seen_lower.add(lw)
        keywords.append(term)
        if len(keywords) >= max_kw:
            break
    return keywords


def run(cmd: list[str], cwd: Path | None = None, check: bool = True,
        capture: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=cwd,
        check=check,
        capture_output=capture,
        text=True,
        encoding="utf-8",
        errors="replace",
    )


def repo_cache_dir(cache_root: Path, repo: str) -> Path:
    return cache_root / repo.replace("/", "__")


def ensure_repo(cache_root: Path, repo: str) -> Path:
    path = repo_cache_dir(cache_root, repo)
    if (path / ".git").is_dir():
        return path
    path.parent.mkdir(parents=True, exist_ok=True)
    print(f"  cloning {repo}…", file=sys.stderr)
    t0 = time.time()
    url = f"https://github.com/{repo}.git"
    run(["git", "clone", "--quiet", url, str(path)])
    print(f"    done in {time.time()-t0:.1f}s", file=sys.stderr)
    return path


def checkout(repo_path: Path, sha: str) -> bool:
    try:
        run(["git", "reset", "--hard", "--quiet"], cwd=repo_path, check=False)
        run(["git", "clean", "-fdx", "--quiet"], cwd=repo_path, check=False)
    except Exception:
        pass
    p = run(["git", "checkout", "--quiet", sha], cwd=repo_path, check=False)
    if p.returncode == 0:
        return True
    # sha may not be local yet — try fetch-by-sha.
    run(["git", "fetch", "--quiet", "origin", sha], cwd=repo_path, check=False)
    p = run(["git", "checkout", "--quiet", sha], cwd=repo_path, check=False)
    if p.returncode == 0:
        return True
    print(f"    checkout failed for {sha}: {p.stderr[:200]}", file=sys.stderr)
    return False


def rg_files_with_pattern(repo_path: Path, pattern: str,
                          file_types: list[str] = FILE_TYPES) -> list[str]:
    """Return repo-relative paths containing `pattern`, restricted to file_types.

    Uses `git grep` — always available (git is installed), uses the index for
    speed, no PATH issues on Windows.
    """
    cmd = ["git", "grep", "-l", "-F", "--", pattern]
    for ft in file_types:
        cmd.append(f"*.{ft}")
    p = run(cmd, cwd=repo_path, check=False)
    # git grep: 0 = found, 1 = nothing matched, 128 = not a git repo / bad args
    if p.returncode not in (0, 1):
        return []
    return [line.strip() for line in p.stdout.splitlines() if line.strip()]


def intersect_preserve_order(lists: list[list[str]]) -> list[str]:
    non_empty = [s for s in lists if s]
    if not non_empty:
        return []
    common = set(non_empty[0])
    for other in non_empty[1:]:
        common &= set(other)
    return [p for p in non_empty[0] if p in common]


def union_preserve_order(lists: list[list[str]]) -> list[str]:
    seen: set[str] = set()
    out: list[str] = []
    for lst in lists:
        for p in lst:
            if p not in seen:
                seen.add(p)
                out.append(p)
    return out


def find_by_filetype(repo_path: Path, types: list[str] = FILE_TYPES,
                     limit: int = 200) -> list[str]:
    """List tracked files by extension via `git ls-files`."""
    cmd = ["git", "ls-files"]
    for ft in types:
        cmd.append(f"*.{ft}")
    p = run(cmd, cwd=repo_path, check=False)
    if p.returncode != 0:
        return []
    paths = [line.strip() for line in p.stdout.splitlines() if line.strip()]
    return paths[:limit]


def keyword_overlap_score(path: str, stmt_tokens: set[str]) -> float:
    if not stmt_tokens:
        return 0.0
    parts = re.split(r"[/_\-.]", path)
    path_tokens = {p.lower() for p in parts if len(p) >= MIN_KEYWORD_LEN}
    if not path_tokens:
        return 0.0
    inter = path_tokens & stmt_tokens
    denom = math.sqrt(max(len(path_tokens), 1) * max(len(stmt_tokens), 1))
    return len(inter) / denom


def generate_for_task(repo_path: Path, statement: str,
                      max_candidates: int = MAX_CANDIDATES_PER_TASK
                      ) -> tuple[list[str], list[str]]:
    keywords = extract_keywords(statement)
    if not keywords:
        return find_by_filetype(repo_path)[:max_candidates], []

    per_kw = [rg_files_with_pattern(repo_path, kw) for kw in keywords]

    candidates = intersect_preserve_order(per_kw[:3])
    if not candidates:
        candidates = intersect_preserve_order(per_kw[:2])
    if not candidates:
        candidates = union_preserve_order(per_kw)
    if not candidates:
        candidates = find_by_filetype(repo_path)

    return candidates[:max_candidates], keywords


def file_metadata(repo_path: Path, rel_path: str) -> tuple[str, int, int, float]:
    p = repo_path / rel_path
    try:
        st = p.stat()
    except OSError:
        return ("", rel_path.count("/"), 0, 0.0)
    ext = p.suffix.lower().lstrip(".")
    depth = rel_path.count("/")
    return (ext, depth, int(st.st_size), float(st.st_mtime))


def main() -> int:
    load_dotenv(HERE / ".env")
    cache_root = Path(os.environ.get("REPOS_CACHE_DIR")
                      or str(ARTIFACTS / "repos_cache"))
    cache_root.mkdir(parents=True, exist_ok=True)

    if not GOLD_PARQUET.exists():
        print(f"Missing {GOLD_PARQUET}. Run 01_download_swe_bench.py first.",
              file=sys.stderr)
        return 1

    tbl = pq.read_table(GOLD_PARQUET).to_pydict()
    task_rows = [
        dict(
            task_id=tbl["task_id"][i],
            repo=tbl["repo"][i],
            base_commit=tbl["base_commit"][i],
            problem_statement=tbl["problem_statement"][i],
            gold_files=list(tbl["gold_files"][i]),
        )
        for i in range(len(tbl["task_id"]))
    ]
    task_rows.sort(key=lambda r: (r["repo"], r["base_commit"]))

    unique_repos = sorted({r["repo"] for r in task_rows})
    print(f"Processing {len(task_rows)} tasks across {len(unique_repos)} repos",
          file=sys.stderr)

    for repo in unique_repos:
        ensure_repo(cache_root, repo)

    out_rows: list[dict] = []
    total = len(task_rows)
    gold_found = 0
    no_candidates = 0

    current_repo = current_sha = None

    for idx, task in enumerate(task_rows, 1):
        repo = task["repo"]
        sha = task["base_commit"]
        repo_path = repo_cache_dir(cache_root, repo)
        if repo != current_repo or sha != current_sha:
            if not checkout(repo_path, sha):
                print(f"[{idx}/{total}] SKIP {task['task_id']}",
                      file=sys.stderr)
                continue
            current_repo, current_sha = repo, sha

        candidates, _keywords = generate_for_task(
            repo_path, task["problem_statement"])

        if not candidates:
            no_candidates += 1

        gold_set = set(task["gold_files"])
        this_gold_found = any(p in gold_set for p in candidates)
        if this_gold_found:
            gold_found += 1

        stmt_tokens = {
            p.lower() for p in re.split(r"[^A-Za-z0-9]+", task["problem_statement"])
            if len(p) >= MIN_KEYWORD_LEN
        }

        for rank, path in enumerate(candidates):
            ext, depth, size, mtime = file_metadata(repo_path, path)
            out_rows.append(dict(
                task_id=task["task_id"],
                rank=rank,
                path=path,
                ext=ext,
                depth=depth,
                size_bytes=size,
                mtime_epoch=mtime,
                keyword_overlap_score=keyword_overlap_score(path, stmt_tokens),
                is_gold=(path in gold_set),
            ))

        if idx % 25 == 0 or idx == total:
            print(f"[{idx:3d}/{total}] {repo:28s} cand={len(candidates):2d} "
                  f"gold_found={gold_found}/{idx} ({gold_found/idx*100:.1f}%)",
                  file=sys.stderr)

    schema = pa.schema([
        ("task_id", pa.string()),
        ("rank", pa.int32()),
        ("path", pa.string()),
        ("ext", pa.string()),
        ("depth", pa.int32()),
        ("size_bytes", pa.int64()),
        ("mtime_epoch", pa.float64()),
        ("keyword_overlap_score", pa.float32()),
        ("is_gold", pa.bool_()),
    ])
    table = pa.Table.from_pylist(out_rows, schema=schema)
    pq.write_table(table, OUT_PARQUET, compression="zstd")

    print(file=sys.stderr)
    print(f"Wrote {OUT_PARQUET}  rows={len(out_rows)}", file=sys.stderr)
    print(f"Tasks processed:             {len(task_rows)}", file=sys.stderr)
    print(f"Tasks w/ gold in candidates: {gold_found} "
          f"({gold_found/len(task_rows)*100:.1f}%)  "
          "← upper bound on any strategy's p1",
          file=sys.stderr)
    print(f"Tasks w/ zero candidates:    {no_candidates}", file=sys.stderr)
    if out_rows:
        per_task = Counter(r["task_id"] for r in out_rows)
        sizes = sorted(per_task.values())
        print(f"Candidates/task min/median/p90/max: "
              f"{sizes[0]}/{sizes[len(sizes)//2]}/"
              f"{sizes[int(len(sizes)*0.9)]}/{sizes[-1]}",
              file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())
