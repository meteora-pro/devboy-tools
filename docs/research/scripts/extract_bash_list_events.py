#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0"]
# ///
"""
Extract Bash-level gold-selection events.

Pattern: a Bash call of type `file_search` (grep/find/ls/rg/fd) produces
output with many candidate file paths. The agent's NEXT tool action within
the same loop references ONE specific file (the "gold"):
  - Read tool_use with file_path argument
  - Bash cat/head/tail/less with path argument

For each such event, record:
  - n_candidates: how many unique paths were in the search output
  - gold_position: index of the referenced path (0-based)
  - gold_fraction: gold_position / (n_candidates - 1)
  - gold_source: "read_tool" | "bash_view" | "text_mention"

Never stores raw paths — only counts and positions.

Output: /tmp/claude_analysis/bash_list_events.parquet
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


# ─── Path-like token extraction ───────────────────────────────────────────
# Match anything that looks like a filesystem path:
#   - contains /
#   - OR has a file extension (.ext)
# Cap length to avoid matching large blobs.

PATH_RE = re.compile(
    r"(?:^|(?<=\s)|(?<=:))"         # start-of-line / whitespace / colon (grep -n prefix)
    r"(\.{0,2}/?[\w.-]+(?:/[\w.-]+)+|"
    r"[\w.-]+\.[a-zA-Z][a-zA-Z0-9]{0,6})"
    r"(?=[:\s]|$)"
)
# Grep output: path:line:match — we want just the path before :digit
GREP_LINE_RE = re.compile(
    r"^([^\s:]+(?:/[^\s:]+)*(?:\.[a-zA-Z][a-zA-Z0-9]*)?):\d+:",
    re.MULTILINE,
)


def extract_paths(text):
    """
    Return list of unique path-like tokens in the order they appear in text.
    Never escapes this function — only used to compute counts/positions.
    """
    if not text:
        return []
    seen = set()
    out = []
    # First, try grep-style "path:line:content"
    for m in GREP_LINE_RE.finditer(text):
        p = m.group(1)
        if p and p not in seen:
            seen.add(p); out.append(p)
    # Then, general path-like tokens (covers ls, find, rg with different flags)
    for m in PATH_RE.finditer(text):
        p = m.group(1)
        # Skip tiny tokens or obvious non-paths
        if len(p) < 3 or p in ("./", "../", "..", "./ "):
            continue
        if p.startswith(("http://", "https://", "localhost")):
            continue
        # Deduplicate; keep first occurrence position
        if p not in seen:
            seen.add(p); out.append(p)
    return out


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


def find_path_in_candidates(target, candidates):
    """
    Find position (0-based index) of `target` in `candidates`.
    Match strategy:
      1. Exact full-path match
      2. Substring match (target endswith candidate or vice versa)
      3. Basename match
    Returns position or -1.
    """
    if not target:
        return -1
    # Exact
    if target in candidates:
        return candidates.index(target)
    target_base = target.rsplit("/", 1)[-1]
    for i, c in enumerate(candidates):
        # endswith / startswith
        if target.endswith(c) or c.endswith(target):
            return i
        c_base = c.rsplit("/", 1)[-1]
        if target_base and c_base == target_base:
            return i
    return -1


def extract_bash_target_from_command(cmd):
    """
    From a bash command like `cat /path/file.ts` or `head -20 file.py`,
    extract the file path argument.
    """
    if not cmd:
        return ""
    c = cmd.strip()
    # Strip common prefixes / chains — take first sub-command only
    first = c.split("&&", 1)[0].split(";", 1)[0].split("|", 1)[0].strip()
    # Viewer commands
    m = re.match(r"^(?:cat|head|tail|less|more|bat)\s+(?:-\w+\s+)*(\S+)", first)
    if m:
        return m.group(1).strip("\"'")
    return ""


def find_label_to_path(root=None):
    """Reproduce analyze_sessions.py label→file mapping."""
    if root is None:
        root = Path.home() / ".claude/projects"
    main_files = sorted(p for p in root.rglob("*.jsonl") if "/subagents/" not in str(p))
    sub_files = sorted(p for p in root.rglob("*.jsonl") if "/subagents/" in str(p))
    label_to_path = {}
    seen_main = OrderedDict()
    for p in main_files:
        if p.stem not in seen_main:
            seen_main[p.stem] = f"s{len(seen_main)+1:04d}"
    for uuid, label in seen_main.items():
        for p in main_files:
            if p.stem == uuid:
                label_to_path[label] = p
                break
    for i, p in enumerate(sub_files, 1):
        label_to_path[f"a{i:04d}"] = p
    return label_to_path


def extract_events_from_file(path, session_label):
    """
    Parse one JSONL, emit events: for each file_search Bash call,
    find next Read/Bash-viewer call in same loop and check gold position.
    """
    events = []
    try:
        lines = path.read_text(errors="ignore").splitlines()
    except Exception:
        return events

    # Track loop_id + pending file_search results
    loop_id = 0
    cur_loop_turn = 0
    # tool_use_id → dict with candidates, loop_id, ts, cmd_type
    search_pending = {}
    # Track which loops have already matched a gold (avoid double-count per search)
    matched = set()

    for line in lines:
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        ts = parse_ts(rec)
        msg = rec.get("message", {})
        role = msg.get("role", "")
        content = msg.get("content")

        # Human turn resets loop
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
                if cur_loop_turn > 0:
                    loop_id += 1
                    cur_loop_turn = 0

            # tool_result — extract candidates for matching pending searches
            if is_tool_result and isinstance(content, list):
                for c in content:
                    if not isinstance(c, dict) or c.get("type") != "tool_result":
                        continue
                    tuid = c.get("tool_use_id", "")
                    if tuid not in search_pending:
                        continue
                    result = c.get("content", "")
                    if isinstance(result, list):
                        result = "\n".join(
                            x.get("text", "") for x in result if isinstance(x, dict)
                        )
                    elif not isinstance(result, str):
                        result = json.dumps(result)
                    candidates = extract_paths(result)
                    # Store only COUNT locally — NOT the paths themselves (avoid leaks)
                    # But we need them for matching next tool. Keep scoped.
                    search_pending[tuid]["candidates"] = candidates

        elif role == "assistant":
            cur_loop_turn += 1
            if not isinstance(content, list):
                continue
            for b in content:
                if not isinstance(b, dict):
                    continue
                if b.get("type") != "tool_use":
                    continue
                name = b.get("name", "")
                inp = b.get("input", {}) or {}

                # 1. File-search Bash → add to pending
                if name == "Bash":
                    cmd = inp.get("command", "") or ""
                    is_file_search = bool(re.match(
                        r"^\s*(?:sudo\s+)?(grep|egrep|fgrep|rg|ripgrep|ag|fd|find|ls)\b", cmd))
                    if is_file_search:
                        tuid = b.get("id", "")
                        search_pending[tuid] = {
                            "loop_id": loop_id,
                            "ts_ms": ts,
                            "candidates": None,
                            "tool": "bash_search",
                        }
                        continue  # don't also match it as a gold consumer
                    # Check if this Bash is viewer (cat/head/tail) referencing a candidate
                    target = extract_bash_target_from_command(cmd)
                    if target:
                        for tuid, info in list(search_pending.items()):
                            if info["candidates"] is None:
                                continue
                            if info["loop_id"] != loop_id:
                                continue
                            if tuid in matched:
                                continue
                            pos = find_path_in_candidates(target, info["candidates"])
                            if pos >= 0:
                                n = len(info["candidates"])
                                events.append({
                                    "session": session_label,
                                    "loop_id": loop_id,
                                    "search_ts_ms": info["ts_ms"],
                                    "gold_ts_ms": ts,
                                    "n_candidates": n,
                                    "gold_position": pos,
                                    "gold_fraction": pos / max(n - 1, 1),
                                    "gold_source": "bash_view",
                                })
                                matched.add(tuid)
                # 2. Read tool → check if file_path matches pending search
                elif name == "Read":
                    target = inp.get("file_path", "") or ""
                    if target:
                        for tuid, info in list(search_pending.items()):
                            if info["candidates"] is None:
                                continue
                            if info["loop_id"] != loop_id:
                                continue
                            if tuid in matched:
                                continue
                            pos = find_path_in_candidates(target, info["candidates"])
                            if pos >= 0:
                                n = len(info["candidates"])
                                events.append({
                                    "session": session_label,
                                    "loop_id": loop_id,
                                    "search_ts_ms": info["ts_ms"],
                                    "gold_ts_ms": ts,
                                    "n_candidates": n,
                                    "gold_position": pos,
                                    "gold_fraction": pos / max(n - 1, 1),
                                    "gold_source": "read_tool",
                                })
                                matched.add(tuid)
                # 3. Edit → agent edited a file that was in search results
                elif name in ("Edit", "Write"):
                    target = inp.get("file_path", "") or ""
                    if target:
                        for tuid, info in list(search_pending.items()):
                            if info["candidates"] is None:
                                continue
                            if info["loop_id"] != loop_id:
                                continue
                            if tuid in matched:
                                continue
                            pos = find_path_in_candidates(target, info["candidates"])
                            if pos >= 0:
                                n = len(info["candidates"])
                                events.append({
                                    "session": session_label,
                                    "loop_id": loop_id,
                                    "search_ts_ms": info["ts_ms"],
                                    "gold_ts_ms": ts,
                                    "n_candidates": n,
                                    "gold_position": pos,
                                    "gold_fraction": pos / max(n - 1, 1),
                                    "gold_source": "edit_tool" if name == "Edit" else "write_tool",
                                })
                                matched.add(tuid)
    return events


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="/tmp/claude_analysis/bash_list_events.parquet")
    ap.add_argument("--limit", type=int, default=None, help="Process only N sessions (for debug)")
    args = ap.parse_args()

    con = duckdb.connect()
    con.execute("CREATE VIEW b AS SELECT * FROM '/tmp/claude_analysis/bash_commands.parquet'")
    # Find sessions that have file_search Bash calls
    sessions_with_search = con.execute("""
      SELECT DISTINCT session FROM b WHERE category = 'file_search'
    """).fetchall()
    target_sessions = {r[0] for r in sessions_with_search}
    print(f"# Sessions with file_search Bash calls: {len(target_sessions)}",
          file=sys.stderr)

    label_to_path = find_label_to_path()

    all_events = []
    processed = 0
    max_sessions = args.limit or len(target_sessions)
    for label in list(target_sessions)[:max_sessions]:
        path = label_to_path.get(label)
        if path is None:
            continue
        events = extract_events_from_file(path, label)
        all_events.extend(events)
        processed += 1
        if processed % 100 == 0:
            print(f"  {processed}/{max_sessions} processed, "
                  f"{len(all_events)} events", file=sys.stderr)

    print(f"# Total events: {len(all_events)}", file=sys.stderr)
    if not all_events:
        return

    schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("loop_id", pa.int32()),
        pa.field("search_ts_ms", pa.int64()),
        pa.field("gold_ts_ms", pa.int64()),
        pa.field("n_candidates", pa.int32()),
        pa.field("gold_position", pa.int32()),
        pa.field("gold_fraction", pa.float64()),
        pa.field("gold_source", pa.string()),
    ])
    pq.write_table(pa.Table.from_pylist(all_events, schema=schema),
                   args.out, compression="zstd")
    size = Path(args.out).stat().st_size
    print(f"# Wrote {len(all_events)} events, {size/1024:.0f} KB → {args.out}",
          file=sys.stderr)


if __name__ == "__main__":
    main()
