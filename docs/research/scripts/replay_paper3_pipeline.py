#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""
Paper 3 — replay validation harness.

Reads devboy telemetry JSONL emitted by the live pipeline (or by
`annotate_cited_prefetches.py`) and computes the four headline
metrics from the Paper 3 §Validation strategy table per session and
across the whole corpus:

  • prefetch_hit_rate     = cited / total_prefetches
  • decline_recall_loss   = late_invoked / total_declines
  • cost_overrun_rate     = overruns / total_predictions  (≥ 130%)
  • inference_calls_saved = prefetch + dedup + fail_fast buckets
  • inference_tokens_saved = sum across the three buckets

Output:

  • per-session CSV table to stdout (or --output PATH).
  • a final summary row "TOTAL" with grand-total counts and rates.

Late-invoked detection: when a PipelineEvent has
`enricher_decline_reason` set, the harness scans the next 5 events
in the same session for an organic (non-prefetched) call to the same
tool — drives the decline_recall_loss numerator.

This harness does NOT spawn a real planner; it strictly aggregates
fields the live pipeline already wrote, so numbers are reproducible
across runs.

Usage:
  uv run docs/research/scripts/replay_paper3_pipeline.py \\
      --input ~/.devboy/telemetry/
  uv run docs/research/scripts/replay_paper3_pipeline.py \\
      --input session.jsonl --output /tmp/p3_metrics.csv
