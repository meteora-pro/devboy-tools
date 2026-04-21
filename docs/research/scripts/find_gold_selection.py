#!/usr/bin/env python3
"""
Extract anonymized gold-selection patterns from Claude Code JSONL logs.

Gold-selection pattern:
  1. Agent calls a list-tool (mcp__*__get_issues / get_merge_requests / etc.)
  2. Tool result contains N items
  3. The next agent action references ONE specific item id (the "gold")
     via enrichment tool call or text mention.

Output is ANONYMIZED by design — project/client names, internal item IDs,
and session UUIDs never reach the CSV. Only distribution data is emitted.

Usage:
  python3 find_gold_selection.py > gold_selection_real.csv

Reproduce on your own logs — works against any ~/.claude/projects/*.jsonl.
"""
import json
import re
import sys
from pathlib import Path
from collections import OrderedDict

# List-returning MCP tools — these trigger a gold-selection event if the agent
# then picks one item from the response.
LIST_TOOLS = re.compile(
    r"mcp__[a-zA-Z0-9_-]+__(?:get_issues|search_issues|"
    r"get_merge_requests|search_merge_requests|"
    r"get_epics|search_epics|"
    r"get_meeting_notes|search_meeting_notes)$"
)
# Enrichment tools — agent follows up on one specific item.
ENRICH_TOOLS = re.compile(
    r"mcp__[a-zA-Z0-9_-]+__(?:get_issue|get_issue_comments|get_issue_relations|"
    r"get_merge_request_discussions|get_merge_request_diffs|"
    r"get_meeting_transcript)$"
)

# Extract item ids from raw tool result text.
ID_PATTERNS = [
    re.compile(r"#(\d+)"),
    re.compile(r"!(\d+)"),
    re.compile(r'"iid":\s*(\d+)'),
    re.compile(r'"id":\s*"([^"]+)"'),
]


def tool_verb(full_name):
    """mcp__<project>__<verb> → <verb>. Strips project/client identity."""
    parts = full_name.split("__")
    return parts[-1] if len(parts) >= 3 else "unknown"


def extract_ids(text):
    """Ordered list of item ids from raw tool result."""
    seen = set()
    out = []
    for pat in ID_PATTERNS:
        for m in pat.finditer(text):
            v = m.group(1)
            if v not in seen:
                seen.add(v)
                out.append(v)
    return out


def process_file(path):
    """Yield gold-selection events from one JSONL session file."""
    try:
        lines = path.read_text(errors="ignore").splitlines()
    except Exception:
        return

    list_invocations = {}

    for i, line in enumerate(lines):
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        msg = rec.get("message", {})
        content = msg.get("content", [])
        if not isinstance(content, list):
            continue
        for c in content:
            if not isinstance(c, dict):
                continue
            ctype = c.get("type")
            if ctype == "tool_use":
                name = c.get("name", "")
                if LIST_TOOLS.match(name):
                    list_invocations[c.get("id", "")] = {
                        "tool_verb": tool_verb(name),
                        "line_idx": i,
                        "items": None,
                    }
            elif ctype == "tool_result":
                tuid = c.get("tool_use_id", "")
                if tuid in list_invocations:
                    result = c.get("content", "")
                    if isinstance(result, list):
                        result = "\n".join(
                            x.get("text", "") for x in result if isinstance(x, dict)
                        )
                    elif not isinstance(result, str):
                        result = json.dumps(result)
                    list_invocations[tuid]["items"] = extract_ids(result)

    # Walk forward for each list invocation to find the gold.
    for inv in list_invocations.values():
        items = inv["items"] or []
        if len(items) < 3:
            continue
        gold_position = None
        gold_source = None
        for j in range(inv["line_idx"] + 1, min(inv["line_idx"] + 30, len(lines))):
            try:
                rec = json.loads(lines[j])
            except json.JSONDecodeError:
                continue
            msg = rec.get("message", {})
            content = msg.get("content", [])
            if not isinstance(content, list):
                continue
            for c in content:
                if not isinstance(c, dict):
                    continue
                ctype = c.get("type")
                if ctype == "tool_use":
                    name = c.get("name", "")
                    if ENRICH_TOOLS.match(name):
                        inp = c.get("input", {})
                        for key in ("iid", "issue_iid", "mr_iid",
                                    "merge_request_iid", "id"):
                            v = str(inp.get(key, ""))
                            if v in items:
                                gold_position = items.index(v)
                                gold_source = "enrichment"
                                break
                        if gold_position is not None:
                            break
                elif ctype == "text":
                    text = c.get("text", "")
                    for pos, item in enumerate(items):
                        if f"#{item}" in text or f"!{item}" in text:
                            gold_position = pos
                            gold_source = "text_mention"
                            break
                    if gold_position is not None:
                        break
            if gold_position is not None:
                break

        if gold_position is None:
            continue
        yield {
            "tool_verb": inv["tool_verb"],
            "n_items": len(items),
            "gold_position": gold_position,
            "gold_source": gold_source,
        }


def main():
    root = Path.home() / ".claude/projects"
    events = []
    # Session anonymization: assign sequential label on first encounter.
    session_labels = OrderedDict()

    for jsonl in root.rglob("*.jsonl"):
        # Use file mtime+size as a stable but non-identifying session token.
        sid = jsonl.stem  # raw UUID, never leaves this function
        for ev in process_file(jsonl):
            if sid not in session_labels:
                session_labels[sid] = f"s{len(session_labels) + 1:02d}"
            ev["session"] = session_labels[sid]
            events.append(ev)

    print("# Gold-selection events from Claude Code JSONL logs")
    print("# Anonymized: project/client names stripped to tool verb; sessions → s01..sNN;")
    print("# item IDs not emitted. Reproducible via docs/research/scripts/find_gold_selection.py")
    print(f"# Total events: {len(events)}  Sessions: {len(session_labels)}")
    print()
    print("session,tool_verb,n_items,gold_position,gold_fraction,gold_source")
    for ev in events:
        n = ev["n_items"]
        frac = ev["gold_position"] / max(n - 1, 1)
        print(f"{ev['session']},{ev['tool_verb']},{n},{ev['gold_position']},"
              f"{frac:.3f},{ev['gold_source']}")


if __name__ == "__main__":
    main()
