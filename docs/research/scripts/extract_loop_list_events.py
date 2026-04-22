#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0"]
# ///
"""
Per-loop list-tool events with gold-position detection.

For each loop that contains mcp_list call(s), re-parse the original JSONL to:
  1. Extract the list response items (IDs, in order)
  2. Find the first specific item the agent references after the list
     (via enrichment tool call or text mention within the SAME loop)
  3. Record gold_position and list_response_size

Output: /tmp/claude_analysis/loop_list_events.parquet

Ties gold-selection events back to specific loops so we can:
- See which loop sizes have gold-knapsack applicability
- Compute p1 per loop bucket
- Filter to Paper-1-applicable loops
"""
import argparse
import json
import re
import sys
from collections import OrderedDict
from pathlib import Path

import duckdb
import pyarrow as pa
import pyarrow.parquet as pq


ID_PATTERNS = [
    re.compile(r"#(\d+)"),
    re.compile(r"!(\d+)"),
    re.compile(r'"iid":\s*(\d+)'),
    re.compile(r'"id":\s*"([^"]+)"'),
]

LIST_VERB_RE = re.compile(
    r"mcp__[a-zA-Z0-9_-]+__(get_issues|search_issues|get_merge_requests|"
    r"search_merge_requests|get_epics|search_epics|"
    r"get_meeting_notes|search_meeting_notes)$"
)
ENRICH_VERB_RE = re.compile(
    r"mcp__[a-zA-Z0-9_-]+__(get_issue|get_issue_comments|get_issue_relations|"
    r"get_merge_request|get_merge_request_discussions|get_merge_request_diffs|"
    r"get_meeting_transcript)$"
)


def extract_ids(text):
    seen = set()
    out = []
    for pat in ID_PATTERNS:
        for m in pat.finditer(text):
            v = m.group(1)
            if v not in seen:
                seen.add(v); out.append(v)
    return out


def find_label_for_uuid(target_uuid, root=None):
    """Reproduce analyze_sessions.py labeling: files sorted, first occurrence gets label."""
    if root is None:
        root = Path.home() / ".claude/projects"
    main_files = sorted(p for p in root.rglob("*.jsonl") if "/subagents/" not in str(p))
    sub_files = sorted(p for p in root.rglob("*.jsonl") if "/subagents/" in str(p))

    seen = OrderedDict()
    for p in main_files:
        if p.stem not in seen:
            seen[p.stem] = (f"s{len(seen)+1:04d}", p)
    # Subagents get a-labels but we also need them
    sub_counter = 0
    for p in sub_files:
        sub_counter += 1
        seen[f"_sub_{p.name}"] = (f"a{sub_counter:04d}", p)
    return seen


def build_label_to_file(seen):
    label_to_file = {}
    for key, (label, path) in seen.items():
        label_to_file[label] = path
    return label_to_file


def parse_ts(rec):
    ts = rec.get("timestamp")
    if not ts:
        return None
    try:
        import datetime as dt
        if ts.endswith("Z"):
            ts = ts[:-1] + "+00:00"
        return int(dt.datetime.fromisoformat(ts).timestamp() * 1000)
    except Exception:
        return None


