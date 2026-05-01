"""Parsers for Claude Code session jsonl files.

Reads `~/.claude/projects/<project>/<session_uuid>.jsonl` and the matching
subagent files at `~/.claude/projects/<project>/<session_uuid>/subagents/agent-*.jsonl`.

The parsers return plain Python data — extractors decide how to aggregate.
No anonymization happens here; that lives in `lib/anonymize.py` and is applied
by the extractor when writing to `outputs/anon/`.
"""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from pathlib import Path
from typing import Iterable, Iterator


# ─── Paths ────────────────────────────────────────────────────────────────

DEFAULT_ROOT = Path(os.environ.get("CLAUDE_PROJECTS_ROOT", str(Path.home() / ".claude" / "projects")))


def find_session_files(root: Path | None = None) -> list[Path]:
    """Return all main-session jsonl files (top-level under each project dir).

    Subagent jsonls live one level deeper (in `<sid>/subagents/`) and are
    excluded here. Use `find_subagents()` for those.
    """
    if root is None:
        root = Path(os.environ.get("CLAUDE_PROJECTS_ROOT", str(DEFAULT_ROOT)))
    if not root.exists():
        return []
    out: list[Path] = []
    for proj_dir in sorted(root.iterdir()):
        if not proj_dir.is_dir():
            continue
        for jf in sorted(proj_dir.glob("*.jsonl")):
            out.append(jf)
    return out


def find_subagents(session_jsonl: Path) -> list[Path]:
    """Return subagent jsonl files spawned from a given main-session jsonl.

    Subagents live at `<project>/<sid>/subagents/agent-*.jsonl` where
    `<sid>` matches the basename of the main jsonl (without `.jsonl`).
    """
    sid = session_jsonl.stem
    sub_dir = session_jsonl.parent / sid / "subagents"
    if not sub_dir.exists():
        return []
    return sorted(sub_dir.glob("agent-*.jsonl"))


def project_name_from_path(jsonl: Path, root: Path | None = None) -> str:
    """Reconstruct the original project path from the encoded directory name.

    Claude Code stores `~/projects/foo/bar` as `~/.claude/projects/-foo-bar`
    (slashes replaced with dashes, leading dash for the absolute root).
    """
    if root is None:
        root = Path(os.environ.get("CLAUDE_PROJECTS_ROOT", str(DEFAULT_ROOT)))
    proj_dir = jsonl.parent
    while proj_dir.parent != root and proj_dir != root:
        # subagent jsonl is two levels deep; walk up to the project dir
        proj_dir = proj_dir.parent
        if proj_dir == proj_dir.parent:  # filesystem root, give up
            return jsonl.parent.name
    return proj_dir.name.lstrip("-").replace("-", "/")


# ─── Event model ─────────────────────────────────────────────────────────


@dataclass
class Event:
    """One parsed line from a session jsonl."""

    ts: datetime
    type: str  # 'user' | 'assistant' | 'compact_boundary' | 'system' | ...
    raw: dict  # original record for downstream consumers

    # User-message-only fields
    user_kind: str | None = None
    """One of: 'real_human' | 'tool_result' | 'task_notification' |
    'meta' | 'slash_meta' | 'system_reminder' | 'hook' | 'caveat' |
    'cmd_output' | 'unknown'.

    Set only when `type == 'user'`. `real_human` is the only category that
    counts as a user-typed prompt for biome/growth metrics.
    """
    user_text: str = ""
    """First text-block content if type=='user' and user_kind in
    ('real_human', 'task_notification'). Empty otherwise."""

    # Assistant-message-only fields
    text_chars: int = 0
    tool_uses: list[dict] = field(default_factory=list)
    """Each entry: {'name': str, 'input': dict, 'id': str}."""
    in_tokens: int = 0
    out_tokens: int = 0
    cache_read: int = 0
    cache_create: int = 0


META_PREFIXES = (
    "<command-name>",
    "<system-reminder>",
    "<user-prompt-submit-hook>",
    "<local-command-stdout>",
    "Caveat:",
)


