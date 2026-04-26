#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=18.0", "tqdm>=4.0"]
# ///
"""
Paper 3 — Tool-Aware Knapsack Enrichment — corpus extractor.

Walks ~/.claude/projects/*.jsonl and emits two anonymized parquet files:

  paper3_tool_events.parquet
    one row per `tool_use` with the tool name (anonymized for MCP slugs),
    response size, and the tools that fired in the next 1, 2, 3 turns of
    the same session.

  paper3_followup_edges.parquet
    aggregated `(prev_tool, next_tool, lag) → count` matrix — the basis
    for the default `[tools.<name>].follow_up.likely_next` annotations
    in Paper 3.

Anonymisation contract (matches `extract_paper2_format_events.py`):
  - No raw response content is stored; only its length in bytes.
  - tool_use_id is hashed; session id is hashed.
  - MCP `mcp__<slug>__<verb>` tools have `<slug>` replaced with a hash
    so private project names do not leak.
  - File paths and arguments are not stored at all.

Usage:
  uv run docs/research/scripts/extract_paper3_followups.py
  uv run docs/research/scripts/extract_paper3_followups.py \\
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

import pyarrow as pa
import pyarrow.parquet as pq

# ─── Anonymisation helpers ───────────────────────────────────────────────

MCP_RE = re.compile(r"^mcp__")


def hash_short(s: str, n: int = 8) -> str:
    return hashlib.sha1(s.encode("utf-8")).hexdigest()[:n]


def anonymize_tool(name: str) -> str:
    """`mcp__<slug>__<verb>` → `mcp__p<hash6>__<verb>`; built-ins pass through."""
    if not name:
        return ""
    if not MCP_RE.match(name):
        return name
    prefix_end = len("mcp__")
    verb_start = name.rfind("__")
    if verb_start < prefix_end:
        return name
    slug = name[prefix_end:verb_start]
    verb = name[verb_start + 2 :]
    return f"mcp__p{hash_short(slug, 6)}__{verb}"


# ─── Per-session walk ────────────────────────────────────────────────────

def iter_tool_events(jsonl_path: Path) -> Iterator[dict]:
    """Yield (turn_idx, tool_anon, tool_use_id_hash, response_chars).

    Walks the session in JSONL order. Pairs each `tool_use` with the
    `tool_result` carrying the same `tool_use_id`; `response_chars` stays
    `None` if the result lands later (we re-link in a second pass below).
    """
    pending_uses: dict[str, dict] = {}
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
                    name = blk.get("name") or ""
                    tu_id = blk.get("id") or ""
                    pending_uses[tu_id] = {
                        "tool_anon": anonymize_tool(name),
                        "tool_use_id_hash": hash_short(tu_id),
                        "line_no": line_no,
                        "response_chars": None,
                    }
                elif t == "tool_result":
                    tu_id = blk.get("tool_use_id") or ""
                    body = blk.get("content")
                    if isinstance(body, list):
                        body_chars = sum(
                            len(x.get("text", "")) for x in body
                            if isinstance(x, dict) and x.get("type") == "text"
                        )
                    elif isinstance(body, str):
                        body_chars = len(body)
                    else:
                        body_chars = 0
                    if tu_id in pending_uses:
                        pending_uses[tu_id]["response_chars"] = body_chars

    # Emit in order of original tool_use occurrence.
    ordered = sorted(pending_uses.values(), key=lambda r: r["line_no"])
    for idx, rec in enumerate(ordered):
        yield {
            "turn_idx": idx,
            "tool_anon": rec["tool_anon"],
            "tool_use_id_hash": rec["tool_use_id_hash"],
            "response_chars": rec["response_chars"] if rec["response_chars"] is not None else 0,
        }


# ─── Event + edge accumulation ───────────────────────────────────────────

def process_session(jsonl_path: Path) -> tuple[list[dict], Counter]:
    session_id = jsonl_path.stem
    session_hash = hash_short(session_id, 12)

    events = list(iter_tool_events(jsonl_path))
    rows: list[dict] = []
    edges: Counter = Counter()

    for i, ev in enumerate(events):
        followups = []
        for lag in (1, 2, 3):
            j = i + lag
            tail = events[j]["tool_anon"] if j < len(events) else None
            followups.append(tail)
            if tail:
                edges[(ev["tool_anon"], tail, lag)] += 1
        rows.append(
            {
                "session_hash": session_hash,
                "turn_idx": ev["turn_idx"],
                "tool_anon": ev["tool_anon"],
                "tool_use_id_hash": ev["tool_use_id_hash"],
                "response_chars": ev["response_chars"],
                "followup_lag1": followups[0] or "",
                "followup_lag2": followups[1] or "",
                "followup_lag3": followups[2] or "",
            }
        )

    return rows, edges


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input-dir", type=Path, default=Path.home() / ".claude/projects")
    ap.add_argument(
        "--output-dir",
        type=Path,
        default=Path("/tmp/claude_analysis"),
    )
    ap.add_argument("--max-sessions", type=int, default=0, help="0 = no cap")
    args = ap.parse_args()

    out_dir = args.output_dir
    out_dir.mkdir(parents=True, exist_ok=True)

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

    all_rows: list[dict] = []
    edge_counter: Counter = Counter()
    skipped = 0

    for path in pbar:
        try:
            rows, edges = process_session(path)
        except Exception as e:  # noqa: BLE001 — corpus has malformed lines, skip & log
            skipped += 1
            print(f"skip {path.name}: {e}", file=sys.stderr)
            continue
        all_rows.extend(rows)
        edge_counter.update(edges)

    # ── Per-event parquet ─────────────────────────────────────────────
    if all_rows:
        events_table = pa.Table.from_pylist(all_rows)
        events_path = out_dir / "paper3_tool_events.parquet"
        pq.write_table(events_table, events_path)
        print(f"wrote {events_path} — {len(all_rows):,} events")

    # ── Edge aggregate parquet ────────────────────────────────────────
    if edge_counter:
        edge_rows = [
            {"prev_tool": prev, "next_tool": nxt, "lag": lag, "count": cnt}
            for (prev, nxt, lag), cnt in edge_counter.most_common()
        ]
        edges_table = pa.Table.from_pylist(edge_rows)
        edges_path = out_dir / "paper3_followup_edges.parquet"
        pq.write_table(edges_table, edges_path)
        print(f"wrote {edges_path} — {len(edge_rows):,} edges")

    # ── Cheap printable summary ───────────────────────────────────────
    tool_volume: Counter = Counter()
    for r in all_rows:
        tool_volume[r["tool_anon"]] += 1
    print("\nTop 10 tools by call volume:")
    for tool, n in tool_volume.most_common(10):
        print(f"  {n:>8,}  {tool}")

    print("\nTop 15 follow-up edges (lag=1):")
    for (prev, nxt, lag), n in edge_counter.most_common(200):
        if lag != 1:
            continue
        print(f"  {n:>6,}  {prev}  →  {nxt}")
        if n < 50:
            break

    if skipped:
        print(f"\n{skipped} session(s) skipped due to errors", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
