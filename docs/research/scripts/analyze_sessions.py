#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pyarrow>=18.0"]
# ///
"""
Per-session + per-turn + per-event statistical analysis of Claude Code JSONL traces.

Emits anonymized Parquet tables for queryable analysis (DuckDB / polars / pandas)
— Paper 4's "Notebook + Parquet" idea applied to our own research data.

Output tables (all in --out-dir):
  - sessions.parquet     — one row per session (main + subagent)
  - turns.parquet        — one row per conversational turn (human/agent/tool_result)
                           includes context_size_tokens (prompt size at each agent turn)
  - meta_events.parquet  — Claude Code internal events (progress / file-history /
                           system / summary / attachment / queue-operation)
                           structural fields only; user-typed content is never stored
                           (only char counts)
  - compactions.parquet  — compact_boundary events with trigger + pre_tokens;
                           key signal for context-overflow research

Anonymization:
  - session UUIDs → s0001..sNNNN (main) / a0001..aNNNN (subagents)
  - MCP tool names stripped to verb bucket
  - Message text never stored — only text_chars (length)
  - Away/queue/summary text: only content_chars
  - Tool use IDs / hook command strings / cwd paths: dropped

Usage:
  uv run docs/research/scripts/analyze_sessions.py --out-dir /tmp/claude_analysis
"""
import argparse
import datetime as dt
import hashlib
import json
import re
import sys
from collections import OrderedDict, Counter
from pathlib import Path
from statistics import mean, median

import pyarrow as pa
import pyarrow.parquet as pq


def project_hash(cwd):
    """Stable, anonymized 6-char project identifier derived from cwd."""
    if not cwd:
        return ""
    h = hashlib.sha1(cwd.encode("utf-8")).hexdigest()[:6]
    return f"p{h}"

MCP_VERB_RE = re.compile(r"^mcp__[a-zA-Z0-9_-]+__(.+)$")

# ─── Bash command categorization ──────────────────────────────────────────
# Classify the *first significant token* in a bash command into a bucket.
# Never store the raw command — only the category + structural flags.
# Order matters: more specific patterns first.
BASH_CATEGORIES = [
    ("git_read",    re.compile(r"^\s*git\s+(status|log|diff|show|blame|branch(?!\s+-[dD])|reflog|remote|stash\s+list|config\s+--get)\b")),
    ("git_write",   re.compile(r"^\s*git\s+(add|commit|push|pull|fetch|checkout|switch|merge|rebase|reset|stash(?!\s+list)|cherry-pick|revert|tag|branch\s+-[dD])\b")),
    ("build",       re.compile(r"^\s*(npm\s+run\s+build|pnpm\s+build|yarn\s+build|cargo\s+build|go\s+build|make\b|gradle|mvn|nx\s+(build|run))\b")),
    ("test",        re.compile(r"^\s*(npm\s+(run\s+)?test|pnpm\s+(run\s+)?test|yarn\s+(run\s+)?test|cargo\s+test|go\s+test|pytest|jest|vitest|nx\s+test|bundle\s+exec\s+rspec)\b")),
    ("lint",        re.compile(r"^\s*(eslint|prettier|ruff|cargo\s+clippy|cargo\s+fmt|gofmt|rubocop|pylint|mypy|tsc(?!\s+--build))\b")),
    ("file_read",   re.compile(r"^\s*(cat|head|tail|less|more|bat)\b")),
    ("file_search", re.compile(r"^\s*(grep|egrep|fgrep|rg|ripgrep|ag|fd|find(?!\s+\.|.*-exec))\b")),
    ("file_ops",    re.compile(r"^\s*(ls|cp|mv|rm|mkdir|touch|chmod|chown|ln|stat|tree)\b")),
    ("diff_view",   re.compile(r"^\s*(diff|colordiff|delta)\b")),
    ("pkg_install", re.compile(r"^\s*(npm\s+install|pnpm\s+(add|install)|yarn\s+add|pip\s+install|pip3\s+install|cargo\s+(add|install)|brew\s+install|apt(-get)?\s+install|go\s+(get|install))\b")),
    ("pkg_run",     re.compile(r"^\s*(npm\s+run|pnpm\s+run|yarn\s+run|cargo\s+run|go\s+run)\b")),
    ("script_run",  re.compile(r"^\s*(python3?|node|deno|bun|ruby|perl|sh|bash|zsh|./[\w./-]+\.(sh|py|js|ts|rb))\b")),
    ("container",   re.compile(r"^\s*(docker|docker-compose|podman|kubectl|helm|k9s|minikube)\b")),
    ("http",        re.compile(r"^\s*(curl|wget|httpie|http)\b")),
    ("process",     re.compile(r"^\s*(ps|kill|pkill|killall|top|htop|df|du|free|uname|hostname|uptime|nproc|which|whereis)\b")),
    ("devenv",     re.compile(r"^\s*(ssh|scp|rsync|systemctl|service|journalctl|tmux|screen)\b")),
    ("env_inspect", re.compile(r"^\s*(env|printenv|pwd|whoami|id|date|echo\s+\$)")),
    ("echo_print",  re.compile(r"^\s*echo\b")),
    ("heredoc",     re.compile(r"^\s*(cat\s*<<|tee)")),
    ("tool_help",   re.compile(r"^\s*\S+\s+--help")),
]
BASH_CHAIN_RE = re.compile(r"&&|\|\||;\s*\S")
BASH_PIPE_RE = re.compile(r"(?<!\|)\|(?!\|)")      # single | not ||
BASH_REDIR_RE = re.compile(r"(^|\s)(>{1,2}|<)\s")

# ─── Refined categorizer (multi-subcommand aware) ─────────────────────────
# Categories here are more fine-grained than top-level BASH_CATEGORIES.
# Used for per-subcommand analysis.

