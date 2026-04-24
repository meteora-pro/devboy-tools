#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["duckdb>=1.0", "pyarrow>=18.0", "pandas>=2.0"]
# ///
"""
Paper 2 — layered pipeline simulator.

Replays every tool-response event in its real session order, applies the
L0–L3 encoding pipeline, and measures actual token savings and decision
breakdown.

Layers:
  L0 — LRU hint-dedup (content_sha256 match within current context_partition)
  L1 — per-endpoint templates (mcp_*_get_issues → csv_from_md;
                                mcp_*_get_*_pipeline → deep_mckp;
                                mcp_*_merge_request_diffs → deep_mckp)
  L2 — generic MCKP rules (shape + inner-format dispatched)
  L3 — as-is fallback

L0 dedup keys on `content_sha256`, so matches indicate byte-identical
content within the current context_partition. LRU size and
context_partition tracking are configurable.

Inputs:
  /tmp/claude_analysis/paper2_format_events.parquet  — enriched events

Output:
  /tmp/claude_analysis/paper2_simulation_decisions.parquet  — per-event row
  stdout — aggregate metrics + per-layer breakdown + per-tool-family table
"""
from __future__ import annotations

import argparse
import sys
from collections import Counter, deque
from pathlib import Path
from typing import Any

import duckdb
import pyarrow as pa
import pyarrow.parquet as pq


HINT_TOKENS_SMALL = 15   # "[dup of tc_X, unchanged]" — exact match
HINT_TOKENS_MED   = 30   # with endpoint info

MIN_CHARS_TO_OPTIMIZE = 200

# Tools that mutate files — trigger cache invalidation for the target path
_MUTATING_TOOLS = frozenset({"Edit", "Write", "MultiEdit", "NotebookEdit"})
# Tools that read files — affected by mutations on same path
_READING_TOOLS = frozenset({"Read", "Grep", "Glob", "NotebookRead"})


class DedupCacheEntry:
    __slots__ = ("sha", "tc_id", "tool", "file_hash", "partition")
    def __init__(self, sha: str, tc_id: str, tool: str, file_hash: str, partition: int):
        self.sha = sha
        self.tc_id = tc_id
        self.tool = tool
        self.file_hash = file_hash
        self.partition = partition


def layer0_dedup_decision(
    event: dict,
    cache: deque,
    partition: int,
) -> tuple[str | None, int, str | None]:
    """Look up content-hash match within current partition.

    Uses cryptographic SHA-256 prefix, so matches imply byte-identical content.
    Returns ``(decision, tokens_final, reference_tc_id)``:
    ``decision is None`` means no L0 hint was emitted.
    """
    sha = event["content_sha256"]
    if not sha:
        return (None, event["tokens_baseline"], None)
    for entry in cache:
        if entry.partition != partition:
            continue
        if entry.sha == sha:
            return ("hint_exact", HINT_TOKENS_SMALL, entry.tc_id)
    return (None, event["tokens_baseline"], None)


def invalidate_on_mutation(cache: deque, file_hash: str) -> int:
    """Remove cache entries for the given file_hash that belong to read tools.
    Returns count invalidated."""
    if not file_hash:
        return 0
    dropped = 0
    survivors = deque(maxlen=cache.maxlen)
    for e in cache:
        if e.tool in _READING_TOOLS and e.file_hash == file_hash:
            dropped += 1
            continue
        survivors.append(e)
    # Replace in place
    cache.clear()
    cache.extend(survivors)
    return dropped


def layer1_endpoint_template(event: dict) -> tuple[str, int]:
    """Per-endpoint template dispatch based on tool_name pattern."""
    tool = event["tool_name_anon"] or ""
    shape = event["shape"]

    # mcp__<hash>__get_issues* / get_epics → csv_from_md
    if ("get_issues" in tool or "get_epics" in tool) and shape == "markdown_table":
        if event["tokens_csv_from_md"] > 0 and event["tokens_csv_from_md"] < event["tokens_baseline"]:
            return ("L1_mcp_issues_csv", event["tokens_csv_from_md"])

    # mcp__<hash>__get_*_pipeline / get_pipeline → deep_mckp
    if ("pipeline" in tool) and shape == "nested_object":
        if event["tokens_deep_mckp"] > 0 and event["tokens_deep_mckp"] < event["tokens_json_compact"]:
            return ("L1_mcp_pipeline_dmckp", event["tokens_deep_mckp"])

    # mcp__<hash>__get_merge_request_diffs → deep_mckp
    if "merge_request_diffs" in tool and shape == "nested_object":
        if event["tokens_deep_mckp"] > 0:
            return ("L1_mcp_mr_diffs", event["tokens_deep_mckp"])

    return ("", 0)


