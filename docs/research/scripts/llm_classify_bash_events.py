#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0", "httpx[http2]>=0.27"]
# ///
"""
Classify all Bash gold-selection events into higher-level categories via LLM.

Uses z.ai coding-plan Anthropic endpoint with prompt caching.
Incremental checkpointing per 20 completions.

Output: bash_events_classified.parquet with:
  event_id, session, loop_id, n_candidates, gold_position, gold_fraction,
  category, use_case, priority_signal, fifo_would_work, notes
"""
import argparse
import asyncio
import json
import os
import re
import sys
import time
from collections import OrderedDict
from pathlib import Path

import duckdb
import httpx
import pyarrow as pa
import pyarrow.parquet as pq


SYSTEM_PROMPT = """You classify Claude Code agent sessions where a Bash search
command (grep/find/ls/rg) produced candidate file paths and the agent opened
ONE of them (the "gold"). For each event, output JSON with these fields:

{
  "category": one of ["research", "issue_tracking", "code", "config", "docs", "devops", "debug", "other"],
  "use_case": one of [
    "code_navigation",      // agent searching code to understand a specific entity/function
    "code_exploration",     // open-ended "understand this codebase area"
    "bugfix_code_hunt",     // searching for broken code, error sources
    "config_lookup",        // pick right config/package.json among many
    "docs_lookup",          // finding relevant docs file
    "test_file_nav",        // test files matching pattern
    "log_analysis",         // logs for errors/events
    "refactor_hunt",        // finding usages to rename
    "entity_find",          // specific entity file (e.g. *.entity.ts)
    "monorepo_disambig",    // pick right package.json from many
    "audit_scan",           // scanning all files of a kind (audit)
    "other"
  ],
  "priority_signal": string (brief: what signal could boost gold's priority? e.g. "keyword-match", "file-ext-prior", "recency", "depth"),
  "fifo_would_work": boolean (true if gold_position = 0)
}

RULES:
- Map use_case → category:
    research: code_exploration, code_navigation, audit_scan
    issue_tracking: entity_find, monorepo_disambig (when pointing to issues/entities)
    code: bugfix_code_hunt, refactor_hunt, test_file_nav
    config: config_lookup
    docs: docs_lookup
    devops: anything around k8s/terraform/ci files (infer from ext)
    debug: log_analysis, bugfix_code_hunt with clear error signal
- Output ONLY a JSON object. No preamble. No code fence.
"""

ID_PATH_RE = re.compile(r"(?:^|(?<=\s)|(?<=:))([/.\w-]+/[\w./-]+|[\w-]+\.[a-zA-Z][a-zA-Z0-9]{0,6})")


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


def parse_ts(rec):
    ts = rec.get("timestamp")
    if not ts: return None
    try:
        import datetime as dt
        if ts.endswith("Z"): ts = ts[:-1] + "+00:00"
        return int(dt.datetime.fromisoformat(ts).timestamp() * 1000)
    except: return None


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
    cur_loop = 0; cur_loop_turn = 0
    for rec in records:
        msg = rec.get("message", {})
        role = msg.get("role", "")
        content = msg.get("content")
        if role == "user":
            is_tr = False; full_text = ""
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


def enrich_event(event, jsonl_path):
    """Add search_cmd, gold_path, first_human."""
    event["search_cmd"] = ""
    event["gold_ext"] = ""
    event["first_human"] = ""
    if not jsonl_path or not jsonl_path.exists():
        return event
    try:
        records = []
        for line in jsonl_path.read_text(errors="ignore").splitlines():
            try: records.append(json.loads(line))
            except: continue
    except: return event

    search_cmd = ""; gold_path = ""
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
    event["gold_ext"] = gold_path.rsplit(".", 1)[-1][:10] if "." in gold_path else ""
    event["first_human"] = first_human_of_loop(records, event["loop_id"])
    return event


def build_user_prompt(ev):
    return (
        f"Event:\n"
        f"- n_candidates: {ev['n_candidates']}, gold_position: {ev['gold_position']} "
        f"(fraction {ev['gold_fraction']:.2f})\n"
        f"- gold ext: .{ev.get('gold_ext', '')}\n"
        f"- search cmd: `{ev.get('search_cmd', '')[:180]}`\n"
        f'- first human prompt:\n  """{(ev.get("first_human") or "")[:350]}"""\n\n'
        f"Classify (JSON only, matching the schema from system prompt):"
    )