def _classify_user_kind(rec: dict, content: list | str | None, ctype: str, text: str) -> str:
    """Classify a user-typed message into one of several buckets."""
    if rec.get("isMeta"):
        return "meta"
    if ctype == "tool_result":
        return "tool_result"
    if not isinstance(text, str):
        return "unknown"
    if text.startswith("<task-notification>"):
        return "task_notification"
    if text.startswith("<command-name>"):
        return "slash_meta"
    if text.startswith("<system-reminder>"):
        return "system_reminder"
    if text.startswith("<user-prompt-submit-hook>"):
        return "hook"
    if text.startswith("<local-command-stdout>"):
        return "cmd_output"
    if text.startswith("Caveat:"):
        return "caveat"
    if ctype in ("string", "text"):
        return "real_human"
    return "unknown"


def _is_compact(rec: dict) -> bool:
    if rec.get("type") == "compact_boundary":
        return True
    if rec.get("isCompactSummary"):
        return True
    if rec.get("type") == "summary":
        return True
    return False


def parse_session(
    jsonl: Path,
    *,
    since: datetime | None = None,
    until: datetime | None = None,
) -> Iterator[Event]:
    """Yield `Event` objects from a session jsonl file.

    Optional `since`/`until` filter by timestamp (UTC, half-open range).
    Malformed lines are skipped silently. Records without a parseable
    timestamp are skipped.
    """
    try:
        fh = jsonl.open("r", encoding="utf-8", errors="ignore")
    except OSError:
        return
    with fh:
        for line in fh:
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            ts_raw = rec.get("timestamp")
            if not ts_raw:
                continue
            try:
                ts = datetime.fromisoformat(str(ts_raw).replace("Z", "+00:00"))
            except (ValueError, TypeError):
                continue
            if since is not None and ts < since:
                continue
            if until is not None and ts >= until:
                continue

            ev_type = rec.get("type", "?")

            # Compact events are surfaced as a special type for downstream consumers.
            if _is_compact(rec):
                yield Event(ts=ts, type="compact", raw=rec)
                continue

            if ev_type == "assistant":
                msg = rec.get("message") or {}
                usage = msg.get("usage", {}) if isinstance(msg, dict) else {}
                content = msg.get("content") if isinstance(msg, dict) else None
                tool_uses: list[dict] = []
                text_chars = 0
                if isinstance(content, list):
                    for blk in content:
                        if not isinstance(blk, dict):
                            continue
                        bt = blk.get("type", "")
                        if bt == "tool_use":
                            tool_uses.append(
                                {
                                    "name": blk.get("name", "?"),
                                    "input": blk.get("input") or {},
                                    "id": blk.get("id", ""),
                                }
                            )
                        elif bt == "text":
                            text_chars += len(blk.get("text", "") or "")
                yield Event(
                    ts=ts,
                    type="assistant",
                    raw=rec,
                    text_chars=text_chars,
                    tool_uses=tool_uses,
                    in_tokens=int(usage.get("input_tokens") or 0),
                    out_tokens=int(usage.get("output_tokens") or 0),
                    cache_read=int(usage.get("cache_read_input_tokens") or 0),
                    cache_create=int(usage.get("cache_creation_input_tokens") or 0),
                )
                continue

            if ev_type == "user":
                msg = rec.get("message") or {}
                content = msg.get("content") if isinstance(msg, dict) else None
                ctype = ""
                text = ""
                if isinstance(content, list) and content:
                    first = content[0] if isinstance(content[0], dict) else {}
                    ctype = first.get("type", "")
                    if ctype == "text":
                        text = first.get("text", "") or ""
                elif isinstance(content, str):
                    ctype = "string"
                    text = content
                kind = _classify_user_kind(rec, content, ctype, text)
                yield Event(
                    ts=ts,
                    type="user",
                    raw=rec,
                    user_kind=kind,
                    user_text=text if kind in ("real_human", "task_notification") else "",
                )
                continue

            # Other types ('system', 'queue-operation', 'attachment', 'pr-link', ...) — pass through.
            yield Event(ts=ts, type=ev_type, raw=rec)


# ─── Bursts ──────────────────────────────────────────────────────────────


