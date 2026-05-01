#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Per-session outputs: commits/pushes/PR/MR/issues/files/+LOC + commit-type breakdown.

Schema (raw):
  sid, project, lines_added, lines_removed, files_edited, files_written,
  edit_calls, write_calls, multiedit_calls,
  commits_total, feat, fix, refactor, chore, docs_c, test_c, ci_c, review_fix,
  pushes, pr_create, mr_create, issue_create, comments,
  unique_branches, unique_issues
"""
from __future__ import annotations

import argparse
import re
import sys
from datetime import datetime
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session, find_subagents  # noqa: E402
from lib.classifiers import commit_type_of, is_review_fix  # noqa: E402
from lib.anonymize import hash_path, SidProjector  # noqa: E402
from lib.io import outputs_dirs, write_parquet  # noqa: E402

RX_GIT_COMMIT_M = re.compile(
    r"git\s+commit\s+(?:[^']*\s)?-m\s+(?:'((?:[^'\\]|\\.)*)'|\"((?:[^\"\\]|\\.)*)\")",
    re.DOTALL,
)
RX_HEREDOC_MSG = re.compile(
    r"-m\s+\"\$\(\s*cat\s*<<\s*'?(\w+)'?\s*\n(.*?)\n\1\s*\)\"",
    re.DOTALL,
)
RX_BRANCH = re.compile(
    r"\bgit\s+(?:checkout\s+-[bB]\s+|switch\s+-c\s+|branch\s+)([\w/\-\.]+)",
    re.I,
)
RX_ISSUE = re.compile(r"(DEV-\d+|CU-[\w]+|JIRA-\d+|GH-\d+|#\d+)", re.I)


def _extract_msgs(cmd: str) -> list[str]:
    out = []
    for m in RX_HEREDOC_MSG.finditer(cmd):
        out.append(m.group(2))
    for m in RX_GIT_COMMIT_M.finditer(cmd):
        out.append(m.group(1) or m.group(2) or "")
    return out


def _cnt_lines(s: str) -> int:
    if not s:
        return 0
    return s.count("\n") + (0 if s.endswith("\n") else 1)


def _aggregate(events) -> dict:
    a = {
        "lines_added": 0, "lines_removed": 0,
        "edit_calls": 0, "write_calls": 0, "multiedit_calls": 0,
        "files_edited": set(), "files_written": set(),
        "commits_total": 0, "feat": 0, "fix": 0, "refactor": 0,
        "chore": 0, "docs_c": 0, "test_c": 0, "ci_c": 0, "review_fix": 0,
        "pushes": 0, "pr_create": 0, "mr_create": 0,
        "issue_create": 0, "comments": 0,
        "unique_branches": set(), "unique_issues": set(),
    }
    for ev in events:
        if ev.type != "assistant":
            continue
        for tu in ev.tool_uses:
            n = tu.get("name", "")
            inp = tu.get("input", {}) or {}
            if n == "Write":
                a["write_calls"] += 1
                fp = inp.get("file_path", "") or ""
                if fp:
                    a["files_written"].add(fp)
                a["lines_added"] += _cnt_lines(inp.get("content", "") or "")
            elif n == "Edit":
                a["edit_calls"] += 1
                fp = inp.get("file_path", "") or ""
                if fp:
                    a["files_edited"].add(fp)
                o = inp.get("old_string", "") or ""
                nn = inp.get("new_string", "") or ""
                ol, nl = _cnt_lines(o), _cnt_lines(nn)
                add = max(nl - ol, 0)
                rem = max(ol - nl, 0)
                if ol == nl and o != nn:
                    add = nl or 1
                    rem = ol or 1
                a["lines_added"] += add
                a["lines_removed"] += rem
            elif n == "MultiEdit":
                a["multiedit_calls"] += 1
                fp = inp.get("file_path", "") or ""
                if fp:
                    a["files_edited"].add(fp)
                for e in inp.get("edits", []) or []:
                    if not isinstance(e, dict):
                        continue
                    o = e.get("old_string", "") or ""
                    nn = e.get("new_string", "") or ""
                    ol, nl = _cnt_lines(o), _cnt_lines(nn)
                    add = max(nl - ol, 0)
                    rem = max(ol - nl, 0)
                    if ol == nl and o != nn:
                        add = nl or 1
                        rem = ol or 1
                    a["lines_added"] += add
                    a["lines_removed"] += rem
            elif n == "Bash":
                cmd = inp.get("command", "") or ""
                if "git push" in cmd:
                    a["pushes"] += 1
                if "git commit" in cmd:
                    for body in _extract_msgs(cmd):
                        first = body.strip().split("\n")[0] if body else ""
                        ctype = commit_type_of(first)
                        a["commits_total"] += 1
                        if ctype == "feat":
                            a["feat"] += 1
                        elif ctype == "fix":
                            a["fix"] += 1
                            if is_review_fix(body):
                                a["review_fix"] += 1
                        elif ctype == "refactor":
                            a["refactor"] += 1
                        elif ctype == "chore":
                            a["chore"] += 1
                        elif ctype == "docs":
                            a["docs_c"] += 1
                        elif ctype == "test":
                            a["test_c"] += 1
                        elif ctype == "ci":
                            a["ci_c"] += 1
                if "gh pr create" in cmd:
                    a["pr_create"] += 1
                if "glab mr create" in cmd:
                    a["mr_create"] += 1
                if "gh issue create" in cmd:
                    a["issue_create"] += 1
                if "gh pr comment" in cmd or "glab mr note" in cmd or "gh issue comment" in cmd:
                    a["comments"] += 1
                for m in RX_BRANCH.finditer(cmd):
                    br = m.group(1)
                    if br:
                        a["unique_branches"].add(br)
                for m in RX_ISSUE.finditer(cmd):
                    a["unique_issues"].add(m.group(0))
    return a


def _row(sess_or_sub_id: str, project: str, agg: dict) -> dict:
    return {
        "sid": sess_or_sub_id,
        "project": project,
        "lines_added": agg["lines_added"],
        "lines_removed": agg["lines_removed"],
        "files_edited": len(agg["files_edited"]),
        "files_written": len(agg["files_written"]),
        "edit_calls": agg["edit_calls"],
        "write_calls": agg["write_calls"],
        "multiedit_calls": agg["multiedit_calls"],
        "commits_total": agg["commits_total"],
        "feat": agg["feat"],
        "fix": agg["fix"],
        "refactor": agg["refactor"],
        "chore": agg["chore"],
        "docs_c": agg["docs_c"],
        "test_c": agg["test_c"],
        "ci_c": agg["ci_c"],
        "review_fix": agg["review_fix"],
        "prod_fix": agg["fix"] - agg["review_fix"],
        "pushes": agg["pushes"],
        "pr_create": agg["pr_create"],
        "mr_create": agg["mr_create"],
        "issue_create": agg["issue_create"],
        "comments": agg["comments"],
        "unique_branches": len(agg["unique_branches"]),
        "unique_issues": len(agg["unique_issues"]),
    }


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--since", default=None)
    p.add_argument("--outputs", default=None)
    args = p.parse_args(argv)

    since = datetime.fromisoformat(args.since) if args.since else None
    raw_dir, anon_dir = outputs_dirs(Path(args.outputs) if args.outputs else None)
    sid_main = SidProjector("s")
    sid_sub = SidProjector("a")
    raw_rows: list[dict] = []
    anon_rows: list[dict] = []

    for jf in find_session_files():
        try:
            sess = load_session(jf, include_subagents=False, since=since)
        except Exception:
            continue
        if not sess.events:
            continue
        agg = _aggregate(sess.events)
        raw_rows.append(_row(sess.sid, sess.project, agg))
        anon_rows.append(_row(sid_main.get(sess.sid), hash_path(sess.project), agg))
        for sf in find_subagents(jf):
            try:
                sub = load_session(sf, include_subagents=False, since=since)
            except Exception:
                continue
            if not sub.events:
                continue
            agg2 = _aggregate(sub.events)
            raw_rows.append(_row("sub_" + sub.sid, sess.project, agg2))
            anon_rows.append(_row(sid_sub.get(sub.sid), hash_path(sess.project), agg2))

    n_raw = write_parquet(raw_rows, raw_dir / "outputs.parquet")
    n_anon = write_parquet(anon_rows, anon_dir / "outputs.parquet")
    print(f"outputs: raw {n_raw}, anon {n_anon}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
