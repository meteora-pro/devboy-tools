#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0", "httpx[http2]>=0.27", "anyio>=4.0"]
# ///
"""
LLM session classification pipeline.

Classifies a stratified sample of sessions via GLM-4.5 (z.ai) using a
KV-cache-optimized prompt structure:

  SYSTEM (cached): task schema + rubric + few-shot examples (~3k tokens)
  USER (per-call): session's algo features + first human turn (~500-800 tokens)

Features:
  - Stratified sampling across size_bucket × went_wrong × bug_fix × provider
  - Concurrency 10 with retry/backoff
  - Output: llm_classifications.parquet (NEVER commits content; only enum labels)

Usage:
  uv run docs/research/scripts/llm_classify_sessions.py \\
      --sample-size 50 \\
      --env-file /tmp/claude_analysis/.env.zai \\
      --out /tmp/claude_analysis/llm_classifications.parquet
"""
import argparse
import asyncio
import json
import os
import random
import sys
import time
from pathlib import Path

import duckdb
import httpx
import pyarrow as pa
import pyarrow.parquet as pq


# ─── SYSTEM PROMPT — stable, KV-cache-friendly ─────────────────────────────
SYSTEM_PROMPT = """You are an expert analyst classifying Claude Code coding agent sessions.

You will receive:
1. Structural metadata (algo-computed features) about a single session
2. The first human prompt (up to 500 chars, actual user text)
3. A summary of tool usage patterns

Your task: output strict JSON matching this schema.

SCHEMA:
{
  "task_type": one of [bug_fix, feature, refactor, research, config, docs, ops, testing, planning, chat],
  "primary_domain": one of [frontend, backend, devops, data, ml, docs, infra, mobile, research, mixed],
  "task_complexity": one of [trivial, simple, medium, hard, extreme],
  "prompt_specificity": one of [vague, medium, specific, very_specific],
  "task_outcome_signal": one of [converged, still_exploring, abandoned, unclear],
  "conversation_mode": one of [instruction, exploration, chat, debugging, planning, mixed],
  "requires_external_data": boolean,
  "multi_session_work": boolean,
  "intent_tags": list of up to 5 short tags (e.g. ["fix-bug", "add-test", "read-docs"]),
  "primary_pattern": one of [
    "L-BROWSE", "L-FIND", "L-ENRICH", "L-EXHAUST", "L-FILTER",
    "E-COLD-START", "E-VERIFY", "E-LOOP", "E-SKIP",
    "S-READ-GRIND", "S-COMPACT-CYCLE", "S-QUERY-REPEAT", "S-DELEGATE", "S-FAIL-RETRY",
    "NONE"
  ]
}

RUBRIC:
- task_type: what kind of work overall?
    bug_fix=fixing specific broken behavior (errors, failed tests, unexpected behavior)
    feature=new capability/code
    refactor=restructure without behavior change
    research=EXPLORING, finding info, analyzing options (including "get/find/retrieve X" requests)
    config=settings/deploy config changes
    docs=documentation
    ops=infra/deployment/cluster/tools setup
    testing=writing/running tests as primary focus
    planning=outlining approach without code
    chat=ONLY informal discussion/jokes/small-talk. NEVER use chat for task requests — even short ones like "get the transcript" are research/ops, NOT chat.
- IMPORTANT: any task directive ("получи X", "найди Y", "сделай Z") is research/ops/feature, NEVER chat.
- task_complexity: trivial (1-2 file touches, obvious), simple (single small feature/fix), medium (multi-file, clear scope), hard (cross-cutting changes, multiple unknowns), extreme (architectural, deep research needed)
- prompt_specificity: vague (no concrete target, e.g. "help me with X"), medium (general direction like "how do I Y"), specific (concrete target/action, even if short like "найди Y в логах"), very_specific (fully-specified with file paths, error messages, exact steps)
- task_outcome_signal: converged (looks finished based on MR created / tests green / summary), still_exploring (agent still investigating), abandoned (user gave up or session ended unresolved), unclear
- multi_session_work: true if this work continues prior sessions (issue refs, "continue X", git context)
- primary_pattern: the DOMINANT workflow pattern. Pick NONE if no pattern STRONGLY matches — do NOT guess.
    IMPORTANT: `has_tool_loop=true` or `longest_same_tool_run≥5` in features means "same tool category
    repeated many times" — this ALONE does NOT indicate S-QUERY-REPEAT. To match a tool-loop pattern,
    pick the MOST LIKELY variant:

    - S-READ-GRIND: if top_tools shows Read/Glob/Grep ≥ 20 calls (agent reading many files)
    - S-FAIL-RETRY: if api_errors≥3 AND the loop tool is Bash (retrying failed commands)
    - S-QUERY-REPEAT: ONLY if you see signal of the SAME query being repeated — typically means
      agent is confused (cold evidence: e.g. tool_calls >> unique patterns; usually only obvious
      with huge bash_count and low tool_diversity)
    - S-DELEGATE: subagents≥3 visible in features
    - S-COMPACT-CYCLE: compactions>0 AND session continues after
    - L-BROWSE/L-FIND/L-ENRICH/L-EXHAUST/L-FILTER: ONLY if mcp_list:* is in top_tools
    - E-COLD-START/E-VERIFY/E-LOOP: ONLY if mcp_enrich:* is in top_tools

    Default to NONE when evidence is weak. Do not pick S-QUERY-REPEAT just because has_tool_loop=true.

PATTERN DEFINITIONS:
- L-BROWSE: agent lists items then summarizes/overviews everything
- L-FIND: agent lists items and picks one specific target
- L-ENRICH: list then deep-dive on ONE item
- L-EXHAUST: list then iterate through many items
- L-FILTER: list then re-list with different filter
- E-COLD-START: thin list response triggers immediate enrichment calls
- E-VERIFY: single enrichment to verify a specific fact
- E-LOOP: re-queries same item multiple times (stuck)
- E-SKIP: list sufficient, no enrichment
- S-READ-GRIND: many Read/Glob calls accumulating code context
- S-COMPACT-CYCLE: compaction followed by continued work
- S-QUERY-REPEAT: same tool with same input called 3+ times
- S-DELEGATE: 5+ subagents spawned for parallel exploration
- S-FAIL-RETRY: repeated errors with fix-retry cycles

FEW-SHOT EXAMPLES:

Example 1 input:
{
  "features": {"duration_h": 0.5, "agent_turns": 12, "tool_calls": 34, "bash_pct": 0.55, "has_tool_loop": true, "api_errors": 3, "cache_hit": 0.92, "cost_usd": 2.30, "compactions": 0},
  "first_prompt": "fix failing tests in auth module, they fail with 401 after recent token refresh changes",
  "tool_summary": "Bash(18) Read(8) Edit(4) Grep(4). 3 failed tests re-run. 2 commits, 1 push."
}
Example 1 output:
{"task_type":"bug_fix","primary_domain":"backend","task_complexity":"simple","prompt_specificity":"specific","task_outcome_signal":"converged","conversation_mode":"debugging","requires_external_data":false,"multi_session_work":false,"intent_tags":["fix-tests","token-refresh","auth"],"primary_pattern":"S-FAIL-RETRY"}

Example 2 input:
{
  "features": {"duration_h": 4.2, "agent_turns": 87, "tool_calls": 342, "bash_pct": 0.40, "subagents": 12, "compactions": 3, "mr_create": 2, "issue_create": 0, "resume_count": 2},
  "first_prompt": "refactor OAuth integration to split provider-specific logic into separate components, keep behavior identical, add characterization tests first",
  "tool_summary": "Bash(135) Read(54) Edit(62) Task(12 subagents) mcp:create_merge_request(2)"
}
Example 2 output:
{"task_type":"refactor","primary_domain":"backend","task_complexity":"hard","prompt_specificity":"very_specific","task_outcome_signal":"converged","conversation_mode":"instruction","requires_external_data":false,"multi_session_work":true,"intent_tags":["refactor","oauth","test-first","componentize"],"primary_pattern":"S-DELEGATE"}

Example 3 input:
{
  "features": {"duration_h": 0.1, "agent_turns": 4, "tool_calls": 6, "bash_pct": 0.2, "has_tool_loop": false, "cost_usd": 0.08},
  "first_prompt": "что такое async/await в js?",
  "tool_summary": "WebSearch(2) Read(1)"
}
Example 3 output:
{"task_type":"chat","primary_domain":"mixed","task_complexity":"trivial","prompt_specificity":"medium","task_outcome_signal":"converged","conversation_mode":"exploration","requires_external_data":true,"multi_session_work":false,"intent_tags":["learn","async","javascript"],"primary_pattern":"NONE"}

RULES:
- Respond with ONLY valid JSON, no preamble or trailing text
- Never invent facts — base labels strictly on provided input
- If insufficient evidence for any field, use 'unclear' or empty list
- Be decisive — pick the MOST representative primary_pattern, or NONE
"""


