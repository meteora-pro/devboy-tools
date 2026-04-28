#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""
Paper 3 — offline `cited_in_next_n_turns` post-pass.

Reads a devboy telemetry JSONL (one `PipelineEvent` per line, optionally
mixed with `{"type": "session_summary", ...}` lines) and sets the
`cited_in_next_n_turns` field on every event where
`enricher_prefetched = true`, based on a simple heuristic:

  cited iff any of the next N events in the *same session* invoked the
  same `tool_name_anon` *without* `enricher_prefetched` — i.e. the LLM
  itself ended up issuing the call the planner had pre-fetched.

This is an honest, measurable proxy for "did the planner's speculation
help?" without needing access to the LLM's textual output. Stricter
projection-aware citation tracking would require storing tool-argument
fingerprints in `PipelineEvent`, which we deliberately do not — the
schema is anonymisation-first.

Output: a sibling `<input>.annotated.jsonl` file with the same line
order, where every prefetched event carries either
`"cited_in_next_n_turns": true` or `"cited_in_next_n_turns": false`.
Non-prefetched events and `session_summary` lines pass through unchanged.

Usage:
  uv run docs/research/scripts/annotate_cited_prefetches.py \\
      --input ~/.devboy/telemetry/mcp_12345.jsonl
  uv run docs/research/scripts/annotate_cited_prefetches.py \\
      --input ~/.devboy/telemetry/ --recursive
"""
from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

# How many future events count as "next 1-3 turns". Matches the
# `cited_in_next_n_turns` field name and the Paper 3 §Adaptive tuning
# definition. Picking 3 — wider windows would dilute the signal
# (planner gets credit for late-arriving accidents).
LOOKAHEAD_EVENTS = 3


def is_pipeline_event(rec: dict) -> bool:
    """True if `rec` looks like a PipelineEvent (not a session_summary
    wrapper). Robust to future schema additions."""
    if rec.get("type") == "session_summary":
        return False
    return "tool_name_anon" in rec and "session_hash" in rec


def annotate_session(events: list[dict]) -> int:
    """Mutate `events` in place, setting `cited_in_next_n_turns` on every
    `enricher_prefetched=true` event. Returns the count of citations
    found (`cited=true` only)."""
    cited_count = 0
    for i, e in enumerate(events):
        if not e.get("enricher_prefetched"):
            continue
        target = e.get("tool_name_anon")
        if not target:
            continue
        end = min(i + 1 + LOOKAHEAD_EVENTS, len(events))
        cited = False
        for j in range(i + 1, end):
            future = events[j]
            if future.get("tool_name_anon") != target:
                continue
            # Only count *organic* future calls. A future prefetched
            # call with the same name does not count — the LLM did not
            # actually issue it.
            if future.get("enricher_prefetched"):
                continue
            cited = True
            break
        e["cited_in_next_n_turns"] = cited
        if cited:
            cited_count += 1
    return cited_count


def process_file(path: Path, output: Path) -> tuple[int, int]:
    """Annotate one JSONL file. Returns `(prefetches_seen, cited_count)`."""
    by_session: dict[str, list[dict]] = defaultdict(list)
    raw_lines: list[tuple[int, dict]] = []

    with path.open("r", encoding="utf-8", errors="replace") as f:
        for idx, line in enumerate(f):
            line = line.strip()
            if not line:
                raw_lines.append((idx, {}))  # blank-line marker
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                raw_lines.append((idx, {"__raw__": line}))
                continue
            raw_lines.append((idx, rec))
            if is_pipeline_event(rec):
                by_session[rec["session_hash"]].append(rec)

    # Annotate every session in place.
    prefetches_seen = 0
    cited_count = 0
    for session_events in by_session.values():
        prefetches_seen += sum(1 for e in session_events if e.get("enricher_prefetched"))
        cited_count += annotate_session(session_events)

    # Write the annotated JSONL preserving original ordering.
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("w", encoding="utf-8") as out:
        for _, rec in raw_lines:
            if not rec:
                out.write("\n")
            elif "__raw__" in rec:
                out.write(rec["__raw__"] + "\n")
            else:
                out.write(json.dumps(rec, separators=(",", ":")) + "\n")

    return prefetches_seen, cited_count


def find_inputs(target: Path, recursive: bool) -> list[Path]:
    if target.is_file():
        return [target]
    if not target.is_dir():
        return []
    if recursive:
        return sorted(target.rglob("*.jsonl"))
    return sorted(target.glob("*.jsonl"))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", type=Path, required=True,
                    help="JSONL file or directory of JSONL files")
    ap.add_argument("--output-dir", type=Path,
                    help="If set, write annotated files here instead of "
                         "<input>.annotated.jsonl alongside the source.")
    ap.add_argument("--recursive", action="store_true",
                    help="When --input is a directory, walk subdirectories.")
    args = ap.parse_args()

    files = find_inputs(args.input, args.recursive)
    if not files:
        print(f"no jsonl files under {args.input}", file=sys.stderr)
        return 1

    grand_prefetches = 0
    grand_cited = 0
    for src in files:
        if args.output_dir:
            dst = args.output_dir / (src.stem + ".annotated.jsonl")
        else:
            dst = src.with_suffix(".annotated.jsonl")
        try:
            prefetches, cited = process_file(src, dst)
        except OSError as e:
            print(f"skip {src}: {e}", file=sys.stderr)
            continue
        grand_prefetches += prefetches
        grand_cited += cited
        if prefetches > 0:
            rate = cited / prefetches * 100
            print(f"{src} → {dst}  prefetches={prefetches}  cited={cited}  "
                  f"hit_rate={rate:.1f}%")
        else:
            print(f"{src} → {dst}  no prefetches recorded (no-op)")

    if grand_prefetches > 0:
        rate = grand_cited / grand_prefetches * 100
        print(f"\nTOTAL  prefetches={grand_prefetches}  cited={grand_cited}  "
              f"hit_rate={rate:.1f}%")
    else:
        print("\nNo enricher_prefetched events found in any file. "
              "Either the host has not started issuing speculative pre-fetches "
              "yet, or the telemetry sink is not capturing them.",
              file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