@dataclass
class Burst:
    """One human-prompt → agent-response cycle."""

    sid: str
    project: str
    start: datetime
    end: datetime
    is_notify: bool
    """True if this burst was opened by a `<task-notification>` (Monitor
    trigger) rather than a real human prompt."""
    prompt: str
    """First 200 chars of the user prompt (or notification summary)."""
    inferences: int = 0
    text_chars: int = 0
    tool_uses: list[dict] = field(default_factory=list)
    in_tokens: int = 0
    out_tokens: int = 0
    cache_read: int = 0
    cache_create: int = 0
    context_size_max: int = 0
    """Max of (input_tokens + cache_read + cache_create) across the burst's
    assistant turns. Approximates the largest context window observed."""
    final_text: str = ""
    """Last assistant text-only/mixed message, truncated to 200 chars."""


def build_bursts(events: Iterable[Event], sid: str, project: str) -> list[Burst]:
    """Group events into bursts (one per real_human or task_notification prompt).

    Tool-result and meta user messages are absorbed into the current burst.
    Compact events terminate the current burst (the next user prompt starts
    a fresh one).
    """
    bursts: list[Burst] = []
    cur: Burst | None = None
    for ev in events:
        if ev.type == "compact":
            if cur is not None:
                bursts.append(cur)
                cur = None
            continue
        if ev.type == "user":
            if ev.user_kind in ("real_human", "task_notification"):
                if cur is not None:
                    bursts.append(cur)
                cur = Burst(
                    sid=sid,
                    project=project,
                    start=ev.ts,
                    end=ev.ts,
                    is_notify=(ev.user_kind == "task_notification"),
                    prompt=ev.user_text[:200].replace("\n", " / "),
                )
            # tool_result / meta / etc. — stay in current burst, no new burst
            continue
        if ev.type == "assistant" and cur is not None:
            cur.inferences += 1
            cur.end = ev.ts
            cur.text_chars += ev.text_chars
            cur.tool_uses.extend(ev.tool_uses)
            cur.in_tokens += ev.in_tokens
            cur.out_tokens += ev.out_tokens
            cur.cache_read += ev.cache_read
            cur.cache_create += ev.cache_create
            ctx = ev.in_tokens + ev.cache_read + ev.cache_create
            if ctx > cur.context_size_max:
                cur.context_size_max = ctx
            for blk in ev.raw.get("message", {}).get("content", []) or []:
                if isinstance(blk, dict) and blk.get("type") == "text":
                    t = (blk.get("text") or "")[:200].replace("\n", " / ")
                    if t:
                        cur.final_text = t
    if cur is not None:
        bursts.append(cur)
    return bursts


# ─── Bash chain splitter ─────────────────────────────────────────────────


_SHELL_OPEN = re.compile(r"\b(for|while|until|if|case|select)\b")
_SHELL_CLOSE = re.compile(r"\b(done|fi|esac)\b")
_HEREDOC_OPEN = re.compile(r"<<-?\s*([\'\"]?)(\w+)\1")