REFINED_CATS = [
    # Inline eval — specifically risky patterns where agent runs arbitrary code
    ("inline_python", re.compile(r"^python3?\s+-c\b"), "inline_exec"),
    ("inline_node",   re.compile(r"^node\s+-e\b"), "inline_exec"),
    ("inline_bash",   re.compile(r"^bash\s+-c\b"), "inline_exec"),
    ("inline_sh",     re.compile(r"^sh\s+-c\b"), "inline_exec"),
    ("inline_perl",   re.compile(r"^perl\s+-e\b"), "inline_exec"),
    ("inline_ruby",   re.compile(r"^ruby\s+-e\b"), "inline_exec"),
    # Scratch writes (temp dirs or heredoc to /tmp)
    ("scratch_heredoc", re.compile(r"^cat\s*>\s*/tmp/"), "scratch_write"),
    ("scratch_echo",    re.compile(r"^echo\s+.+>\s*/tmp/"), "scratch_write"),
    # Version control — read
    ("git_status",  re.compile(r"^git\s+status\b"), "read"),
    ("git_log",     re.compile(r"^git\s+log\b"), "read"),
    ("git_diff",    re.compile(r"^git\s+diff\b"), "read"),
    ("git_show",    re.compile(r"^git\s+(show|blame|reflog|remote|branch(?!\s+-[dD]))\b"), "read"),
    # Version control — write
    ("git_commit",  re.compile(r"^git\s+(commit|add)\b"), "write"),
    ("git_push",    re.compile(r"^git\s+(push|pull|fetch)\b"), "write"),
    ("git_branch",  re.compile(r"^git\s+(checkout|switch|branch\s+-[dD]|merge|rebase|reset|stash(?!\s+list)|cherry-pick|revert|tag)\b"), "write"),
    # GitHub CLI
    ("gh_read",     re.compile(r"^gh\s+(pr\s+(list|view|checks|status|diff)|issue\s+(list|view)|repo\s+(list|view)|run\s+(list|view))\b"), "read"),
    ("gh_write",    re.compile(r"^gh\s+(pr\s+(create|merge|close|reopen|comment|review|ready)|issue\s+(create|edit|close|reopen|comment)|run\s+(cancel|rerun))\b"), "write"),
    ("gh_other",    re.compile(r"^gh\b"), "execute"),
    # GitLab CLI
    ("glab_read",   re.compile(r"^glab\s+(mr\s+(list|view|diff)|issue\s+(list|view)|repo\s+(view))\b"), "read"),
    ("glab_write",  re.compile(r"^glab\s+(mr\s+(create|merge|close|update|note)|issue\s+(create|close|update|note))\b"), "write"),
    ("glab_other",  re.compile(r"^glab\b"), "execute"),
    # DevBoy CLI
    ("devboy_cli",  re.compile(r"^devboy\b"), "execute"),
    # Package managers — install = write
    ("pkg_install", re.compile(r"^(npm\s+install|npm\s+i(?!\w)|pnpm\s+(add|install|i(?!\w))|yarn\s+(add|install)|pip\s+install|pip3\s+install|cargo\s+(add|install|publish)|brew\s+(install|upgrade)|apt(-get)?\s+install|go\s+(get|install))\b"), "write"),
    # Package managers — run/exec = execute
    ("pkg_run",     re.compile(r"^(npm\s+run|pnpm\s+(run|exec)|yarn\s+(run|exec)|cargo\s+run|go\s+run|npx\b|pnpx\b)"), "execute"),
    # Build / test / lint
    ("test",        re.compile(r"^(npm\s+(run\s+)?test|pnpm\s+(run\s+)?test|yarn\s+(run\s+)?test|cargo\s+test|go\s+test|pytest|jest|vitest|nx\s+test|bundle\s+exec\s+rspec)\b"), "execute"),
    ("build",       re.compile(r"^(npm\s+run\s+build|pnpm\s+build|yarn\s+build|cargo\s+build|go\s+build|make\b|gradle|mvn|nx\s+(build|run)|tsc\b)"), "execute"),
    ("lint",        re.compile(r"^(eslint|prettier|ruff|cargo\s+clippy|cargo\s+fmt|gofmt|rubocop|pylint|mypy)\b"), "execute"),
    # File operations
    ("file_read",   re.compile(r"^(cat|head|tail|less|more|bat)\b"), "read"),
    ("file_search", re.compile(r"^(grep|egrep|fgrep|rg|ripgrep|ag|fd|find(?!\s+\.|.*-exec))\b"), "read"),
    ("ls",          re.compile(r"^(ls|tree|stat|file)\b"), "read"),
    ("file_edit",   re.compile(r"^(sed\s+-i|awk\s+-i)"), "write"),          # in-place edit
    ("file_pipe",   re.compile(r"^(sed|awk|cut|sort|uniq|tr|wc|head|tail|column|jq|yq)\b"), "read"),  # pipe transforms
    ("file_write",  re.compile(r"^(cp|mv|rm|mkdir|touch|chmod|chown|ln(?!\s+-s\s+)|ln\s+-s|rsync)\b"), "write"),
    # Diff view
    ("diff_view",   re.compile(r"^(diff|colordiff|delta)\b"), "read"),
    # Text manipulation / heredoc
    ("echo",        re.compile(r"^echo\b"), "read"),
    ("tee",         re.compile(r"^tee\b"), "write"),
    ("heredoc",     re.compile(r"^cat\s*(?:>|<<)"), "write"),
    # Container / orchestration — read
    ("k8s_read",    re.compile(r"^kubectl\s+(get|describe|logs|top|explain|cluster-info|config\s+get)"), "read"),
    ("k8s_write",   re.compile(r"^kubectl\s+(apply|create|delete|patch|edit|replace|scale|rollout|exec|port-forward|cp|drain|cordon|uncordon|label|annotate)"), "write"),
    ("docker_read", re.compile(r"^docker\s+(ps|images|logs|inspect|stats|version|info|top|history)"), "read"),
    ("docker_write", re.compile(r"^docker\s+(run|exec|start|stop|rm|rmi|build|push|pull|tag|commit|network|volume)"), "write"),
    ("docker_cmp",  re.compile(r"^docker-compose\s+(up|down|build|pull|push|restart)"), "write"),
    ("helm_read",   re.compile(r"^helm\s+(list|get|history|show|status|search)"), "read"),
    ("helm_write",  re.compile(r"^helm\s+(install|upgrade|uninstall|rollback|repo)"), "write"),
    # Playwright CLI — browser verification
    ("playwright",  re.compile(r"^playwright(-cli)?\b"), "verify"),
    # HTTP — mostly read unless POST/PUT/PATCH/DELETE
    ("http_write",  re.compile(r"^curl\b.*\s-X\s+(POST|PUT|PATCH|DELETE)"), "write"),
    ("http_read",   re.compile(r"^(curl|wget|httpie|http)\b"), "read"),
    # Process inspection
    ("process",     re.compile(r"^(ps|pgrep|top|htop|df|du|free|uname|hostname|uptime|nproc|which|whereis|lsof)\b"), "read"),
    ("kill",        re.compile(r"^(kill|pkill|killall)\b"), "write"),
    # Shell built-ins
    ("navigate",    re.compile(r"^cd\b"), "navigate"),
    ("wait",        re.compile(r"^sleep\b"), "wait"),
    ("env_set",     re.compile(r"^(export|unset|source|\.)\b"), "shell_eval"),
    ("env_read",    re.compile(r"^(env|printenv|pwd|whoami|id|date)\b"), "read"),
    # Control flow — we peek inside
    ("for_loop",    re.compile(r"^for\s+\w+\s+in\s+"), "control_flow"),
    ("while_loop",  re.compile(r"^while\s+"), "control_flow"),
    ("if_cond",     re.compile(r"^if\s+\["), "control_flow"),
    # Scripts
    ("script_run",  re.compile(r"^(python3?|node|deno|bun|ruby|perl|sh|bash|zsh|\./)\b"), "execute"),
    # SSH / remote
    ("remote",      re.compile(r"^(ssh|scp|rsync|systemctl|journalctl)\b"), "execute"),
    # Comments
    ("comment",     re.compile(r"^#"), "comment"),
    ("help",        re.compile(r"^\S+\s+--help"), "read"),
]


def _strip_env_prefix(c):
    """Remove env var assignments at start: FOO=bar BAZ=qux command."""
    while True:
        m = re.match(r"^\w+=(?:\".*?\"|'.*?'|\$\([^)]*\)|\S*)\s+", c)
        if not m:
            return c
        c = c[m.end():]


def _strip_builtin_prefix(c):
    """Remove sudo/time/nohup/eval prefixes."""
    while True:
        m = re.match(r"^(sudo|time|nohup|eval)\s+", c)
        if not m:
            return c
        c = c[m.end():]


def _split_subcommands(cmd):
    """
    Split a compound command into sub-commands on &&, ||, ;, |.
    Not fully shell-accurate (ignores quotes, subshells) but good enough for stats.
    """
    if not cmd:
        return []
    parts = re.split(r"&&|\|\||;|(?<!\|)\|(?!\|)", cmd)
    return [p.strip() for p in parts if p.strip()]


def _classify_subcommand(sub):
    """Classify a single sub-command (already stripped of prefixes)."""
    for name, pat, io in REFINED_CATS:
        if pat.match(sub):
            return name, io
    return "other", "other"


def analyze_bash_compound(cmd):
    """
    Multi-subcommand analysis of a Bash command.

    Returns: (primary_io_type, subcategories_pipe_joined, has_chain, has_pipe, has_redir)

    primary_io_type: "write" > "execute" > "verify" > "read" > other hierarchical
    subcategories_pipe_joined: "git_commit|file_edit|test" — what sub-cmds were present
    """
    if not cmd or not cmd.strip():
        return ("other", "", False, False, False)

    has_chain = bool(BASH_CHAIN_RE.search(cmd))
    has_pipe = bool(BASH_PIPE_RE.search(cmd))
    has_redir = bool(BASH_REDIR_RE.search(cmd))

    subs = _split_subcommands(cmd)
    cats = []
    ios = []

    for s in subs:
        s = s.strip()
        s = _strip_env_prefix(s)
        s = _strip_builtin_prefix(s)
        s = s.lstrip("(").strip()  # strip subshell
        if not s:
            continue
        name, io = _classify_subcommand(s)

        # For control flow, peek inside the body
        if io == "control_flow":
            # for X in Y; do BODY; done — pull BODY
            m = re.search(r"\bdo\s+(.+?)(?:\s*;\s*done\b|$)", cmd)
            if m:
                inner = m.group(1).strip()
                inner = _strip_env_prefix(_strip_builtin_prefix(inner))
                inner_name, inner_io = _classify_subcommand(inner)
                cats.append(f"{name}+{inner_name}")
                ios.append(inner_io)
                continue
        cats.append(name)
        ios.append(io)

    if not cats:
        return ("other", "", has_chain, has_pipe, has_redir)

    # Priority: write > inline_exec > scratch_write > verify > execute > read > others
    io_priority = {"write": 6, "inline_exec": 5, "scratch_write": 5,
                   "verify": 4, "execute": 3, "read": 2,
                   "wait": 1, "navigate": 1, "shell_eval": 1,
                   "comment": 0, "other": 0, "control_flow": 0}
    dominant = max(ios, key=lambda x: io_priority.get(x, 0))
    subcats_str = "|".join(cats[:5])  # cap at 5 for compactness
    return (dominant, subcats_str, has_chain, has_pipe, has_redir)

# Git verb subcategorization — applied only when category == 'git_write' or 'git_read'
GIT_SUBCATS = [
    ("git_status",   re.compile(r"\bgit\s+status\b")),
    ("git_log",      re.compile(r"\bgit\s+log\b")),
    ("git_diff",     re.compile(r"\bgit\s+diff\b")),
    ("git_show",     re.compile(r"\bgit\s+show\b")),
    ("git_blame",    re.compile(r"\bgit\s+blame\b")),
    ("git_branch",   re.compile(r"\bgit\s+branch\b")),
    ("git_commit",   re.compile(r"\bgit\s+commit\b")),
    ("git_push",     re.compile(r"\bgit\s+push\b")),
    ("git_pull",     re.compile(r"\bgit\s+pull\b")),
    ("git_fetch",    re.compile(r"\bgit\s+fetch\b")),
    ("git_merge",    re.compile(r"\bgit\s+merge\b")),
    ("git_rebase",   re.compile(r"\bgit\s+rebase\b")),
    ("git_checkout", re.compile(r"\bgit\s+(checkout|switch)\b")),
    ("git_stash",    re.compile(r"\bgit\s+stash\b")),
    ("git_reset",    re.compile(r"\bgit\s+reset\b")),
    ("git_add",      re.compile(r"\bgit\s+add\b")),
    ("git_cherry_pick", re.compile(r"\bgit\s+cherry-pick\b")),
    ("git_revert",   re.compile(r"\bgit\s+revert\b")),
    ("git_tag",      re.compile(r"\bgit\s+tag\b")),
    ("git_remote",   re.compile(r"\bgit\s+remote\b")),
    ("git_config",   re.compile(r"\bgit\s+config\b")),
    ("git_reflog",   re.compile(r"\bgit\s+reflog\b")),
]