def extract_loop_list_events_from_file(path, session_label, target_loop_ids):
    """
    Parse JSONL, find list_tool calls that belong to target_loop_ids,
    and extract gold position.
    Returns list of event dicts.

    target_loop_ids: set of loop_ids we care about (from loops.parquet).

    We must reproduce the loop_id logic from analyze_sessions.py:
      loop_id starts at 0, increments at each human turn.
    """
    try:
        lines = path.read_text(errors="ignore").splitlines()
    except Exception:
        return []

    events = []

    # Reproduce loop tracking + collect tool_use_id → list response items
    loop_id = 0
    cur_loop_turn_count = 0
    list_calls_pending = {}  # tool_use_id → {loop_id, items, ts}
    loop_gold_found = {}     # loop_id → True when we found first gold (to skip duplicates)

    for line in lines:
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        ts = parse_ts(rec)
        msg = rec.get("message", {})
        role = msg.get("role", "")
        content = msg.get("content")

        # Human turn → close loop, increment
        if role == "user":
            is_tool_result = False
            has_text = False
            if isinstance(content, list):
                for c in content:
                    if isinstance(c, dict):
                        if c.get("type") == "tool_result":
                            is_tool_result = True
                        elif c.get("type") == "text":
                            has_text = True
            elif isinstance(content, str):
                has_text = bool(content.strip())

            if has_text and not is_tool_result:
                if cur_loop_turn_count > 0:
                    loop_id += 1
                    cur_loop_turn_count = 0

            # Process tool_result → if matches a pending list call, extract items
            if is_tool_result and isinstance(content, list):
                for c in content:
                    if not isinstance(c, dict) or c.get("type") != "tool_result":
                        continue
                    tuid = c.get("tool_use_id", "")
                    if tuid not in list_calls_pending:
                        continue
                    result = c.get("content", "")
                    if isinstance(result, list):
                        result = "\n".join(
                            x.get("text", "") for x in result if isinstance(x, dict)
                        )
                    elif not isinstance(result, str):
                        result = json.dumps(result)
                    items = extract_ids(result)
                    list_calls_pending[tuid]["items"] = items
                    list_calls_pending[tuid]["response_chars"] = len(result)

        elif role == "assistant":
            cur_loop_turn_count += 1
            if not isinstance(content, list):
                continue
            for c in content:
                if not isinstance(c, dict):
                    continue
                if c.get("type") == "tool_use":
                    name = c.get("name", "") or ""
                    if LIST_VERB_RE.match(name):
                        # Record pending list call
                        list_calls_pending[c.get("id", "")] = {
                            "loop_id": loop_id,
                            "tool_name": name,
                            "ts_ms": ts,
                            "items": None,
                            "response_chars": 0,
                        }
                    elif ENRICH_VERB_RE.match(name):
                        # Gold detection: enrichment within same loop as earlier list
                        inp = c.get("input", {}) or {}
                        for tuid, info in list_calls_pending.items():
                            if info["loop_id"] != loop_id or info["items"] is None:
                                continue
                            # Skip if already recorded gold for this list call
                            event_key = (session_label, loop_id, tuid)
                            if event_key in loop_gold_found:
                                continue
                            # Try to find a referenced item id
                            for key in ("iid", "issue_iid", "mr_iid",
                                        "merge_request_iid", "id"):
                                v = str(inp.get(key, ""))
                                if v in info["items"]:
                                    pos = info["items"].index(v)
                                    events.append({
                                        "session": session_label,
                                        "loop_id": loop_id,
                                        "list_tool_verb": _strip_mcp(info["tool_name"]),
                                        "enrich_tool_verb": _strip_mcp(name),
                                        "list_response_size": len(info["items"]),
                                        "list_response_chars": info["response_chars"],
                                        "gold_position": pos,
                                        "gold_fraction": pos / max(len(info["items"]) - 1, 1),
                                        "gold_source": "enrichment",
                                        "lag_ms": (ts - info["ts_ms"]) if ts and info["ts_ms"] else 0,
                                    })
                                    loop_gold_found[event_key] = True
                                    break
                elif c.get("type") == "text":
                    # text mention of list items within same loop
                    text = c.get("text", "") or ""
                    for tuid, info in list_calls_pending.items():
                        if info["loop_id"] != loop_id or info["items"] is None:
                            continue
                        event_key = (session_label, loop_id, tuid)
                        if event_key in loop_gold_found:
                            continue
                        for pos, item in enumerate(info["items"]):
                            if f"#{item}" in text or f"!{item}" in text:
                                events.append({
                                    "session": session_label,
                                    "loop_id": loop_id,
                                    "list_tool_verb": _strip_mcp(info["tool_name"]),
                                    "enrich_tool_verb": "",
                                    "list_response_size": len(info["items"]),
                                    "list_response_chars": info["response_chars"],
                                    "gold_position": pos,
                                    "gold_fraction": pos / max(len(info["items"]) - 1, 1),
                                    "gold_source": "text_mention",
                                    "lag_ms": (ts - info["ts_ms"]) if ts and info["ts_ms"] else 0,
                                })
                                loop_gold_found[event_key] = True
                                break

    return events


def _strip_mcp(name):
    """Extract verb from mcp__project__verb."""
    if not name:
        return ""
    parts = name.split("__")
    return parts[-1] if len(parts) >= 3 else name


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in-dir", default="/tmp/claude_analysis")
    ap.add_argument("--out", default="/tmp/claude_analysis/loop_list_events.parquet")
    args = ap.parse_args()

    in_dir = Path(args.in_dir)
    con = duckdb.connect()
    con.execute(f"CREATE VIEW loops AS SELECT * FROM '{in_dir}/loops.parquet'")

    # Get sessions that have any mcp_list call
    sessions_with_list = con.execute("""
      SELECT DISTINCT session
      FROM loops
      WHERE (mcp_list_issues + mcp_list_mrs) > 0
    """).fetchall()
    target_sessions = {r[0] for r in sessions_with_list}
    print(f"# Sessions with mcp_list calls: {len(target_sessions)}", file=sys.stderr)

    seen = find_label_for_uuid(None)
    label_to_file = build_label_to_file(seen)

    all_events = []
    processed = 0
    for label in target_sessions:
        path = label_to_file.get(label)
        if path is None:
            continue
        events = extract_loop_list_events_from_file(path, label, set())
        all_events.extend(events)
        processed += 1
        if processed % 50 == 0:
            print(f"  {processed}/{len(target_sessions)} processed, "
                  f"{len(all_events)} events", file=sys.stderr)

    print(f"# Total events: {len(all_events)}", file=sys.stderr)

    if not all_events:
        print("No events found", file=sys.stderr)
        return

    schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("loop_id", pa.int32()),
        pa.field("list_tool_verb", pa.string()),
        pa.field("enrich_tool_verb", pa.string()),
        pa.field("list_response_size", pa.int32()),
        pa.field("list_response_chars", pa.int32()),
        pa.field("gold_position", pa.int32()),
        pa.field("gold_fraction", pa.float64()),
        pa.field("gold_source", pa.string()),
        pa.field("lag_ms", pa.int64()),
    ])
    pq.write_table(pa.Table.from_pylist(all_events, schema=schema), args.out,
                   compression="zstd")
    size = Path(args.out).stat().st_size
    print(f"# Wrote {len(all_events)} events, {size/1024:.0f} KB → {args.out}",
          file=sys.stderr)


if __name__ == "__main__":
    main()
