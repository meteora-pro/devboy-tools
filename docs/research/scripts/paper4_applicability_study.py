#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["tqdm>=4.0"]
# ///
"""
Paper 4 — applicability frontier study.

Three independent measurements that together draw the boundary
where Parquet+notebook starts paying back:

  1. THEORETICAL break-even — given measured Parquet overhead (file
     header + footer + row-group metadata) and a notebook-header
     token cost, compute the minimum dataset size where Parquet
     wins over MCKP / JSON-full.

  2. EMPIRICAL selectivity on our corpus — for every list→detail
     loop in `~/.claude/projects/*.jsonl`, count how many *unique*
     rows from the list are subsequently fetched in detail. A
     loop with selectivity ≪ 1.0 is the textbook Parquet case
     (LLM does not need every row); selectivity ≈ 1.0 means
     the agent reads everything anyway and Parquet's filter
     story is wasted.

  3. SYNTHETIC microbenchmark — generate 10/100/1000-row issue
     fixtures and report the byte/token cost of:
        a) full JSON
        b) MCKP-style markdown table (rough proxy: csv-from-md)
        c) Parquet header + sample-rows-only
     Reads /tmp/claude_analysis/paper4_session_signals.csv from
     the prior corpus extractor for empirical mix.

The script intentionally does NOT write any Parquet itself — that
lives on the Rust side. We only count what *would* be written so
the applicability matrix matches what the production serialiser
will emit.

Output: docs/research/data/paper4_applicability_matrix.csv +
stderr summary.
"""
from __future__ import annotations

import argparse
import csv
import hashlib
import json
import re
import sys
from collections import defaultdict
from pathlib import Path

# Empirical Parquet overhead constants — measured on a small
# fixture, refined later. These are *bytes* on disk, not tokens.
# They give the floor below which Parquet always loses.
PARQUET_FILE_OVERHEAD_BYTES = 600  # header + footer + magic
PARQUET_PER_COLUMN_OVERHEAD_BYTES = 80  # column metadata in row-group descriptor
# Notebook-header *token* cost the LLM pays to see the schema +
# 5 sample rows + a handful of helper functions. Anchored on
# Paper 4 §Token Economics (~200 tokens).
NOTEBOOK_HEADER_TOKENS = 200
# A typical LLM query (`df[df.priority == 'high'].head(20)`).
LLM_QUERY_TOKENS = 50
# Minimum useful response — agent gets back ≥ this many tokens.
MIN_RESULT_TOKENS = 100

# 4 chars per token — same heuristic Paper 2 / 3 use until the
# accurate tokenizer ships in this script.
CHARS_PER_TOKEN = 4