def git_subcategory(cmd):
    """Return git-verb subcategory or '' if not a git command."""
    if not cmd:
        return ""
    for name, pat in GIT_SUBCATS:
        if pat.search(cmd):
            return name
    return ""


# ─── Human-turn structural feature extraction (NO raw text stored) ────────

QUESTION_RE = re.compile(r"\?")
CODE_BLOCK_RE = re.compile(r"```|<code>|`[^`]+`")
URL_RE = re.compile(r"https?://[^\s)]+|www\.[^\s)]+")
ISSUE_REF_RE = re.compile(r"#\d+|!\d+|[A-Z]{2,}-\d+")  # #123, !456, JIRA-789
PATH_RE = re.compile(r"/\w+/\w+|\w+\.(ts|js|py|rs|go|rb|java|md|yaml|json|tsx|jsx)")
# Stop / reject / correct signals
NEGATION_RE = re.compile(
    r"\b(stop|wait|halt|cancel|abort|нет|не так|неправильно|не надо|отмена|подожди|стоп|don['']?t|do not|do'nt|don't)\b",
    re.IGNORECASE,
)
URGENCY_RE = re.compile(
    r"\b(asap|urgent|срочно|now|сейчас|быстро|important)\b", re.IGNORECASE,
)
APPROVAL_RE = re.compile(
    r"\b(ok|okay|yes|да|go|go ahead|continue|давай|делай|норм|да норм|хорошо|ага)\b",
    re.IGNORECASE,
)
ERROR_REPORT_RE = re.compile(
    r"\b(error|fail|failed|crash|crashed|broken|traceback|exception|panic|stack trace|ошибка|падает|сломан)\b",
    re.IGNORECASE,
)
PLEASE_RE = re.compile(r"\b(please|пожалуйста|could you|can you)\b", re.IGNORECASE)
LIST_MARKER_RE = re.compile(r"^\s*[-*1-9]\.?\s+", re.MULTILINE)


def length_bucket(n):
    if n == 0: return "0.empty"
    if n < 20: return "1.ack <20"
    if n < 100: return "2.short 20-99"
    if n < 500: return "3.med 100-499"
    if n < 2000: return "4.long 500-1999"
    return "5.spec 2000+"


def extract_human_features(text):
    """
    Return a dict of structural features about human text.
    NEVER returns text — only booleans/counts/bucket labels.
    """
    if not text:
        text = ""
    chars = len(text)
    lines = text.count("\n") + (1 if text.strip() else 0)

    # Build features without storing text
    return {
        "chars": chars,
        "lines": lines,
        "length_bucket": length_bucket(chars),
        "has_question": bool(QUESTION_RE.search(text)),
        "has_code_block": bool(CODE_BLOCK_RE.search(text)),
        "has_url": bool(URL_RE.search(text)),
        "has_issue_ref": bool(ISSUE_REF_RE.search(text)),
        "has_file_path": bool(PATH_RE.search(text)),
        "has_negation": bool(NEGATION_RE.search(text)),
        "has_urgency": bool(URGENCY_RE.search(text)),
        "has_approval": bool(APPROVAL_RE.search(text)),
        "has_error_report": bool(ERROR_REPORT_RE.search(text)),
        "has_please": bool(PLEASE_RE.search(text)),
        "has_list_markers": bool(LIST_MARKER_RE.search(text)),
        "list_item_count": len(LIST_MARKER_RE.findall(text)),
        "url_count": len(URL_RE.findall(text)),
        "question_count": len(QUESTION_RE.findall(text)),
    }


def classify_human_intent(feats):
    """Heuristic intent class from structural features."""
    c = feats["chars"]
    if c < 20 and feats["has_approval"]:
        return "approve_short"
    if c < 20:
        return "micro"
    if feats["has_negation"]:
        return "course_correct"
    if feats["has_error_report"]:
        return "report_error"
    if feats["has_question"] and c < 200:
        return "quick_question"
    if c >= 500 and feats["has_list_markers"]:
        return "detailed_spec"
    if feats["has_question"]:
        return "question"
    if feats["has_urgency"]:
        return "urgent_request"
    if c >= 200:
        return "instruction"
    return "short_prompt"


# Map bash category → I/O type (read / write / execute / mixed)
BASH_IO_MAP = {
    "file_read": "read",
    "file_search": "read",
    "git_read": "read",
    "env_inspect": "read",
    "process": "read",
    "http": "read",          # mostly GET
    "diff_view": "read",
    "tool_help": "read",
    "echo_print": "read",

    "git_write": "write",
    "file_ops": "write",     # cp/mv/rm/mkdir/chmod — mostly write
    "pkg_install": "write",
    "heredoc": "write",      # tee/cat <<

    "build": "execute",
    "test": "execute",
    "lint": "execute",
    "script_run": "execute",
    "pkg_run": "execute",
    "container": "execute",  # docker/kubectl — mixed but execute-flavored
    "devenv": "execute",

    "other": "other",
    "empty": "other",
}


def bash_io_type(category):
    """Map bash category to read/write/execute/other."""
    return BASH_IO_MAP.get(category, "other")


def categorize_bash(cmd):
    """
    Return (category, has_chain, has_pipe, has_redirect).
    cmd: raw command string. Raw content is NOT returned — only classification.
    """
    if not cmd or not isinstance(cmd, str):
        return ("empty", False, False, False)
    c = cmd.strip()
    if not c:
        return ("empty", False, False, False)
    # Strip common prefix noise: `sudo `, `time `, `env VAR=x `
    while True:
        m = re.match(r"^(sudo|time|nohup|env\s+\w+=\S*)\s+", c)
        if not m: break
        c = c[m.end():]
    has_chain = bool(BASH_CHAIN_RE.search(c))
    has_pipe = bool(BASH_PIPE_RE.search(c))
    has_redir = bool(BASH_REDIR_RE.search(c))
    # Match categories in order
    for name, pat in BASH_CATEGORIES:
        if pat.match(c):
            return (name, has_chain, has_pipe, has_redir)
    return ("other", has_chain, has_pipe, has_redir)

MCP_LIST_VERBS = {
    "get_issues", "search_issues", "get_merge_requests", "search_merge_requests",
    "get_epics", "search_epics", "get_meeting_notes", "search_meeting_notes",
}
MCP_ENRICH_VERBS = {
    "get_issue", "get_issue_comments", "get_issue_relations",
    "get_merge_request_discussions", "get_merge_request_diffs",
    "get_meeting_transcript",
}
CONVERSATIONAL_TYPES = {"user", "assistant"}


def tool_category(name):
    if not name:
        return "unknown"
    m = MCP_VERB_RE.match(name)
    if m:
        verb = m.group(1)
        if verb in MCP_LIST_VERBS:
            return f"mcp_list:{verb}"
        if verb in MCP_ENRICH_VERBS:
            return f"mcp_enrich:{verb}"
        if verb.startswith(("create_", "update_", "add_", "link_", "unlink_",
                            "delete_", "remove_")):
            return f"mcp_write:{verb}"
        return f"mcp_other:{verb}"
    if name in ("Read", "Write", "Edit", "Glob", "Grep", "Bash",
                "TodoWrite", "NotebookEdit"):
        return f"builtin:{name}"
    if name in ("Agent", "Task"):
        return "builtin:subagent"
    if name in ("WebFetch", "WebSearch"):
        return f"builtin:{name}"
    if name.startswith("ToolSearch"):
        return "builtin:ToolSearch"
    return f"other:{name}"


def file_ext_category(path):
    """
    Extract file extension and classify by type. Takes only the extension,
    never the full path, to avoid leaking project structure.
    """
    if not path:
        return "", "unknown"
    # Get just the filename
    base = path.rsplit("/", 1)[-1]
    if "." not in base:
        return "", "no_ext"
    ext = base.rsplit(".", 1)[-1].lower()
    # Classify
    if ext in ("ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs", "go",
               "rb", "java", "kt", "swift", "c", "cpp", "cc", "h", "hpp",
               "cs", "scala", "clj", "ex", "exs"):
        return ext, "code"
    if ext in ("yaml", "yml", "toml", "json", "ini", "env", "conf",
               "properties", "xml"):
        return ext, "config"
    if ext in ("md", "mdx", "txt", "rst", "adoc"):
        return ext, "docs"
    if ext in ("test.ts", "spec.ts", "test.py", "test.js"):
        return ext, "test"
    # Check test suffix
    if "test" in base.lower() or "spec" in base.lower():
        return ext, "test"
    if ext in ("html", "css", "scss", "sass", "less"):
        return ext, "ui"
    if ext in ("sql",):
        return ext, "sql"
    if ext in ("sh", "bash", "zsh", "fish", "ps1"):
        return ext, "script"
    if ext in ("tf", "hcl", "nomad"):
        return ext, "infra"
    if ext in ("dockerfile",) or base.lower() == "dockerfile":
        return ext, "infra"
    if ext in ("lock", "sum"):
        return ext, "lockfile"
    return ext, "other"


