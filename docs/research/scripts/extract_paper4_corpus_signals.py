#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["tqdm>=4.0"]
# ///
"""
Paper 4 — Notebook+Parquet corpus signal extractor.

Walks ~/.claude/projects/*.jsonl and counts the four signals that
indicate the push-based knapsacks (Papers 1-3) are leaving tokens
on the table:

  1. **Big-response tail** — fraction of tool calls whose response
     `> N kB`. Per Paper 4 economics, every >10 kB response is
     where Parquet header + query starts winning over full body.
  2. **Pagination loops** — sequences where the same tool+args
     fires with `chunk=1, chunk=2, chunk=3, …`. Direct signal:
     agent is reading a dataset linearly, one Parquet artifact +
     LLM-side filter would replace N round-trips with 1.
  3. **Repeat-call rate** — same `(tool, fingerprint)` fires more
     than N times in one session. If a Read of /src/main.rs
     happens 5x or a get_issues with same filters repeats, the
     Parquet artifact pays back its serialisation cost.
  4. **List → detail loops** — list-style tool followed by N
     detail calls (Glob → Read ×N, get_issues → get_issue ×N).
     Paper 3 prefetches these; Paper 4 replaces them with a JOIN
     in the notebook so total calls drop to 1.

All four are *per-session* aggregates so cross-session noise does
not inflate the numbers.

Anonymisation contract is the same as paper3_followup_edges:
  - response sizes are character counts (`len(text)`), no raw text
  - tool_use_id and session_id are hashed
  - MCP slugs hashed (`mcp__p<hash6>__verb`)
  - file paths / arg values are *fingerprinted* (sha1 prefix), not
    stored in plaintext

Usage:
  uv run docs/research/scripts/extract_paper4_corpus_signals.py
  uv run docs/research/scripts/extract_paper4_corpus_signals.py \\
      --input-dir ~/.claude/projects --max-sessions 200
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Iterator

MCP_RE = re.compile(r"^mcp__")

# Big-response thresholds (chars).
BIG_THRESHOLDS = [4_000, 10_000, 25_000, 50_000]

# Repeat-call: how many times same (tool, fingerprint) before we
# count the call as a repeat.
REPEAT_FLOOR = 3


def hash_short(s: str, n: int = 8) -> str:
    return hashlib.sha1(s.encode("utf-8")).hexdigest()[:n]


def anonymize_tool(name: str) -> str:
    if not name or not MCP_RE.match(name):
        return name or ""
    prefix_end = len("mcp__")
    verb_start = name.rfind("__")
    if verb_start < prefix_end:
        return name
    slug = name[prefix_end:verb_start]
    verb = name[verb_start + 2 :]
    return f"mcp__p{hash_short(slug, 6)}__{verb}"


def args_fingerprint(args: dict | None) -> str:
    """Stable short hash of a tool_use's args (or `""` when missing).

    Strips the `chunk` field so paginated calls collapse to the
    same fingerprint — that is exactly the signal we want for the
    pagination-loop counter.
    """
    if not isinstance(args, dict):
        return ""
    stripped = {k: v for k, v in args.items() if k != "chunk"}
    return hash_short(json.dumps(stripped, sort_keys=True), 10)


def chunk_value(args: dict | None) -> int | None:
    if not isinstance(args, dict):
        return None
    v = args.get("chunk")
    if isinstance(v, (int, float)):
        return int(v)
    return None


def iter_tool_uses(jsonl_path: Path) -> Iterator[dict]:
    """Yield tool_use → tool_result pairs in JSONL order. Result body
    is `len(text)` only; original text never leaves this function."""
    pending: dict[str, dict] = {}
    with jsonl_path.open("r", encoding="utf-8", errors="replace") as f:
        for line_no, raw in enumerate(f):
            try:
                rec = json.loads(raw)
            except json.JSONDecodeError:
                continue
            content = rec.get("message", {}).get("content")
            if not isinstance(content, list):
                continue
            for blk in content:
                if not isinstance(blk, dict):
                    continue
                t = blk.get("type")
                if t == "tool_use":
                    tu_id = blk.get("id") or ""
                    args = blk.get("input")
                    pending[tu_id] = {
                        "tool_anon": anonymize_tool(blk.get("name") or ""),
                        "fingerprint": args_fingerprint(args),
                        "chunk": chunk_value(args),
                        "line_no": line_no,
                        "response_chars": None,
                    }
                elif t == "tool_result":
                    tu_id = blk.get("tool_use_id") or ""
                    body = blk.get("content")
                    if isinstance(body, list):
                        body_chars = sum(
                            len(x.get("text", ""))
                            for x in body
                            if isinstance(x, dict) and x.get("type") == "text"
                        )
                    elif isinstance(body, str):
                        body_chars = len(body)
                    else:
                        body_chars = 0
                    if tu_id in pending:
                        pending[tu_id]["response_chars"] = body_chars

    ordered = sorted(pending.values(), key=lambda r: r["line_no"])
    for ev in ordered:
        if ev["response_chars"] is None:
            ev["response_chars"] = 0
        yield ev


# ─── Per-session counters ────────────────────────────────────────────

LIST_FAMILY_PATTERNS = [
    "Glob",
    "list",
    "get_issues",
    "get_merge_requests",
    "get_pipelines",
    "search_",
]


def is_list_family(tool: str) -> bool:
    return any(p in tool.lower() for p in (lp.lower() for lp in LIST_FAMILY_PATTERNS))


DETAIL_FAMILY_PATTERNS = [
    "Read",
    "get_issue",
    "get_merge_request",
    "get_page",
    "get_meeting_transcript",
    "WebFetch",
]


def is_detail_family(tool: str) -> bool:
    # plain `Read` is the canonical "detail" tool for files. Also any
    # singular get_ which is not list_ / get_*s.
    if tool == "Read" or tool == "WebFetch":
        return True
    if tool.startswith("mcp__") and "__get_" in tool and not tool.endswith("s"):
        return True
    return any(p in tool for p in ("get_issue", "get_merge_request", "get_page", "get_meeting_transcript")) and not tool.endswith("s")


def process_session(jsonl_path: Path) -> dict:
    """Returns one dict of per-session signals."""
    big_buckets: dict[int, int] = {t: 0 for t in BIG_THRESHOLDS}
    total_calls = 0
    total_chars = 0

    # Pagination: same fingerprint + chunk values seen.
    fp_chunks: dict[tuple[str, str], list[int]] = defaultdict(list)
    # Repeats: count of (tool, fingerprint) occurrences.
    fp_counts: dict[tuple[str, str], int] = defaultdict(int)
    # List→Detail: pairs of (list_tool, detail_tool) within session.
    list_to_detail_pairs: Counter = Counter()

    events = list(iter_tool_uses(jsonl_path))
    for ev in events:
        total_calls += 1
        chars = ev["response_chars"]
        total_chars += chars
        for thresh in BIG_THRESHOLDS:
            if chars >= thresh:
                big_buckets[thresh] += 1
        key = (ev["tool_anon"], ev["fingerprint"])
        fp_counts[key] += 1
        if ev["chunk"] is not None:
            fp_chunks[key].append(ev["chunk"])

    # Pagination loops: a (tool, fingerprint-without-chunk) seen with
    # ≥ 2 distinct chunk values is a hit.
    pagination_hits = sum(1 for chunks in fp_chunks.values() if len(set(chunks)) >= 2)
    pagination_chunks_total = sum(len(set(chunks)) for chunks in fp_chunks.values() if len(set(chunks)) >= 2)

    # Repeats: count of (tool, fp) pairs that fired ≥ REPEAT_FLOOR times.
    repeated_fps = sum(1 for n in fp_counts.values() if n >= REPEAT_FLOOR)
    repeat_total_calls = sum(n for n in fp_counts.values() if n >= REPEAT_FLOOR)

    # List→Detail loops: walk the events linearly, every list-family
    # call followed by ≥ 3 detail-family calls of any type within the
    # next 10 events counts as a hit.
    LOOKAHEAD = 10
    detail_floor = 3
    list_detail_hits = 0
    list_detail_detail_count = 0
    for i, ev in enumerate(events):
        if not is_list_family(ev["tool_anon"]):
            continue
        end = min(i + 1 + LOOKAHEAD, len(events))
        n_detail = sum(1 for j in range(i + 1, end) if is_detail_family(events[j]["tool_anon"]))
        if n_detail >= detail_floor:
            list_detail_hits += 1
            list_detail_detail_count += n_detail
            list_to_detail_pairs[ev["tool_anon"]] += 1

    return {
        "session_hash": hash_short(jsonl_path.stem, 12),
        "total_calls": total_calls,
        "total_response_chars": total_chars,
        "big_4kb": big_buckets[4_000],
        "big_10kb": big_buckets[10_000],
        "big_25kb": big_buckets[25_000],
        "big_50kb": big_buckets[50_000],
        "pagination_loops": pagination_hits,
        "pagination_chunks_total": pagination_chunks_total,
        "repeated_fingerprints": repeated_fps,
        "repeated_call_count": repeat_total_calls,
        "list_to_detail_loops": list_detail_hits,
        "list_to_detail_detail_total": list_detail_detail_count,
        "_top_list_to_detail": list_to_detail_pairs,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input-dir", type=Path, default=Path.home() / ".claude/projects")
    ap.add_argument("--output-dir", type=Path, default=Path("/tmp/claude_analysis"))
    ap.add_argument("--max-sessions", type=int, default=0)
    args = ap.parse_args()

    sessions = sorted(args.input_dir.rglob("*.jsonl"))
    if args.max_sessions:
        sessions = sessions[: args.max_sessions]
    if not sessions:
        print(f"no sessions under {args.input_dir}", file=sys.stderr)
        return 1

    try:
        from tqdm import tqdm
        pbar = tqdm(sessions, desc="sessions", unit="s")
    except ImportError:
        pbar = sessions

    grand = {
        "sessions": 0,
        "total_calls": 0,
        "total_response_chars": 0,
        "big_4kb": 0,
        "big_10kb": 0,
        "big_25kb": 0,
        "big_50kb": 0,
        "pagination_loops": 0,
        "pagination_chunks_total": 0,
        "repeated_fingerprints": 0,
        "repeated_call_count": 0,
        "list_to_detail_loops": 0,
        "list_to_detail_detail_total": 0,
    }
    list_to_detail_pairs: Counter = Counter()
    skipped = 0

    args.output_dir.mkdir(parents=True, exist_ok=True)
    rows_path = args.output_dir / "paper4_session_signals.csv"
    with rows_path.open("w", encoding="utf-8") as out:
        out.write(
            "session_hash,total_calls,total_response_chars,big_4kb,big_10kb,big_25kb,big_50kb,"
            "pagination_loops,pagination_chunks_total,repeated_fingerprints,repeated_call_count,"
            "list_to_detail_loops,list_to_detail_detail_total\n"
        )
        for path in pbar:
            try:
                row = process_session(path)
            except Exception as e:  # noqa: BLE001
                skipped += 1
                print(f"skip {path.name}: {e}", file=sys.stderr)
                continue
            grand["sessions"] += 1
            for k in grand:
                if k in row and isinstance(row[k], int):
                    grand[k] += row[k]
            list_to_detail_pairs.update(row["_top_list_to_detail"])
            out.write(
                f"{row['session_hash']},{row['total_calls']},{row['total_response_chars']},"
                f"{row['big_4kb']},{row['big_10kb']},{row['big_25kb']},{row['big_50kb']},"
                f"{row['pagination_loops']},{row['pagination_chunks_total']},"
                f"{row['repeated_fingerprints']},{row['repeated_call_count']},"
                f"{row['list_to_detail_loops']},{row['list_to_detail_detail_total']}\n"
            )

    # Headline summary on stderr.
    s = grand
    pct = lambda a, b: (100.0 * a / b) if b else 0.0
    print(
        f"\n=== Paper 4 corpus signals — {s['sessions']:,} sessions / {s['total_calls']:,} calls ===",
        file=sys.stderr,
    )
    print(
        f"  big-response tail (≥10kB / ≥25kB / ≥50kB):  "
        f"{s['big_10kb']:,} ({pct(s['big_10kb'], s['total_calls']):.1f}%) / "
        f"{s['big_25kb']:,} ({pct(s['big_25kb'], s['total_calls']):.2f}%) / "
        f"{s['big_50kb']:,} ({pct(s['big_50kb'], s['total_calls']):.2f}%)",
        file=sys.stderr,
    )
    print(
        f"  pagination loops:  {s['pagination_loops']:,} loops, "
        f"{s['pagination_chunks_total']:,} chunked calls "
        f"({pct(s['pagination_chunks_total'], s['total_calls']):.2f}% of total)",
        file=sys.stderr,
    )
    print(
        f"  repeat-call hotspots (same args ≥ {REPEAT_FLOOR}× in 1 session):  "
        f"{s['repeated_fingerprints']:,} fingerprints, "
        f"{s['repeated_call_count']:,} calls "
        f"({pct(s['repeated_call_count'], s['total_calls']):.1f}% of total)",
        file=sys.stderr,
    )
    print(
        f"  list→detail loops:  {s['list_to_detail_loops']:,} loops, "
        f"{s['list_to_detail_detail_total']:,} downstream detail calls "
        f"({pct(s['list_to_detail_detail_total'], s['total_calls']):.1f}% of total)",
        file=sys.stderr,
    )
    print(f"\n  per-session CSV → {rows_path}", file=sys.stderr)
    if list_to_detail_pairs:
        print(f"\n  Top list-tool launchers of detail loops:", file=sys.stderr)
        for tool, n in list_to_detail_pairs.most_common(10):
            print(f"    {n:>6,}  {tool}", file=sys.stderr)
    if skipped:
        print(f"\n  {skipped} session(s) skipped due to errors", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
