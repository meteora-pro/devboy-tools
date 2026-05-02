#!/usr/bin/env python3
"""
Drift gate: ensure docs/guide/index.md is the README rendered for the docs site.

The site landing page is generated from README.md by this script. If they
fall out of sync, the gate prints a diff and exits non-zero so CI catches
it before reviewers do. To re-sync:

    python3 scripts/check-readme-sync.py --write

Strategy:
- Read README.md verbatim.
- Rewrite repo-relative links so they work from inside the Rspress site
  (`docs/guide/index.md`).
- Prepend a frontmatter block.
- Compare the result to the committed `docs/guide/index.md`.
"""

from __future__ import annotations

import argparse
import difflib
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
GH_BASE = "https://github.com/meteora-pro/devboy-tools/blob/main"

FRONTMATTER = """---
title: DevBoy tools
description: A research-driven tool bundle for AI coding agents — MCP, CLI, or skills. GitHub, GitLab, Jira, ClickUp, Slack, Fireflies. Token-aware pipeline backed by four research papers.
---

"""


def render_from_readme(readme_text: str) -> str:
    text = readme_text

    # docs/guide/* → ./* (relative within site)
    #
    # Strip the trailing `.md` extension on the way through: Rspress (and
    # most static-site routers) serve `./quick-start` and 404 on
    # `./quick-start.md`. The rest of the guide already uses the
    # extensionless form, so emit consistent canonical URLs from the
    # README sync. Anchors and trailing slashes are preserved.
    def site_link(match: re.Match[str]) -> str:
        path = match.group(1)
        # Split off `?query` / `#anchor` if any.
        suffix = ""
        for sep in ("#", "?"):
            if sep in path:
                head, _, tail = path.partition(sep)
                path, suffix = head, sep + tail
                break
        if path.endswith(".md"):
            path = path[: -len(".md")]
        return f"](./{path}{suffix})"

    text = re.sub(r"\]\(docs/guide/([^)]+)\)", site_link, text)
    # Other docs/* → absolute GitHub
    text = re.sub(r"\]\(docs/", f"]({GH_BASE}/docs/", text)
    # crates/* → absolute GitHub
    text = re.sub(r"\]\(crates/", f"]({GH_BASE}/crates/", text)
    # CONTRIBUTING / LICENSE → absolute GitHub
    text = re.sub(r"\]\(CONTRIBUTING\.md\)", f"]({GH_BASE}/CONTRIBUTING.md)", text)
    text = re.sub(r"\]\(LICENSE\)", f"]({GH_BASE}/LICENSE)", text)
    # .claude/* → absolute GitHub (analyze-usage backend lives here)
    text = re.sub(r"\]\(\./\.claude/", f"]({GH_BASE}/.claude/", text)
    text = re.sub(r"\]\(\.claude/", f"]({GH_BASE}/.claude/", text)
    # skills/* → absolute GitHub
    text = re.sub(r"\]\(skills/", f"]({GH_BASE}/skills/", text)
    return FRONTMATTER + text


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--write",
        action="store_true",
        help="Re-write docs/guide/index.md from README.md instead of just checking.",
    )
    args = p.parse_args()

    readme_path = REPO_ROOT / "README.md"
    index_path = REPO_ROOT / "docs" / "guide" / "index.md"

    if not readme_path.exists():
        print(f"error: {readme_path} missing", file=sys.stderr)
        return 1

    expected = render_from_readme(readme_path.read_text(encoding="utf-8"))

    if args.write:
        index_path.write_text(expected, encoding="utf-8")
        print(f"wrote {index_path}")
        return 0

    if not index_path.exists():
        print(
            f"error: {index_path} missing — run "
            f"`python3 scripts/check-readme-sync.py --write`",
            file=sys.stderr,
        )
        return 1

    actual = index_path.read_text(encoding="utf-8")
    if actual == expected:
        print(f"ok: {index_path.relative_to(REPO_ROOT)} is in sync with README.md")
        return 0

    diff = "".join(
        difflib.unified_diff(
            actual.splitlines(keepends=True),
            expected.splitlines(keepends=True),
            fromfile="docs/guide/index.md (committed)",
            tofile="docs/guide/index.md (from README.md)",
            n=3,
        )
    )
    print(diff, end="", file=sys.stderr)
    print(file=sys.stderr)
    print(
        "drift detected — run `python3 scripts/check-readme-sync.py --write` "
        "to re-sync and commit the result.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
