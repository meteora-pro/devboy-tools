"""Issue #235 — MCP-mediated MR/PR/issue creation must count toward
`mr_create` / `pr_create` / `issue_create` / `comments` aggregates.

Regression: only Bash-CLI variants (`gh pr create` / `glab mr create`)
were counted, so any session that used `mcp__<server>__create_*`
silently produced 0 PR/MR creates and the DORA radar undercounted.
"""
from __future__ import annotations

import sys
from dataclasses import dataclass
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SKILL = _HERE.parent
sys.path.insert(0, str(_SKILL))
sys.path.insert(0, str(_SKILL / "scripts"))

from extract_outputs import _aggregate  # noqa: E402


@dataclass
class _Ev:
    """Minimal stand-in for the real `Event` ABC the parser yields —
    `_aggregate` only reads `.type` and `.tool_uses`."""

    type: str
    tool_uses: list[dict]


def _tool(name: str, **input_kw) -> dict:
    return {"name": name, "input": input_kw}


def test_mcp_mr_create_counted():
    events = [
        _Ev("assistant", [_tool("mcp__devboy-prod__create_merge_request",
                                title="ship", source_branch="feat/x")]),
        _Ev("assistant", [_tool("mcp__dev-boy-shiftgears__create_merge_request",
                                title="x")]),
        _Ev("assistant", [_tool("mcp__some__open_merge_request", title="y")]),
        _Ev("assistant", [_tool("mcp__some__submit_merge_request", title="z")]),
    ]
    agg = _aggregate(events)
    assert agg["mr_create"] == 4, agg
    assert agg["pr_create"] == 0, agg


def test_mcp_pr_create_counted():
    events = [
        _Ev("assistant", [_tool("mcp__github__create_pull_request",
                                title="x", body="y")]),
        _Ev("assistant", [_tool("mcp__gh-internal__open_pull_request",
                                title="z")]),
    ]
    agg = _aggregate(events)
    assert agg["pr_create"] == 2, agg
    assert agg["mr_create"] == 0, agg


def test_mcp_issue_create_counted():
    events = [
        _Ev("assistant", [_tool("mcp__jira__create_issue", summary="bug")]),
        _Ev("assistant", [_tool("mcp__github__open_issue", title="bug")]),
    ]
    agg = _aggregate(events)
    assert agg["issue_create"] == 2, agg


def test_mcp_comment_variants_counted():
    events = [
        _Ev("assistant", [_tool("mcp__devboy-prod__create_merge_request_comment",
                                body="lgtm")]),
        _Ev("assistant", [_tool("mcp__github__create_pull_request_comment",
                                body="nit")]),
        _Ev("assistant", [_tool("mcp__jira__create_issue_comment", body="t")]),
        _Ev("assistant", [_tool("mcp__github__add_pull_request_comment",
                                body="x")]),
        # GitHub's inline-review comments end in `_pull_request_review_comment`;
        # they're still discussion comments and should roll up here.
        _Ev("assistant", [_tool("mcp__github__create_pull_request_review_comment",
                                body="inline")]),
    ]
    agg = _aggregate(events)
    assert agg["comments"] == 5, agg
    # Comments must NOT be miscounted as creates — anchor regression.
    assert agg["mr_create"] == 0, agg
    assert agg["pr_create"] == 0, agg


def test_anchor_does_not_overmatch_comment_variants():
    """Critical regression guard for the create regexes. They anchor on
    `_merge_request$` / `_pull_request$` (the bare nouns), so a sibling
    tool whose name extends past the create noun must NOT increment
    `mr_create` / `pr_create`."""
    events = [
        _Ev("assistant", [_tool("mcp__x__create_merge_request_comment", body="x")]),
        _Ev("assistant", [_tool("mcp__x__create_pull_request_review_comment", body="y")]),
    ]
    agg = _aggregate(events)
    assert agg["mr_create"] == 0, "anchor failure — comment counted as MR"
    assert agg["pr_create"] == 0, "anchor failure — comment counted as PR"


def test_bash_cli_still_counted():
    """Pre-#235 behaviour preserved — Bash CLI still increments."""
    events = [
        _Ev("assistant", [_tool("Bash", command="gh pr create --title x")]),
        _Ev("assistant", [_tool("Bash", command="glab mr create --title y")]),
        _Ev("assistant", [_tool("Bash", command="gh issue create --title z")]),
        _Ev("assistant", [_tool("Bash", command="gh pr comment 12 --body x")]),
        _Ev("assistant", [_tool("Bash", command="glab mr note 4 --message y")]),
    ]
    agg = _aggregate(events)
    assert agg["pr_create"] == 1, agg
    assert agg["mr_create"] == 1, agg
    assert agg["issue_create"] == 1, agg
    assert agg["comments"] == 2, agg


def test_mixed_bash_and_mcp_summed():
    events = [
        _Ev("assistant", [_tool("Bash", command="gh pr create --title cli")]),
        _Ev("assistant", [_tool("mcp__github__create_pull_request", title="mcp")]),
        _Ev("assistant", [_tool("mcp__devboy__create_merge_request", title="m")]),
        _Ev("assistant", [_tool("Bash", command="glab mr create --title cli")]),
    ]
    agg = _aggregate(events)
    assert agg["pr_create"] == 2, agg
    assert agg["mr_create"] == 2, agg