async def classify_one(client, ev, url, api_key, model, sem, max_retries=3):
    user_prompt = build_user_prompt(ev)
    body = {
        "model": model, "max_tokens": 600,
        "system": [{"type": "text", "text": SYSTEM_PROMPT,
                    "cache_control": {"type": "ephemeral"}}],
        "messages": [{"role": "user", "content": user_prompt}],
        "temperature": 0.1,
    }
    async with sem:
        for attempt in range(max_retries):
            try:
                resp = await client.post(
                    url, json=body,
                    headers={"x-api-key": api_key,
                             "anthropic-version": "2023-06-01",
                             "Content-Type": "application/json"},
                    timeout=60.0,
                )
                if resp.status_code == 429 or resp.status_code >= 500:
                    await asyncio.sleep(2 ** attempt); continue
                resp.raise_for_status()
                data = resp.json()
                content = ""
                for blk in data.get("content", []):
                    if isinstance(blk, dict) and blk.get("type") == "text":
                        content = blk.get("text", ""); break
                usage = data.get("usage", {})
                return {
                    "event_id": ev["event_id"],
                    "raw": content,
                    "input_tokens": usage.get("input_tokens", 0),
                    "cached_tokens": usage.get("cache_read_input_tokens", 0),
                    "output_tokens": usage.get("output_tokens", 0),
                    "error": None,
                }
            except Exception as e:
                if attempt == max_retries - 1:
                    return {"event_id": ev["event_id"], "raw": "",
                            "input_tokens": 0, "cached_tokens": 0, "output_tokens": 0,
                            "error": str(e)[:150]}
                await asyncio.sleep(2 ** attempt)
    return {"event_id": ev["event_id"], "raw": "",
            "input_tokens": 0, "cached_tokens": 0, "output_tokens": 0,
            "error": "retries_exhausted"}


def parse_json_permissive(raw):
    s = (raw or "").strip()
    if s.startswith("```"):
        s = s.split("```", 2)[1]
        if s.startswith("json"): s = s[4:]
        s = s.rsplit("```", 1)[0].strip()
    try: return json.loads(s)
    except: return {}


SCHEMA = pa.schema([
    pa.field("event_id", pa.string()),
    pa.field("session", pa.string()),
    pa.field("loop_id", pa.int32()),
    pa.field("n_candidates", pa.int32()),
    pa.field("gold_position", pa.int32()),
    pa.field("gold_fraction", pa.float64()),
    pa.field("gold_source", pa.string()),
    pa.field("category", pa.string()),
    pa.field("use_case", pa.string()),
    pa.field("priority_signal", pa.string()),
    pa.field("fifo_would_work", pa.bool_()),
    pa.field("input_tokens", pa.int32()),
    pa.field("cached_tokens", pa.int32()),
    pa.field("output_tokens", pa.int32()),
    pa.field("error", pa.string()),
])


def flush(path, rows):
    p = Path(path)
    new_table = pa.Table.from_pylist(rows, schema=SCHEMA)
    if p.exists():
        existing = pq.read_table(p)
        combined = pa.concat_tables([existing, new_table])
    else:
        combined = new_table
    tmp = p.with_suffix(p.suffix + ".tmp")
    pq.write_table(combined, tmp, compression="zstd")
    tmp.replace(p)


def already_done(path):
    p = Path(path)
    if not p.exists(): return set()
    try:
        tbl = pq.read_table(p, columns=["event_id"])
        return set(tbl.column("event_id").to_pylist())
    except: return set()


def load_env(path):
    env = {}
    try:
        for line in Path(path).read_text().splitlines():
            line = line.strip()
            if line.startswith("#") or not line or "=" not in line: continue
            k, v = line.split("=", 1)
            env[k.strip()] = v.strip().strip('"').strip("'")
    except FileNotFoundError: pass
    return env