def split_chain(cmd: str) -> list[str]:
    """Split a bash command into top-level parts at &&, ||, and ;.

    Honors single/double/backtick quoting, $(...) / ${...} substitution,
    parentheses/braces depth, heredocs, and control-flow blocks (so
    `for x; do a; done` stays one part). Escape sequences are passed
    through unchanged.

    Used by `lib/classifiers.classify_bash_part` to label each part as
    read / write / build / test / env / neutral.
    """
    if not cmd:
        return []

    parts: list[str] = []
    cur: list[str] = []
    in_single = False
    in_double = False
    in_back = False
    in_heredoc = False
    heredoc_marker: str | None = None
    paren_depth = 0
    cf_depth = 0
    i = 0
    n = len(cmd)

    def at_word_boundary(pos: int) -> bool:
        return pos == 0 or (not cmd[pos - 1].isalnum() and cmd[pos - 1] != "_")

    while i < n:
        c = cmd[i]

        # Inside a heredoc body, copy verbatim and look for the closing marker
        if in_heredoc:
            cur.append(c)
            if c == "\n":
                rest = cmd[i + 1 :]
                line_end = rest.find("\n")
                line = rest if line_end < 0 else rest[:line_end]
                if line.strip() == heredoc_marker:
                    cur.append(line + ("\n" if line_end >= 0 else ""))
                    i += 1 + len(line) + (1 if line_end >= 0 else 0)
                    in_heredoc = False
                    heredoc_marker = None
                    continue
            i += 1
            continue

        # Quote toggling
        if not in_double and not in_back and c == "'":
            in_single = not in_single
            cur.append(c)
            i += 1
            continue
        if not in_single and not in_back and c == '"':
            in_double = not in_double
            cur.append(c)
            i += 1
            continue
        if not in_single and not in_double and c == "`":
            in_back = not in_back
            cur.append(c)
            i += 1
            continue
        if in_single or in_double or in_back:
            cur.append(c)
            i += 1
            continue

        # Escape
        if c == "\\" and i + 1 < n:
            cur.append(c)
            cur.append(cmd[i + 1])
            i += 2
            continue

        # Heredoc opener
        m_hd = _HEREDOC_OPEN.match(cmd[i:])
        if m_hd:
            heredoc_marker = m_hd.group(2)
            in_heredoc = True
            cur.append(cmd[i : i + m_hd.end()])
            i += m_hd.end()
            continue

        # Substitution: $(...) and ${...}
        if cmd[i : i + 2] in ("$(", "${"):
            paren_depth += 1
            cur.append(cmd[i])
            cur.append(cmd[i + 1])
            i += 2
            continue
        if c == "(" or c == "{":
            paren_depth += 1
            cur.append(c)
            i += 1
            continue
        if c == ")" or c == "}":
            paren_depth = max(0, paren_depth - 1)
            cur.append(c)
            i += 1
            continue

        # Control-flow keywords (only at word boundaries, top paren level)
        if paren_depth == 0 and at_word_boundary(i):
            mo = _SHELL_OPEN.match(cmd[i:])
            if mo:
                cur.append(cmd[i : i + mo.end()])
                cf_depth += 1
                i += mo.end()
                continue
            mc = _SHELL_CLOSE.match(cmd[i:])
            if mc:
                cur.append(cmd[i : i + mc.end()])
                cf_depth = max(0, cf_depth - 1)
                i += mc.end()
                continue

        # Top-level splitters
        if paren_depth == 0 and cf_depth == 0:
            if cmd[i : i + 2] in ("&&", "||"):
                parts.append("".join(cur).strip())
                cur = []
                i += 2
                continue
            if cmd[i : i + 2] == ";;":  # case fallthrough — keep glued
                cur.append(cmd[i : i + 2])
                i += 2
                continue
            if c == ";":
                parts.append("".join(cur).strip())
                cur = []
                i += 1
                continue

        cur.append(c)
        i += 1

    if cur:
        parts.append("".join(cur).strip())
    return [p for p in parts if p]


# ─── Convenience: end-to-end load ────────────────────────────────────────


@dataclass
class ParsedSession:
    """Result of parsing one main-session jsonl plus its subagents."""

    sid: str
    """Full session UUID (basename of the jsonl file)."""
    project: str
    """Project path, dashes restored back to slashes."""
    jsonl_path: Path
    events: list[Event]
    bursts: list[Burst]
    subagents: list["ParsedSubagent"] = field(default_factory=list)


@dataclass
class ParsedSubagent:
    sub_id: str
    parent_sid: str
    jsonl_path: Path
    events: list[Event]


def load_session(
    jsonl: Path,
    *,
    since: datetime | None = None,
    until: datetime | None = None,
    include_subagents: bool = True,
    root: Path | None = None,
) -> ParsedSession:
    """Parse one main-session jsonl and (optionally) all its subagents."""
    events = list(parse_session(jsonl, since=since, until=until))
    sid = jsonl.stem
    project = project_name_from_path(jsonl, root=root)
    bursts = build_bursts(events, sid=sid, project=project)
    subagents: list[ParsedSubagent] = []
    if include_subagents:
        for sf in find_subagents(jsonl):
            sub_events = list(parse_session(sf, since=since, until=until))
            subagents.append(
                ParsedSubagent(
                    sub_id=sf.stem.removeprefix("agent-"),
                    parent_sid=sid,
                    jsonl_path=sf,
                    events=sub_events,
                )
            )
    return ParsedSession(
        sid=sid,
        project=project,
        jsonl_path=jsonl,
        events=events,
        bursts=bursts,
        subagents=subagents,
    )
