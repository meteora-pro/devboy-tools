#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0", "duckdb>=1.0"]
# ///
"""
Export LLM classifications + original first_human_turn for manual review.

Does NOT modify parquet — just produces a markdown file for human/Claude review.
"""
import argparse
import json
import sys
from pathlib import Path

import duckdb


def find_jsonl(session_label, root=None):
    if root is None:
        root = Path.home() / ".claude/projects"
    main_files = sorted(p for p in root.rglob("*.jsonl") if "/subagents/" not in str(p))
    seen = {}
    for p in main_files:
        if p.stem not in seen:
            seen[p.stem] = f"s{len(seen)+1:04d}"
    for uuid, label in seen.items():
        if label == session_label:
            for p in main_files:
                if p.stem == uuid:
                    return p
    return None


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
            open_t = f"<{tag}>"
            close_t = f"</{tag}>"
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


def extract_first_human(jsonl_path, max_chars=800):
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
                if any(isinstance(c, dict) and c.get("type") == "tool_result"
                       for c in content):
                    continue
                texts = [c.get("text", "") for c in content
                         if isinstance(c, dict) and c.get("type") == "text"]
                combined = "\n".join(texts)
            elif isinstance(content, str):
                combined = content
            else:
                continue
            if not combined.strip(): continue
            cleaned = _strip_wrappers(combined).strip()
            if (not cleaned or cleaned.startswith("<")
                or cleaned.startswith("Caveat:")
                or cleaned.startswith("[Request interrupted")
                or cleaned.startswith("[Interrupted]")
                or cleaned.startswith("[Meta]")):
                continue
            return cleaned[:max_chars]
    except Exception:
        pass
    return ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--classifications",
                    default="/tmp/claude_analysis/llm_classifications.parquet")
    ap.add_argument("--sessions",
                    default="/tmp/claude_analysis/sessions_enriched.parquet")
    ap.add_argument("--out", default="/tmp/llm_review.md")
    args = ap.parse_args()

    con = duckdb.connect()
    con.execute(f"CREATE VIEW c AS SELECT * FROM '{args.classifications}'")
    con.execute(f"CREATE VIEW se AS SELECT * FROM '{args.sessions}'")

    rows = con.execute("""
      SELECT c.session, c.size_bucket, c.top_model AS cls_model,
        c.task_type, c.primary_domain, c.task_complexity,
        c.prompt_specificity, c.task_outcome_signal, c.conversation_mode,
        c.requires_external_data, c.multi_session_work,
        c.intent_tags, c.primary_pattern,
        c.signal_went_wrong, c.is_bug_fix_session,
        c.error,
        -- Get session features (just first matching row since might be dups)
        (SELECT duration_active_sec / 3600 FROM se s
         WHERE s.session = c.session ORDER BY s.total_records DESC LIMIT 1) AS duration_h,
        (SELECT agent_turns FROM se s
         WHERE s.session = c.session ORDER BY s.total_records DESC LIMIT 1) AS agent_turns,
        (SELECT tool_calls_total FROM se s
         WHERE s.session = c.session ORDER BY s.total_records DESC LIMIT 1) AS tool_calls,
        (SELECT cost_usd_total FROM se s
         WHERE s.session = c.session ORDER BY s.total_records DESC LIMIT 1) AS cost_usd,
        (SELECT bash_count FROM se s
         WHERE s.session = c.session ORDER BY s.total_records DESC LIMIT 1) AS bash_count,
        (SELECT subagents_in_tree FROM se s
         WHERE s.session = c.session ORDER BY s.total_records DESC LIMIT 1) AS subagents,
        (SELECT top_tool_categories FROM se s
         WHERE s.session = c.session ORDER BY s.total_records DESC LIMIT 1) AS top_tools
      FROM c
      ORDER BY c.size_bucket DESC, c.signal_went_wrong DESC
    """).fetchall()

    out_lines = []
    out_lines.append("# GLM-4.5 classification review\n")
    out_lines.append(f"## {len(rows)} sessions for validation\n\n")

    for i, r in enumerate(rows, 1):
        session = r[0]
        jsonl = find_jsonl(session)
        first_human = extract_first_human(jsonl) if jsonl else ""
        out_lines.append(f"---\n\n## [{i}/{len(rows)}] `{session}` [{r[1]}] {r[2]}\n\n")

        out_lines.append("**Algo features:**\n")
        out_lines.append(f"- duration: {r[16]:.1f}h · turns: {r[17]}/{r[18]} · "
                         f"${r[19]:.2f} · bash: {r[20]} · subagents: {r[21]}\n")
        out_lines.append(f"- signal_went_wrong: {r[13]} · is_bug_fix: {r[14]}\n")
        out_lines.append(f"- top_tools: `{(r[22] or '')[:150]}`\n\n")

        out_lines.append("**First human prompt:**\n")
        if first_human:
            # Indent as blockquote for readability
            q = "\n".join(f"> {line}" for line in first_human.strip().split("\n"))
            out_lines.append(q + "\n\n")
        else:
            out_lines.append("> (not found)\n\n")

        out_lines.append("**GLM-4.5 classification:**\n")
        out_lines.append(f"- task_type: **{r[3]}** · domain: {r[4]} · complexity: {r[5]}\n")
        out_lines.append(f"- specificity: {r[6]} · outcome: {r[7]} · mode: {r[8]}\n")
        out_lines.append(f"- requires_external: {r[9]} · multi_session: {r[10]}\n")
        out_lines.append(f"- **pattern: `{r[12]}`** · tags: {r[11]}\n")
        if r[15]:
            out_lines.append(f"- ⚠️ error: {r[15]}\n")
        out_lines.append("\n")

    Path(args.out).write_text("".join(out_lines))
    print(f"Wrote review doc → {args.out}", file=sys.stderr)
    print(f"  {len(rows)} session classifications", file=sys.stderr)


if __name__ == "__main__":
    main()