def chars_to_tokens(c: int) -> int:
    return max(1, c // CHARS_PER_TOKEN)


# ─── 1. Theoretical break-even ───────────────────────────────────────


def theoretical_breakeven_rows(
    avg_row_chars: int,
    cols: int,
    selectivity: float,
    mckp_compression_ratio: float = 0.4,
) -> int:
    """Minimum row count where Parquet beats MCKP under the given
    selectivity (fraction of rows the LLM actually reads back).

    Cost model (in tokens, agent-visible):
      mckp(N) = chars_to_tokens(N * avg_row_chars * mckp_compression_ratio)
      parquet(N, sel) = NOTEBOOK_HEADER_TOKENS + LLM_QUERY_TOKENS
                      + chars_to_tokens(sel * N * avg_row_chars * mckp_compression_ratio)

    Set parquet(N, sel) < mckp(N) and solve for N.
    """
    fixed_cost_tokens = NOTEBOOK_HEADER_TOKENS + LLM_QUERY_TOKENS
    # Result is also some fraction of selectivity * raw, but with
    # a floor at MIN_RESULT_TOKENS:
    delta_per_row_tokens = avg_row_chars * mckp_compression_ratio / CHARS_PER_TOKEN
    if selectivity >= 1.0 or delta_per_row_tokens <= 0:
        return -1  # Parquet never wins
    saved_per_row = delta_per_row_tokens * (1.0 - selectivity)
    if saved_per_row <= 0:
        return -1
    n = int(fixed_cost_tokens / saved_per_row) + 1
    return n


def theoretical_table() -> list[dict]:
    rows: list[dict] = []
    # avg_row_chars: small (issue summary ~150), medium (issue+desc ~500),
    # large (full file content per row ~2000)
    for label, avg_row in [("small (150c)", 150), ("medium (500c)", 500), ("large (2000c)", 2000)]:
        for sel_pct in [10, 25, 50, 80]:
            sel = sel_pct / 100.0
            n_min = theoretical_breakeven_rows(avg_row, cols=8, selectivity=sel)
            rows.append({
                "row_size": label,
                "selectivity_pct": sel_pct,
                "breakeven_rows": n_min if n_min > 0 else "never",
                "verdict": "parquet wins" if 0 < n_min < 1000 else (
                    "borderline" if 0 < n_min <= 5000 else "mckp wins"
                ),
            })
    return rows


# ─── 2. Empirical selectivity from our corpus ────────────────────────

MCP_RE = re.compile(r"^mcp__")


def hash_short(s: str, n: int = 8) -> str:
    return hashlib.sha1(s.encode("utf-8")).hexdigest()[:n]


def anonymize_tool(name: str) -> str:
    if not name or not MCP_RE.match(name):
        return name or ""
    prefix_end = len("mcp__")
    verb_start = name.rfind("__")
    if verb_start < prefix_end:
        return name
    slug = name[prefix_end:verb_start]
    verb = name[verb_start + 2:]
    return f"mcp__p{hash_short(slug, 6)}__{verb}"


LIST_FAMILY_PATTERNS = ["Glob", "list_", "get_issues", "get_merge_requests", "search_"]


def is_list_family(tool: str) -> bool:
    lt = tool.lower()
    return any(p.lower() in lt for p in LIST_FAMILY_PATTERNS)


def extract_ids_from_response(text_chars: int, body_str: str | None) -> set[str]:
    """Heuristic: pull `id`-shaped tokens from the response body.
    Used to estimate how many distinct list rows the agent later
    references in detail.

    The corpus extractor strips bodies before disk; here we accept
    the raw `body_str` only when called inline on a fresh JSONL —
    no body data is ever serialised to disk.
    """
    if not body_str:
        return set()
    # Match #123, IID-456, PROJ-42, sha-shaped 7-40 char hashes
    pat = re.compile(r"\b(?:#\d{1,8}|[A-Z]{2,8}-\d{1,7}|[a-f0-9]{7,40})\b")
    return set(pat.findall(body_str))


def empirical_selectivity(input_dir: Path, max_sessions: int = 0) -> dict:
    """For each list→detail loop in the corpus, measure
    `unique_detail_calls / list_row_count_estimate`.

    We approximate `list_row_count_estimate` from the response
    `body_chars` divided by an average row size guess; this is
    rough but consistent across sessions.
    """
    sessions = sorted(input_dir.rglob("*.jsonl"))
    if max_sessions:
        sessions = sessions[:max_sessions]
    if not sessions:
        return {}

    LOOKAHEAD = 10
    AVG_ROW_CHARS = 200  # rough heuristic for list responses

    selectivity_buckets: dict[str, list[float]] = defaultdict(list)
    sample_size_buckets: dict[str, list[int]] = defaultdict(list)
    total_loops = 0

    try:
        from tqdm import tqdm
        pbar = tqdm(sessions, desc="empirical-selectivity", unit="s")
    except ImportError:
        pbar = sessions

    for session_path in pbar:
        events: list[dict] = []
        with session_path.open("r", encoding="utf-8", errors="replace") as f:
            pending: dict[str, dict] = {}
            for line_no, raw in enumerate(f):
                try:
                    rec = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                content = rec.get("message", {}).get("content")
                if not isinstance(content, list):
                    continue
                for blk in content:
                    if not isinstance(blk, dict):
                        continue
                    t = blk.get("type")
                    if t == "tool_use":
                        tu_id = blk.get("id") or ""
                        pending[tu_id] = {
                            "tool_anon": anonymize_tool(blk.get("name") or ""),
                            "line_no": line_no,
                            "body_chars": 0,
                            "body_text_sample": "",
                        }
                    elif t == "tool_result":
                        tu_id = blk.get("tool_use_id") or ""
                        body = blk.get("content")
                        if isinstance(body, list):
                            text = " ".join(
                                x.get("text", "")
                                for x in body
                                if isinstance(x, dict) and x.get("type") == "text"
                            )
                        elif isinstance(body, str):
                            text = body
                        else:
                            text = ""
                        if tu_id in pending:
                            pending[tu_id]["body_chars"] = len(text)
                            # Keep a sample for id extraction; cap at
                            # 100 kB to avoid pathological lines.
                            pending[tu_id]["body_text_sample"] = text[:100_000]

            events = sorted(pending.values(), key=lambda r: r["line_no"])

        for i, ev in enumerate(events):
            if not is_list_family(ev["tool_anon"]):
                continue
            list_chars = ev["body_chars"]
            if list_chars < 500:
                continue  # too small to be a meaningful list response
            list_ids = extract_ids_from_response(list_chars, ev["body_text_sample"])
            if not list_ids:
                continue

            end = min(i + 1 + LOOKAHEAD, len(events))
            detail_ids: set[str] = set()
            for j in range(i + 1, end):
                future = events[j]
                future_ids = extract_ids_from_response(
                    future["body_chars"], future["body_text_sample"]
                )
                # Only count IDs that were referenced as the *target*
                # of the detail call, i.e. they showed up in the list.
                detail_ids.update(future_ids & list_ids)

            estimated_rows = max(1, list_chars // AVG_ROW_CHARS)
            sel = min(1.0, len(detail_ids) / estimated_rows)
            selectivity_buckets[ev["tool_anon"]].append(sel)
            sample_size_buckets[ev["tool_anon"]].append(estimated_rows)
            total_loops += 1

    return {
        "total_loops": total_loops,
        "per_tool": {
            tool: {
                "n_loops": len(vals),
                "median_selectivity": sorted(vals)[len(vals) // 2] if vals else 0,
                "p90_selectivity": sorted(vals)[int(len(vals) * 0.9)] if vals else 0,
                "mean_estimated_rows": sum(sample_size_buckets[tool]) / max(1, len(sample_size_buckets[tool])),
            }
            for tool, vals in selectivity_buckets.items()
            if len(vals) >= 5  # K=5 anonymity threshold
        },
    }


# ─── 3. Synthetic microbenchmark ─────────────────────────────────────


def synthetic_issue(i: int) -> dict:
    return {
        "id": 1000 + i,
        "title": f"Issue #{1000 + i}: lorem ipsum dolor sit amet consectetur",
        "status": ["open", "in_progress", "closed"][i % 3],
        "priority": ["low", "normal", "high", "urgent"][i % 4],
        "assignee": f"@user{i % 23}",
        "created_at": f"2026-{1 + i % 12:02d}-{1 + i % 28:02d}T10:00:00Z",
        "due_date": f"2026-{1 + (i + 3) % 12:02d}-{1 + (i + 5) % 28:02d}T23:59:59Z",
        "story_points": float(1 + (i * 7) % 13),
    }


def microbenchmark() -> list[dict]:
    rows: list[dict] = []
    for n in (10, 100, 1000):
        ds = [synthetic_issue(i) for i in range(n)]
        json_chars = len(json.dumps(ds, indent=2))
        # MCKP/markdown-table proxy: rough column-csv encoding.
        cols = list(ds[0].keys())
        mckp_chars = len(",".join(cols)) + 1
        for row in ds:
            mckp_chars += sum(len(str(row[c])) for c in cols) + len(cols)
        # Parquet path tokens: header + query + result(20 rows × 3 cols).
        sample_str = "\n".join(
            "  " + " | ".join(f"{k}={v}" for k, v in r.items())
            for r in ds[:5]
        )
        notebook_chars = (
            f"# === Issues Dataset | rows: {n} ===\n"
            f"import polars as pl\n"
            f"df = pl.read_parquet('/tmp/ctx/issues.parquet')\n"
            f"# Schema: id Int64 | title Utf8 | status Utf8 | priority Utf8 |"
            f" assignee Utf8 | created_at Datetime | due_date Datetime |"
            f" story_points Float64\n"
            f"# Sample (5 rows):\n{sample_str}\n"
            f"def high_priority(): return df.filter(pl.col('priority') == 'high')\n"
        )
        notebook_chars = len(notebook_chars)
        # Assume LLM filters to top-20 high-priority rows
        result_chars = sum(
            len(str(r))
            for r in ds[:20]
            if r["priority"] == "high"
        )
        parquet_total_chars = (
            notebook_chars + LLM_QUERY_TOKENS * CHARS_PER_TOKEN + result_chars
        )

        rows.append({
            "n_rows": n,
            "json_full_tokens": chars_to_tokens(json_chars),
            "mckp_tokens": chars_to_tokens(mckp_chars),
            "parquet_path_tokens": chars_to_tokens(parquet_total_chars),
            "vs_json_pct": round(100.0 * (1 - parquet_total_chars / max(1, json_chars)), 1),
            "vs_mckp_pct": round(100.0 * (1 - parquet_total_chars / max(1, mckp_chars)), 1),
        })
    return rows


# ─── Main ────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input-dir", type=Path, default=Path.home() / ".claude/projects")
    ap.add_argument("--output-csv", type=Path,
                    default=Path("docs/research/data/paper4_applicability_matrix.csv"))
    ap.add_argument("--max-sessions", type=int, default=200,
                    help="Empirical selectivity is expensive — cap by default")
    ap.add_argument("--skip-empirical", action="store_true")
    args = ap.parse_args()

    print("=== 1. Theoretical break-even ===", file=sys.stderr)
    theo = theoretical_table()
    for row in theo:
        print(f"  row={row['row_size']:<14} sel={row['selectivity_pct']:>3}%  "
              f"min_rows={row['breakeven_rows']:<8}  → {row['verdict']}",
              file=sys.stderr)

    emp = {}
    if not args.skip_empirical:
        print("\n=== 2. Empirical selectivity on corpus ===", file=sys.stderr)
        emp = empirical_selectivity(args.input_dir, args.max_sessions)
        print(f"  total list→detail loops scanned: {emp.get('total_loops', 0):,}",
              file=sys.stderr)
        for tool, stats in sorted(
            emp.get("per_tool", {}).items(),
            key=lambda kv: kv[1]["n_loops"],
            reverse=True,
        )[:10]:
            print(
                f"  {tool:<35} loops={stats['n_loops']:>4} "
                f"median_sel={stats['median_selectivity']:.2f} "
                f"p90_sel={stats['p90_selectivity']:.2f} "
                f"avg_rows≈{stats['mean_estimated_rows']:.0f}",
                file=sys.stderr,
            )

    print("\n=== 3. Synthetic microbenchmark (issues) ===", file=sys.stderr)
    micro = microbenchmark()
    for row in micro:
        print(
            f"  N={row['n_rows']:>4}  json={row['json_full_tokens']:>6}t  "
            f"mckp={row['mckp_tokens']:>6}t  parquet_path={row['parquet_path_tokens']:>5}t "
            f"vs_json={row['vs_json_pct']:>+5.1f}%  vs_mckp={row['vs_mckp_pct']:>+5.1f}%",
            file=sys.stderr,
        )

    args.output_csv.parent.mkdir(parents=True, exist_ok=True)
    with args.output_csv.open("w", encoding="utf-8") as f:
        f.write("# Paper 4 — applicability frontier\n")
        f.write("# section,key,value\n")
        f.write("section,row_size,selectivity_pct,breakeven_rows,verdict\n")
        for row in theo:
            f.write(
                f"theoretical,{row['row_size']},{row['selectivity_pct']},"
                f"{row['breakeven_rows']},{row['verdict']}\n"
            )
        f.write("\n")
        f.write("section,n_rows,json_tokens,mckp_tokens,parquet_tokens,vs_json_pct,vs_mckp_pct\n")
        for row in micro:
            f.write(
                f"microbench,{row['n_rows']},{row['json_full_tokens']},"
                f"{row['mckp_tokens']},{row['parquet_path_tokens']},"
                f"{row['vs_json_pct']},{row['vs_mckp_pct']}\n"
            )
        if emp.get("per_tool"):
            f.write("\n")
            f.write("section,tool_anon,n_loops,median_selectivity,p90_selectivity,mean_estimated_rows\n")
            for tool, stats in sorted(
                emp["per_tool"].items(), key=lambda kv: kv[1]["n_loops"], reverse=True
            ):
                f.write(
                    f"empirical,{tool},{stats['n_loops']},"
                    f"{stats['median_selectivity']:.3f},{stats['p90_selectivity']:.3f},"
                    f"{stats['mean_estimated_rows']:.0f}\n"
                )

    print(f"\n  wrote {args.output_csv}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
