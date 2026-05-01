#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Drill-down view of one session by sid (UUID prefix is enough).

Usage: view_session.py 2c052d83
"""
from __future__ import annotations

import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session  # noqa: E402
from lib.classifiers import (  # noqa: E402
    biome_of, archetype_of, rhythm_of, bash_sub_of,
    BIOME_EMOJI, ARCHETYPE_EMOJI, RHYTHM_EMOJI,
)


def main(argv: list[str] | None = None) -> int:
    if not argv:
        argv = sys.argv[1:]
    if not argv:
        print("usage: view_session.py <sid_prefix>", file=sys.stderr)
        return 2
    prefix = argv[0]
    files = find_session_files()
    matches = [f for f in files if f.stem.startswith(prefix)]
    if not matches:
        print(f"no session matching {prefix!r}", file=sys.stderr)
        return 1
    if len(matches) > 1:
        print(f"ambiguous: {len(matches)} matches", file=sys.stderr)
        for m in matches[:10]:
            print(f"  {m.stem}", file=sys.stderr)
        return 1
    jf = matches[0]
    sess = load_session(jf, include_subagents=True)

    e = b = r = 0
    seq: list[str] = []
    bash_subs: dict[str, int] = {}
    real_prompts = 0
    compacts = 0
    pivots = 0
    for ev in sess.events:
        if ev.type == "compact":
            compacts += 1
            continue
        if ev.type == "user":
            if ev.user_kind == "real_human":
                real_prompts += 1
            if ev.user_text.startswith("[Request interrupted by user"):
                pivots += 1
            continue
        if ev.type != "assistant":
            continue
        for tu in ev.tool_uses:
            n = tu.get("name", "")
            if n in ("Edit", "MultiEdit", "Write"):
                e += 1
                seq.append("Edit")
            elif n in ("Read", "Grep", "Glob", "ToolSearch"):
                r += 1
                seq.append("Read")
            elif n == "Bash":
                b += 1
                cmd = (tu.get("input", {}) or {}).get("command", "") or ""
                sub = bash_sub_of(cmd)
                bash_subs[sub] = bash_subs.get(sub, 0) + 1
                seq.append(f"Bash:{sub}")

    biome = biome_of(real_prompts)
    arch = archetype_of(e, b, r)
    rh = rhythm_of(seq)

    print(f"╔══ {sess.sid[:8]} ═══════════════════════════════════════════════════════════════")
    print(f"║  project: {sess.project}")
    print(f"║  events: {len(sess.events)}, bursts: {len(sess.bursts)}, subagents: {len(sess.subagents)}")
    print(f"║  {BIOME_EMOJI[biome]} {biome}  |  {ARCHETYPE_EMOJI[arch]} {arch}  |  {RHYTHM_EMOJI[rh]} {rh}")
    print(f"║  R: {real_prompts}  Edit: {e}  Bash: {b}  Read: {r}")
    print(f"║  compacts: {compacts}  pivots: {pivots}")
    if bash_subs:
        bs = ", ".join(f"{k} {v}" for k, v in sorted(bash_subs.items(), key=lambda x: -x[1])[:5])
        print(f"║  bash sub: {bs}")
    if sess.subagents:
        print(f"║  subagent count: {len(sess.subagents)}")
        for sub in sess.subagents[:5]:
            asst_n = sum(1 for ev in sub.events if ev.type == "assistant")
            print(f"║    sub {sub.sub_id[:8]} asst={asst_n}")
    print(f"╚{'═'*78}")

    print("\nFirst 5 real-human prompts:")
    cnt = 0
    for ev in sess.events:
        if ev.type == "user" and ev.user_kind == "real_human":
            print(f"  • {ev.user_text[:140]!r}")
            cnt += 1
            if cnt >= 5:
                break
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