def layer2_generic_mckp(event: dict) -> tuple[str, int]:
    """Generic shape-based format selection."""
    shape = event["shape"]

    if shape == "markdown_table":
        if event["md_n_cols"] >= 2 and event["tokens_csv_from_md"] > 0:
            return ("L2_md_csv", event["tokens_csv_from_md"])
        return ("", 0)

    if shape == "array_of_objects":
        if event["n_items"] >= 4 and event["key_stability"] >= 0.7 and event["tokens_csv"] > 0:
            return ("L2_arr_csv", event["tokens_csv"])
        return ("L2_arr_json", event["tokens_json_compact"])

    if shape == "nested_object":
        # Use deep_mckp when it's cheaper than json_compact
        if event["tokens_deep_mckp"] > 0 and event["tokens_deep_mckp"] < event["tokens_json_compact"]:
            return ("L2_nested_dmckp", event["tokens_deep_mckp"])
        return ("L2_nested_json", event["tokens_json_compact"])

    if shape == "flat_object":
        if event["n_fields"] >= 8 and event["tokens_kv"] > 0 and event["tokens_kv"] < event["tokens_json_compact"]:
            return ("L2_flat_kv", event["tokens_kv"])
        return ("L2_flat_json", event["tokens_json_compact"])

    return ("", 0)


def _new_cache_entry(e: dict, partition: int) -> DedupCacheEntry:
    return DedupCacheEntry(
        sha=e["content_sha256"],
        tc_id=e["tool_use_id_hash"],
        tool=e["tool_name_anon"],
        file_hash=e.get("file_path_hash", ""),
        partition=partition,
    )