def anonymize_tool_name(name):
    """
    Replace project slug between mcp__ and verb with a stable hash.
    mcp__<project-slug>__get_issues → mcp__p{hash6}__get_issues
    Non-MCP names pass through unchanged.
    """
    if not name:
        return ""
    m = MCP_VERB_RE.match(name)
    if not m:
        return name
    # Full name: mcp__<slug>__<verb>. Replace <slug> with stable hash.
    prefix_end = len("mcp__")
    verb_start = name.rfind("__")
    if verb_start < prefix_end:
        return name
    slug = name[prefix_end:verb_start]
    slug_hash = hashlib.sha1(slug.encode("utf-8")).hexdigest()[:6]
    verb = name[verb_start + 2:]
    return f"mcp__p{slug_hash}__{verb}"


# ─── Tool family / action / workflow_role taxonomy ────────────────────────
# Operates on the verb (post-MCP-prefix strip) and the tool name itself.

def _tool_verb(name):
    """Extract the verb from an MCP tool name, or return the name for builtins."""
    m = MCP_VERB_RE.match(name or "")
    return m.group(1) if m else (name or "")


def tool_family(name):
    """Product/functional family — agnostic of project-slug."""
    if not name:
        return "unknown"
    m = MCP_VERB_RE.match(name)
    if not m:
        # builtins
        if name in ("Read", "Write", "Edit", "NotebookEdit"):
            return "builtin:file"
        if name in ("Grep", "Glob") or name.startswith("ToolSearch"):
            return "builtin:search"
        if name == "Bash":
            return "builtin:exec"
        if name in ("Agent", "Task"):
            return "builtin:subagent"
        if name in ("WebFetch", "WebSearch"):
            return "builtin:web"
        if name in ("TodoWrite",) or name.startswith("Task"):
            return "builtin:mgmt"
        return f"other:{name}"
    verb = m.group(1)
    # DevBoy MCP families: match by verb regardless of project slug
    if "issue" in verb or "epic" in verb:
        return "devboy:issues"
    if "merge_request" in verb or verb in ("create_mr", "update_mr"):
        return "devboy:mrs"
    if "pipeline" in verb or verb.startswith("get_branch") or "job" in verb:
        return "devboy:ci"
    if "meeting" in verb or "transcript" in verb:
        return "devboy:meetings"
    if verb in ("get_project_info", "get_branch_info"):
        return "devboy:repo"
    # Non-devboy MCP
    slug = name[len("mcp__"):name.rfind("__")]
    if "playwright" in slug:
        return "mcp:browser"
    if "claude-in-chrome" in slug:
        return "mcp:browser"
    if "Gmail" in slug or "Calendar" in slug or "Drive" in slug:
        return "mcp:productivity"
    return "mcp:other"


def tool_action(name):
    """CRUD-like action the tool performs."""
    if not name:
        return "unknown"
    m = MCP_VERB_RE.match(name)
    if not m:
        if name in ("Read",): return "detail"
        if name in ("Write",): return "create"
        if name in ("Edit", "NotebookEdit"): return "update"
        if name in ("Grep", "Glob") or name.startswith("ToolSearch"):
            return "search"
        if name == "Bash": return "execute"
        if name in ("Agent", "Task"): return "delegate"
        if name in ("WebFetch",): return "fetch"
        if name in ("WebSearch",): return "search"
        if name == "TodoWrite": return "plan"
        return "other"
    verb = m.group(1)
    if verb.startswith("get_") and not any(k in verb for k in ("_issue", "_mr", "_meeting", "_project", "_branch", "_transcript")):
        return "detail"
    if verb.startswith("search_") or verb in MCP_LIST_VERBS:
        return "list"
    if verb.startswith(("get_issues", "get_merge_requests", "get_epics", "get_meeting_notes")):
        return "list"
    if "comment" in verb or "discussion" in verb:
        if verb.startswith(("get_", "search_")):
            return "detail"
        return "comment"
    if verb.startswith("create_"): return "create"
    if verb.startswith("update_"): return "update"
    if verb.startswith("add_"): return "comment" if "comment" in verb else "update"
    if verb.startswith("link_") or verb.startswith("unlink_"): return "link"
    if verb.startswith("delete_") or verb.startswith("remove_"): return "delete"
    if verb.startswith("get_"): return "detail"
    return "other"


def workflow_phase(family, action, name):
    """
    Higher-level phase classification:
    plan / research / implement / verify / deploy / collab / other
    """
    if name in ("TodoWrite", "TaskCreate", "TaskUpdate"):
        return "plan"
    if family in ("builtin:search",) or (family.startswith("devboy:") and action == "list"):
        return "research"
    if family == "builtin:file" and action == "detail":
        return "research"
    if family == "builtin:subagent":
        return "research"  # subagents usually spawn for research/exploration
    if family == "builtin:web":
        return "research"
    if family == "devboy:meetings":
        return "research"
    if family == "builtin:file" and action in ("create", "update"):
        return "implement"
    if family == "builtin:exec":
        return "execute"  # could be verify or deploy, disambiguated by bash category later
    if family == "devboy:ci":
        return "verify"
    if family.startswith("devboy:") and action == "create":
        return "deploy"
    if family.startswith("devboy:") and action in ("update", "comment"):
        return "collab"
    if family.startswith("devboy:") and action == "detail":
        return "research"
    return "other"


def workflow_role(family, action):
    """Role this tool plays in a typical agent loop."""
    if family.startswith("builtin:file") and action in ("update", "create"):
        return "implementation"
    if family == "builtin:file" and action == "detail":
        return "deep_dive"
    if family == "builtin:search":
        return "discovery"
    if family == "builtin:exec":
        return "execute"  # further classified via bash category
    if family == "builtin:subagent":
        return "delegate"
    if family == "builtin:web":
        return "research_external"
    if family == "builtin:mgmt":
        return "synthesis"
    if family in ("devboy:issues", "devboy:mrs"):
        if action == "list": return "discovery"
        if action == "detail": return "deep_dive"
        if action == "create": return "deployment"
        if action == "update": return "implementation"
        if action == "comment": return "collaboration"
        if action == "link": return "synthesis"
    if family == "devboy:ci":
        return "verification"
    if family == "devboy:meetings":
        return "research_external"
    if family == "devboy:repo":
        return "discovery"
    if family.startswith("mcp:browser"):
        return "interact_browser"
    return "other"


def parse_ts(rec):
    ts = rec.get("timestamp")
    if not ts:
        return None
    try:
        if ts.endswith("Z"):
            ts = ts[:-1] + "+00:00"
        return int(dt.datetime.fromisoformat(ts).timestamp() * 1000)
    except Exception:
        return None


def content_blocks(msg):
    content = msg.get("content")
    if isinstance(content, str):
        yield ("text", {"text": content})
    elif isinstance(content, list):
        for c in content:
            if isinstance(c, dict):
                yield (c.get("type", "unknown"), c)


def classify_user_turn(msg, is_subagent):
    """
    Classify a role=user message.
    Returns (kind, total_text_chars, full_text).
    full_text is used locally for feature extraction and is NEVER persisted.
    """
    has_tool_result = False
    text_parts = []
    for btype, b in content_blocks(msg):
        if btype == "tool_result":
            has_tool_result = True
        elif btype == "text":
            t = b.get("text", "") or ""
            text_parts.append(t)
    full_text = "\n".join(text_parts)
    total_text_chars = len(full_text)
    if has_tool_result:
        return "tool_result", total_text_chars, full_text
    if total_text_chars > 0:
        if is_subagent:
            if full_text.strip() == "Warmup":
                return "warmup", total_text_chars, full_text
            return "parent_prompt", total_text_chars, full_text
        return "human", total_text_chars, full_text
    return "other", total_text_chars, full_text