"""
from __future__ import annotations

import argparse
import csv
import json
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

LATE_INVOKE_LOOKAHEAD = 5
COST_OVERRUN_RATIO_PCT = 130  # actual ≥ 130% of predicted


@dataclass
class SessionMetrics:
    session_hash: str
    total_events: int = 0
    total_prefetches: int = 0
    cited_prefetches: int = 0
    total_declines: int = 0
    late_invoked_after_decline: int = 0
    cost_overrun_count: int = 0
    total_predictions: int = 0
    inference_calls_saved_prefetch: int = 0
    inference_calls_saved_dedup: int = 0
    inference_calls_saved_fail_fast: int = 0
    inference_tokens_saved: int = 0

    def fold(self, ev: dict) -> None:
        self.total_events += 1
        is_dedup = bool(ev.get("is_dedup_hit"))
        baseline = int(ev.get("tokens_baseline", 0))

        if ev.get("enricher_prefetched"):
            self.total_prefetches += 1
            self.total_predictions += 1
            predicted = int(ev.get("enricher_predicted_cost_tokens", 0))
            if predicted > 0 and baseline * 100 >= predicted * COST_OVERRUN_RATIO_PCT:
                self.cost_overrun_count += 1
            if ev.get("cited_in_next_n_turns") is True:
                self.cited_prefetches += 1
                self.inference_calls_saved_prefetch += 1
                self.inference_tokens_saved += baseline

        if is_dedup:
            self.inference_calls_saved_dedup += 1
            self.inference_tokens_saved += baseline

        if ev.get("enricher_decline_reason"):
            self.total_declines += 1

    def total_calls_saved(self) -> int:
        return (
            self.inference_calls_saved_prefetch
            + self.inference_calls_saved_dedup
            + self.inference_calls_saved_fail_fast
        )

    def prefetch_hit_rate(self) -> float | None:
        if self.total_prefetches == 0:
            return None
        return self.cited_prefetches / self.total_prefetches

    def decline_recall_loss(self) -> float | None:
        if self.total_declines == 0:
            return None
        return self.late_invoked_after_decline / self.total_declines

    def cost_overrun_rate(self) -> float | None:
        if self.total_predictions == 0:
            return None
        return self.cost_overrun_count / self.total_predictions


@dataclass
class SessionEvents:
    metrics: SessionMetrics
    events: list[dict] = field(default_factory=list)


def is_pipeline_event(rec: dict) -> bool:
    if rec.get("type") == "session_summary":
        return False
    return "tool_name_anon" in rec and "session_hash" in rec


def find_input_files(target: Path, recursive: bool) -> list[Path]:
    if target.is_file():
        return [target]
    if not target.is_dir():
        return []
    return sorted(target.rglob("*.jsonl") if recursive else target.glob("*.jsonl"))


def detect_late_invoked(session_events: list[dict]) -> int:
    """Count events with `enricher_decline_reason` whose tool was
    organically called in the next LATE_INVOKE_LOOKAHEAD events."""
    count = 0
    for i, ev in enumerate(session_events):
        if not ev.get("enricher_decline_reason"):
            continue
        target = ev.get("tool_name_anon")
        end = min(i + 1 + LATE_INVOKE_LOOKAHEAD, len(session_events))
        for j in range(i + 1, end):
            future = session_events[j]
            if (
                future.get("tool_name_anon") == target
                and not future.get("enricher_prefetched")
            ):
                count += 1
                break
    return count


def fmt_pct(rate: float | None) -> str:
    if rate is None:
        return "n/a"
    return f"{rate * 100:.1f}%"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", type=Path, required=True,
                    help="JSONL file or directory of JSONL files")
    ap.add_argument("--output", type=Path,
                    help="Write the per-session CSV to PATH (default: stdout).")
    ap.add_argument("--recursive", action="store_true",
                    help="When --input is a directory, walk subdirectories.")
    args = ap.parse_args()

    files = find_input_files(args.input, args.recursive)
    if not files:
        print(f"no jsonl files under {args.input}", file=sys.stderr)
        return 1

    by_session: dict[str, SessionEvents] = {}

    for f in files:
        try:
            with f.open("r", encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        rec = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if not is_pipeline_event(rec):
                        continue
                    sid = rec["session_hash"]
                    if sid not in by_session:
                        by_session[sid] = SessionEvents(
                            metrics=SessionMetrics(session_hash=sid)
                        )
                    bucket = by_session[sid]
                    bucket.events.append(rec)
                    bucket.metrics.fold(rec)
        except OSError as e:
            print(f"skip {f}: {e}", file=sys.stderr)
            continue

    if not by_session:
        print("no PipelineEvent records found", file=sys.stderr)
        return 1

    # Second pass: detect late-invoked-after-decline within each
    # session. This requires the full event list so it cannot run
    # inside `fold`.
    for bucket in by_session.values():
        bucket.metrics.late_invoked_after_decline = detect_late_invoked(bucket.events)

    # Build CSV.
    columns = [
        "session_hash",
        "total_events",
        "total_prefetches",
        "cited_prefetches",
        "prefetch_hit_rate",
        "total_declines",
        "late_invoked_after_decline",
        "decline_recall_loss",
        "cost_overrun_count",
        "total_predictions",
        "cost_overrun_rate",
        "inference_calls_saved_prefetch",
        "inference_calls_saved_dedup",
        "inference_calls_saved_fail_fast",
        "inference_calls_saved_total",
        "inference_tokens_saved",
    ]

    grand = SessionMetrics(session_hash="TOTAL")
    rows: list[dict] = []
    for sid in sorted(by_session.keys()):
        m = by_session[sid].metrics
        rows.append({
            "session_hash": m.session_hash,
            "total_events": m.total_events,
            "total_prefetches": m.total_prefetches,
            "cited_prefetches": m.cited_prefetches,
            "prefetch_hit_rate": fmt_pct(m.prefetch_hit_rate()),
            "total_declines": m.total_declines,
            "late_invoked_after_decline": m.late_invoked_after_decline,
            "decline_recall_loss": fmt_pct(m.decline_recall_loss()),
            "cost_overrun_count": m.cost_overrun_count,
            "total_predictions": m.total_predictions,
            "cost_overrun_rate": fmt_pct(m.cost_overrun_rate()),
            "inference_calls_saved_prefetch": m.inference_calls_saved_prefetch,
            "inference_calls_saved_dedup": m.inference_calls_saved_dedup,
            "inference_calls_saved_fail_fast": m.inference_calls_saved_fail_fast,
            "inference_calls_saved_total": m.total_calls_saved(),
            "inference_tokens_saved": m.inference_tokens_saved,
        })
        # Roll into grand total.
        for fld in [
            "total_events",
            "total_prefetches",
            "cited_prefetches",
            "total_declines",
            "late_invoked_after_decline",
            "cost_overrun_count",
            "total_predictions",
            "inference_calls_saved_prefetch",
            "inference_calls_saved_dedup",
            "inference_calls_saved_fail_fast",
            "inference_tokens_saved",
        ]:
            setattr(grand, fld, getattr(grand, fld) + getattr(m, fld))

    rows.append({
        "session_hash": "TOTAL",
        "total_events": grand.total_events,
        "total_prefetches": grand.total_prefetches,
        "cited_prefetches": grand.cited_prefetches,
        "prefetch_hit_rate": fmt_pct(grand.prefetch_hit_rate()),
        "total_declines": grand.total_declines,
        "late_invoked_after_decline": grand.late_invoked_after_decline,
        "decline_recall_loss": fmt_pct(grand.decline_recall_loss()),
        "cost_overrun_count": grand.cost_overrun_count,
        "total_predictions": grand.total_predictions,
        "cost_overrun_rate": fmt_pct(grand.cost_overrun_rate()),
        "inference_calls_saved_prefetch": grand.inference_calls_saved_prefetch,
        "inference_calls_saved_dedup": grand.inference_calls_saved_dedup,
        "inference_calls_saved_fail_fast": grand.inference_calls_saved_fail_fast,
        "inference_calls_saved_total": grand.total_calls_saved(),
        "inference_tokens_saved": grand.inference_tokens_saved,
    })

    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with args.output.open("w", encoding="utf-8", newline="") as out:
            w = csv.DictWriter(out, fieldnames=columns)
            w.writeheader()
            w.writerows(rows)
        print(f"wrote {args.output} — {len(rows) - 1} session(s) + TOTAL row")
    else:
        w = csv.DictWriter(sys.stdout, fieldnames=columns)
        w.writeheader()
        w.writerows(rows)

    # Headline summary to stderr so it survives even when stdout is
    # captured to a file.
    print(
        f"\nPaper 3 corpus replay: {len(by_session)} session(s), "
        f"{grand.total_events} events, prefetch_hit={fmt_pct(grand.prefetch_hit_rate())} "
        f"cost_overrun={fmt_pct(grand.cost_overrun_rate())} "
        f"calls_saved={grand.total_calls_saved()} "
        f"tokens_saved={grand.inference_tokens_saved}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
