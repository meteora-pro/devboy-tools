#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0"]
# ///
"""
Sample Bash gold-selection events with full context for review.

For each sampled event, re-parse JSONL to extract:
  - search command (grep/find/ls pattern — truncated, no project paths leak)
  - gold file extension (ext only — ".ts", ".py", etc.)
  - first human turn of the containing loop (up to 300 chars)

Output:
  1. bash_gold_cases.md — markdown review doc (ephemeral, /tmp)
  2. bash_gold_cases.parquet — structured sample for aggregation

Heuristic classification attempted based on:
  - search command tool (grep / find / rg / ls)
  - gold file extension → category
  - human intent signals
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


def parse_ts(rec):
    ts = rec.get("timestamp")
    if not ts: return None
    try:
        import datetime as dt
        if ts.endswith("Z"):
            ts = ts[:-1] + "+00:00"
        return int(dt.datetime.fromisoformat(ts).timestamp() * 1000)
    except Exception:
        return None


def label_to_path_map(root=None):
    if root is None:
        root = Path.home() / ".claude/projects"
    main_files = sorted(p for p in root.rglob("*.jsonl") if "/subagents/" not in str(p))
    sub_files = sorted(p for p in root.rglob("*.jsonl") if "/subagents/" in str(p))
    mapping = {}
    seen = OrderedDict()
    for p in main_files:
        if p.stem not in seen:
            seen[p.stem] = f"s{len(seen)+1:04d}"
    for uuid, label in seen.items():
        for p in main_files:
            if p.stem == uuid:
                mapping[label] = p; break
    for i, p in enumerate(sub_files, 1):
        mapping[f"a{i:04d}"] = p
    return mapping


_WRAPPERS = ("local-command-caveat", "command-message", "command-name",
             "command-args", "command-stdout", "command-stderr",
             "system-reminder", "user-prompt-submit-hook")


def _strip_wrappers(text):
    s = text
    while True:
        ss = s.lstrip()
        if not ss.startswith("<"):
            return s
        advanced = False
        for tag in _WRAPPERS:
            open_t = f"<{tag}>"; close_t = f"</{tag}>"
            if ss.startswith(open_t):
                end = ss.find(close_t)
                if end >= 0:
                    s = ss[end + len(close_t):]
                    advanced = True; break
            if ss.startswith(f"<{tag}"):
                end = ss.find(">")
                if end >= 0:
                    s = ss[end + 1:]
                    advanced = True; break
        if not advanced:
            return s


def first_human_of_loop(records, loop_id):
    """Extract first real human turn text belonging to `loop_id`."""
    cur_loop = 0
    cur_loop_turn = 0
    for rec in records:
        msg = rec.get("message", {})
        role = msg.get("role", "")
        content = msg.get("content")
        if role == "user":
            is_tool_result = False
            full_text = ""
            if isinstance(content, list):
                for c in content:
                    if isinstance(c, dict):
                        if c.get("type") == "tool_result":
                            is_tool_result = True
                        elif c.get("type") == "text":
                            full_text += c.get("text", "") or ""
            elif isinstance(content, str):
                full_text = content
            if full_text.strip() and not is_tool_result:
                if cur_loop_turn > 0:
                    cur_loop += 1
                    cur_loop_turn = 0
                if cur_loop == loop_id:
                    cleaned = _strip_wrappers(full_text).strip()
                    if (cleaned and not cleaned.startswith("<")
                        and not cleaned.startswith("Caveat:")
                        and not cleaned.startswith("[Request interrupted")):
                        return cleaned[:400]
        elif role == "assistant":
            cur_loop_turn += 1
    return ""


def file_ext_from_path(path):
    if not path: return ""
    base = path.rsplit("/", 1)[-1]
    if "." not in base: return ""
    return base.rsplit(".", 1)[-1].lower()[:10]


def classify_search_cmd(cmd):
    """Classify grep/find/ls/rg into subtype."""
    c = cmd.strip()
    c = re.sub(r"^(sudo|time|nohup)\s+", "", c)
    if re.match(r"^(grep|egrep|fgrep)\s+-[irlnw]?", c):
        return "grep"
    if re.match(r"^grep\b", c): return "grep"
    if re.match(r"^(rg|ripgrep)\b", c): return "rg"
    if re.match(r"^find\b", c): return "find"
    if re.match(r"^(ls|tree)\b", c): return "ls"
    if re.match(r"^(fd)\b", c): return "fd"
    if re.match(r"^(ag)\b", c): return "ag"
    return "other"


def classify_use_case(ext, search_cmd, first_human, n_candidates):
    """Heuristic classification of gold-selection use case."""
    ext_l = ext.lower() if ext else ""
    cmd_l = search_cmd.lower() if search_cmd else ""
    hum_l = (first_human or "").lower()

    # Heuristic rules
    if ext_l in ("log", ""):
        if "log" in hum_l or "error" in hum_l or "fail" in hum_l:
            return "log_analysis"
    if ext_l in ("md", "rst", "txt"):
        return "docs_lookup"
    if ext_l in ("yaml", "yml", "toml", "json", "env", "conf"):
        return "config_lookup"
    if ext_l in ("sql",):
        return "sql_or_migration"
    if ext_l in ("test.ts", "spec.ts", "test.py", "spec.rb"):
        return "test_search"
    if ext_l in ("sh", "bash", "zsh"):
        return "script_search"
    if ext_l in ("tf", "hcl", "yaml", "yml"):
        return "infra_search"
    if ext_l in ("ts", "tsx", "js", "jsx", "py", "rs", "go", "rb", "java"):
        if "test" in hum_l or "spec" in hum_l or "test" in cmd_l:
            return "test_in_code"
        if "error" in hum_l or "fix" in hum_l or "bug" in hum_l:
            return "bugfix_code_hunt"
        if "where" in hum_l or "what" in hum_l or "find" in hum_l or "how" in hum_l:
            return "code_navigation"
        if "refactor" in hum_l or "rename" in hum_l:
            return "refactor_hunt"
        return "code_exploration"
    return "other"


def sample_stratified(con, n_per_bucket=25):
    """Stratified sample of Bash gold events by n_candidates bucket."""
    buckets = [
        ("small", 3, 9),
        ("med", 10, 29),
        ("large", 30, 99),
        ("huge", 100, 99999),
    ]
    all_rows = []
    for bucket_name, lo, hi in buckets:
        rows = con.execute(f"""
          SELECT session, loop_id, search_ts_ms, gold_ts_ms,
            n_candidates, gold_position, gold_fraction, gold_source
          FROM bash_list_events
          WHERE n_candidates BETWEEN {lo} AND {hi}
          ORDER BY random() LIMIT {n_per_bucket}
        """).fetchall()
        cols = [d[0] for d in con.description]
        for r in rows:
            d = dict(zip(cols, r))
            d["bucket"] = bucket_name
            all_rows.append(d)
    return all_rows


def enrich_event(event, jsonl_path):
    """Add search_cmd, gold_ext, first_human from JSONL."""
    if not jsonl_path or not jsonl_path.exists():
        return event

    try:
        records = []
        for line in jsonl_path.read_text(errors="ignore").splitlines():
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    except Exception:
        return event

    search_cmd = ""
    gold_path = ""
    search_ts = event["search_ts_ms"]
    gold_ts = event["gold_ts_ms"]

    for rec in records:
        ts = parse_ts(rec)
        if ts is None: continue
        msg = rec.get("message", {})
        if msg.get("role") != "assistant":
            continue
        blocks = msg.get("content", [])
        if not isinstance(blocks, list): continue
        for b in blocks:
            if not isinstance(b, dict) or b.get("type") != "tool_use":
                continue
            # Match search ts (within 100ms tolerance)
            if abs(ts - search_ts) < 100 and b.get("name") == "Bash":
                inp = b.get("input", {}) or {}
                cmd = (inp.get("command", "") or "")[:200]
                search_cmd = cmd
            # Match gold ts
            if abs(ts - gold_ts) < 100:
                inp = b.get("input", {}) or {}
                fp = inp.get("file_path", "")
                if fp:
                    gold_path = fp
                elif b.get("name") == "Bash":
                    # Extract path from cat/head command
                    cmd = inp.get("command", "") or ""
                    m = re.match(r"^\s*(?:sudo\s+)?(?:cat|head|tail|less)\s+(?:-\w+\s+)*(\S+)", cmd)
                    if m:
                        gold_path = m.group(1).strip("\"'")

    event["search_cmd_snippet"] = search_cmd[:120]
    event["search_cmd_type"] = classify_search_cmd(search_cmd)
    event["gold_ext"] = file_ext_from_path(gold_path)
    event["gold_path_len"] = len(gold_path)  # no raw path stored
    event["first_human"] = first_human_of_loop(records, event["loop_id"])[:250]
    event["use_case"] = classify_use_case(
        event["gold_ext"],
        event["search_cmd_snippet"],
        event["first_human"],
        event["n_candidates"],
    )
    return event


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp",
                    default="/tmp/claude_analysis/bash_list_events.parquet")
    ap.add_argument("--n-per-bucket", type=int, default=25)
    ap.add_argument("--out-md", default="/tmp/bash_gold_cases.md")
    ap.add_argument("--out-parquet", default="/tmp/claude_analysis/bash_gold_samples.parquet")
    args = ap.parse_args()

    con = duckdb.connect()
    con.execute(f"CREATE VIEW bash_list_events AS SELECT * FROM '{args.inp}'")

    print(f"# Sampling {args.n_per_bucket} events per bucket …", file=sys.stderr)
    sample = sample_stratified(con, args.n_per_bucket)
    print(f"# Got {len(sample)} events", file=sys.stderr)

    mapping = label_to_path_map()

    enriched = []
    for i, ev in enumerate(sample):
        path = mapping.get(ev["session"])
        ev = enrich_event(ev, path)
        enriched.append(ev)
        if (i + 1) % 20 == 0:
            print(f"  {i+1}/{len(sample)} enriched", file=sys.stderr)

    # Write parquet (only safe structural fields)
    schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("loop_id", pa.int32()),
        pa.field("bucket", pa.string()),
        pa.field("n_candidates", pa.int32()),
        pa.field("gold_position", pa.int32()),
        pa.field("gold_fraction", pa.float64()),
        pa.field("gold_source", pa.string()),
        pa.field("search_cmd_type", pa.string()),
        pa.field("gold_ext", pa.string()),
        pa.field("gold_path_len", pa.int32()),
        pa.field("first_human_chars", pa.int32()),
        pa.field("use_case", pa.string()),
    ])
    structured = [{
        "session": e["session"], "loop_id": e["loop_id"], "bucket": e["bucket"],
        "n_candidates": e["n_candidates"], "gold_position": e["gold_position"],
        "gold_fraction": e["gold_fraction"], "gold_source": e["gold_source"],
        "search_cmd_type": e.get("search_cmd_type", ""),
        "gold_ext": e.get("gold_ext", ""),
        "gold_path_len": e.get("gold_path_len", 0),
        "first_human_chars": len(e.get("first_human", "") or ""),
        "use_case": e.get("use_case", ""),
    } for e in enriched]
    pq.write_table(pa.Table.from_pylist(structured, schema=schema),
                   args.out_parquet, compression="zstd")

    # Write markdown review
    lines = [f"# Bash gold-selection cases — review\n\n{len(enriched)} stratified samples\n"]
    for i, e in enumerate(enriched, 1):
        lines.append(f"\n---\n\n## [{i}/{len(enriched)}] `{e['session']}` loop#{e['loop_id']} [{e['bucket']}]\n")
        lines.append(f"- n_candidates: **{e['n_candidates']}** · gold_position: **{e['gold_position']}** "
                     f"(fraction **{e['gold_fraction']:.2f}**) · source: {e['gold_source']}\n")
        lines.append(f"- search cmd type: **{e.get('search_cmd_type', '?')}** · "
                     f"gold ext: `.{e.get('gold_ext', '')}`\n")
        lines.append(f"- heuristic use_case: **{e.get('use_case', '?')}**\n")
        if e.get("search_cmd_snippet"):
            lines.append(f"- search cmd (trunc): `{e['search_cmd_snippet']}`\n")
        if e.get("first_human"):
            q = "\n".join(f"  > {l}" for l in e['first_human'].split('\n'))
            lines.append(f"- first human prompt:\n{q}\n")

    Path(args.out_md).write_text("".join(lines))
    print(f"# Wrote review → {args.out_md}", file=sys.stderr)
    print(f"# Wrote parquet → {args.out_parquet}", file=sys.stderr)


if __name__ == "__main__":
    main()