def simulate_pipeline(events_by_session: dict[str, list[dict]],
                      lru_size: int) -> list[dict]:
    """Replay events per-session with sha256-based dedup + mutation invalidation."""
    out: list[dict] = []
    total_invalidations = 0

    for session, events in events_by_session.items():
        partition_caches: dict[int, deque] = {}

        for e in events:
            partition = e["context_partition"]
            cache = partition_caches.setdefault(partition, deque(maxlen=lru_size))
            baseline = e["tokens_baseline"]
            tool = e["tool_name_anon"] or ""

            row = {
                "session": session,
                "tool_use_id_hash": e["tool_use_id_hash"],
                "tool_name_anon": tool,
                "endpoint_class": e["endpoint_class"],
                "shape": e["shape"],
                "tool_result_ordinal": e["tool_result_ordinal"],
                "context_partition": partition,
                "is_sidechain": e["is_sidechain"],
                "content_sha256": e["content_sha256"],
                "file_path_hash": e.get("file_path_hash", ""),
                "baseline_tokens": baseline,
                "decision": "",
                "layer": "",
                "final_tokens": baseline,
                "tokens_saved": 0,
                "hint_ref_tc": None,
                "invalidated_entries": 0,
            }

            # Mutation-first: a mutation's response is rarely itself cacheable,
            # but we must invalidate existing cache entries for same file.
            if tool in _MUTATING_TOOLS and e.get("file_path_hash"):
                dropped = invalidate_on_mutation(cache, e["file_path_hash"])
                total_invalidations += dropped
                row["invalidated_entries"] = dropped

            if baseline == 0 or e["raw_chars"] < MIN_CHARS_TO_OPTIMIZE:
                row["decision"] = "L3_tiny_as_is"
                row["layer"] = "L3"
                out.append(row)
                cache.append(_new_cache_entry(e, partition))
                continue

            # L0: content-hash dedup
            dec0, new_tok0, ref_tc = layer0_dedup_decision(e, cache, partition)
            if dec0:
                row["decision"] = dec0
                row["layer"] = "L0"
                row["final_tokens"] = new_tok0
                row["tokens_saved"] = baseline - new_tok0
                row["hint_ref_tc"] = ref_tc
                out.append(row)
                continue

            # L1: per-endpoint template
            dec1, new_tok1 = layer1_endpoint_template(e)
            if dec1:
                row["decision"] = dec1
                row["layer"] = "L1"
                row["final_tokens"] = new_tok1
                row["tokens_saved"] = max(baseline - new_tok1, 0)
                out.append(row)
                cache.append(_new_cache_entry(e, partition))
                continue

            # L2: generic MCKP
            dec2, new_tok2 = layer2_generic_mckp(e)
            if dec2 and new_tok2 > 0 and new_tok2 < baseline:
                row["decision"] = dec2
                row["layer"] = "L2"
                row["final_tokens"] = new_tok2
                row["tokens_saved"] = baseline - new_tok2
            else:
                row["decision"] = "L3_as_is"
                row["layer"] = "L3"

            out.append(row)
            cache.append(_new_cache_entry(e, partition))

    print(f"# total cache invalidations from mutations: {total_invalidations:,}",
          file=sys.stderr)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="inp", default="/tmp/claude_analysis/paper2_format_events.parquet")
    ap.add_argument("--out", default="/tmp/claude_analysis/paper2_simulation_decisions.parquet")
    ap.add_argument("--lru-size", type=int, default=5,
                    help="Per-partition LRU cache size for dedup")
    args = ap.parse_args()

    con = duckdb.connect()
    inp = args.inp
    n = con.execute(f"SELECT COUNT(*) FROM '{inp}'").fetchone()[0]
    print(f"# simulating {n:,} events with LRU={args.lru_size}", file=sys.stderr)

    # Load sorted by (session, ordinal)
    rows = con.execute(f"""
        SELECT session, tool_use_id_hash, tool_name_anon, endpoint_class,
               content_sha256, file_path_hash,
               shape, raw_chars, tokens_baseline, tokens_json_compact,
               tokens_csv, tokens_md_table, tokens_kv, tokens_csv_from_md,
               tokens_deep_mckp, tokens_mckp,
               md_n_cols, md_n_rows, n_items, n_fields, key_stability,
               is_sidechain, context_partition, tool_result_ordinal,
               event_idx_in_session
        FROM '{inp}'
        ORDER BY session, context_partition, tool_result_ordinal
    """).fetchall()
    cols = [d[0] for d in con.description]

    # Group by session
    events_by_session: dict[str, list[dict]] = {}
    for r in rows:
        d = dict(zip(cols, r))
        events_by_session.setdefault(d["session"], []).append(d)

    print(f"# {len(events_by_session)} sessions", file=sys.stderr)

    # Run simulation
    decisions = simulate_pipeline(events_by_session, lru_size=args.lru_size)

    # Write output
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    table = pa.Table.from_pylist(decisions)
    pq.write_table(table, args.out, compression="zstd")
    print(f"# wrote {len(decisions):,} rows → {args.out}", file=sys.stderr)

    # Aggregate report
    print("\n=== AGGREGATE ===", file=sys.stderr)
    df = con.execute(f"""
        SELECT
          COUNT(*) events,
          ROUND(SUM(baseline_tokens)/1e6, 3) baseline_mtok,
          ROUND(SUM(final_tokens)/1e6, 3) final_mtok,
          ROUND(SUM(tokens_saved)/1e6, 3) saved_mtok,
          ROUND(100.0*SUM(tokens_saved)/SUM(baseline_tokens), 2) saved_pct
        FROM '{args.out}'
    """).fetchdf()
    print(df.to_string(index=False), file=sys.stderr)

    print("\n=== PER LAYER ===", file=sys.stderr)
    df = con.execute(f"""
        SELECT layer, decision, COUNT(*) n,
               ROUND(SUM(baseline_tokens)/1e6, 2) base_mtok,
               ROUND(SUM(final_tokens)/1e6, 2) final_mtok,
               ROUND(SUM(tokens_saved)/1e6, 3) saved_mtok,
               ROUND(AVG(100.0*tokens_saved/NULLIF(baseline_tokens, 0)), 1) avg_savings_pct
        FROM '{args.out}'
        GROUP BY layer, decision ORDER BY saved_mtok DESC
    """).fetchdf()
    print(df.to_string(index=False), file=sys.stderr)


if __name__ == "__main__":
    main()
