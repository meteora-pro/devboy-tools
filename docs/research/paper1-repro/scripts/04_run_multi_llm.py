#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=18.0", "numpy>=1.26", "python-dotenv>=1.0", "requests>=2.32"]
# ///
"""
04 — E2 Multi-LLM comprehension on compressed candidate lists.

For each (task × strategy × budget × model):
  1. Apply strategy + knapsack → selected candidate paths
  2. Render as numbered Markdown list
  3. Build prompt: stable system prefix + variable task block
  4. Call Ollama /api/chat with think=high, num_ctx=8192, keep_alive=2h
  5. Parse {"chosen_file": ...} from content
  6. Compare to gold_files → is_correct

Inputs:
  artifacts/candidates.parquet      (from 02)
  artifacts/swe_bench_gold.parquet  (from 01)
  prompts/system_cache_prefix.md
  prompts/task_variable.md

Output:
  artifacts/llm_results.<suffix>.parquet  (one file per model run)

Checkpointing: flush every N completions; --resume skips already-done tuples.
Run sequentially per model — VRAM swap cost is too high otherwise.
"""
from __future__ import annotations

import argparse
import json
import os
import random
import re
import sys
import threading
import time
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

import numpy as np
import pyarrow as pa
import pyarrow.parquet as pq
import requests
from dotenv import load_dotenv


HERE = Path(__file__).resolve().parent.parent
ARTIFACTS = HERE / "artifacts"
PROMPTS = HERE / "prompts"
CANDIDATES_PARQUET = ARTIFACTS / "candidates.parquet"
GOLD_PARQUET = ARTIFACTS / "swe_bench_gold.parquet"

TOKENS_PER_CAND_BASE = 30
BUDGETS = [1000, 2000, 4000, 8000]
STRATEGIES = ["fifo", "priority_kw", "priority_kw_fallback"]

OLLAMA_NUM_CTX = 8192
OLLAMA_NUM_PREDICT = 4096
OLLAMA_KEEP_ALIVE = "2h"
OLLAMA_THINK_LEVEL = "high"
OLLAMA_TIMEOUT_S = 300


# ─── Prompts ──────────────────────────────────────────────────────────────────

def strip_html_comments(s: str) -> str:
    return re.sub(r"<!--.*?-->", "", s, flags=re.DOTALL).strip()


def load_prompts() -> tuple[str, str]:
    system = strip_html_comments((PROMPTS / "system_cache_prefix.md").read_text(encoding="utf-8"))
    task = strip_html_comments((PROMPTS / "task_variable.md").read_text(encoding="utf-8"))
    return system, task


# ─── Strategies (kept in sync with 03) ───────────────────────────────────────