def extract_meta_event(rec, session_label, ts):
    """
    Return a meta_events row dict, or None if this record is not a meta event.
    Extracts ONLY structural fields — no raw content.
    """
    rtype = rec.get("type", "")
    if rtype in CONVERSATIONAL_TYPES:
        return None

    row = {
        "session": session_label,
        "ts_ms": ts,
        "event_type": rtype,
        "event_subtype": rec.get("subtype", "") or "",
        "is_sidechain": bool(rec.get("isSidechain", False)),
        "duration_ms": 0,
        "count": 0,
        "content_chars": 0,
        "hook_event": "",
        "hook_name": "",
        "attachment_type": "",
        "error_status": 0,
        "error_type": "",
        "retry_attempt": 0,
        "trigger": "",
    }

    if rtype == "progress":
        data = rec.get("data", {}) or {}
        row["hook_event"] = data.get("hookEvent", "") or ""
        row["hook_name"] = data.get("hookName", "") or ""
    elif rtype == "file-history-snapshot":
        snap = rec.get("snapshot", {}) or {}
        tracked = snap.get("trackedFileBackups", {}) or {}
        row["count"] = len(tracked) if isinstance(tracked, dict) else 0
    elif rtype == "summary":
        s = rec.get("summary", "") or ""
        row["content_chars"] = len(s)
    elif rtype == "system":
        sub = row["event_subtype"]
        if sub == "turn_duration":
            row["duration_ms"] = rec.get("durationMs", 0) or 0
            row["count"] = rec.get("messageCount", 0) or 0
        elif sub == "api_error":
            err = rec.get("error", {}) or {}
            row["error_status"] = err.get("status", 0) or 0
            row["retry_attempt"] = rec.get("retryAttempt", 0) or 0
            nested = err.get("error", {}) or {}
            inner = nested.get("error", {}) or {}
            row["error_type"] = inner.get("type", "") or ""
        elif sub == "stop_hook_summary":
            row["count"] = rec.get("hookCount", 0) or 0
            hooks = rec.get("hookInfos", []) or []
            total_dur = sum((h or {}).get("durationMs", 0) or 0 for h in hooks)
            row["duration_ms"] = total_dur
        elif sub == "away_summary":
            # User-typed content length only — never content itself
            c = rec.get("content", "") or ""
            row["content_chars"] = len(c)
        elif sub == "compact_boundary":
            # Also emitted as compaction row separately
            meta = rec.get("compactMetadata", {}) or {}
            row["trigger"] = meta.get("trigger", "") or ""
            row["count"] = meta.get("preTokens", 0) or 0
        # other system subtypes: bridge_status, local_command, etc.
    elif rtype == "attachment":
        att = rec.get("attachment", {}) or {}
        row["attachment_type"] = att.get("type", "") or ""
    elif rtype == "queue-operation":
        # User-typed content length only — never content itself
        c = rec.get("content", "") or ""
        row["content_chars"] = len(c)
        row["trigger"] = rec.get("operation", "") or ""

    return row


def collect_subagent_spawns(path):
    """
    Scan a main-session JSONL for Agent/Task tool_use calls.
    Returns a list of (ts_ms, ordinal, tool_use_id) for each spawn, in order.
    """
    spawns = []
    ordinal = 0
    try:
        lines = path.read_text(errors="ignore").splitlines()
    except Exception:
        return spawns
    for line in lines:
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        msg = rec.get("message", {})
        if msg.get("role") != "assistant":
            continue
        blocks = msg.get("content", [])
        if not isinstance(blocks, list):
            continue
        for b in blocks:
            if (isinstance(b, dict) and b.get("type") == "tool_use"
                    and b.get("name") in ("Agent", "Task")):
                ordinal += 1
                spawns.append((parse_ts(rec), ordinal, b.get("id", "")))
    return spawns


