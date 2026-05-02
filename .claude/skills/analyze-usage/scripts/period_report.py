#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# ///
"""period_report.py — graphic monthly/weekly digest with our metaphors.

Aquarium of biomes (🐋🦈🐬🐟🦐🦠), archetypes (⚙️🔬🌐🛠📝🔍🏗💬), rhythm,
stack palette, DORA radar (CFR + lead time + pushes), friction (compacts,
pivots, subagents). Active-vs-wallclock ratio.

Output formats: terminal text / markdown / html / all.

Usage:
    period_report.py --from 2026-02-01 --to 2026-04-30 --period both
    period_report.py --from 2026-04-23 --to 2026-04-30 --format html --open
    period_report.py --from 2026-02-01 --to 2026-04-30 --format all --out-dir /tmp/usage
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import webbrowser
from collections import Counter, defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path

# Make `lib/` importable when invoked directly
_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.classifiers import (  # noqa: E402
    biome_of, archetype_of, rhythm_of, stack_of, bash_sub_of,
    commit_type_of, is_review_fix,
)
from lib.render import (  # noqa: E402
    render_period_terminal,
    render_period_markdown,
    render_period_html_section,
    render_html_doc,
    render_weekly_table_terminal,
    render_weekly_table_markdown,
    render_weekly_table_html,
    render_trends_terminal,
    render_trends_markdown,
    render_trends_html,
)

DEFAULT_ROOT = Path.home() / ".claude" / "projects"

# ─── Commit message extraction (heredoc + -m '...') ───────────────────────
RX_GIT_COMMIT_M = re.compile(
    r"git\s+commit\s+(?:[^']*\s)?-m\s+(?:'((?:[^'\\]|\\.)*)'|\"((?:[^\"\\]|\\.)*)\")",
    re.DOTALL,
)
RX_HEREDOC_MSG = re.compile(
    r"-m\s+\"\$\(\s*cat\s*<<\s*'?(\w+)'?\s*\n(.*?)\n\1\s*\)\"",
    re.DOTALL,
)

# Issue #235 — keep these in lockstep with extract_outputs.py.
RX_MCP_MR_CREATE = re.compile(r"^mcp__.*__(create|open|submit)_merge_request$")
RX_MCP_PR_CREATE = re.compile(r"^mcp__.*__(create|open|submit)_pull_request$")


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


def _new_session() -> dict:
    return {
        "real": 0, "asst": 0, "project": None,
        "first_ts": None, "last_ts": None,
        "edit": 0, "bash": 0, "read": 0, "mcp": 0, "sub": 0, "lines_added": 0,
        "compacts": 0, "pivots": 0,
        "commits": Counter(), "review_fixes": 0,
        "pr_create": 0, "mr_create": 0, "pushes": 0,
        "event_seq": [], "stack_loc": Counter(), "all_event_ts": [],
    }


def parse_all(root: Path, since: datetime | None = None, until: datetime | None = None) -> dict:
    """Parse every main jsonl under root. Returns dict[sid] -> session dict."""
    sessions: dict[str, dict] = defaultdict(_new_session)
    if not root.exists():
        return sessions
    for proj_dir in sorted(root.iterdir()):
        if not proj_dir.is_dir():
            continue
        proj_slug = proj_dir.name
        for jf in sorted(proj_dir.glob("*.jsonl")):
            sid = jf.stem
            try:
                with jf.open() as fh:
                    for line in fh:
                        try:
                            m = json.loads(line)
                        except Exception:
                            continue
                        sd = sessions[sid]
                        sd["project"] = proj_slug
                        if m.get("isCompactSummary"):
                            sd["compacts"] += 1
                        ts_str = m.get("timestamp")
                        t = None
                        if ts_str:
                            try:
                                t = datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
                            except Exception:
                                pass
                        if t:
                            if sd["first_ts"] is None or t < sd["first_ts"]:
                                sd["first_ts"] = t
                            if sd["last_ts"] is None or t > sd["last_ts"]:
                                sd["last_ts"] = t
                            sd["all_event_ts"].append(t)
                        typ = m.get("type", "?")
                        msg = m.get("message", {})
                        content = msg.get("content") if isinstance(msg, dict) else None
                        if typ == "assistant":
                            sd["asst"] += 1
                            if not isinstance(content, list):
                                continue
                            for blk in content:
                                if not isinstance(blk, dict) or blk.get("type") != "tool_use":
                                    continue
                                name = blk.get("name", "?")
                                inp = blk.get("input", {}) if isinstance(blk.get("input"), dict) else {}
                                if name in ("Edit", "MultiEdit", "Write"):
                                    sd["edit"] += 1
                                    sd["event_seq"].append("Edit")
                                    fp = inp.get("file_path", "") or ""
                                    stack = stack_of(fp) if fp else "other"
                                    if name == "Write":
                                        cnt = inp.get("content", "") or ""
                                        nl = _cnt_lines(cnt)
                                        sd["lines_added"] += nl
                                        sd["stack_loc"][stack] += nl
                                    elif name == "Edit":
                                        o = inp.get("old_string", "") or ""
                                        n = inp.get("new_string", "") or ""
                                        ol, nl = _cnt_lines(o), _cnt_lines(n)
                                        add = max(nl - ol, 0)
                                        if ol == nl and o != n:
                                            add = nl or 1
                                        sd["lines_added"] += add
                                        sd["stack_loc"][stack] += add
                                    else:  # MultiEdit
                                        for e in inp.get("edits", []):
                                            if not isinstance(e, dict):
                                                continue
                                            o = e.get("old_string", "") or ""
                                            n = e.get("new_string", "") or ""
                                            ol, nl = _cnt_lines(o), _cnt_lines(n)
                                            add = max(nl - ol, 0)
                                            if ol == nl and o != n:
                                                add = nl or 1
                                            sd["lines_added"] += add
                                            sd["stack_loc"][stack] += add
                                elif name in ("Read", "Grep", "Glob", "ToolSearch"):
                                    sd["read"] += 1
                                    sd["event_seq"].append("Read")
                                elif name == "Bash":
                                    sd["bash"] += 1
                                    cmd = inp.get("command", "") or ""
                                    sd["event_seq"].append(f"Bash:{bash_sub_of(cmd)}")
                                    if "git push" in cmd:
                                        sd["pushes"] += 1
                                    if "git commit" in cmd:
                                        for body in _extract_msgs(cmd):
                                            first = body.strip().split("\n")[0] if body else ""
                                            ctype = commit_type_of(first)
                                            sd["commits"][ctype] += 1
                                            if ctype == "fix" and is_review_fix(body):
                                                sd["review_fixes"] += 1
                                    if "gh pr create" in cmd:
                                        sd["pr_create"] += 1
                                    if "glab mr create" in cmd:
                                        sd["mr_create"] += 1
                                elif name in ("Task", "Agent"):
                                    sd["sub"] += 1
                                elif name.startswith("mcp__"):
                                    sd["mcp"] += 1
                                    # Issue #235: also count MCP-mediated
                                    # MR/PR creation toward the same
                                    # `pr_create`/`mr_create` totals as
                                    # the Bash CLI variants above. Anchored
                                    # `_request$` so we don't double-count
                                    # comment / discussion variants.
                                    if RX_MCP_MR_CREATE.match(name):
                                        sd["mr_create"] += 1
                                    elif RX_MCP_PR_CREATE.match(name):
                                        sd["pr_create"] += 1
                        elif typ == "user":
                            is_meta = m.get("isMeta", False)
                            ctype = ""
                            text = ""
                            if isinstance(content, list) and content:
                                fb = content[0] if isinstance(content[0], dict) else {}
                                ctype = fb.get("type", "")
                                if ctype == "text":
                                    text = fb.get("text", "")
                            elif isinstance(content, str):
                                ctype = "string"
                                text = content
                            if not is_meta and ctype in ("string", "text"):
                                txt = text.lstrip()
                                if txt.startswith("[Request interrupted by user"):
                                    sd["pivots"] += 1
                                elif not txt.startswith((
                                    "<command-name>", "<system-reminder>",
                                    "<user-prompt-submit-hook>", "Caveat:",
                                    "<local-command-stdout>", "<task-notification>",
                                    "This session is being continued",
                                )):
                                    sd["real"] += 1
            except Exception:
                pass
    if since or until:
        kept = {}
        for sid, sd in sessions.items():
            if sd["first_ts"] is None:
                continue
            if since and sd["first_ts"] < since:
                continue
            if until and sd["first_ts"] > until:
                continue
            kept[sid] = sd
        return kept
    return sessions


def active_minutes(ts_list: list[datetime], gap_min: int = 30) -> float:
    if len(ts_list) < 2:
        return 0
    s = sorted(ts_list)
    total = 0.0
    gap = timedelta(minutes=gap_min)
    for i in range(1, len(s)):
        d = s[i] - s[i - 1]
        if d <= gap:
            total += d.total_seconds()
    return total / 60


def aggregate(sessions: list[tuple[str, dict]]) -> dict:
    a = Counter()
    biomes = Counter()
    archs = Counter()
    rhythms = Counter()
    stack_loc = Counter()
    a["n"] = len(sessions)
    wall = 0.0
    act = 0.0
    for sid, d in sessions:
        biomes[biome_of(d["real"])] += 1
        archs[archetype_of(d["edit"], d["bash"], d["read"])] += 1
        rhythms[rhythm_of(d["event_seq"])] += 1
        for k, v in d["stack_loc"].items():
            stack_loc[k] += v
        a["feat"] += d["commits"].get("feat", 0)
        a["fix"] += d["commits"].get("fix", 0)
        a["review_fix"] += d["review_fixes"]
        a["lines_added"] += d["lines_added"]
        a["pushes"] += d["pushes"]
        a["pr_mr"] += d["pr_create"] + d["mr_create"]
        a["compacts"] += d["compacts"]
        a["pivots"] += d["pivots"]
        a["subs"] += d["sub"]
        if d["first_ts"] and d["last_ts"]:
            wall += (d["last_ts"] - d["first_ts"]).total_seconds() / 60
            act += active_minutes(d["all_event_ts"])
    a["wall_min"] = wall
    a["active_min"] = act
    a["biomes"] = biomes
    a["archs"] = archs
    a["rhythms"] = rhythms
    a["stack_loc"] = stack_loc
    return a


# ─── Format builders ──────────────────────────────────────────────────────


RU_MONTHS = {1: "Январь ❄️", 2: "Февраль ❄️", 3: "Март 🌸", 4: "Апрель 🌱",
             5: "Май ☀️", 6: "Июнь ☀️", 7: "Июль ☀️", 8: "Август 🌾",
             9: "Сентябрь 🍂", 10: "Октябрь 🍁", 11: "Ноябрь 🌨", 12: "Декабрь 🎄"}


def build_terminal(period: str, dt_from, dt_to,
                   month_groups, week_groups, n_total) -> str:
    out = []
    out.append("")
    out.append("🌊" * 50)
    out.append(f"  ОКЕАН CLAUDE CODE — {dt_from.date()} … {dt_to.date()}")
    out.append(f"  {n_total} sessions, биомы от 🦠 до 🐋")
    out.append("🌊" * 50)
    if period in ("monthly", "both"):
        months_data = {}
        months_labels = {}
        for (y, m) in sorted(month_groups.keys()):
            agg = aggregate(month_groups[(y, m)])
            label = f"📅 {RU_MONTHS.get(m, str(m))} {y}"
            out.extend(render_period_terminal(label, agg))
            months_data[m] = agg
            months_labels[m] = RU_MONTHS.get(m, str(m)).split(" ")[0]
        if len(months_data) >= 2:
            out.extend(render_trends_terminal(months_data, months_labels))
    if period in ("weekly", "both"):
        weeks = sorted(week_groups.keys())
        weekly_aggs = [(w, aggregate(week_groups[w])) for w in weeks]
        out.extend(render_weekly_table_terminal(weekly_aggs))
    return "\n".join(out)


def build_markdown(period: str, dt_from, dt_to,
                   month_groups, week_groups, n_total) -> str:
    out = []
    out.append(f"# 🌊 Океан Claude Code — {dt_from.date()} … {dt_to.date()}")
    out.append("")
    out.append(f"**{n_total} sessions** · биомы от 🦠 до 🐋")
    out.append("")
    if period in ("monthly", "both"):
        months_data = {}
        months_labels = {}
        for (y, m) in sorted(month_groups.keys()):
            agg = aggregate(month_groups[(y, m)])
            label = f"📅 {RU_MONTHS.get(m, str(m))} {y}"
            out.extend(render_period_markdown(label, agg))
            months_data[m] = agg
            months_labels[m] = RU_MONTHS.get(m, str(m)).split(" ")[0]
        if len(months_data) >= 2:
            out.extend(render_trends_markdown(months_data, months_labels))
    if period in ("weekly", "both"):
        weeks = sorted(week_groups.keys())
        weekly_aggs = [(w, aggregate(week_groups[w])) for w in weeks]
        out.extend(render_weekly_table_markdown(weekly_aggs))
    return "\n".join(out)


def build_html(period: str, dt_from, dt_to,
               month_groups, week_groups, n_total) -> str:
    title = f"Океан Claude Code · {dt_from.date()} … {dt_to.date()} · {n_total} sessions"
    sections: list[str] = []
    if period in ("monthly", "both"):
        months_data = {}
        months_labels = {}
        for (y, m) in sorted(month_groups.keys()):
            agg = aggregate(month_groups[(y, m)])
            label = f"📅 {RU_MONTHS.get(m, str(m))} {y}"
            sections.append(render_period_html_section(label, agg))
            months_data[m] = agg
            months_labels[m] = RU_MONTHS.get(m, str(m)).split(" ")[0]
        if len(months_data) >= 2:
            tr = render_trends_html(months_data, months_labels)
            if tr:
                sections.append(tr)
    if period in ("weekly", "both"):
        weeks = sorted(week_groups.keys())
        weekly_aggs = [(w, aggregate(week_groups[w])) for w in weeks]
        wt = render_weekly_table_html(weekly_aggs)
        if wt:
            sections.append(wt)
    return render_html_doc(title, sections)


# ─── Main ─────────────────────────────────────────────────────────────────


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description="Graphic period digest of Claude Code usage.")
    p.add_argument("--from", dest="dt_from", required=True, help="YYYY-MM-DD start (inclusive)")
    p.add_argument("--to", dest="dt_to", required=True, help="YYYY-MM-DD end (inclusive)")
    p.add_argument("--period", choices=["monthly", "weekly", "both"], default="monthly")
    p.add_argument("--format", choices=["text", "markdown", "html", "all"], default="text",
                   help="Output format. 'all' writes text+md+html into --out-dir.")
    p.add_argument("--root", default=str(DEFAULT_ROOT), help="Path to ~/.claude/projects")
    p.add_argument("--out", default=None, help="Write to file (single format)")
    p.add_argument("--out-dir", default=None,
                   help="Directory for --format all (default: /tmp/analyze-usage-<date>)")
    p.add_argument("--open", dest="open_in_browser", action="store_true",
                   help="Auto-open HTML output in default browser (requires html or all format)")
    args = p.parse_args(argv)

    dt_from = datetime.fromisoformat(args.dt_from).replace(tzinfo=timezone.utc)
    dt_to = datetime.fromisoformat(args.dt_to).replace(hour=23, minute=59, second=59, tzinfo=timezone.utc)

    print(f"Парсим {args.root}…", file=sys.stderr, flush=True)
    sessions = parse_all(Path(args.root), since=dt_from, until=dt_to)
    items = list(sessions.items())
    n_total = len(items)
    print(f"  in period: {n_total} sessions", file=sys.stderr)

    month_groups = defaultdict(list)
    week_groups = defaultdict(list)
    for sid, d in items:
        if d["first_ts"] is None:
            continue
        month_groups[(d["first_ts"].year, d["first_ts"].month)].append((sid, d))
        week_groups[d["first_ts"].isocalendar()[:2]].append((sid, d))

    formats = [args.format] if args.format != "all" else ["text", "markdown", "html"]
    artefacts: dict[str, str] = {}
    for fmt in formats:
        if fmt == "text":
            artefacts["text"] = build_terminal(args.period, dt_from, dt_to,
                                                month_groups, week_groups, n_total)
        elif fmt == "markdown":
            artefacts["markdown"] = build_markdown(args.period, dt_from, dt_to,
                                                    month_groups, week_groups, n_total)
        elif fmt == "html":
            artefacts["html"] = build_html(args.period, dt_from, dt_to,
                                            month_groups, week_groups, n_total)

    out_paths: dict[str, Path] = {}
    if args.format == "all":
        out_dir = Path(args.out_dir) if args.out_dir else Path(f"/tmp/analyze-usage-{dt_to.date()}")
        out_dir.mkdir(parents=True, exist_ok=True)
        for fmt, content in artefacts.items():
            ext = {"text": "txt", "markdown": "md", "html": "html"}[fmt]
            p_path = out_dir / f"report.{ext}"
            p_path.write_text(content, encoding="utf-8")
            out_paths[fmt] = p_path
        print(f"\n[saved 3 files to {out_dir}/]", file=sys.stderr)
        # Print text to stdout for convenience
        print(artefacts["text"])
    elif args.out:
        out_path = Path(args.out)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(artefacts[args.format], encoding="utf-8")
        out_paths[args.format] = out_path
        print(f"[saved to {out_path}]", file=sys.stderr)
        # Also dump to stdout if format is text
        if args.format == "text":
            print(artefacts["text"])
    else:
        # No file output → just stdout (only valid for text by default)
        if args.format == "text":
            print(artefacts["text"])
        elif args.format == "markdown":
            print(artefacts["markdown"])
        elif args.format == "html":
            # html to stdout works but is weird; auto-save to /tmp instead
            tmp_path = Path(f"/tmp/analyze-usage-{dt_to.date()}.html")
            tmp_path.write_text(artefacts["html"], encoding="utf-8")
            out_paths["html"] = tmp_path
            print(f"[html saved to {tmp_path}]", file=sys.stderr)

    # Auto-open in browser
    if args.open_in_browser:
        html_path = out_paths.get("html")
        if html_path:
            webbrowser.open(html_path.resolve().as_uri())
            print(f"[opening {html_path} in browser]", file=sys.stderr)
        else:
            print("--open requires html or all format (no html artefact produced)", file=sys.stderr)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