def load_env(path):
    env = {}
    try:
        for line in Path(path).read_text().splitlines():
            line = line.strip()
            if line.startswith("#") or not line or "=" not in line:
                continue
            k, v = line.split("=", 1)
            env[k.strip()] = v.strip().strip('"').strip("'")
    except FileNotFoundError:
        pass
    return env


def select_stratified_sample(con, n_total, include_subagents=False):
    """Pick n_total sessions stratified across size_bucket × went_wrong × bug_fix."""
    sub_filter = "" if include_subagents else "AND NOT is_subagent"
    query = f"""
      WITH dedup AS (
        SELECT *,
          ROW_NUMBER() OVER (PARTITION BY session ORDER BY total_records DESC) AS rn
        FROM sessions_enriched
        WHERE first_human_turn_chars > 10 AND tool_calls_total >= 5 {sub_filter}
      )
      SELECT session, size_bucket, signal_went_wrong, is_bug_fix_session,
        provider, top_model, cost_usd_total,
        duration_active_sec/3600 AS duration_h,
        agent_turns, tool_calls_total,
        cache_hit_rate, compaction_count,
        api_error_count, tool_error_count,
        interruption_mid_loop_count, resume_with_prompt_count,
        subagents_in_tree, mcp_fraction, bash_fraction,
        has_tool_loop, first_human_turn_chars,
        tool_diversity, longest_same_tool_run,
        uses_planning, correction_rate,
        top_tool_categories
      FROM dedup WHERE rn = 1
    """
    rows = con.execute(query).fetchall()
    cols = [d[0] for d in con.description]
    sessions = [dict(zip(cols, r)) for r in rows]

    # Stratify: 5 per (size_bucket × went_wrong), random within
    strata = {}
    for s in sessions:
        key = (s["size_bucket"], bool(s["signal_went_wrong"]))
        strata.setdefault(key, []).append(s)

    random.seed(42)
    # If n_total is huge, return all
    if n_total >= len(sessions):
        return sessions
    picked = []
    per_stratum = max(2, n_total // max(len(strata), 1))
    for key, group in strata.items():
        random.shuffle(group)
        picked.extend(group[:per_stratum])
    random.shuffle(picked)
    return picked[:n_total]


def fetch_first_human_turn_text(con, session_uuid_to_label):
    """
    Fetch first_human_turn text from the original JSONL files.
    Returns dict: session_label → text (up to 500 chars, never stored in parquet).

    NOTE: We need to parse original JSONL since text is not in parquet (by design).
    """
    # session_uuid_to_label maps original UUID → session label
    # For each session we need to find its JSONL and extract first human turn text
    # For now: attempt to find by label → file
    # This requires us to know the label-to-UUID mapping from analyze_sessions.py
    # We approximate by reading the first human turn when we parse the metadata

    # Simpler approach: extract from the parquet's first_human_turn_chars as a stand-in
    # and note we're using it as a feature only (char count)
    # Actual text pulled at classify time from JSONL

    # This function is a stub — real text fetch happens in `build_user_prompt`
    return {}


def find_jsonl_for_session_label(session_label, root=None):
    """
    Given a session label (s0001..sNNNN or a0001..aNNNN), find the JSONL file.
    Requires the `main_labels` mapping which we don't persist — so we
    recompute by listing main files in the expected order.
    """
    if root is None:
        root = Path.home() / ".claude/projects"
    # Recompute label → UUID (same ordering as analyze_sessions.py)
    main_files = sorted(p for p in root.rglob("*.jsonl") if "/subagents/" not in str(p))
    # Assign labels in order
    seen = {}
    for i, p in enumerate(main_files):
        uuid = p.stem
        if uuid not in seen:
            seen[uuid] = f"s{len(seen)+1:04d}"
    target_uuid = None
    for uuid, label in seen.items():
        if label == session_label:
            target_uuid = uuid
            break
    if not target_uuid:
        return None
    # Find the file
    for p in main_files:
        if p.stem == target_uuid:
            return p
    return None


_SYSTEM_WRAPPER_PREFIXES = (
    "<local-command-caveat",
    "<command-message>",
    "<command-name>",
    "<command-args>",
    "<command-stdout>",
    "<command-stderr>",
    "<system-reminder>",
    "<user-prompt-submit-hook>",
    "Caveat:",
    "[Request interrupted",
    "[Interrupted]",
    "[Meta]",
)


def _is_system_wrapped(text):
    s = text.strip()
    if not s:
        return True
    for p in _SYSTEM_WRAPPER_PREFIXES:
        if s.startswith(p):
            return True
    return False


def _strip_system_wrappers(text):
    """Remove known system-wrapping tags/blocks from the start."""
    s = text
    changed = True
    while changed:
        changed = False
        s_stripped = s.lstrip()
        # Strip full-line XML-ish tags: <tag>...</tag>\n
        for tag in ("local-command-caveat", "command-message", "command-name",
                    "command-args", "command-stdout", "command-stderr",
                    "system-reminder", "user-prompt-submit-hook"):
            # Full block <tag>...</tag>
            open_tag = f"<{tag}>"
            close_tag = f"</{tag}>"
            if s_stripped.startswith(open_tag):
                end = s_stripped.find(close_tag)
                if end >= 0:
                    s = s_stripped[end + len(close_tag):]
                    changed = True
                    break
            # Also self-closing style <tag ...>
            if s_stripped.startswith(f"<{tag}"):
                end = s_stripped.find(">")
                if 0 <= end:
                    s = s_stripped[end + 1:]
                    changed = True
                    break
    return s


def extract_first_human_text(jsonl_path, max_chars=500):
    """Extract first REAL human text from a JSONL session (skip system wrappers)."""
    if jsonl_path is None or not jsonl_path.exists():
        return ""
    try:
        for line in jsonl_path.read_text(errors="ignore").splitlines():
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            msg = rec.get("message", {})
            if msg.get("role") != "user":
                continue
            content = msg.get("content")
            if isinstance(content, list):
                has_tool_result = any(
                    isinstance(c, dict) and c.get("type") == "tool_result"
                    for c in content
                )
                if has_tool_result: continue
                texts = [c.get("text", "") for c in content
                         if isinstance(c, dict) and c.get("type") == "text"]
                combined = "\n".join(texts)
            elif isinstance(content, str):
                combined = content
            else:
                continue
            if not combined.strip():
                continue
            # Strip system wrappers
            cleaned = _strip_system_wrappers(combined).strip()
            # If AFTER stripping still starts with a wrapper (nested), or is empty, skip
            if not cleaned or _is_system_wrapped(cleaned):
                continue
            return cleaned[:max_chars]
    except Exception:
        pass
    return ""


def build_user_prompt(session_data, first_human_text):
    """Build the per-session user prompt (minimal, KV-cache efficient)."""
    feats = {
        "duration_h": round(float(session_data.get("duration_h") or 0), 2),
        "agent_turns": session_data.get("agent_turns"),
        "tool_calls": session_data.get("tool_calls_total"),
        "bash_pct": round(float(session_data.get("bash_fraction") or 0), 2),
        "mcp_pct": round(float(session_data.get("mcp_fraction") or 0), 2),
        "cache_hit": round(float(session_data.get("cache_hit_rate") or 0), 2),
        "compactions": session_data.get("compaction_count"),
        "subagents": session_data.get("subagents_in_tree"),
        "api_errors": session_data.get("api_error_count"),
        "tool_errors": session_data.get("tool_error_count"),
        "interrupts": session_data.get("interruption_mid_loop_count"),
        "resumes": session_data.get("resume_with_prompt_count"),
        "has_tool_loop": bool(session_data.get("has_tool_loop")),
        "uses_planning": bool(session_data.get("uses_planning")),
        "cost_usd": round(float(session_data.get("cost_usd_total") or 0), 2),
        "longest_same_tool_run": session_data.get("longest_same_tool_run"),
        "tool_diversity": session_data.get("tool_diversity"),
        "top_model": session_data.get("top_model", ""),
    }
    tool_summary = session_data.get("top_tool_categories", "")[:200]
    first_prompt = (first_human_text or "").strip()[:500]
    return (
        f"Session features:\n{json.dumps(feats, indent=2)}\n\n"
        f"Top tool categories (pipe-separated):\n{tool_summary}\n\n"
        f'First human prompt (raw, up to 500 chars):\n"""\n{first_prompt}\n"""\n\n'
        f"Classify (JSON only):"
    )


async def classify_one(client, session_data, first_human_text,
                       url, api_key, model, semaphore, max_retries=3):
    """
    Call z.ai coding-plan Anthropic-compatible endpoint.

    Uses `cache_control: {"type": "ephemeral"}` on system prompt to enable
    KV-cache on the stable prefix. Per-request user content is unique.
    """
    user_prompt = build_user_prompt(session_data, first_human_text)
    # Anthropic Messages format: system is a top-level field (not in messages);
    # wrap as list-with-cache_control for prompt caching.
    body = {
        "model": model,
        "max_tokens": 1500,
        "system": [
            {
                "type": "text",
                "text": SYSTEM_PROMPT,
                "cache_control": {"type": "ephemeral"},
            }
        ],
        "messages": [
            {"role": "user", "content": user_prompt},
        ],
        "temperature": 0.1,
    }
    async with semaphore:
        last_err = "rate_limited_or_5xx_after_all_retries"
        for attempt in range(max_retries):
            try:
                t0 = time.time()
                resp = await client.post(
                    url, json=body,
                    headers={"x-api-key": api_key,
                             "anthropic-version": "2023-06-01",
                             "Content-Type": "application/json"},
                    timeout=60.0,
                )
                latency_ms = int((time.time() - t0) * 1000)
                if resp.status_code == 429 or resp.status_code >= 500:
                    last_err = f"http_{resp.status_code}"
                    await asyncio.sleep(2 ** attempt)
                    continue
                resp.raise_for_status()
                data = resp.json()
                # Anthropic format: content is a list of blocks; take first text.
                content = ""
                for block in data.get("content", []):
                    if isinstance(block, dict) and block.get("type") == "text":
                        content = block.get("text", "")
                        break
                usage = data.get("usage", {})
                return {
                    "session": session_data["session"],
                    "raw_content": content,
                    "input_tokens": usage.get("input_tokens", 0),
                    "output_tokens": usage.get("output_tokens", 0),
                    "cached_tokens": usage.get("cache_read_input_tokens", 0),
                    "cache_creation_tokens": usage.get("cache_creation_input_tokens", 0),
                    "latency_ms": latency_ms,
                    "error": None,
                }
            except Exception as e:
                last_err = str(e)[:200]
                if attempt == max_retries - 1:
                    return {
                        "session": session_data["session"],
                        "raw_content": "",
                        "input_tokens": 0, "output_tokens": 0,
                        "cached_tokens": 0, "cache_creation_tokens": 0,
                        "latency_ms": 0,
                        "error": last_err,
                    }
                await asyncio.sleep(2 ** attempt)
        return {
            "session": session_data["session"],
            "raw_content": "",
            "input_tokens": 0, "output_tokens": 0,
            "cached_tokens": 0, "cache_creation_tokens": 0,
            "latency_ms": 0,
            "error": last_err,
        }


def parse_llm_json(raw):
    """Parse the LLM's JSON response, permissively."""
    if not raw:
        return {}
    # Strip code fences if present
    s = raw.strip()
    if s.startswith("```"):
        s = s.split("```", 2)[1]
        if s.startswith("json"): s = s[4:]
        s = s.rsplit("```", 1)[0].strip()
    try:
        return json.loads(s)
    except json.JSONDecodeError:
        return {}


async def run_classification(sample, sample_by_session, client, url, api_key,
                             model, concurrency, verbose, checkpoint_path,
                             checkpoint_every):
    """
    Run classification with incremental checkpointing.
    Writes completed rows to checkpoint_path every `checkpoint_every` completions.
    """
    sem = asyncio.Semaphore(concurrency)
    tasks = []
    for sd in sample:
        jsonl = find_jsonl_for_session_label(sd["session"])
        first_human = extract_first_human_text(jsonl, max_chars=500)
        tasks.append(classify_one(client, sd, first_human, url, api_key, model, sem))

    done = 0
    results = []
    pending_rows = []  # buffered rows awaiting flush

    for coro in asyncio.as_completed(tasks):
        r = await coro
        if not r or not isinstance(r, dict) or not r.get("session"):
            # Defensive: classify_one should always return a dict, but be safe
            done += 1
            print(f"  [{done}/{len(sample)}] WARN: got {type(r).__name__} result, skipping",
                  file=sys.stderr)
            continue
        results.append(r)
        done += 1

        # Build the row immediately — don't wait for batch end
        sd = sample_by_session[r["session"]]
        parsed = parse_llm_json(r.get("raw_content", ""))
        parse_err = "parse_failed" if (r.get("raw_content") and not parsed) else ""
        intent_tags = parsed.get("intent_tags", [])
        intent_tags_str = ",".join(str(t)[:40] for t in intent_tags[:5]) \
            if isinstance(intent_tags, list) else ""

        row = {
            "session": sd["session"],
            "size_bucket": sd["size_bucket"],
            "signal_went_wrong": bool(sd["signal_went_wrong"]),
            "is_bug_fix_session": bool(sd["is_bug_fix_session"]),
            "top_model": sd.get("top_model", ""),
            "task_type": str(parsed.get("task_type", "")),
            "primary_domain": str(parsed.get("primary_domain", "")),
            "task_complexity": str(parsed.get("task_complexity", "")),
            "prompt_specificity": str(parsed.get("prompt_specificity", "")),
            "task_outcome_signal": str(parsed.get("task_outcome_signal", "")),
            "conversation_mode": str(parsed.get("conversation_mode", "")),
            "requires_external_data": bool(parsed.get("requires_external_data", False)),
            "multi_session_work": bool(parsed.get("multi_session_work", False)),
            "intent_tags": intent_tags_str,
            "primary_pattern": str(parsed.get("primary_pattern", "")),
            "input_tokens": r.get("input_tokens", 0),
            "cached_tokens": r.get("cached_tokens", 0),
            "output_tokens": r.get("output_tokens", 0),
            "latency_ms": r.get("latency_ms", 0),
            "error": r.get("error") or parse_err,
        }
        pending_rows.append(row)

        if verbose or done % 5 == 0:
            e = r.get("error")
            tag = f" ERROR:{e}" if e else f" cached={r['cached_tokens']}/{r['input_tokens']} out={r['output_tokens']}"
            print(f"  [{done}/{len(sample)}] {r['session']}{tag}", file=sys.stderr)

        # Incremental flush
        if len(pending_rows) >= checkpoint_every:
            flush_checkpoint(checkpoint_path, pending_rows)
            print(f"  ✓ checkpoint: {done} total classifications saved",
                  file=sys.stderr)
            pending_rows = []

    # Final flush for remaining
    if pending_rows:
        flush_checkpoint(checkpoint_path, pending_rows)
        print(f"  ✓ final checkpoint: {done} total saved", file=sys.stderr)

    return results


def flush_checkpoint(path, new_rows):
    """
    Append new_rows to an existing parquet file (or create new).
    Uses read-existing + merge + write-atomic pattern for safety.
    """
    schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("size_bucket", pa.string()),
        pa.field("signal_went_wrong", pa.bool_()),
        pa.field("is_bug_fix_session", pa.bool_()),
        pa.field("top_model", pa.string()),
        pa.field("task_type", pa.string()),
        pa.field("primary_domain", pa.string()),
        pa.field("task_complexity", pa.string()),
        pa.field("prompt_specificity", pa.string()),
        pa.field("task_outcome_signal", pa.string()),
        pa.field("conversation_mode", pa.string()),
        pa.field("requires_external_data", pa.bool_()),
        pa.field("multi_session_work", pa.bool_()),
        pa.field("intent_tags", pa.string()),
        pa.field("primary_pattern", pa.string()),
        pa.field("input_tokens", pa.int32()),
        pa.field("cached_tokens", pa.int32()),
        pa.field("output_tokens", pa.int32()),
        pa.field("latency_ms", pa.int32()),
        pa.field("error", pa.string()),
    ])
    p = Path(path)
    new_table = pa.Table.from_pylist(new_rows, schema=schema)
    if p.exists():
        existing = pq.read_table(p)
        combined = pa.concat_tables([existing, new_table])
    else:
        combined = new_table
    # Atomic write via temp file
    tmp = p.with_suffix(p.suffix + ".tmp")
    pq.write_table(combined, tmp, compression="zstd")
    tmp.replace(p)


