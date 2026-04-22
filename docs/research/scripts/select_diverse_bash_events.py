#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0"]
# ///
"""
Select diverse Bash gold-selection events for subagent analysis.

Multi-dimensional stratification:
  - n_candidates bucket (tiny/small/med/large/huge)
  - gold_fraction bucket (pos_0, top-20%, mid, tail)
  - gold_source (read_tool, bash_view, edit_tool, write_tool)

Output: 5 batches of ~60 events each, each batch as a markdown file
        ready to feed into a subagent for classification.
"""
import argparse
import json
import re
import sys
from collections import OrderedDict
from pathlib import Path

import duckdb


BATCH_INSTRUCTIONS = """# Bash gold-selection use-case classification — batch {batch_num} of {total_batches}

You are analyzing {n_events} real Claude Code agent sessions where a Bash
command (grep/find/ls/rg) produced a list of candidate file paths, and the
agent then opened ONE specific file from that list (the "gold").

For each event below, classify the **use case** into one of:

  1. `code_navigation`        — agent searching code to understand a specific entity/function
  2. `code_exploration`       — open-ended "understand this codebase area" task
  3. `bugfix_code_hunt`       — searching for broken code, error sources
  4. `config_lookup`          — picking right config/package.json among many
  5. `docs_lookup`            — finding relevant documentation file
  6. `test_file_nav`          — finding test files matching some pattern
  7. `log_analysis`           — searching logs for errors/events
  8. `refactor_hunt`          — finding usages to rename/restructure
  9. `entity_find`            — finding specific entity file (e.g. `*.entity.ts`)
 10. `monorepo_disambig`      — monorepo with many package.json/similar, need right one
 11. `audit_scan`             — scanning all files of a kind (listing all .tf, all providers)
 12. `other`                  — doesn't fit

Additionally, for each event note:
  - `gold_signal`: what could have boosted the correct file's priority score?
                   (keywords from prompt / path depth / file type / recency)
  - `fifo_would_work`: bool — true if gold_position = 0

Output JSON array, one object per event:
  [
    {{
      "event_id": <id from input>,
      "use_case": "<from list above>",
      "gold_signal": "<brief signal>",
      "fifo_would_work": <bool>,
      "notes": "<optional 1-line observation>"
    }},
    ...
  ]

Return ONLY the JSON array, no preamble.

---

## Events

"""


def parse_ts(rec):
    ts = rec.get("timestamp")
    if not ts: return None
    try:
        import datetime as dt
        if ts.endswith("Z"): ts = ts[:-1] + "+00:00"
        return int(dt.datetime.fromisoformat(ts).timestamp() * 1000)
    except: return None


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
        if not ss.startswith("<"): return s
        advanced = False
        for tag in _WRAPPERS:
            for open_t, is_full in [(f"<{tag}>", True), (f"<{tag}", False)]:
                if ss.startswith(open_t):
                    if is_full:
                        end = ss.find(f"</{tag}>")
                        if end >= 0:
                            s = ss[end + len(f"</{tag}>"):]
                            advanced = True; break
                    else:
                        end = ss.find(">")
                        if end >= 0:
                            s = ss[end + 1:]
                            advanced = True; break
            if advanced: break
        if not advanced: return s


def first_human_of_loop(records, loop_id):
    cur_loop = 0
    cur_loop_turn = 0
    for rec in records:
        msg = rec.get("message", {})
        role = msg.get("role", "")
        content = msg.get("content")
        if role == "user":
            is_tr = False
            full_text = ""
            if isinstance(content, list):
                for c in content:
                    if isinstance(c, dict):
                        if c.get("type") == "tool_result": is_tr = True
                        elif c.get("type") == "text": full_text += c.get("text", "") or ""
            elif isinstance(content, str):
                full_text = content
            if full_text.strip() and not is_tr:
                if cur_loop_turn > 0:
                    cur_loop += 1; cur_loop_turn = 0
                if cur_loop == loop_id:
                    cleaned = _strip_wrappers(full_text).strip()
                    if (cleaned and not cleaned.startswith("<")
                        and not cleaned.startswith("Caveat:")
                        and not cleaned.startswith("[Request interrupted")):
                        return cleaned[:350]
        elif role == "assistant":
            cur_loop_turn += 1
    return ""


