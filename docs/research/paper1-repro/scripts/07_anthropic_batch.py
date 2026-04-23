#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=18.0", "numpy>=1.26", "python-dotenv>=1.0", "requests>=2.32"]
# ///
"""
07 — Anthropic Batch API eval with prompt caching (E2 frontier tier).

Per (task × strategy × budget):
  1. Build compressed candidate list via 04's logic (fifo / priority_kw / priority_kw_fallback).
  2. Compose system (cacheable, with cache_control=ephemeral) + user (variable).
  3. Submit all requests as a single batch to Anthropic /v1/messages/batches.
  4. Poll status until `processing_status == "ended"`.
  5. Fetch results, parse `chosen_file` JSON, compare to gold, write parquet.

Batch API gives 50% discount + prompt caching drops input by 10x for stable
system prefix after first request in the window.

Estimated cost for 800 calls × Sonnet 4.5 (with cache + batch discount): ~$4.

Cache requirement: system prompt must be ≥ 1024 tokens (our expanded prompt is ~1400).

Usage:
  python 07_anthropic_batch.py --model claude-sonnet-4-5 --tasks 100 \\
    --strategies fifo,priority_kw,priority_kw_fallback --budgets 1000,2000,4000,8000 \\
    --output-partial sonnet45

Env: ANTHROPIC_API_KEY (in paper1/.env)
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from collections import defaultdict
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

# Must match 04_run_multi_llm.py — keep in sync
TOKENS_PER_CAND_BASE = 30


# ─── Prompts (shared logic with 04) ──────────────────────────────────────────

def strip_html_comments(s: str) -> str:
    return re.sub(r"<!--.*?-->", "", s, flags=re.DOTALL).strip()


def load_prompts() -> tuple[str, str]:
    system = strip_html_comments((PROMPTS / "system_cache_prefix.md").read_text(encoding="utf-8"))
    task = strip_html_comments((PROMPTS / "task_variable.md").read_text(encoding="utf-8"))
    return system, task


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


# ─── Strategies (kept in sync with 03 and 04) ────────────────────────────────

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
        for p in selected
    )
    is_correct = any(
        chosen_n == g or chosen_n.endswith("/" + g) or g.endswith("/" + chosen_n)
        for g in gold_norm
    )
    return (is_correct, in_selected)


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
            path=ct["path"][i], rank=ct["rank"][i], ext=ct["ext"][i],
            depth=ct["depth"][i], size_bytes=ct["size_bytes"][i],
            mtime_epoch=ct["mtime_epoch"][i],
            keyword_overlap_score=ct["keyword_overlap_score"][i],
        ))
    for tid in candidates:
        candidates[tid].sort(key=lambda r: r["rank"])
    gt = pq.read_table(GOLD_PARQUET).to_pydict()
    gold = {gt["task_id"][i]: {g for g in gt["gold_files"][i]}
            for i in range(len(gt["task_id"]))}
    issue = {gt["task_id"][i]: gt["problem_statement"][i]
             for i in range(len(gt["task_id"]))}
    return candidates, gold, issue


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
    ("tokens_prompt_fresh", pa.int32()),
    ("tokens_cache_read", pa.int32()),
    ("tokens_cache_create", pa.int32()),
    ("tokens_completion", pa.int32()),
    ("error", pa.string()),
])


# ─── Anthropic Batch API ─────────────────────────────────────────────────────

ANTHROPIC_URL = "https://api.anthropic.com"
HEADERS_BASE = {"anthropic-version": "2023-06-01"}


def anth_headers(key: str) -> dict[str, str]:
    return {**HEADERS_BASE, "x-api-key": key, "Content-Type": "application/json"}


def encode_custom_id(task_id: str, strategy: str, budget: int) -> str:
    """Anthropic caps custom_id at 64 chars ascii.
    SWE-bench task_ids are like 'astropy__astropy-12345' (~25 chars); short enough.
    """
    # sanitize — only allow [a-zA-Z0-9_.-]
    safe_tid = re.sub(r"[^A-Za-z0-9._-]", "_", task_id)[:40]
    cid = f"{safe_tid}__{strategy[:12]}__{budget}"
    return cid[:64]


def decode_custom_id(cid: str) -> tuple[str, str, int]:
    # reverse: task_id__strategy__budget
    # task_id may contain underscores; split from the right
    parts = cid.rsplit("__", 2)
    if len(parts) != 3:
        raise ValueError(f"bad custom_id: {cid}")
    return parts[0], parts[1], int(parts[2])


def submit_batch(key: str, requests_list: list[dict]) -> str:
    """POST /v1/messages/batches, return batch_id."""
    r = requests.post(
        f"{ANTHROPIC_URL}/v1/messages/batches",
        headers=anth_headers(key),
        json={"requests": requests_list},
        timeout=120,
    )
    if not r.ok:
        print(f"Submit failed: {r.status_code} {r.text[:500]}", file=sys.stderr)
        r.raise_for_status()
    return r.json()["id"]


def poll_batch(key: str, batch_id: str, poll_interval_s: int = 30,
               timeout_s: int = 24 * 3600) -> dict:
    """Poll batch status until processing_status == 'ended'. Return final status dict."""
    url = f"{ANTHROPIC_URL}/v1/messages/batches/{batch_id}"
    deadline = time.time() + timeout_s
    while True:
        r = requests.get(url, headers=anth_headers(key), timeout=60)
        r.raise_for_status()
        d = r.json()
        status = d.get("processing_status")
        counts = d.get("request_counts") or {}
        total_req = sum(counts.values()) or 1
        done = counts.get("succeeded", 0) + counts.get("errored", 0) + \
               counts.get("canceled", 0) + counts.get("expired", 0)
        print(f"  [{status}] processed={done}/{total_req}  "
              f"succ={counts.get('succeeded',0)}  err={counts.get('errored',0)}  "
              f"proc={counts.get('processing',0)}",
              file=sys.stderr)
        if status == "ended":
            return d
        if time.time() > deadline:
            raise TimeoutError(f"batch {batch_id} exceeded {timeout_s}s")
        time.sleep(poll_interval_s)


def fetch_results(key: str, results_url: str) -> list[dict]:
    """Fetch results JSONL, return list of parsed lines."""
    r = requests.get(results_url, headers=anth_headers(key), timeout=300, stream=True)
    r.raise_for_status()
    out = []
    for line in r.iter_lines():
        if not line:
            continue
        out.append(json.loads(line))
    return out


# ─── Main ────────────────────────────────────────────────────────────────────

def main() -> int:
    load_dotenv(HERE / ".env")
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default="claude-sonnet-4-5")
    parser.add_argument("--tasks", type=int, default=100)
    parser.add_argument("--strategies", default="fifo,priority_kw,priority_kw_fallback")
    parser.add_argument("--budgets", default="1000,2000,4000,8000")
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--max-tokens", type=int, default=1024,
                        help="Tighter than Ollama — the task asks for a single short JSON.")
    parser.add_argument("--output-partial", default=None)
    parser.add_argument("--poll-interval", type=int, default=30)
    parser.add_argument("--cache-ttl-5m", action="store_true", default=True,
                        help="Use 5-minute cache (default; 1h costs 2x input write)")
    parser.add_argument("--submit-only", action="store_true",
                        help="Submit batch, print batch_id, exit without polling")
    parser.add_argument("--fetch-existing", default=None,
                        help="Skip submit; given batch_id, poll + fetch + save")
    args = parser.parse_args()

    key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not key:
        print("ANTHROPIC_API_KEY not set", file=sys.stderr)
        return 2

    strategies = [s.strip() for s in args.strategies.split(",") if s.strip()]
    budgets = [int(b) for b in args.budgets.split(",") if b.strip()]
    for s in strategies:
        if s not in VALUE_FNS:
            print(f"Unknown strategy: {s}", file=sys.stderr)
            return 2

    suffix = args.output_partial or re.sub(r"[^a-zA-Z0-9]+", "_", args.model)
    partial_path = ARTIFACTS / f"llm_results.{suffix}.parquet"

    system, task_tmpl = load_prompts()
    print(f"System prompt: ~{len(system)//4} tok est. Must be ≥1024 for cache.",
          file=sys.stderr)

    candidates_by, gold_by, issue_by = load_task_data()
    strata = {tid: bool(cands and cands[0]["path"] in gold_by.get(tid, set()))
              for tid, cands in candidates_by.items()}
    tasks_with_gold = [tid for tid, cands in candidates_by.items()
                       if any(c["path"] in gold_by.get(tid, set()) for c in cands)]
    sampled = stratified_sample(tasks_with_gold, strata, args.tasks, seed=args.seed)
    print(f"Sampled {len(sampled)} tasks "
          f"(pos={sum(1 for t in sampled if strata.get(t))}, "
          f"neg={sum(1 for t in sampled if not strata.get(t))})",
          file=sys.stderr)

    # ── Build batch requests ──────────────────────────────────────────────
    cache_params = {"type": "ephemeral"}
    if not args.cache_ttl_5m:
        cache_params["ttl"] = "1h"

    # `selected` lookup to reuse in result parsing
    selected_by_cell: dict[tuple[str, str, int], list[str]] = {}
    gold_in_sel_by_cell: dict[tuple[str, str, int], bool] = {}

    requests_list: list[dict] = []
    for tid in sampled:
        cands = candidates_by[tid]
        gold = gold_by.get(tid, set())
        issue = issue_by.get(tid, "")
        for s in strategies:
            for b in budgets:
                selected = compress(cands, s, b)
                selected_by_cell[(tid, s, b)] = selected
                gold_in_sel_by_cell[(tid, s, b)] = any(p in gold for p in selected)
                user = build_user_block(task_tmpl, issue, selected, len(cands), b)
                cid = encode_custom_id(tid, s, b)
                requests_list.append({
                    "custom_id": cid,
                    "params": {
                        "model": args.model,
                        "max_tokens": args.max_tokens,
                        "system": [{
                            "type": "text",
                            "text": system,
                            "cache_control": cache_params,
                        }],
                        "messages": [{"role": "user", "content": user}],
                    }
                })

    total = len(requests_list)
    print(f"Batch size: {total} requests ({len(sampled)} × "
          f"{len(strategies)} × {len(budgets)})",
          file=sys.stderr)

    if args.fetch_existing:
        batch_id = args.fetch_existing
        print(f"Fetching existing batch {batch_id}…", file=sys.stderr)
    else:
        # Submit
        print(f"Submitting batch to {ANTHROPIC_URL}/v1/messages/batches…",
              file=sys.stderr)
        batch_id = submit_batch(key, requests_list)
        print(f"  batch_id = {batch_id}", file=sys.stderr)

        (ARTIFACTS / f"batch_id.{suffix}.txt").write_text(batch_id)
        if args.submit_only:
            print("--submit-only: exit without polling. Re-run with --fetch-existing "
                  f"{batch_id} to complete.", file=sys.stderr)
            return 0

    # Poll
    print("Polling status (every "
          f"{args.poll_interval}s, max 24h)…", file=sys.stderr)
    status = poll_batch(key, batch_id, poll_interval_s=args.poll_interval)
    results_url = status.get("results_url")
    if not results_url:
        print(f"No results_url in batch status: {status}", file=sys.stderr)
        return 3

    # Fetch + parse
    print(f"Fetching results from {results_url}…", file=sys.stderr)
    lines = fetch_results(key, results_url)
    print(f"  fetched {len(lines)} result lines", file=sys.stderr)

    out_rows = []
    correct = 0
    errors = 0
    total_cache_create = 0
    total_cache_read = 0
    total_input_fresh = 0
    total_output = 0
    for line in lines:
        cid = line.get("custom_id", "")
        try:
            tid, strategy, budget = decode_custom_id(cid)
        except ValueError:
            errors += 1
            continue
        result_block = line.get("result") or {}
        error = ""
        chosen_file = ""
        confidence = 0.0
        is_correct = False
        chosen_in_sel = False
        usage = {}
        content_text = ""

        if result_block.get("type") == "succeeded":
            msg = result_block.get("message") or {}
            usage = msg.get("usage") or {}
            for c in msg.get("content", []) or []:
                if c.get("type") == "text":
                    content_text += c.get("text", "")
            parsed = extract_json(content_text)
            if parsed and isinstance(parsed, dict):
                chosen_file = str(parsed.get("chosen_file") or "").strip()
                try:
                    confidence = float(parsed.get("confidence") or 0.0)
                except (TypeError, ValueError):
                    confidence = 0.0
                gold = gold_by.get(tid, set())
                selected = selected_by_cell.get((tid, strategy, budget), [])
                is_correct, chosen_in_sel = judge(chosen_file, list(gold), selected)
            else:
                error = "parse_failure"
        else:
            err_obj = result_block.get("error") or {}
            error = f"{result_block.get('type','unknown')}: {err_obj.get('type','')}: {err_obj.get('message','')[:120]}"
            errors += 1

        cache_create = usage.get("cache_creation_input_tokens", 0) or 0
        cache_read = usage.get("cache_read_input_tokens", 0) or 0
        input_fresh = usage.get("input_tokens", 0) or 0
        output_tok = usage.get("output_tokens", 0) or 0
        total_cache_create += cache_create
        total_cache_read += cache_read
        total_input_fresh += input_fresh
        total_output += output_tok

        if is_correct:
            correct += 1

        out_rows.append(dict(
            task_id=tid, model=args.model, strategy=strategy,
            budget_tok=int(budget), chosen_file=chosen_file,
            confidence=float(confidence), is_correct=bool(is_correct),
            chosen_in_selected=bool(chosen_in_sel),
            gold_in_selected=bool(gold_in_sel_by_cell.get((tid, strategy, budget), False)),
            n_selected=int(len(selected_by_cell.get((tid, strategy, budget), []))),
            n_candidates=int(len(candidates_by.get(tid, []))),
            tokens_prompt_fresh=int(input_fresh),
            tokens_cache_read=int(cache_read),
            tokens_cache_create=int(cache_create),
            tokens_completion=int(output_tok),
            error=error,
        ))

    table = pa.Table.from_pylist(out_rows, schema=OUTPUT_SCHEMA)
    pq.write_table(table, partial_path, compression="zstd")

    # Cost estimate (Sonnet 4.5: input $3/M, output $15/M, cache_read 0.1x, cache_write 1.25x, batch 50%)
    # Approx — using Sonnet pricing; adjust for other models externally.
    price_in = 3.0
    price_out = 15.0
    price_cache_read = 0.1 * price_in
    price_cache_write = 1.25 * price_in  # 5m
    batch_discount = 0.5
    cost = batch_discount * (
        total_input_fresh * price_in / 1e6
        + total_cache_read * price_cache_read / 1e6
        + total_cache_create * price_cache_write / 1e6
        + total_output * price_out / 1e6
    )

    print(file=sys.stderr)
    print(f"Wrote {partial_path}  rows={len(out_rows)}", file=sys.stderr)
    print(f"Final: correct={correct}/{len(out_rows)}  errors={errors}",
          file=sys.stderr)
    print(f"Token usage:", file=sys.stderr)
    print(f"  input (fresh):     {total_input_fresh:>10,}", file=sys.stderr)
    print(f"  cache read:        {total_cache_read:>10,}", file=sys.stderr)
    print(f"  cache create (5m): {total_cache_create:>10,}", file=sys.stderr)
    print(f"  output:            {total_output:>10,}", file=sys.stderr)
    cache_hit_rate = total_cache_read / max(total_cache_read + total_cache_create + total_input_fresh, 1)
    print(f"  cache hit rate:    {cache_hit_rate*100:>9.1f}%", file=sys.stderr)
    print(f"Estimated cost (Sonnet pricing × batch 50%): ${cost:.3f}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())