def analyze_file(path, session_label, is_subagent, parent_label):
    """
    Return (session_row, turn_rows, meta_rows, compaction_rows, bash_rows).
    bash_rows: one row per Bash tool_use with category + structural flags.
    """
    try:
        raw = path.read_text(errors="ignore").splitlines()
    except Exception:
        return None, [], [], [], [], [], []

    records = []
    for line in raw:
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    if not records:
        return None, [], [], [], [], [], []

    turn_rows, meta_rows, compaction_rows, bash_rows, tool_rows, human_rows = [], [], [], [], [], []

    # For linking tool_use to its tool_result (general table)
    # tool_use_id → index into tool_rows
    tool_pending = {}
    # tool_use_id → index into bash_rows (specialization)
    bash_pending = {}

    # Resume detection: track gaps between consecutive timestamps (any record type)
    resume_gaps_sec = []
    prev_any_ts = None
    RESUME_THRESHOLD_SEC = 30 * 60  # 30 minutes
    # LLM work time from system:turn_duration events
    llm_time_ms_total = 0
    # Interruption detection
    interruption_mid_loop = 0  # human turn while agent loop was active
    queue_events_count = 0     # user typed while agent running
    # Resume classification
    resume_followed_by_human_prompt = 0  # resume → immediate human turn
    resume_continuation = 0              # resume → agent just continues
    _pending_resume_check = False
    # Error tracking
    tool_error_count_session = 0

    human_turns = agent_turns = tool_result_turns = 0
    tool_calls_total = 0
    tool_cat_counts = Counter()
    in_tokens_list, out_tokens_list = [], []
    cache_read_list, cache_create_list = [], []
    context_size_list = []
    models_seen = Counter()
    first_human_chars = 0
    compaction_count = 0
    summary_count = 0
    progress_count = 0
    api_error_count = 0
    queue_count = 0
    away_count = 0
    file_snapshot_count = 0

    ts_first = None
    ts_last = None
    agent_loops_turns, agent_loops_calls, agent_loops_durations_ms = [], [], []
    cur_loop_turns = cur_loop_calls = 0
    cur_loop_start = cur_loop_last = None
    current_loop_id = 0
    prev_ts = None
    sidechain_count = 0
    cc_version = ""
    cwd_seen = ""

    for rec in records:
        if not cc_version:
            v = rec.get("version")
            if v: cc_version = v
        if not cwd_seen:
            c = rec.get("cwd")
            if c: cwd_seen = c

        # Parse timestamp early for gap detection
        _ts_now = parse_ts(rec)
        if _ts_now and prev_any_ts:
            gap_sec = (_ts_now - prev_any_ts) / 1000
            if gap_sec > RESUME_THRESHOLD_SEC:
                resume_gaps_sec.append(gap_sec)
                _pending_resume_check = True
        if _ts_now:
            prev_any_ts = _ts_now
        rtype = rec.get("type", "")
        ts = parse_ts(rec)
        if ts:
            if ts_first is None or ts < ts_first:
                ts_first = ts
            if ts_last is None or ts > ts_last:
                ts_last = ts
        is_sidechain = bool(rec.get("isSidechain", False))
        if is_sidechain:
            sidechain_count += 1

        # Meta event?
        if rtype not in CONVERSATIONAL_TYPES:
            meta_row = extract_meta_event(rec, session_label, ts)
            if meta_row:
                meta_rows.append(meta_row)
                if meta_row["event_type"] == "system" and \
                   meta_row["event_subtype"] == "turn_duration":
                    llm_time_ms_total += meta_row["duration_ms"]
            if rtype == "summary":
                summary_count += 1
            elif rtype == "progress":
                progress_count += 1
            elif rtype == "file-history-snapshot":
                file_snapshot_count += 1
            elif rtype == "queue-operation":
                queue_count += 1
            elif rtype == "system":
                sub = rec.get("subtype", "")
                if sub == "api_error":
                    api_error_count += 1
                elif sub == "away_summary":
                    away_count += 1
                elif sub == "compact_boundary":
                    compaction_count += 1
                    meta = rec.get("compactMetadata", {}) or {}
                    compaction_rows.append({
                        "session": session_label,
                        "ts_ms": ts,
                        "trigger": meta.get("trigger", "") or "",
                        "pre_tokens": meta.get("preTokens", 0) or 0,
                    })
            continue

        msg = rec.get("message", {})
        role = msg.get("role", "")

        turn_kind = None
        if role == "user":
            kind, user_text_chars, _user_text = classify_user_turn(msg, is_subagent)
            if kind in ("human", "parent_prompt"):
                turn_kind = kind
                if kind == "human":
                    human_turns += 1
                    is_mid_loop = False
                    # Mid-loop interruption: human appeared while agent loop was open
                    if cur_loop_turns > 0 and _ts_now and cur_loop_last:
                        if (_ts_now - cur_loop_last) / 1000 < 300:
                            interruption_mid_loop += 1
                            is_mid_loop = True
                    # Extract human-turn structural features (no text stored!)
                    feats = extract_human_features(_user_text)
                    intent = classify_human_intent(feats)
                    is_session_start = (human_turns == 1)
                    came_after_resume = _pending_resume_check  # about to be reset
                    human_rows.append({
                        "session": session_label,
                        "ts_ms": ts,
                        "turn_idx_in_session": human_turns,
                        "is_session_start": is_session_start,
                        "is_mid_loop_interrupt": is_mid_loop,
                        "after_resume": came_after_resume,
                        "intent": intent,
                        "length_bucket": feats["length_bucket"],
                        "chars": feats["chars"],
                        "lines": feats["lines"],
                        "has_question": feats["has_question"],
                        "has_code_block": feats["has_code_block"],
                        "has_url": feats["has_url"],
                        "has_issue_ref": feats["has_issue_ref"],
                        "has_file_path": feats["has_file_path"],
                        "has_negation": feats["has_negation"],
                        "has_urgency": feats["has_urgency"],
                        "has_approval": feats["has_approval"],
                        "has_error_report": feats["has_error_report"],
                        "has_please": feats["has_please"],
                        "has_list_markers": feats["has_list_markers"],
                        "list_item_count": feats["list_item_count"],
                        "url_count": feats["url_count"],
                        "question_count": feats["question_count"],
                    })
                # Resume classification
                if _pending_resume_check:
                    if kind == "human":
                        resume_followed_by_human_prompt += 1
                    _pending_resume_check = False
                if first_human_chars == 0:
                    first_human_chars = user_text_chars
                if cur_loop_turns > 0:
                    agent_loops_turns.append(cur_loop_turns)
                    agent_loops_calls.append(cur_loop_calls)
                    if cur_loop_start and cur_loop_last:
                        agent_loops_durations_ms.append(cur_loop_last - cur_loop_start)
                    current_loop_id += 1
                    cur_loop_turns = cur_loop_calls = 0
                    cur_loop_start = cur_loop_last = None
            elif kind == "tool_result":
                turn_kind = "tool_result"
                tool_result_turns += 1
                # Extract output chars for ALL tools (and Bash-specific)
                for btype, b in content_blocks(msg):
                    if btype == "tool_result":
                        tuid = b.get("tool_use_id", "")
                        is_error = bool(b.get("is_error", False))
                        result_content = b.get("content", "")
                        if isinstance(result_content, list):
                            out_chars = sum(
                                len(c.get("text", "") or "")
                                for c in result_content if isinstance(c, dict)
                            )
                        elif isinstance(result_content, str):
                            out_chars = len(result_content)
                        else:
                            out_chars = 0
                        if tuid in tool_pending:
                            idx = tool_pending[tuid]
                            tool_rows[idx]["output_chars"] = out_chars
                            tool_rows[idx]["is_error"] = is_error
                            tu_ts = tool_rows[idx]["ts_ms"]
                            if ts and tu_ts:
                                tool_rows[idx]["duration_ms"] = ts - tu_ts
                            del tool_pending[tuid]
                        if tuid in bash_pending:
                            bash_rows[bash_pending[tuid]]["output_chars"] = out_chars
                            bash_rows[bash_pending[tuid]]["is_error"] = is_error
                            # Try to extract exit code from result text
                            exit_code = None
                            if isinstance(result_content, list):
                                for c in result_content:
                                    if isinstance(c, dict):
                                        t = c.get("text", "") or ""
                                        em = re.search(r"exit code[:\s]+(\-?\d+)", t[:500], re.IGNORECASE)
                                        if em:
                                            exit_code = int(em.group(1))
                                            break
                            bash_rows[bash_pending[tuid]]["exit_code"] = exit_code
                            del bash_pending[tuid]
            elif kind == "warmup":
                turn_kind = "warmup"
            else:
                continue
        elif role == "assistant":
            turn_kind = "agent"
            agent_turns += 1
            cur_loop_turns += 1
            if cur_loop_start is None:
                cur_loop_start = ts
            if ts:
                cur_loop_last = ts
            # Resume followed by agent (continuation, no human prompt)
            if _pending_resume_check:
                resume_continuation += 1
                _pending_resume_check = False
        else:
            continue

        in_tok = out_tok = cread = ccreate = 0
        text_chars = tool_calls_in_turn = 0
        first_tool_cat = ""
        model = ""
        ephemeral_1h = ephemeral_5m = 0
        context_size = 0

        if turn_kind == "agent":
            usage = msg.get("usage") or {}
            in_tok = usage.get("input_tokens") or 0
            out_tok = usage.get("output_tokens") or 0
            cread = usage.get("cache_read_input_tokens") or 0
            ccreate = usage.get("cache_creation_input_tokens") or 0
            cache_detail = usage.get("cache_creation", {}) or {}
            ephemeral_1h = cache_detail.get("ephemeral_1h_input_tokens", 0) or 0
            ephemeral_5m = cache_detail.get("ephemeral_5m_input_tokens", 0) or 0
            context_size = in_tok + cread + ccreate
            model = msg.get("model", "") or ""
            if in_tok: in_tokens_list.append(in_tok)
            if out_tok: out_tokens_list.append(out_tok)
            if cread: cache_read_list.append(cread)
            if ccreate: cache_create_list.append(ccreate)
            if context_size: context_size_list.append(context_size)
            if model: models_seen[model] += 1
            for btype, b in content_blocks(msg):
                if btype == "text":
                    text_chars += len(b.get("text", "") or "")
                elif btype == "tool_use":
                    name = b.get("name", "")
                    cat = tool_category(name)
                    tool_cat_counts[cat] += 1
                    tool_calls_total += 1
                    tool_calls_in_turn += 1
                    cur_loop_calls += 1
                    if not first_tool_cat:
                        first_tool_cat = cat
                    # General tool_calls table — input size + link for response
                    inp = b.get("input", {}) or {}
                    try:
                        input_json_chars = len(json.dumps(inp, ensure_ascii=False))
                    except Exception:
                        input_json_chars = 0
                    tool_use_id = b.get("id", "")
                    tool_pending[tool_use_id] = len(tool_rows)
                    fam = tool_family(name)
                    act = tool_action(name)
                    role = workflow_role(fam, act)
                    phase = workflow_phase(fam, act, name)
                    # Extract file extension for Edit/Write (never the path itself)
                    file_ext = ""
                    file_type = ""
                    if name in ("Edit", "Write", "NotebookEdit", "Read"):
                        fpath = inp.get("file_path", "") or inp.get("notebook_path", "")
                        file_ext, file_type = file_ext_category(fpath)
                    tool_rows.append({
                        "session": session_label,
                        "ts_ms": ts,
                        "tool_name": name,
                        "tool_name_anon": anonymize_tool_name(name),
                        "tool_category": cat,
                        "tool_family": fam,
                        "tool_action": act,
                        "workflow_role": role,
                        "workflow_phase": phase,
                        "file_ext": file_ext,
                        "file_type": file_type,
                        "input_chars": input_json_chars,
                        "output_chars": 0,
                        "duration_ms": 0,
                        "is_error": False,
                        "loop_id": current_loop_id,
                    })
                    # Bash: compound-command analysis (never store raw command)
                    if b.get("name") == "Bash":
                        inp = b.get("input", {}) or {}
                        cmd = inp.get("command", "") or ""
                        # Old single-token categorizer (kept for backwards compat)
                        bcat, _hc, _hp, _hr = categorize_bash(cmd)
                        # New compound-aware analysis
                        primary_io, subcats, has_chain, has_pipe, has_redir = \
                            analyze_bash_compound(cmd)
                        git_sub = git_subcategory(cmd)
                        cmd_len = len(cmd)
                        tool_use_id = b.get("id", "")
                        bash_pending[tool_use_id] = len(bash_rows)
                        bash_rows.append({
                            "session": session_label,
                            "ts_ms": ts,
                            "category": bcat,
                            "io_type": primary_io,
                            "subcategories": subcats,
                            "git_subcategory": git_sub,
                            "cmd_chars": cmd_len,
                            "has_chain": has_chain,
                            "has_pipe": has_pipe,
                            "has_redirect": has_redir,
                            "output_chars": 0,
                            "exit_code": None,
                            "is_error": False,
                        })
        elif turn_kind == "human":
            for btype, b in content_blocks(msg):
                if btype == "text":
                    text_chars += len(b.get("text", "") or "")

        latency_ms = (ts - prev_ts) if (ts and prev_ts) else None
        if ts:
            prev_ts = ts

        turn_rows.append({
            "session": session_label,
            "ts_ms": ts,
            "turn_kind": turn_kind,
            "is_subagent_session": is_subagent,
            "is_sidechain": is_sidechain,
            "loop_id": current_loop_id,
            "loop_turn_idx": (cur_loop_turns - 1) if turn_kind == "agent" else None,
            "tool_calls_in_turn": tool_calls_in_turn,
            "first_tool_category": first_tool_cat,
            "text_chars": text_chars,
            "in_tokens": in_tok,
            "out_tokens": out_tok,
            "cache_read": cread,
            "cache_create": ccreate,
            "ephemeral_1h_tokens": ephemeral_1h,
            "ephemeral_5m_tokens": ephemeral_5m,
            "context_size_tokens": context_size,
            "model": model,
            "latency_since_prev_ms": latency_ms,
        })

    if cur_loop_turns > 0:
        agent_loops_turns.append(cur_loop_turns)
        agent_loops_calls.append(cur_loop_calls)
        if cur_loop_start and cur_loop_last:
            agent_loops_durations_ms.append(cur_loop_last - cur_loop_start)

    def safe(fn, seq, default=0):
        return fn(seq) if seq else default

    # Count tool errors (is_error=true) from tool_rows
    tool_error_count_session = sum(1 for t in tool_rows if t.get("is_error"))
    bash_error_count = sum(1 for b in bash_rows if b.get("is_error"))

    duration_sec_raw = ((ts_last - ts_first) / 1000) if (ts_first and ts_last) else 0
    # Robust gap detection: re-sort all seen timestamps and recompute gaps.
    # (Files occasionally have records out of chronological order, which would
    # produce phantom gaps in a pure-sequential scan.)
    all_timestamps = sorted(
        parse_ts(r) for r in records if parse_ts(r) is not None
    )
    real_gaps_sec = []
    for i in range(1, len(all_timestamps)):
        g = (all_timestamps[i] - all_timestamps[i-1]) / 1000
        if g > RESUME_THRESHOLD_SEC:
            real_gaps_sec.append(g)
    resume_gaps_sec = real_gaps_sec  # override with sorted version
    idle_sec = sum(resume_gaps_sec)
    active_sec = max(0, duration_sec_raw - idle_sec)
    llm_time_sec = llm_time_ms_total / 1000
    # Bash aggregates
    bash_total_out_chars = sum(b["output_chars"] for b in bash_rows)
    bash_total = len(bash_rows)
    bash_avg_out = int(bash_total_out_chars / bash_total) if bash_total else 0
    top_tools = tool_cat_counts.most_common(5)
    hour_utc = dow_utc = None
    if ts_first:
        d = dt.datetime.fromtimestamp(ts_first / 1000, tz=dt.timezone.utc)
        hour_utc, dow_utc = d.hour, d.weekday()

    # Context-size stats across assistant turns
    ctx_max = safe(max, context_size_list)
    ctx_mean = round(safe(mean, context_size_list))
    ctx_median = round(safe(median, context_size_list))

    # Compute year_week for bucketing
    year_week = ""
    if ts_first:
        d = dt.datetime.fromtimestamp(ts_first / 1000, tz=dt.timezone.utc)
        iso = d.isocalendar()
        year_week = f"{iso[0]}-W{iso[1]:02d}"

    session_row = {
        "session": session_label,
        "is_subagent": is_subagent,
        "parent_session": parent_label or "",
        "root_session": "",   # filled in second pass
        "parent_turn_ts_ms": None,    # filled in second pass
        "parent_tool_use_ordinal": 0, # filled in second pass
        "subagent_depth": 0,  # filled in second pass (0=root, 1=sub of root, 2=sub of sub, ...)
        "claude_code_version": cc_version,
        "project_hash": project_hash(cwd_seen),
        "year_week": year_week,
        "ts_start_ms": ts_first,
        "ts_end_ms": ts_last,
        "duration_sec": round(duration_sec_raw, 1),
        "duration_active_sec": round(active_sec, 1),
        "duration_idle_sec": round(idle_sec, 1),
        "llm_work_sec": round(llm_time_sec, 1),
        "resume_count": len(resume_gaps_sec),
        "longest_gap_sec": round(max(resume_gaps_sec) if resume_gaps_sec else 0, 1),
        "bash_count": bash_total,
        "bash_output_chars_total": bash_total_out_chars,
        "bash_avg_output_chars": bash_avg_out,
        "hour_utc": hour_utc,
        "dow_utc": dow_utc,
        "total_records": len(records),
        "human_turns": human_turns,
        "agent_turns": agent_turns,
        "tool_result_turns": tool_result_turns,
        "sidechain_turns": sidechain_count,
        "tool_calls_total": tool_calls_total,
        "agent_loops": len(agent_loops_turns),
        "loop_turns_mean": round(safe(mean, agent_loops_turns), 2),
        "loop_turns_median": safe(median, agent_loops_turns),
        "loop_turns_max": safe(max, agent_loops_turns),
        "loop_calls_mean": round(safe(mean, agent_loops_calls), 2),
        "loop_calls_median": safe(median, agent_loops_calls),
        "loop_calls_max": safe(max, agent_loops_calls),
        "loop_duration_mean_sec": round(safe(mean, agent_loops_durations_ms) / 1000, 1),
        "loop_duration_max_sec": round(safe(max, agent_loops_durations_ms) / 1000, 1),
        "in_tokens_mean": round(safe(mean, in_tokens_list)),
        "out_tokens_mean": round(safe(mean, out_tokens_list)),
        "cache_read_mean": round(safe(mean, cache_read_list)),
        "cache_create_mean": round(safe(mean, cache_create_list)),
        "in_tokens_total": sum(in_tokens_list),
        "out_tokens_total": sum(out_tokens_list),
        "cache_read_total": sum(cache_read_list),
        "cache_create_total": sum(cache_create_list),
        "context_size_max": ctx_max,
        "context_size_mean": ctx_mean,
        "context_size_median": ctx_median,
        "top_model": models_seen.most_common(1)[0][0] if models_seen else "",
        "models_count": len(models_seen),
        "top_tool_categories": "|".join(f"{k}={v}" for k, v in top_tools),
        "first_human_turn_chars": first_human_chars,
        "compaction_count": compaction_count,
        "summary_count": summary_count,
        "progress_count": progress_count,
        "file_snapshot_count": file_snapshot_count,
        "api_error_count": api_error_count,
        "queue_operation_count": queue_count,
        "away_summary_count": away_count,
        "interruption_mid_loop_count": interruption_mid_loop,
        "resume_with_prompt_count": resume_followed_by_human_prompt,
        "resume_continuation_count": resume_continuation,
        "tool_error_count": tool_error_count_session,
        "bash_error_count": bash_error_count,
    }
    return session_row, turn_rows, meta_rows, compaction_rows, bash_rows, tool_rows, human_rows