def load_already_processed(path):
    """Return set of session labels already in checkpoint."""
    p = Path(path)
    if not p.exists():
        return set()
    try:
        table = pq.read_table(p, columns=["session"])
        return set(table.column("session").to_pylist())
    except Exception:
        return set()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp",
                    default="/tmp/claude_analysis/sessions_enriched.parquet")
    ap.add_argument("--out", default="/tmp/claude_analysis/llm_classifications.parquet")
    ap.add_argument("--env-file", default="/tmp/claude_analysis/.env.zai")
    ap.add_argument("--sample-size", type=int, default=50,
                    help="How many sessions to classify. Use 999999 for all.")
    ap.add_argument("--concurrency", type=int, default=10)
    ap.add_argument("--include-subagents", action="store_true")
    ap.add_argument("--checkpoint-every", type=int, default=20,
                    help="Flush parquet every N completions (default 20)")
    ap.add_argument("--resume", action="store_true",
                    help="Skip sessions already present in --out parquet")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    env = load_env(args.env_file)
    api_key = env.get("ZAI_API_KEY") or os.environ.get("ZAI_API_KEY", "")
    base_url = env.get("ZAI_BASE_URL", "https://api.z.ai/api/anthropic")
    model = env.get("ZAI_MODEL", "glm-4.6")
    if not api_key:
        print("ERROR: ZAI_API_KEY not set (env file or env var)", file=sys.stderr)
        sys.exit(1)
    # Anthropic-compatible endpoint: /v1/messages
    url = base_url.rstrip("/") + "/v1/messages"
    print(f"# LLM classify: model={model} concurrency={args.concurrency}",
          file=sys.stderr)

    con = duckdb.connect()
    con.execute(f"CREATE VIEW sessions_enriched AS SELECT * FROM '{args.inp}'")

    sample = select_stratified_sample(con, args.sample_size,
                                      include_subagents=args.include_subagents)
    print(f"# Sampled {len(sample)} sessions across strata", file=sys.stderr)

    # Resume filter: drop sessions already present in checkpoint
    if args.resume:
        already = load_already_processed(args.out)
        before = len(sample)
        sample = [s for s in sample if s["session"] not in already]
        skipped = before - len(sample)
        print(f"# Resume: skipped {skipped} already-classified sessions, "
              f"remaining {len(sample)}", file=sys.stderr)

    strata_counts = {}
    for s in sample:
        key = (s["size_bucket"], bool(s["signal_went_wrong"]))
        strata_counts[key] = strata_counts.get(key, 0) + 1
    for k, v in sorted(strata_counts.items()):
        print(f"  {k}: {v}", file=sys.stderr)

    sample_by_session = {s["session"]: s for s in sample}

    if not sample:
        print("# Nothing to classify", file=sys.stderr)
        return

    # Run async with incremental checkpointing
    async def go():
        async with httpx.AsyncClient(http2=True) as client:
            return await run_classification(
                sample, sample_by_session, client, url, api_key, model,
                args.concurrency, args.verbose,
                checkpoint_path=args.out,
                checkpoint_every=args.checkpoint_every,
            )

    t_start = time.time()
    raw_results = asyncio.run(go())
    elapsed = time.time() - t_start

    # Stats from this run
    errors = sum(1 for r in raw_results if r.get("error"))
    total_in = sum(r.get("input_tokens", 0) for r in raw_results)
    total_cached = sum(r.get("cached_tokens", 0) for r in raw_results)
    total_out = sum(r.get("output_tokens", 0) for r in raw_results)

    print(f"\n# Classification run complete in {elapsed:.1f}s", file=sys.stderr)
    print(f"# Run processed: {len(raw_results)} sessions, errors: {errors}",
          file=sys.stderr)
    if total_in:
        hit_rate = total_cached / max(total_in, 1) * 100
        print(f"# Tokens: input={total_in} cached={total_cached} ({hit_rate:.1f}%) output={total_out}",
              file=sys.stderr)

    # Report final checkpoint state
    total_rows = load_already_processed(args.out)
    print(f"# Checkpoint total: {len(total_rows)} classifications on disk → {args.out}",
          file=sys.stderr)


if __name__ == "__main__":
    main()