def enrich(event, jsonl_path):
    if not jsonl_path or not jsonl_path.exists():
        return event
    try:
        records = []
        for line in jsonl_path.read_text(errors="ignore").splitlines():
            try: records.append(json.loads(line))
            except: continue
    except: return event

    search_cmd = ""
    gold_path = ""
    for rec in records:
        ts = parse_ts(rec)
        if ts is None: continue
        msg = rec.get("message", {})
        if msg.get("role") != "assistant": continue
        blocks = msg.get("content", [])
        if not isinstance(blocks, list): continue
        for b in blocks:
            if not isinstance(b, dict) or b.get("type") != "tool_use": continue
            if abs(ts - event["search_ts_ms"]) < 100 and b.get("name") == "Bash":
                inp = b.get("input", {}) or {}
                search_cmd = (inp.get("command", "") or "")[:180]
            if abs(ts - event["gold_ts_ms"]) < 100:
                inp = b.get("input", {}) or {}
                fp = inp.get("file_path", "")
                if fp: gold_path = fp
                elif b.get("name") == "Bash":
                    cmd = inp.get("command", "") or ""
                    m = re.match(r"^\s*(?:sudo\s+)?(?:cat|head|tail|less)\s+(?:-\w+\s+)*(\S+)", cmd)
                    if m: gold_path = m.group(1).strip("\"'")

    event["search_cmd"] = search_cmd
    event["gold_path_snippet"] = gold_path[-80:] if gold_path else ""  # last 80 chars
    event["gold_ext"] = gold_path.rsplit(".", 1)[-1][:10] if "." in gold_path else ""
    event["first_human"] = first_human_of_loop(records, event["loop_id"])
    return event


def select_diverse(con, n_per_group=6):
    """Select diverse events across multiple axes (stratification in Python)."""
    import random
    rows = con.execute("""
      SELECT session, loop_id, search_ts_ms, gold_ts_ms,
        n_candidates, gold_position, gold_fraction, gold_source
      FROM bash_list_events
    """).fetchall()
    cols = [d[0] for d in con.description]
    events = [dict(zip(cols, r)) for r in rows]

    def size_bucket(n):
        if n < 3: return "tiny"
        if n < 10: return "small"
        if n < 30: return "med"
        if n < 100: return "large"
        return "huge"

    def frac_bucket(f):
        if f is None: return "unk"
        if f < 0.01: return "pos0"
        if f < 0.2: return "top20"
        if f < 0.6: return "mid"
        return "tail"

    groups = {}
    for e in events:
        e["size_bucket"] = size_bucket(e["n_candidates"])
        e["frac_bucket"] = frac_bucket(e["gold_fraction"])
        key = (e["size_bucket"], e["frac_bucket"], e["gold_source"])
        groups.setdefault(key, []).append(e)

    random.seed(42)
    picked = []
    for key, group in groups.items():
        random.shuffle(group)
        picked.extend(group[:n_per_group])
    random.shuffle(picked)
    return picked


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp",
                    default="/tmp/claude_analysis/bash_list_events.parquet")
    ap.add_argument("--n-per-group", type=int, default=6)
    ap.add_argument("--batches", type=int, default=5)
    ap.add_argument("--out-dir", default="/tmp/bash_batches")
    args = ap.parse_args()

    con = duckdb.connect()
    con.execute(f"CREATE VIEW bash_list_events AS SELECT * FROM '{args.inp}'")

    events = select_diverse(con, args.n_per_group)
    print(f"# Selected {len(events)} diverse events", file=sys.stderr)

    mapping = label_to_path_map()
    for i, ev in enumerate(events):
        ev["event_id"] = f"E{i+1:04d}"
        path = mapping.get(ev["session"])
        enrich(ev, path)
        if (i + 1) % 30 == 0:
            print(f"  {i+1}/{len(events)} enriched", file=sys.stderr)

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    # Split into batches
    batch_size = (len(events) + args.batches - 1) // args.batches
    for b in range(args.batches):
        batch_events = events[b * batch_size:(b + 1) * batch_size]
        if not batch_events: continue
        lines = [BATCH_INSTRUCTIONS.format(
            batch_num=b + 1, total_batches=args.batches,
            n_events=len(batch_events))]
        for ev in batch_events:
            lines.append(f"\n### {ev['event_id']}  [{ev['size_bucket']}/"
                         f"frac={ev['frac_bucket']}/{ev['gold_source']}]\n")
            lines.append(f"- n_candidates: **{ev['n_candidates']}**, "
                         f"gold_position: **{ev['gold_position']}** "
                         f"(fraction {ev['gold_fraction']:.2f})\n")
            lines.append(f"- gold ext: `.{ev.get('gold_ext', '')}`  "
                         f"· source: {ev['gold_source']}\n")
            if ev.get('search_cmd'):
                lines.append(f"- search cmd (trunc): `{ev['search_cmd']}`\n")
            if ev.get('first_human'):
                quote = "\n".join(f"  > {l}" for l in ev['first_human'].split("\n"))
                lines.append(f"- first human prompt:\n{quote}\n")
        out_path = out_dir / f"batch_{b+1:02d}.md"
        out_path.write_text("".join(lines))
        print(f"  Wrote {out_path} — {len(batch_events)} events", file=sys.stderr)


if __name__ == "__main__":
    main()