def write_parquet(rows, schema, path):
    if not rows:
        return 0
    pq.write_table(pa.Table.from_pylist(rows, schema=schema), path, compression="zstd")
    return len(rows)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out-dir", default="/tmp/claude_analysis")
    ap.add_argument("--root", default=str(Path.home() / ".claude/projects"))
    args = ap.parse_args()
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    main_files, subagent_files = [], []
    for p in Path(args.root).rglob("*.jsonl"):
        (subagent_files if "/subagents/" in str(p) else main_files).append(p)
    main_files.sort()
    subagent_files.sort()
    print(f"Main sessions: {len(main_files)}  Subagent sessions: {len(subagent_files)}",
          file=sys.stderr)

    main_labels = OrderedDict()
    for p in main_files:
        main_labels[p.stem] = f"s{len(main_labels)+1:04d}"

    def subagent_parent(p):
        try:
            return main_labels.get(p.parent.parent.name, "")
        except Exception:
            return ""

    session_rows, turn_rows, meta_rows, compaction_rows, bash_rows, tool_rows, human_rows = [], [], [], [], [], [], []
    total = len(main_files) + len(subagent_files)
    i = 0
    for p in main_files:
        i += 1
        res = analyze_file(p, main_labels[p.stem], False, "")
        srow, trows, mrows, crows, brows, trs, hrs = res
        if srow:
            session_rows.append(srow)
            turn_rows.extend(trows)
            meta_rows.extend(mrows)
            compaction_rows.extend(crows)
            bash_rows.extend(brows)
            tool_rows.extend(trs)
            human_rows.extend(hrs)
        if i % 500 == 0:
            print(f"  {i}/{total}", file=sys.stderr)

    sub_counter = 0
    subagent_paths_by_label = {}
    for p in subagent_files:
        i += 1
        sub_counter += 1
        label = f"a{sub_counter:04d}"
        subagent_paths_by_label[label] = p
        res = analyze_file(p, label, True, subagent_parent(p))
        srow, trows, mrows, crows, brows, trs, hrs = res
        if srow:
            session_rows.append(srow)
            turn_rows.extend(trows)
            meta_rows.extend(mrows)
            compaction_rows.extend(crows)
            bash_rows.extend(brows)
            tool_rows.extend(trs)
            human_rows.extend(hrs)
        if i % 500 == 0:
            print(f"  {i}/{total}", file=sys.stderr)

    # ─── Second pass: link subagents to their parent Agent/Task tool_use ───
    # Build a map from main session UUID → list of (ts_ms, ordinal) spawn points.
    # Then for each subagent, find the closest spawn in its parent.
    print("Linking subagent → parent tool_use…", file=sys.stderr)
    main_path_by_uuid = {p.stem: p for p in main_files}
    parent_spawns = {}  # main_label → list of (ts_ms, ordinal)

    # Group subagents by their parent main session
    subs_by_parent = {}
    for row in session_rows:
        if row["is_subagent"] and row["parent_session"]:
            subs_by_parent.setdefault(row["parent_session"], []).append(row)

    # For each parent that has subagents, collect its Agent/Task spawns
    for row in session_rows:
        if not row["is_subagent"] and row["session"] in subs_by_parent:
            # Find the original UUID for this label
            uuid = None
            for u, lbl in main_labels.items():
                if lbl == row["session"]:
                    uuid = u; break
            if uuid and uuid in main_path_by_uuid:
                parent_spawns[row["session"]] = collect_subagent_spawns(main_path_by_uuid[uuid])

    # Match each subagent to nearest spawn by ts_start
    for parent_label, subs in subs_by_parent.items():
        spawns = parent_spawns.get(parent_label, [])
        for sub in subs:
            if sub["ts_start_ms"] is None or not spawns:
                continue
            # Find spawn with closest ts_ms
            best = None
            best_dist = None
            for spawn_ts, ordinal, tool_use_id in spawns:
                if spawn_ts is None: continue
                d = abs(spawn_ts - sub["ts_start_ms"])
                if best_dist is None or d < best_dist:
                    best_dist = d
                    best = (spawn_ts, ordinal, tool_use_id)
            if best and best_dist is not None and best_dist < 300_000:  # within 5 min
                sub["parent_turn_ts_ms"] = best[0]
                sub["parent_tool_use_ordinal"] = best[1]

    # Compute root_session (follow parent chain up) and depth
    sessions_by_label = {r["session"]: r for r in session_rows}
    for row in session_rows:
        # Walk up
        depth = 0
        cur = row
        while cur["is_subagent"] and cur["parent_session"]:
            nxt = sessions_by_label.get(cur["parent_session"])
            if nxt is None or nxt["session"] == cur["session"]:
                break
            depth += 1
            cur = nxt
            if depth > 10:
                break
        row["subagent_depth"] = depth
        row["root_session"] = cur["session"]

    sessions_schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("is_subagent", pa.bool_()),
        pa.field("parent_session", pa.string()),
        pa.field("root_session", pa.string()),
        pa.field("parent_turn_ts_ms", pa.int64()),
        pa.field("parent_tool_use_ordinal", pa.int32()),
        pa.field("subagent_depth", pa.int32()),
        pa.field("claude_code_version", pa.string()),
        pa.field("project_hash", pa.string()),
        pa.field("year_week", pa.string()),
        pa.field("ts_start_ms", pa.int64()),
        pa.field("ts_end_ms", pa.int64()),
        pa.field("duration_sec", pa.float64()),
        pa.field("duration_active_sec", pa.float64()),
        pa.field("duration_idle_sec", pa.float64()),
        pa.field("llm_work_sec", pa.float64()),
        pa.field("resume_count", pa.int32()),
        pa.field("longest_gap_sec", pa.float64()),
        pa.field("bash_count", pa.int32()),
        pa.field("bash_output_chars_total", pa.int64()),
        pa.field("bash_avg_output_chars", pa.int32()),
        pa.field("hour_utc", pa.int32()),
        pa.field("dow_utc", pa.int32()),
        pa.field("total_records", pa.int32()),
        pa.field("human_turns", pa.int32()),
        pa.field("agent_turns", pa.int32()),
        pa.field("tool_result_turns", pa.int32()),
        pa.field("sidechain_turns", pa.int32()),
        pa.field("tool_calls_total", pa.int32()),
        pa.field("agent_loops", pa.int32()),
        pa.field("loop_turns_mean", pa.float32()),
        pa.field("loop_turns_median", pa.float32()),
        pa.field("loop_turns_max", pa.int32()),
        pa.field("loop_calls_mean", pa.float32()),
        pa.field("loop_calls_median", pa.float32()),
        pa.field("loop_calls_max", pa.int32()),
        pa.field("loop_duration_mean_sec", pa.float64()),
        pa.field("loop_duration_max_sec", pa.float64()),
        pa.field("in_tokens_mean", pa.int32()),
        pa.field("out_tokens_mean", pa.int32()),
        pa.field("cache_read_mean", pa.int32()),
        pa.field("cache_create_mean", pa.int32()),
        pa.field("in_tokens_total", pa.int64()),
        pa.field("out_tokens_total", pa.int64()),
        pa.field("cache_read_total", pa.int64()),
        pa.field("cache_create_total", pa.int64()),
        pa.field("context_size_max", pa.int32()),
        pa.field("context_size_mean", pa.int32()),
        pa.field("context_size_median", pa.int32()),
        pa.field("top_model", pa.string()),
        pa.field("models_count", pa.int32()),
        pa.field("top_tool_categories", pa.string()),
        pa.field("first_human_turn_chars", pa.int32()),
        pa.field("compaction_count", pa.int32()),
        pa.field("summary_count", pa.int32()),
        pa.field("progress_count", pa.int32()),
        pa.field("file_snapshot_count", pa.int32()),
        pa.field("api_error_count", pa.int32()),
        pa.field("queue_operation_count", pa.int32()),
        pa.field("away_summary_count", pa.int32()),
        pa.field("interruption_mid_loop_count", pa.int32()),
        pa.field("resume_with_prompt_count", pa.int32()),
        pa.field("resume_continuation_count", pa.int32()),
        pa.field("tool_error_count", pa.int32()),
        pa.field("bash_error_count", pa.int32()),
    ])
    turns_schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("ts_ms", pa.int64()),
        pa.field("turn_kind", pa.string()),
        pa.field("is_subagent_session", pa.bool_()),
        pa.field("is_sidechain", pa.bool_()),
        pa.field("loop_id", pa.int32()),
        pa.field("loop_turn_idx", pa.int32()),
        pa.field("tool_calls_in_turn", pa.int32()),
        pa.field("first_tool_category", pa.string()),
        pa.field("text_chars", pa.int32()),
        pa.field("in_tokens", pa.int32()),
        pa.field("out_tokens", pa.int32()),
        pa.field("cache_read", pa.int32()),
        pa.field("cache_create", pa.int32()),
        pa.field("ephemeral_1h_tokens", pa.int32()),
        pa.field("ephemeral_5m_tokens", pa.int32()),
        pa.field("context_size_tokens", pa.int32()),
        pa.field("model", pa.string()),
        pa.field("latency_since_prev_ms", pa.int64()),
    ])
    meta_schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("ts_ms", pa.int64()),
        pa.field("event_type", pa.string()),
        pa.field("event_subtype", pa.string()),
        pa.field("is_sidechain", pa.bool_()),
        pa.field("duration_ms", pa.int64()),
        pa.field("count", pa.int64()),
        pa.field("content_chars", pa.int32()),
        pa.field("hook_event", pa.string()),
        pa.field("hook_name", pa.string()),
        pa.field("attachment_type", pa.string()),
        pa.field("error_status", pa.int32()),
        pa.field("error_type", pa.string()),
        pa.field("retry_attempt", pa.int32()),
        pa.field("trigger", pa.string()),
    ])
    compactions_schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("ts_ms", pa.int64()),
        pa.field("trigger", pa.string()),
        pa.field("pre_tokens", pa.int32()),
    ])
    bash_schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("ts_ms", pa.int64()),
        pa.field("category", pa.string()),
        pa.field("io_type", pa.string()),
        pa.field("subcategories", pa.string()),
        pa.field("git_subcategory", pa.string()),
        pa.field("cmd_chars", pa.int32()),
        pa.field("has_chain", pa.bool_()),
        pa.field("has_pipe", pa.bool_()),
        pa.field("has_redirect", pa.bool_()),
        pa.field("output_chars", pa.int32()),
        pa.field("exit_code", pa.int32()),
        pa.field("is_error", pa.bool_()),
    ])
    tool_calls_schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("ts_ms", pa.int64()),
        pa.field("tool_name", pa.string()),        # raw (internal only — strip before export)
        pa.field("tool_name_anon", pa.string()),   # safe: project slug hashed
        pa.field("tool_category", pa.string()),
        pa.field("tool_family", pa.string()),
        pa.field("tool_action", pa.string()),
        pa.field("workflow_role", pa.string()),
        pa.field("workflow_phase", pa.string()),
        pa.field("file_ext", pa.string()),
        pa.field("file_type", pa.string()),
        pa.field("input_chars", pa.int32()),
        pa.field("output_chars", pa.int32()),
        pa.field("duration_ms", pa.int64()),
        pa.field("is_error", pa.bool_()),
        pa.field("loop_id", pa.int32()),
    ])

    ns = write_parquet(session_rows, sessions_schema, out_dir / "sessions.parquet")
    nt = write_parquet(turn_rows, turns_schema, out_dir / "turns.parquet")
    nm = write_parquet(meta_rows, meta_schema, out_dir / "meta_events.parquet")
    nc = write_parquet(compaction_rows, compactions_schema, out_dir / "compactions.parquet")
    nb = write_parquet(bash_rows, bash_schema, out_dir / "bash_commands.parquet")
    ntc = write_parquet(tool_rows, tool_calls_schema, out_dir / "tool_calls.parquet")

    human_schema = pa.schema([
        pa.field("session", pa.string()),
        pa.field("ts_ms", pa.int64()),
        pa.field("turn_idx_in_session", pa.int32()),
        pa.field("is_session_start", pa.bool_()),
        pa.field("is_mid_loop_interrupt", pa.bool_()),
        pa.field("after_resume", pa.bool_()),
        pa.field("intent", pa.string()),
        pa.field("length_bucket", pa.string()),
        pa.field("chars", pa.int32()),
        pa.field("lines", pa.int32()),
        pa.field("has_question", pa.bool_()),
        pa.field("has_code_block", pa.bool_()),
        pa.field("has_url", pa.bool_()),
        pa.field("has_issue_ref", pa.bool_()),
        pa.field("has_file_path", pa.bool_()),
        pa.field("has_negation", pa.bool_()),
        pa.field("has_urgency", pa.bool_()),
        pa.field("has_approval", pa.bool_()),
        pa.field("has_error_report", pa.bool_()),
        pa.field("has_please", pa.bool_()),
        pa.field("has_list_markers", pa.bool_()),
        pa.field("list_item_count", pa.int32()),
        pa.field("url_count", pa.int32()),
        pa.field("question_count", pa.int32()),
    ])
    nh = write_parquet(human_rows, human_schema, out_dir / "human_turns.parquet")
    print(f"sessions.parquet:       {ns} rows", file=sys.stderr)
    print(f"turns.parquet:          {nt} rows", file=sys.stderr)
    print(f"meta_events.parquet:    {nm} rows", file=sys.stderr)
    print(f"compactions.parquet:    {nc} rows", file=sys.stderr)
    print(f"bash_commands.parquet:  {nb} rows", file=sys.stderr)
    print(f"tool_calls.parquet:     {ntc} rows", file=sys.stderr)
    print(f"human_turns.parquet:    {nh} rows", file=sys.stderr)


if __name__ == "__main__":
    main()