def token_cost(path: str) -> int:
    return TOKENS_PER_CAND_BASE + max(1, len(path) // 4)


def value_fifo(cands: list[dict]) -> np.ndarray:
    n = len(cands)
    if n == 0:
        return np.empty(0)
    return np.array([1.0 - (i / n) for i in range(n)], dtype=np.float64)


def value_priority_kw(cands: list[dict]) -> np.ndarray:
    return np.array([c["keyword_overlap_score"] for c in cands], dtype=np.float64)


def value_priority_kw_fallback(cands: list[dict]) -> np.ndarray:
    """Priority-KW with FIFO fallback when keyword signal is zero across all candidates."""
    kws = [c["keyword_overlap_score"] for c in cands]
    if not kws or max(kws) == 0:
        return value_fifo(cands)
    n = len(cands)
    eps = 1e-6
    return np.array(
        [kw + eps * (1.0 - i / max(n - 1, 1)) for i, kw in enumerate(kws)],
        dtype=np.float64,
    )


VALUE_FNS = {
    "fifo": value_fifo,
    "priority_kw": value_priority_kw,
    "priority_kw_fallback": value_priority_kw_fallback,
}


def knapsack_01(values: np.ndarray, costs: np.ndarray, budget: int) -> list[int]:
    n = len(values)
    if n == 0 or budget <= 0:
        return []
    dp = np.zeros(budget + 1, dtype=np.float64)
    keep = np.zeros((n, budget + 1), dtype=bool)
    for i in range(n):
        ci = int(costs[i])
        if ci > budget:
            continue
        vi = float(values[i])
        new_dp = dp.copy()
        for c in range(ci, budget + 1):
            alt = dp[c - ci] + vi
            if alt > dp[c]:
                new_dp[c] = alt
                keep[i, c] = True
        dp = new_dp
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


def compress(cands: list[dict], strategy: str, budget: int) -> list[str]:
    if not cands:
        return []
    values = VALUE_FNS[strategy](cands)
    costs = np.array([token_cost(c["path"]) for c in cands], dtype=np.int64)
    idxs = knapsack_01(values, costs, budget)
    idxs.sort(key=lambda i: -values[i])
    return [cands[i]["path"] for i in idxs]


# ─── LLM call ────────────────────────────────────────────────────────────────

def render_candidate_list(paths: list[str]) -> str:
    if not paths:
        return "  (no candidates)"
    return "\n".join(f"  {i+1}. {p}" for i, p in enumerate(paths))


def build_user_block(task_template: str, issue: str, paths: list[str],
                     n_total: int, budget: int) -> str:
    chunk_hint = (f"[showing {len(paths)} of {n_total} candidates]"
                  if len(paths) < n_total else "")
    return (
        task_template
        .replace("{{ISSUE_TEXT}}", issue.strip())
        .replace("{{N_ITEMS}}", str(n_total))
        .replace("{{BUDGET_TOK}}", str(budget))
        .replace("{{CANDIDATE_LIST}}", render_candidate_list(paths))
        .replace("{{CHUNK_HINT}}", chunk_hint)
    )


def call_zai_anthropic(url: str, api_key: str, model: str, system: str, user: str,
                        max_tokens: int, thinking_budget: int,
                        timeout_s: int,
                        max_retries: int = 4) -> dict:
    """Call z.ai's Anthropic-compatible /v1/messages endpoint.

    Retries on 429 (rate limit) and 5xx with exponential backoff: 2s, 5s, 15s, 45s.
    Improvements vs naive retry:
      - `Retry-After` header respected when the server provides one
      - ±25% jitter on every wait to avoid thundering-herd synchronization
    If thinking_budget > 0, enables extended thinking. Content blocks are merged:
    the `text` block (final answer) goes into result["content"], and `thinking`
    blocks concatenated into result["thinking"].
    """
    headers = {
        "Content-Type": "application/json",
        "x-api-key": api_key,
        "anthropic-version": "2023-06-01",
    }
    body = {
        "model": model,
        "max_tokens": max_tokens,
        "messages": [{"role": "user", "content": user}],
    }
    if system:
        body["system"] = system
    if thinking_budget > 0:
        body["thinking"] = {"type": "enabled", "budget_tokens": thinking_budget}

    backoffs = [2, 5, 15, 45]

    def wait_for(attempt: int, retry_after: str | None = None) -> float:
        """Seconds to sleep before next retry. Honors Retry-After, jitters ±25%."""
        if retry_after:
            try:
                base = max(0.5, float(retry_after))
            except ValueError:
                # Header may be an HTTP date — fall back to table
                base = float(backoffs[min(attempt, len(backoffs) - 1)])
        else:
            base = float(backoffs[min(attempt, len(backoffs) - 1)])
        return base * random.uniform(0.75, 1.25)

    last_err: Exception | None = None
    r = None  # last response, used in no-retry raise at loop end
    t0 = time.time()
    for attempt in range(max_retries):
        try:
            r = requests.post(url, json=body, headers=headers, timeout=timeout_s)
            if r.status_code == 429 or 500 <= r.status_code < 600:
                retry_after = r.headers.get("Retry-After") or r.headers.get("retry-after")
                time.sleep(wait_for(attempt, retry_after))
                continue
            r.raise_for_status()
            break
        except requests.exceptions.RequestException as e:
            last_err = e
            time.sleep(wait_for(attempt))
    else:
        # all retries exhausted
        if last_err:
            raise last_err
        if r is not None:
            r.raise_for_status()
        raise RuntimeError("call_zai_anthropic: retries exhausted, no response captured")

    latency_ms = int((time.time() - t0) * 1000)
    d = r.json()
    text_parts, thinking_parts = [], []
    for c in d.get("content", []) or []:
        if c.get("type") == "text":
            text_parts.append(c.get("text") or "")
        elif c.get("type") == "thinking":
            thinking_parts.append(c.get("thinking") or "")
    usage = d.get("usage") or {}
    return {
        "content": "".join(text_parts).strip(),
        "thinking": "\n".join(thinking_parts).strip(),
        "eval_count": usage.get("output_tokens", 0),
        "prompt_eval_count": usage.get("input_tokens", 0),
        "total_duration_ms": latency_ms,
        "latency_ms": latency_ms,
    }


def call_ollama_chat(url: str, model: str, system: str, user: str,
                     num_ctx: int, num_predict: int, keep_alive: str,
                     think_level: str, timeout_s: int) -> dict:
    body = {
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "think": think_level,
        "options": {
            "num_ctx": num_ctx,
            "num_predict": num_predict,
            "temperature": 0.0,
        },
        "keep_alive": keep_alive,
        "stream": False,
    }
    t0 = time.time()
    r = requests.post(url, json=body, timeout=timeout_s)
    latency_ms = int((time.time() - t0) * 1000)
    r.raise_for_status()
    d = r.json()
    msg = d.get("message") or {}
    return {
        "content": (msg.get("content") or "").strip(),
        "thinking": (msg.get("thinking") or "").strip(),
        "eval_count": d.get("eval_count", 0),
        "prompt_eval_count": d.get("prompt_eval_count", 0),
        "total_duration_ms": int(d.get("total_duration", 0) / 1e6),
        "latency_ms": latency_ms,
    }


# ─── Response parsing ────────────────────────────────────────────────────────

JSON_OBJ = re.compile(r"\{[^{}]*\"chosen_file\"[^{}]*\}", re.DOTALL)


def extract_json(text: str) -> dict | None:
    if not text:
        return None
    t = text.strip()
    if t.startswith("```"):
        t = re.sub(r"^```[a-zA-Z]*\n?", "", t)
        t = re.sub(r"\n?```\s*$", "", t)
        t = t.strip()
    try:
        return json.loads(t)
    except json.JSONDecodeError:
        pass
    m = JSON_OBJ.search(t)
    if m:
        try:
            return json.loads(m.group(0))
        except json.JSONDecodeError:
            return None
    return None


def judge(chosen: str, gold_files: list[str], selected: list[str]) -> tuple[bool, bool]:
    if not chosen:
        return (False, False)
    chosen_n = chosen.strip().lstrip("/")
    gold_norm = [g.strip().lstrip("/") for g in gold_files]
    in_selected = any(
        chosen_n == p or chosen_n.endswith("/" + p) or p.endswith("/" + chosen_n)
        or chosen_n == p
        for p in selected
    )
    is_correct = any(
        chosen_n == g or chosen_n.endswith("/" + g) or g.endswith("/" + chosen_n)
        for g in gold_norm
    )
    return (is_correct, in_selected)


# ─── Sampling ────────────────────────────────────────────────────────────────

def stratified_sample(task_ids: list[str], strata: dict[str, bool],
                      n_sample: int, seed: int = 42) -> list[str]:
    rng = np.random.default_rng(seed)
    pos = [t for t in task_ids if strata.get(t, False)]
    neg = [t for t in task_ids if not strata.get(t, False)]
    rng.shuffle(pos)
    rng.shuffle(neg)
    half = min(n_sample // 2, len(pos))
    rest = n_sample - half
    picked = pos[:half] + neg[:rest]
    if len(picked) < n_sample:
        # Backfill from whichever pool has extras
        extras = (pos[half:] + neg[rest:])
        rng.shuffle(extras)
        picked += extras[:n_sample - len(picked)]
    rng.shuffle(picked)
    return picked[:n_sample]


def load_task_data() -> tuple[dict[str, list[dict]], dict[str, set[str]],
                              dict[str, str]]:
    if not CANDIDATES_PARQUET.exists():
        raise SystemExit(f"Missing {CANDIDATES_PARQUET}. Run 02 first.")
    if not GOLD_PARQUET.exists():
        raise SystemExit(f"Missing {GOLD_PARQUET}. Run 01 first.")

    ct = pq.read_table(CANDIDATES_PARQUET).to_pydict()
    candidates: dict[str, list[dict]] = defaultdict(list)
    for i in range(len(ct["task_id"])):
        candidates[ct["task_id"][i]].append(dict(
            path=ct["path"][i],
            rank=ct["rank"][i],
            ext=ct["ext"][i],
            depth=ct["depth"][i],
            size_bytes=ct["size_bytes"][i],
            mtime_epoch=ct["mtime_epoch"][i],
            keyword_overlap_score=ct["keyword_overlap_score"][i],
        ))
    for tid in candidates:
        candidates[tid].sort(key=lambda r: r["rank"])

    gt = pq.read_table(GOLD_PARQUET).to_pydict()
    gold: dict[str, set[str]] = {}
    issue: dict[str, str] = {}
    for i in range(len(gt["task_id"])):
        tid = gt["task_id"][i]
        gold[tid] = {g for g in gt["gold_files"][i]}
        issue[tid] = gt["problem_statement"][i]
    return candidates, gold, issue


# ─── Parquet I/O ─────────────────────────────────────────────────────────────

OUTPUT_SCHEMA = pa.schema([
    ("task_id", pa.string()),
    ("model", pa.string()),
    ("strategy", pa.string()),
    ("budget_tok", pa.int32()),
    ("chosen_file", pa.string()),
    ("confidence", pa.float32()),
    ("is_correct", pa.bool_()),
    ("chosen_in_selected", pa.bool_()),
    ("gold_in_selected", pa.bool_()),
    ("n_selected", pa.int32()),
    ("n_candidates", pa.int32()),
    ("tokens_prompt", pa.int32()),
    ("tokens_completion", pa.int32()),
    ("latency_ms", pa.int32()),
    ("error", pa.string()),
])


def append_rows(partial_path: Path, rows: list[dict]) -> None:
    if not rows:
        return
    new_tbl = pa.Table.from_pylist(rows, schema=OUTPUT_SCHEMA)
    if partial_path.exists():
        old = pq.read_table(partial_path)
        combined = pa.concat_tables([old, new_tbl])
    else:
        combined = new_tbl
    pq.write_table(combined, partial_path, compression="zstd")


def load_processed_keys(partial_path: Path) -> set[tuple[str, str, str, int]]:
    if not partial_path.exists():
        return set()
    d = pq.read_table(partial_path).to_pydict()
    n = len(d["task_id"])
    return {(d["task_id"][i], d["model"][i], d["strategy"][i], int(d["budget_tok"][i]))
            for i in range(n)}


# ─── Main ────────────────────────────────────────────────────────────────────

def main() -> int:
    load_dotenv(HERE / ".env")
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    parser.add_argument("--tasks", type=int, default=100)
    parser.add_argument("--strategies", default=",".join(STRATEGIES))
    parser.add_argument("--budgets", default=",".join(str(b) for b in BUDGETS))
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--provider", default="ollama",
                        choices=["ollama", "zai"],
                        help="ollama = local native /api/chat; zai = z.ai Anthropic-compat /v1/messages")
    parser.add_argument("--concurrency", type=int, default=1,
                        help="Parallel requests. Safe for cloud APIs (zai); keep 1 for Ollama to avoid KV swap.")
    # Ollama-specific
    parser.add_argument("--num-ctx", type=int, default=OLLAMA_NUM_CTX)
    parser.add_argument("--num-predict", type=int, default=OLLAMA_NUM_PREDICT)
    parser.add_argument("--keep-alive", default=OLLAMA_KEEP_ALIVE)
    parser.add_argument("--think-level", default=OLLAMA_THINK_LEVEL,
                        choices=["low", "medium", "high"])
    parser.add_argument("--ollama-url",
                        default=os.environ.get("OLLAMA_NATIVE_URL",
                                               "http://localhost:11343"))
    # zai-specific
    parser.add_argument("--zai-url",
                        default=os.environ.get("ZAI_BASE_URL",
                                               "https://api.z.ai/api/anthropic"))
    parser.add_argument("--thinking-budget", type=int, default=0,
                        help="zai only: if >0, enable thinking with N budget tokens")
    parser.add_argument("--max-tokens", type=int, default=4096,
                        help="zai only: max response tokens")
    # shared
    parser.add_argument("--timeout-s", type=int, default=OLLAMA_TIMEOUT_S)
    parser.add_argument("--output-partial", default=None)
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--flush-every", type=int, default=25)
    args = parser.parse_args()

    strategies = [s.strip() for s in args.strategies.split(",") if s.strip()]
    budgets = [int(b) for b in args.budgets.split(",") if b.strip()]
    for s in strategies:
        if s not in VALUE_FNS:
            print(f"Unknown strategy: {s}", file=sys.stderr)
            return 2

    suffix = args.output_partial or re.sub(r"[^a-zA-Z0-9]+", "_", args.model)
    partial_path = ARTIFACTS / f"llm_results.{suffix}.parquet"

    system, task_tmpl = load_prompts()
    print(f"System prompt: ~{len(system.split())} words / ~{len(system)//4} tok (est)",
          file=sys.stderr)

    candidates_by, gold_by, issue_by = load_task_data()

    # ── Build provider-specific call closure ────────────────────────────────
    if args.provider == "ollama":
        chat_url = args.ollama_url.rstrip("/") + "/api/chat"

        def do_call(system_: str, user_: str) -> dict:
            return call_ollama_chat(
                chat_url, args.model, system_, user_,
                num_ctx=args.num_ctx, num_predict=args.num_predict,
                keep_alive=args.keep_alive,
                think_level=args.think_level, timeout_s=args.timeout_s,
            )

        prewarm_kwargs = dict(num_ctx=args.num_ctx, num_predict=64,
                              keep_alive=args.keep_alive,
                              think_level="low", timeout_s=args.timeout_s)

        def do_prewarm() -> None:
            call_ollama_chat(chat_url, args.model,
                             system="You are a test bot.",
                             user="Reply with JSON {\"ok\": true}",
                             **prewarm_kwargs)

    elif args.provider == "zai":
        zai_url = args.zai_url.rstrip("/") + "/v1/messages"
        zai_key = os.environ.get("ZAI_API_KEY", "")
        if not zai_key:
            print("ERROR: ZAI_API_KEY not set in environment / .env", file=sys.stderr)
            return 4

        def do_call(system_: str, user_: str) -> dict:
            return call_zai_anthropic(
                zai_url, zai_key, args.model, system_, user_,
                max_tokens=args.max_tokens,
                thinking_budget=args.thinking_budget,
                timeout_s=args.timeout_s,
            )

        def do_prewarm() -> None:
            call_zai_anthropic(zai_url, zai_key, args.model,
                               system="", user="Reply with JSON {\"ok\": true}",
                               max_tokens=64, thinking_budget=0,
                               timeout_s=args.timeout_s)
    else:
        raise SystemExit(f"unknown provider {args.provider}")

    strata = {tid: bool(cands and cands[0]["path"] in gold_by.get(tid, set()))
              for tid, cands in candidates_by.items()}
    tasks_with_gold_in_cand = [
        tid for tid, cands in candidates_by.items()
        if any(c["path"] in gold_by.get(tid, set()) for c in cands)
    ]
    print(f"Tasks w/ gold in candidates: {len(tasks_with_gold_in_cand)}",
          file=sys.stderr)
    sampled = stratified_sample(tasks_with_gold_in_cand, strata,
                                args.tasks, seed=args.seed)
    print(f"Sampled {len(sampled)} tasks (seed={args.seed}), "
          f"positive_strata={sum(1 for t in sampled if strata.get(t))}, "
          f"negative_strata={sum(1 for t in sampled if not strata.get(t))}",
          file=sys.stderr)

    processed = load_processed_keys(partial_path) if args.resume else set()
    if processed:
        print(f"Resuming; {len(processed)} cells already done", file=sys.stderr)

    cells = [
        (tid, s, b)
        for tid in sampled
        for s in strategies
        for b in budgets
        if (tid, args.model, s, b) not in processed
    ]
    total_cells = len(cells)
    print(f"Total cells to evaluate: {total_cells} "
          f"({len(sampled)} × {len(strategies)} × {len(budgets)})",
          file=sys.stderr)
    if total_cells == 0:
        print("Nothing to do.", file=sys.stderr)
        return 0

    # Prewarm
    print(f"Prewarming {args.provider}/{args.model}…", file=sys.stderr)
    try:
        t0 = time.time()
        do_prewarm()
        print(f"  prewarm done in {time.time()-t0:.1f}s", file=sys.stderr)
    except Exception as e:
        print(f"Prewarm failed: {e}", file=sys.stderr)
        return 3

    # Per-cell work function (runs on thread pool for concurrency > 1)
    def eval_one(cell: tuple[str, str, int]) -> dict:
        tid, strategy, budget = cell
        cands = candidates_by[tid]
        gold = gold_by.get(tid, set())
        issue = issue_by.get(tid, "")
        selected = compress(cands, strategy, budget)
        gold_in_selected = any(p in gold for p in selected)
        user = build_user_block(task_tmpl, issue, selected, len(cands), budget)

        error = ""
        chosen_file = ""
        confidence = 0.0
        is_correct = False
        chosen_in_sel = False
        tokens_prompt = 0
        tokens_completion = 0
        latency_ms = 0

        try:
            resp = do_call(system, user)
            tokens_prompt = resp["prompt_eval_count"]
            tokens_completion = resp["eval_count"]
            latency_ms = resp["latency_ms"]
            parsed = extract_json(resp["content"])
            if parsed and isinstance(parsed, dict):
                chosen_file = str(parsed.get("chosen_file") or "").strip()
                try:
                    confidence = float(parsed.get("confidence") or 0.0)
                except (TypeError, ValueError):
                    confidence = 0.0
                is_correct, chosen_in_sel = judge(chosen_file, list(gold), selected)
            else:
                error = "parse_failure"
        except requests.exceptions.RequestException as e:
            error = f"request_error: {type(e).__name__}: {str(e)[:120]}"
        except Exception as e:
            error = f"unexpected: {type(e).__name__}: {str(e)[:120]}"

        return dict(
            task_id=tid, model=args.model, strategy=strategy,
            budget_tok=int(budget), chosen_file=chosen_file,
            confidence=float(confidence), is_correct=bool(is_correct),
            chosen_in_selected=bool(chosen_in_sel),
            gold_in_selected=bool(gold_in_selected),
            n_selected=int(len(selected)), n_candidates=int(len(cands)),
            tokens_prompt=int(tokens_prompt),
            tokens_completion=int(tokens_completion),
            latency_ms=int(latency_ms), error=error,
        )

    pending: list[dict] = []
    flush_lock = threading.Lock()
    correct_so_far = 0
    total_so_far = 0
    t_start = time.time()

    def handle_result(row: dict, idx: int) -> None:
        nonlocal correct_so_far, total_so_far
        with flush_lock:
            if row["is_correct"]:
                correct_so_far += 1
            total_so_far += 1
            pending.append(row)
            if len(pending) >= args.flush_every or idx == total_cells:
                append_rows(partial_path, pending)
                pending.clear()
                elapsed = time.time() - t_start
                rate = idx / elapsed if elapsed else 0
                eta = (total_cells - idx) / rate if rate else 0
                avg_ms = elapsed * 1000 / idx
                print(f"[{idx:4d}/{total_cells}] "
                  f"acc={correct_so_far/max(total_so_far,1)*100:.1f}% "
                  f"({correct_so_far}/{total_so_far})  "
                  f"elapsed={elapsed/60:.1f}m  eta={eta/60:.1f}m  "
                  f"avg={avg_ms:.0f}ms/call  conc={args.concurrency}",
                  file=sys.stderr)

    if args.concurrency <= 1:
        for idx, cell in enumerate(cells, 1):
            row = eval_one(cell)
            handle_result(row, idx)
    else:
        print(f"Running with concurrency={args.concurrency}", file=sys.stderr)
        with ThreadPoolExecutor(max_workers=args.concurrency) as pool:
            futures = {pool.submit(eval_one, cell): cell for cell in cells}
            done_count = 0
            for fut in as_completed(futures):
                row = fut.result()
                done_count += 1
                handle_result(row, done_count)

    # Final flush (in case last batch wasn't the exact multiple of flush_every)
    with flush_lock:
        if pending:
            append_rows(partial_path, pending)
            pending.clear()

    print(file=sys.stderr)
    print(f"Wrote {partial_path}", file=sys.stderr)
    print(f"Final: acc={correct_so_far/max(total_so_far,1)*100:.1f}% "
          f"({correct_so_far}/{total_so_far})  "
          f"total={(time.time()-t_start)/60:.1f}min",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