async def run(events, url, api_key, model, concurrency, checkpoint_path, every):
    sem = asyncio.Semaphore(concurrency)
    async with httpx.AsyncClient(http2=True) as client:
        tasks = [classify_one(client, ev, url, api_key, model, sem) for ev in events]
        ev_by_id = {e["event_id"]: e for e in events}
        done = 0
        pending = []
        for coro in asyncio.as_completed(tasks):
            r = await coro
            if not r: continue
            done += 1
            eid = r["event_id"]
            ev = ev_by_id[eid]
            parsed = parse_json_permissive(r["raw"])
            row = {
                "event_id": eid,
                "session": ev["session"],
                "loop_id": ev["loop_id"],
                "n_candidates": ev["n_candidates"],
                "gold_position": ev["gold_position"],
                "gold_fraction": ev["gold_fraction"],
                "gold_source": ev["gold_source"],
                "category": str(parsed.get("category", ""))[:30],
                "use_case": str(parsed.get("use_case", ""))[:40],
                "priority_signal": str(parsed.get("priority_signal", ""))[:100],
                "fifo_would_work": bool(parsed.get("fifo_would_work", False)),
                "input_tokens": r.get("input_tokens", 0),
                "cached_tokens": r.get("cached_tokens", 0),
                "output_tokens": r.get("output_tokens", 0),
                "error": r.get("error") or ("" if parsed else "parse_failed"),
            }
            pending.append(row)
            if done % 10 == 0:
                print(f"  [{done}/{len(events)}] cached={r['cached_tokens']}/{r['input_tokens']} out={r['output_tokens']}",
                      file=sys.stderr)
            if len(pending) >= every:
                flush(checkpoint_path, pending)
                print(f"  ✓ checkpoint: {done}", file=sys.stderr)
                pending = []
        if pending:
            flush(checkpoint_path, pending)
            print(f"  ✓ final: {done}", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp", default="/tmp/claude_analysis/bash_list_events.parquet")
    ap.add_argument("--out", default="/tmp/claude_analysis/bash_events_classified.parquet")
    ap.add_argument("--env-file", default="/tmp/claude_analysis/.env.zai")
    ap.add_argument("--limit", type=int, default=None)
    ap.add_argument("--concurrency", type=int, default=10)
    ap.add_argument("--checkpoint-every", type=int, default=25)
    ap.add_argument("--resume", action="store_true")
    args = ap.parse_args()

    env = load_env(args.env_file)
    api_key = env.get("ZAI_API_KEY") or os.environ.get("ZAI_API_KEY", "")
    base = env.get("ZAI_BASE_URL", "https://api.z.ai/api/anthropic")
    model = env.get("ZAI_MODEL", "glm-4.6")
    if not api_key:
        print("ERROR: ZAI_API_KEY missing", file=sys.stderr); sys.exit(1)
    url = base.rstrip("/") + "/v1/messages"
    print(f"# classify_bash_events model={model} concurrency={args.concurrency}",
          file=sys.stderr)

    con = duckdb.connect()
    rows = con.execute(f"""
      SELECT session, loop_id, search_ts_ms, gold_ts_ms,
        n_candidates, gold_position, gold_fraction, gold_source
      FROM '{args.inp}'
    """).fetchall()
    cols = [d[0] for d in con.description]
    events = []
    for i, r in enumerate(rows):
        e = dict(zip(cols, r))
        e["event_id"] = f"E{i+1:05d}"
        events.append(e)
    if args.limit:
        events = events[:args.limit]
    print(f"# {len(events)} events to classify", file=sys.stderr)

    if args.resume:
        done = already_done(args.out)
        before = len(events)
        events = [e for e in events if e["event_id"] not in done]
        print(f"# resume: skipped {before-len(events)}, remaining {len(events)}",
              file=sys.stderr)
    if not events: return

    # Enrich with context (search_cmd + gold_ext + first_human)
    print("# Enriching events with JSONL context …", file=sys.stderr)
    mapping = label_to_path_map()
    for i, ev in enumerate(events):
        enrich_event(ev, mapping.get(ev["session"]))
        if (i + 1) % 500 == 0:
            print(f"  enriched {i+1}/{len(events)}", file=sys.stderr)

    t0 = time.time()
    asyncio.run(run(events, url, api_key, model, args.concurrency,
                    args.out, args.checkpoint_every))
    print(f"# Done in {time.time()-t0:.1f}s", file=sys.stderr)

    total = already_done(args.out)
    print(f"# Checkpoint total: {len(total)} → {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
