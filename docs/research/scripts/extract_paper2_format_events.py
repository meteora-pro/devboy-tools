#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=18.0", "tqdm>=4.0"]
# ///
"""
Paper 2 — Format-Adaptive Tree Encoding — dataset extractor.

Scans ~/.claude/projects/*.jsonl logs and emits one anonymized row per
tool_result, recording:

  - structural shape of the response (array_of_objects, flat_object,
    markdown_table, code_block, nested, text, ...)
  - token counts estimated for each viable encoding format
    (json, csv, md_table, key_value, prose, code_fence)
  - best_format per row and projected savings vs JSON baseline
  - paper2_applicable flag: savings ≥ configurable threshold

Anonymization:
  - NO raw content is stored. Only structural counts and sizes.
  - NO field names, values, ids, URLs, or user text leave the script.
  - session is hashed, tool_name is anonymized (MCP slugs → hash).
  - tool_use_id is dropped.

Output: /tmp/claude_analysis/paper2_format_events.parquet

Usage:
  uv run docs/research/scripts/extract_paper2_format_events.py
  uv run docs/research/scripts/extract_paper2_format_events.py --min-chars 200 \\
      --savings-threshold 0.20 --resume
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import statistics
import sys
import time
from collections import Counter
from pathlib import Path
from typing import Any, Iterator

import pyarrow as pa
import pyarrow.parquet as pq

# ─── Anonymization helpers (mirroring analyze_sessions.py) ─────────────────

MCP_RE = re.compile(r"^mcp__")


def hash_short(s: str, n: int = 8) -> str:
    return hashlib.sha1(s.encode("utf-8")).hexdigest()[:n]


def anonymize_tool(name: str) -> str:
    if not name:
        return ""
    if not MCP_RE.match(name):
        return name
    # mcp__<slug>__<verb> → mcp__p<hash6>__<verb>
    prefix_end = len("mcp__")
    verb_start = name.rfind("__")
    if verb_start < prefix_end:
        return name
    slug = name[prefix_end:verb_start]
    verb = name[verb_start + 2:]
    return f"mcp__p{hash_short(slug, 6)}__{verb}"


# ─── Token estimation ──────────────────────────────────────────────────────

# Rough approximation: 1 token ≈ 4 chars (standard for English + code).
# Paper uses same approximation; tiktoken would be more accurate but adds
# a heavy dependency — sufficient for shape-level comparisons.
def est_tokens(s: str) -> int:
    return max(1, len(s) // 4)


# ─── Shape classifier ──────────────────────────────────────────────────────

SHAPE_PRIMITIVE = "primitive"
SHAPE_ARRAY_OBJS = "array_of_objects"
SHAPE_ARRAY_PRIMS = "array_of_primitives"
SHAPE_FLAT_OBJ = "flat_object"
SHAPE_NESTED_OBJ = "nested_object"
SHAPE_EMPTY = "empty"

SHAPE_MD_TABLE = "markdown_table"
SHAPE_CODE_BLOCK = "code_block"
SHAPE_NUMBERED_LIST = "numbered_list"   # like file-read with "   1→..." prefix
SHAPE_BULLET_LIST = "bullet_list"
SHAPE_PROSE = "prose"
SHAPE_UNKNOWN = "unknown"


_CODE_FENCE_RE = re.compile(r"^\s*```", re.MULTILINE)
_MD_TABLE_RE = re.compile(r"^\s*\|[^\n]+\|\s*\n\s*\|[\s:|-]+\|", re.MULTILINE)
_NUMBERED_LINE_RE = re.compile(r"^\s*\d+[→\):]", re.MULTILINE)
_BULLET_RE = re.compile(r"^\s*[-*]\s+\S", re.MULTILINE)


def classify_json_shape(obj: Any) -> tuple[str, dict]:
    """Classify a parsed JSON value; return (shape, metadata dict)."""
    meta: dict = {}

    if obj is None or obj == "":
        return SHAPE_EMPTY, meta
    if isinstance(obj, (str, int, float, bool)):
        return SHAPE_PRIMITIVE, meta

    if isinstance(obj, list):
        if len(obj) == 0:
            return SHAPE_EMPTY, {"n_items": 0}

        # Sample heads to decide object vs primitive
        head_types = [type(x).__name__ for x in obj[:min(len(obj), 50)]]
        if all(t == "dict" for t in head_types):
            # Array of objects — richest Paper 2 case
            n_items = len(obj)
            field_sets = [frozenset(x.keys()) for x in obj[:200] if isinstance(x, dict)]
            all_keys = set().union(*field_sets) if field_sets else set()
            n_fields_per_item = [len(fs) for fs in field_sets]
            # Jaccard-like stability: how much do keys overlap across items?
            if len(field_sets) > 1:
                pair_jac = []
                first = field_sets[0]
                for fs in field_sets[1:min(20, len(field_sets))]:
                    u = len(first | fs)
                    pair_jac.append(len(first & fs) / u if u else 1.0)
                key_stability = statistics.mean(pair_jac) if pair_jac else 1.0
            else:
                key_stability = 1.0

            # Nested-value detection on first item
            first_item = obj[0] if isinstance(obj[0], dict) else {}
            has_nested = any(
                isinstance(v, (dict, list)) for v in first_item.values()
            )

            meta.update({
                "n_items": n_items,
                "n_fields_avg": int(round(statistics.mean(n_fields_per_item))) if n_fields_per_item else 0,
                "n_fields_max": max(n_fields_per_item) if n_fields_per_item else 0,
                "n_unique_fields_total": len(all_keys),
                "key_stability": round(key_stability, 3),
                "has_nested_values": has_nested,
            })
            return SHAPE_ARRAY_OBJS, meta

        # Not all dicts → array of primitives (or mixed). Treat mixed as primitives
        # for Paper 2 purposes — kv/csv not applicable.
        meta["n_items"] = len(obj)
        return SHAPE_ARRAY_PRIMS, meta

    if isinstance(obj, dict):
        if len(obj) == 0:
            return SHAPE_EMPTY, {"n_fields": 0}
        has_nested = any(isinstance(v, (dict, list)) for v in obj.values())
        meta.update({
            "n_fields": len(obj),
            "n_fields_avg": len(obj),
            "n_fields_max": len(obj),
            "has_nested_values": has_nested,
        })
        return (SHAPE_NESTED_OBJ if has_nested else SHAPE_FLAT_OBJ), meta

    return SHAPE_UNKNOWN, meta


def classify_text_shape(text: str) -> tuple[str, dict]:
    """Classify non-JSON text: markdown table, code fence, numbered, prose."""
    meta: dict = {}
    if _CODE_FENCE_RE.search(text):
        meta["n_fences"] = len(_CODE_FENCE_RE.findall(text))
        return SHAPE_CODE_BLOCK, meta
    if _MD_TABLE_RE.search(text):
        # count rows in the first table
        rows = [l for l in text.split("\n") if l.strip().startswith("|")]
        meta["n_rows"] = max(len(rows) - 2, 0)  # minus header + separator
        return SHAPE_MD_TABLE, meta
    if _NUMBERED_LINE_RE.search(text):
        meta["n_lines"] = len(_NUMBERED_LINE_RE.findall(text))
        return SHAPE_NUMBERED_LIST, meta
    bullets = _BULLET_RE.findall(text)
    if len(bullets) >= 3:
        meta["n_bullets"] = len(bullets)
        return SHAPE_BULLET_LIST, meta
    return SHAPE_PROSE, meta


# ─── Format cost estimators ────────────────────────────────────────────────

def cost_json(obj: Any) -> int:
    return est_tokens(json.dumps(obj, ensure_ascii=False, separators=(",", ":")))


def cost_json_pretty(obj: Any) -> int:
    return est_tokens(json.dumps(obj, ensure_ascii=False, indent=2))


def cost_csv_array_of_objects(obj: list) -> int:
    """Estimate CSV token count for array-of-objects.

    Header row + value rows, comma separator, no extra whitespace.
    Nested values get JSON-inlined (realistic worst case).
    """
    if not obj or not isinstance(obj[0], dict):
        return cost_json(obj)
    # Union of keys across items (stable header)
    keys = set()
    for item in obj[:200]:
        if isinstance(item, dict):
            keys.update(item.keys())
    keys = sorted(keys)
    header = ",".join(keys)
    total_chars = len(header) + 1  # newline
    for item in obj:
        if not isinstance(item, dict):
            continue
        row_vals = []
        for k in keys:
            v = item.get(k, "")
            if isinstance(v, (dict, list)):
                v = json.dumps(v, ensure_ascii=False, separators=(",", ":"))
            else:
                v = str(v) if v is not None else ""
            # Basic CSV escape if comma or quote
            if "," in v or '"' in v or "\n" in v:
                v = '"' + v.replace('"', '""') + '"'
            row_vals.append(v)
        total_chars += len(",".join(row_vals)) + 1
    return est_tokens("x" * total_chars)  # shortcut: treat as plain text


def cost_md_table_array_of_objects(obj: list) -> int:
    if not obj or not isinstance(obj[0], dict):
        return cost_json(obj)
    keys = set()
    for item in obj[:200]:
        if isinstance(item, dict):
            keys.update(item.keys())
    keys = sorted(keys)
    header = "| " + " | ".join(keys) + " |\n"
    sep = "|" + "|".join([" --- " for _ in keys]) + "|\n"
    total_chars = len(header) + len(sep)
    for item in obj:
        if not isinstance(item, dict):
            continue
        vals = []
        for k in keys:
            v = item.get(k, "")
            if isinstance(v, (dict, list)):
                v = json.dumps(v, ensure_ascii=False, separators=(",", ":"))
            else:
                v = str(v) if v is not None else ""
            vals.append(v.replace("|", "\\|").replace("\n", " "))
        total_chars += len("| " + " | ".join(vals) + " |\n")
    return est_tokens("x" * total_chars)


def cost_kv_flat_object(obj: dict) -> int:
    """key: value\\n ... — no indentation, no quotes."""
    total_chars = 0
    for k, v in obj.items():
        if isinstance(v, (dict, list)):
            v_str = json.dumps(v, ensure_ascii=False, separators=(",", ":"))
        else:
            v_str = str(v) if v is not None else ""
        total_chars += len(str(k)) + 2 + len(v_str) + 1  # "k: v\n"
    return est_tokens("x" * total_chars)


# ─── Markdown-table parser (for re-encoding as CSV) ────────────────────────

_MD_TBL_LINE_RE = re.compile(r"^\s*\|.*\|\s*$")
_MD_TBL_SEP_RE = re.compile(r"^\s*\|[\s:\-|]+\|\s*$")


def parse_markdown_table(text: str) -> tuple[list[str], list[list[str]]] | None:
    """Extract the first markdown table in text. Returns (headers, rows) or None."""
    lines = text.split("\n")
    table_lines: list[str] = []
    in_table = False
    for line in lines:
        if _MD_TBL_LINE_RE.match(line):
            table_lines.append(line.strip())
            in_table = True
        elif in_table:
            break  # first blank/non-table line after starting = end of table
    if len(table_lines) < 2:
        return None

    def split_row(line: str) -> list[str]:
        # strip leading/trailing |, split on |
        inner = line.strip().strip("|")
        return [c.strip() for c in inner.split("|")]

    # header is first line, separator is usually second
    headers = split_row(table_lines[0])
    start = 1
    if len(table_lines) >= 2 and _MD_TBL_SEP_RE.match(table_lines[1]):
        start = 2
    rows = [split_row(l) for l in table_lines[start:]]
    # normalize width
    ncols = len(headers)
    rows = [r + [""] * (ncols - len(r)) if len(r) < ncols else r[:ncols] for r in rows]
    return headers, rows


def cost_csv_from_md(headers: list[str], rows: list[list[str]]) -> int:
    total_chars = len(",".join(headers)) + 1
    for row in rows:
        vals = []
        for v in row:
            v = v.replace("\n", " ")
            if "," in v or '"' in v:
                v = '"' + v.replace('"', '""') + '"'
            vals.append(v)
        total_chars += len(",".join(vals)) + 1
    return est_tokens("x" * total_chars)


# ─── Deep recursive MCKP cost ─────────────────────────────────────────────

# Fence language hints for structured string content
_FENCE_LANG = {
    "diff": "diff",
    "log": "",
    "md": "",
    "md_with_code": "",
    "md_table": "",
    "xml_html": "html",
    "yaml": "yaml",
    "code_like": "",
    "stack_trace": "",
}


def _cost_leaf_in_json(s: str) -> int:
    """Cost of this string value when JSON-quoted + escaped."""
    return len(s) + s.count("\n") + s.count("\"") + s.count("\\") + 2  # +2 quotes


def _cost_leaf_external(s: str, fmt: str) -> int:
    """Cost of emitting the string outside JSON as fenced code block."""
    lang = _FENCE_LANG.get(fmt, "")
    # "```<lang>\n" + content + "\n```\n" — no escape of \n or "
    return len(s) + len(lang) + 9  # 3+3+lang+newlines


def deep_mckp_cost_chars(obj: Any, depth: int = 0, max_depth: int = 6) -> int:
    """
    Recursive optimal-format cost in CHARS (divide by 4 at end for tokens).
    For each subtree, pick the cheapest valid encoding:
      - scalar string with structured format + long → fence external
      - string with inner JSON → unwrap to native JSON
      - dict → kv-list of children costs
      - list of dicts → CSV
      - list of primitives → inline joined
      - list mixed → JSON array
      - nested dict inside array of dicts → fall back to per-slot
    Falls back to compact JSON for anything not recognized.
    """
    if depth > max_depth:
        # Too deep — fall back to compact JSON size
        return len(json.dumps(obj, ensure_ascii=False, separators=(",", ":")))

    if obj is None or isinstance(obj, bool):
        return len(json.dumps(obj))
    if isinstance(obj, (int, float)):
        return len(str(obj))

    if isinstance(obj, str):
        if len(obj) < 40:
            # too short — fence overhead not worth, keep as JSON-quoted
            return _cost_leaf_in_json(obj)
        fmt = sniff_string_format(obj)
        # Structured formats → fence
        if fmt in ("diff", "log", "md", "md_with_code", "md_table",
                   "xml_html", "yaml", "stack_trace"):
            in_json = _cost_leaf_in_json(obj)
            external = _cost_leaf_external(obj, fmt)
            return min(in_json, external)
        if fmt == "json":
            try:
                inner = json.loads(obj)
                # Inner JSON: recurse (opens up re-encoding of the inner tree)
                return 2 + deep_mckp_cost_chars(inner, depth + 1, max_depth)
            except (json.JSONDecodeError, ValueError):
                pass
        if fmt == "code_like":
            return min(_cost_leaf_in_json(obj), _cost_leaf_external(obj, fmt))
        return _cost_leaf_in_json(obj)

    if isinstance(obj, dict):
        if not obj:
            return 2
        # kv emission: "key: <recursive>\n"
        total = 0
        for k, v in obj.items():
            total += len(str(k)) + 2  # "key: "
            total += deep_mckp_cost_chars(v, depth + 1, max_depth)
            total += 1  # newline
        # compare with JSON-object cost
        json_cost = len(json.dumps(obj, ensure_ascii=False, separators=(",", ":")))
        return min(total, json_cost)

    if isinstance(obj, list):
        if not obj:
            return 2
        # all dicts → try CSV vs per-item MCKP vs plain JSON
        if len(obj) >= 2 and all(isinstance(x, dict) for x in obj[:10]):
            csv_chars = cost_csv_array_of_objects(obj) * 4
            per_item = sum(deep_mckp_cost_chars(x, depth + 1, max_depth) for x in obj[:50])
            if len(obj) > 50:
                per_item = per_item * len(obj) // 50
            per_item += 2 * len(obj)  # separators
            json_cost = len(json.dumps(obj, ensure_ascii=False, separators=(",", ":")))
            return min(csv_chars, per_item, json_cost)

        # primitives → inline
        if all(isinstance(x, (str, int, float, bool)) or x is None for x in obj):
            total = sum(
                _cost_leaf_in_json(x) if isinstance(x, str)
                else len(str(x))
                for x in obj
            ) + len(obj)  # separators
            json_cost = len(json.dumps(obj, ensure_ascii=False, separators=(",", ":")))
            return min(total, json_cost)

        # mixed → recursive JSON
        total = sum(deep_mckp_cost_chars(x, depth + 1, max_depth) for x in obj) + len(obj)
        json_cost = len(json.dumps(obj, ensure_ascii=False, separators=(",", ":")))
        return min(total, json_cost)

    return len(json.dumps(obj, ensure_ascii=False))


def deep_mckp_cost(obj: Any) -> int:
    """Tokens estimate of deep-MCKP optimal encoding."""
    return max(1, deep_mckp_cost_chars(obj) // 4)


# ─── MCKP composition for nested objects ──────────────────────────────────

def mckp_cost_nested_object(obj: dict) -> tuple[int, dict]:
    """
    MCKP composition: for each top-level key, pick the best format.
    Returns (total_tokens, metadata).

    Rules (ascending cost ladder per slot):
      scalar value              → inlined kv ("key: value\\n")
      list of objects           → CSV (header + rows)
      list of primitives        → comma-separated inline
      nested dict               → JSON (compact)
      mixed list                → JSON (compact)
    """
    parts: list[tuple[str, int]] = []  # (format_used, tokens)
    fmt_counts: Counter = Counter()

    for k, v in obj.items():
        key_str = str(k)
        # scalar
        if v is None or isinstance(v, (str, int, float, bool)):
            vs = "" if v is None else str(v)
            chars = len(key_str) + 2 + len(vs) + 1
            parts.append(("kv", est_tokens("x" * chars)))
            fmt_counts["kv"] += 1
            continue

        # list
        if isinstance(v, list):
            if len(v) == 0:
                parts.append(("kv_empty", 3))
                fmt_counts["kv"] += 1
                continue
            # all dicts → CSV subtree
            if all(isinstance(x, dict) for x in v[:20]):
                tokens = cost_csv_array_of_objects(v)
                # Add "## <key>\n" header char cost
                header_chars = len(f"## {key_str}\n")
                tokens = tokens + est_tokens("x" * header_chars)
                parts.append(("csv", tokens))
                fmt_counts["csv"] += 1
                continue
            # primitives → inline
            if all(isinstance(x, (str, int, float, bool)) or x is None for x in v[:20]):
                joined = ",".join("" if x is None else str(x) for x in v)
                chars = len(key_str) + 2 + len(joined) + 1
                parts.append(("inline_list", est_tokens("x" * chars)))
                fmt_counts["inline_list"] += 1
                continue
            # mixed → JSON
            parts.append(("json", cost_json(v)))
            fmt_counts["json"] += 1
            continue

        # dict → nested JSON block
        if isinstance(v, dict):
            parts.append(("json", cost_json(v)))
            fmt_counts["json"] += 1
            continue

        # fallback
        parts.append(("json", cost_json(v)))
        fmt_counts["json"] += 1

    total = sum(t for _, t in parts)
    # Format mix string: sort by count desc for stability
    mix = "+".join(f"{c}{f}" for f, c in fmt_counts.most_common())
    return total, {"mckp_format_mix": mix, "mckp_n_slots": len(parts)}


# ─── Multi-format / nested-format detection ───────────────────────────────

# Order of tests matters — more specific before more generic.
_DIFF_HEADER_RE = re.compile(r"^@@ -\d+,?\d* \+\d+,?\d* @@", re.MULTILINE)
_DIFF_GIT_RE = re.compile(r"^diff --git ", re.MULTILINE)
_STACK_PY_RE = re.compile(r"Traceback \(most recent call last\):")
_STACK_JS_RE = re.compile(r"^\s+at .*\(.+:\d+(?::\d+)?\)$", re.MULTILINE)
_LOG_RE = re.compile(
    r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?\b"
)
_YAML_LINE_RE = re.compile(r"^\s*[a-zA-Z_][\w\-]*:\s", re.MULTILINE)
_XML_OPEN_RE = re.compile(r"<[a-zA-Z][a-zA-Z0-9]*[^>]*>")
_XML_CLOSE_RE = re.compile(r"</[a-zA-Z]")
_URL_RE = re.compile(r"https?://[^\s\"'<>)]+")
_HEX_HASH_RE = re.compile(r"\b[0-9a-f]{7,40}\b")
_CODE_FENCE_FULL_RE = re.compile(r"```([a-zA-Z0-9_+-]*)\s*\n(.*?)\n```", re.DOTALL)
_INLINE_JSON_RE = re.compile(
    r"(?:^|\n)(\{(?:[^{}]|\{[^{}]*\})*\}|\[(?:[^\[\]]|\[[^\[\]]*\])*\])(?:\n|$)",
    re.DOTALL,
)


def sniff_string_format(s: str) -> str:
    """Classify a single string value by dominant format."""
    if not s:
        return "empty"
    s_strip = s.strip()
    if len(s_strip) < 8:
        return "short"

    # URL/email
    if _URL_RE.match(s_strip):
        return "url"

    # Git diff
    if _DIFF_GIT_RE.search(s_strip) or _DIFF_HEADER_RE.search(s_strip):
        return "diff"

    # Stack trace
    if _STACK_PY_RE.search(s_strip) or _STACK_JS_RE.search(s_strip):
        return "stack_trace"

    # JSON
    if (s_strip[0] == "{" and s_strip[-1] == "}") or (
        s_strip[0] == "[" and s_strip[-1] == "]"
    ):
        try:
            json.loads(s_strip)
            return "json"
        except (json.JSONDecodeError, ValueError):
            pass

    # Markdown code fence inside string
    if s_strip.count("```") >= 2:
        return "md_with_code"

    # Markdown table
    if _MD_TABLE_RE.search(s_strip):
        return "md_table"

    # Markdown heading
    if re.search(r"^#{1,6}\s+\S", s_strip, re.MULTILINE):
        return "md"

    # XML / HTML
    if _XML_OPEN_RE.search(s_strip) and _XML_CLOSE_RE.search(s_strip):
        return "xml_html"

    # YAML (multi-line indented key:value)
    yaml_hits = len(_YAML_LINE_RE.findall(s_strip))
    n_lines = s_strip.count("\n") + 1
    if yaml_hits >= 3 and yaml_hits / n_lines > 0.4:
        return "yaml"

    # Numbered file-read prefix
    if re.search(r"^\s*\d+→", s_strip, re.MULTILINE):
        return "numbered_list"

    # Log-ish
    if _LOG_RE.search(s_strip):
        return "log"

    # Hex hash (git sha etc.) standalone
    if len(s_strip) in (7, 8, 10, 40) and _HEX_HASH_RE.fullmatch(s_strip):
        return "hash"

    # Multi-line with mostly indented → code
    if "\n" in s_strip:
        lines = s_strip.split("\n")
        indented = sum(1 for l in lines if l.startswith(("    ", "\t", "  ")))
        if indented / max(len(lines), 1) > 0.5:
            return "code_like"
        return "prose"

    return "string"


def walk_json_for_formats(obj: Any, depth: int = 0, max_depth: int = 5,
                          out: list | None = None) -> list[tuple[int, str, int]]:
    """Walk parsed JSON, classify every string leaf."""
    if out is None:
        out = []
    if depth > max_depth:
        return out
    if isinstance(obj, str):
        fmt = sniff_string_format(obj)
        if fmt not in ("empty", "short", "string"):
            out.append((depth, fmt, len(obj)))
    elif isinstance(obj, dict):
        for v in list(obj.values())[:200]:
            walk_json_for_formats(v, depth + 1, max_depth, out)
    elif isinstance(obj, list):
        for v in obj[:100]:
            walk_json_for_formats(v, depth + 1, max_depth, out)
    return out


def scan_text_for_inner(text: str) -> list[tuple[str, int]]:
    """Detect embedded blocks inside text: code fences, inline JSON, diffs."""
    results: list[tuple[str, int]] = []

    # Code fences with language attribute
    for m in _CODE_FENCE_FULL_RE.finditer(text):
        lang = (m.group(1) or "none").lower()[:16]
        content = m.group(2)
        # Try to identify inner format from content when lang is blank/none
        if lang in ("", "none"):
            inner = sniff_string_format(content)
            label = f"fence:{inner}"
        else:
            label = f"fence:{lang}"
        results.append((label, len(content)))

    # Inline standalone JSON blocks (heuristic — bounded, no nested parsing)
    for m in _INLINE_JSON_RE.finditer(text):
        content = m.group(1).strip()
        if len(content) < 20:
            continue
        try:
            json.loads(content)
            results.append(("inline_json", len(content)))
        except (json.JSONDecodeError, ValueError):
            continue

    # Diff blocks
    for m in _DIFF_HEADER_RE.finditer(text):
        results.append(("inline_diff", 100))  # approx size — we don't parse extent

    # Stack traces
    if _STACK_PY_RE.search(text):
        results.append(("inline_stack_trace", 200))
    if _STACK_JS_RE.search(text):
        results.append(("inline_stack_trace_js", 100))

    return results


def build_format_signature(top_shape: str,
                           json_leaves: list[tuple[int, str, int]],
                           text_blocks: list[tuple[str, int]]) -> tuple[str, int, int, int]:
    """
    Build compact signature string + stats.
    Returns (signature, n_inner, inner_chars_total, depth_max).
    """
    # Aggregate inner formats
    inner_counter: Counter = Counter()
    inner_chars_total = 0
    depth_max = 0

    for depth, fmt, chars in json_leaves:
        inner_counter[fmt] += 1
        inner_chars_total += chars
        if depth > depth_max:
            depth_max = depth

    for fmt, chars in text_blocks:
        inner_counter[fmt] += 1
        inner_chars_total += chars
        # text blocks are depth 1 from the top text shape
        depth_max = max(depth_max, 1)

    # Signature: sorted by count desc, compact
    if not inner_counter:
        return top_shape, 0, 0, 0
    parts = [f"{f}:{c}" if c > 1 else f for f, c in inner_counter.most_common(8)]
    sig = f"{top_shape}>{'+'.join(parts)}"
    return sig, sum(inner_counter.values()), inner_chars_total, depth_max


# ─── Per-event feature row ─────────────────────────────────────────────────

def analyze_tool_result(content_text: str, tool_name_anon: str) -> dict | None:
    """Return a flat feature dict for this tool_result, or None to skip."""
    if not content_text:
        return None
    raw_chars = len(content_text)
    baseline_tokens = est_tokens(content_text)

    # First: is this JSON?
    parsed = None
    if content_text.strip().startswith(("{", "[")):
        try:
            parsed = json.loads(content_text)
        except (json.JSONDecodeError, ValueError):
            parsed = None

    row = {
        "tool_name_anon": tool_name_anon,
        "raw_chars": raw_chars,
        "tokens_baseline": baseline_tokens,
        "is_json": parsed is not None,
        # multi-format signature
        "format_signature": "",
        "inner_formats": "",        # comma-separated fmt names
        "n_inner_formats": 0,
        "n_distinct_inner": 0,
        "inner_chars_total": 0,
        "inner_pct": 0.0,
        "depth_max": 0,
        "is_multi_format": False,
        # shape + per-shape metadata
        "shape": SHAPE_UNKNOWN,
        "n_items": 0,
        "n_fields": 0,
        "n_fields_avg": 0,
        "n_fields_max": 0,
        "n_unique_fields_total": 0,
        "key_stability": 0.0,
        "has_nested_values": False,
        # format costs
        "tokens_json_compact": 0,
        "tokens_json_pretty": 0,
        "tokens_csv": 0,
        "tokens_md_table": 0,
        "tokens_kv": 0,
        # MCKP + markdown re-encoding + deep-MCKP
        "tokens_mckp": 0,
        "tokens_deep_mckp": 0,
        "mckp_format_mix": "",
        "mckp_n_slots": 0,
        "tokens_csv_from_md": 0,
        "md_n_rows": 0,
        "md_n_cols": 0,
        # outcome
        "best_format": "",
        "best_tokens": baseline_tokens,
        "savings_pct_vs_json": 0.0,
        "paper2_applicable": False,
    }

    if parsed is not None:
        shape, meta = classify_json_shape(parsed)
        row["shape"] = shape
        for k, v in meta.items():
            if k in row:
                row[k] = v

        # Walk JSON leaves to find inner formats (md-in-json, diff-in-json, etc.)
        json_leaves = walk_json_for_formats(parsed)
        sig, n_inner, inner_chars, depth = build_format_signature(shape, json_leaves, [])
        row["format_signature"] = sig
        row["n_inner_formats"] = n_inner
        row["inner_chars_total"] = inner_chars
        row["inner_pct"] = round(100.0 * inner_chars / max(raw_chars, 1), 2)
        row["depth_max"] = depth
        distinct = Counter(f for _, f, _ in json_leaves)
        row["n_distinct_inner"] = len(distinct)
        row["inner_formats"] = ",".join(sorted(distinct.keys()))
        row["is_multi_format"] = len(distinct) >= 2

        # Always compute compact JSON cost as baseline
        row["tokens_json_compact"] = cost_json(parsed)
        row["tokens_json_pretty"] = cost_json_pretty(parsed)

        # Deep MCKP — recursive per-leaf format selection
        row["tokens_deep_mckp"] = deep_mckp_cost(parsed)

        candidates = {
            "json_compact": row["tokens_json_compact"],
            "deep_mckp": row["tokens_deep_mckp"],
        }

        if shape == SHAPE_ARRAY_OBJS and isinstance(parsed, list):
            row["tokens_csv"] = cost_csv_array_of_objects(parsed)
            row["tokens_md_table"] = cost_md_table_array_of_objects(parsed)
            candidates["csv"] = row["tokens_csv"]
            candidates["md_table"] = row["tokens_md_table"]
        elif shape == SHAPE_FLAT_OBJ and isinstance(parsed, dict):
            row["tokens_kv"] = cost_kv_flat_object(parsed)
            candidates["kv"] = row["tokens_kv"]
        elif shape == SHAPE_NESTED_OBJ and isinstance(parsed, dict):
            # MCKP: per-top-level-key format selection
            mckp_tok, mckp_meta = mckp_cost_nested_object(parsed)
            row["tokens_mckp"] = mckp_tok
            row["mckp_format_mix"] = mckp_meta["mckp_format_mix"]
            row["mckp_n_slots"] = mckp_meta["mckp_n_slots"]
            candidates["mckp"] = mckp_tok

        best_fmt, best_tok = min(candidates.items(), key=lambda kv: kv[1])
        row["best_format"] = best_fmt
        row["best_tokens"] = best_tok
        baseline = row["tokens_json_compact"] or 1
        row["savings_pct_vs_json"] = round(
            100.0 * (baseline - best_tok) / baseline, 2
        )
        row["paper2_applicable"] = row["savings_pct_vs_json"] >= 20.0
    else:
        shape, meta = classify_text_shape(content_text)
        row["shape"] = shape

        # Detect embedded formats inside text (code fences, inline JSON, diff)
        text_blocks = scan_text_for_inner(content_text)
        sig, n_inner, inner_chars, depth = build_format_signature(shape, [], text_blocks)
        row["format_signature"] = sig
        row["n_inner_formats"] = n_inner
        row["inner_chars_total"] = inner_chars
        row["inner_pct"] = round(100.0 * inner_chars / max(raw_chars, 1), 2)
        row["depth_max"] = depth
        distinct = Counter(f for f, _ in text_blocks)
        row["n_distinct_inner"] = len(distinct)
        row["inner_formats"] = ",".join(sorted(distinct.keys()))
        row["is_multi_format"] = len(distinct) >= 2

        # Markdown tables: try CSV re-encoding vs the original markdown cost
        if shape == SHAPE_MD_TABLE:
            parsed_tbl = parse_markdown_table(content_text)
            if parsed_tbl is not None:
                headers, rows_tbl = parsed_tbl
                row["md_n_rows"] = len(rows_tbl)
                row["md_n_cols"] = len(headers)
                csv_tok = cost_csv_from_md(headers, rows_tbl)
                row["tokens_csv_from_md"] = csv_tok
                # original cost = baseline_tokens (as-received)
                if csv_tok < baseline_tokens:
                    row["best_format"] = "csv_from_md"
                    row["best_tokens"] = csv_tok
                    row["savings_pct_vs_json"] = round(
                        100.0 * (baseline_tokens - csv_tok) / baseline_tokens, 2
                    )
                    row["paper2_applicable"] = row["savings_pct_vs_json"] >= 20.0
                else:
                    row["best_format"] = "as_is"
                    row["best_tokens"] = baseline_tokens
                    row["savings_pct_vs_json"] = 0.0
                    row["paper2_applicable"] = False
                # carry meta
                for k, v in meta.items():
                    if k in row:
                        row[k] = v
                return row

        # other text shapes don't benefit from format selection
        row["best_format"] = "as_is"
        row["best_tokens"] = baseline_tokens
        row["savings_pct_vs_json"] = 0.0
        row["paper2_applicable"] = False

    return row


# ─── JSONL iteration ───────────────────────────────────────────────────────

_BASH_FIRST_WORD_RE = re.compile(r"^\s*(?:[A-Z_][A-Z0-9_]*=\S+\s+)*(\S+)")


def _bash_endpoint_class(cmd: str) -> str:
    """Classify bash command by its main program (after env prefix stripping).

    Anonymization: absolute paths, shell-control tokens, and long/arbitrary
    tokens are mapped to generic buckets (never surfaced verbatim) so the
    endpoint_class column stays safe to commit.
    """
    if not cmd:
        return "bash_empty"
    # Skip past compound prefix like "cd X && "
    parts = re.split(r"\s*(?:&&|;|\|\||\||\n)\s*", cmd, maxsplit=1)
    head = parts[0] if parts else cmd
    m = _BASH_FIRST_WORD_RE.match(head)
    prog_raw = (m.group(1) if m else head.split()[0] if head.split() else "")
    prog = prog_raw.lower()

    # === Anonymization guards (applied before any specific matching) ===
    if not prog:
        return "bash_unk"
    if prog.startswith(("/", "~/", "./", "../")):
        return "bash_path_exec"        # absolute/relative path execution
    if prog.startswith("#"):
        return "bash_comment"          # comment or heredoc marker
    if prog.startswith(("$", "`", "{", "(", "[", "!")):
        return "bash_shell_ctrl"       # shell substitution or control
    if len(prog) > 20 or re.search(r"[/\\]", prog):
        return "bash_long_or_path"     # unusually long or contains separators

    # Normalize common aliases
    if prog in ("git",):
        m2 = re.search(r"^\s*git\s+([a-z][a-z0-9_-]{0,15})", cmd)
        return f"git_{m2.group(1)}" if m2 else "git"
    if prog in ("curl", "wget", "http", "httpie"):
        return "curl"
    if prog in ("jq", "yq", "dasel"):
        return "jq"
    if prog in ("docker", "podman"):
        return "docker"
    if prog in ("kubectl", "helm", "k9s", "terraform", "tf"):
        return "k8s_iac"
    if prog in ("pytest", "jest", "vitest", "cargo", "go", "npm", "pnpm", "yarn", "bundle"):
        return f"test_{prog}" if prog in ("pytest","jest","vitest") else f"build_{prog}"
    if prog in ("ls", "find", "tree", "fd"):
        return "fs_list"
    if prog in ("cat", "head", "tail", "less", "bat"):
        return "fs_read"
    if prog in ("grep", "rg", "ag", "ack"):
        return "search"
    if prog in ("python", "python3", "python2", "node", "deno", "bun"):
        return f"exec_{prog}"
    if prog in ("awk", "sed", "cut", "tr", "sort", "uniq", "wc"):
        return "textproc"
    if prog in ("echo", "printf"):
        return "echo"
    return f"bash_{prog[:20]}"


def _web_endpoint_class(url: str) -> str:
    """Classify URL by domain + coarse path."""
    if not url:
        return "web_empty"
    m = re.match(r"https?://([^/]+)(/[^?#]*)?", url.strip())
    if not m:
        return "web_unk"
    host = m.group(1).lower()
    # Strip common prefixes
    host = re.sub(r"^(www\.|api\.|docs\.)", "", host)
    # Summarize
    parts = host.split(".")
    tld = ".".join(parts[-2:]) if len(parts) >= 2 else host
    return f"web_{tld}"


def derive_endpoint_class(tool: str, input_dict: Any) -> str:
    """Derive endpoint classification from tool name + input.
    For MCP tools, tool_name IS the endpoint (already granular).
    For Bash / WebFetch / WebSearch, parse input to extract semantic endpoint."""
    if not isinstance(input_dict, dict):
        input_dict = {}
    if tool == "Bash":
        return _bash_endpoint_class(input_dict.get("command", ""))
    if tool == "WebFetch":
        return _web_endpoint_class(input_dict.get("url", ""))
    if tool == "WebSearch":
        q = str(input_dict.get("query", ""))[:40]
        return "web_search"  # per-query classification skipped for anonymization
    # MCP and standard tools: endpoint == tool name
    return tool if tool else "unknown"


def _content_hash(text: str) -> str:
    """8-byte (16 hex chars) content fingerprint. Not leaky — pure digest."""
    return hashlib.sha256(text.encode("utf-8", errors="ignore")).hexdigest()[:16]


def _file_path_hash(tool: str, input_dict: Any) -> str:
    """Anonymized hash of the primary file-path argument, if any.

    Used for tool-aware cache invalidation: Edit/Write on hash X →
    drop Read/Grep cache entries with the same hash X.
    Returns '' if tool does not operate on a single file.
    """
    if not isinstance(input_dict, dict):
        return ""
    # Primary file path argument across Read/Edit/Write/MultiEdit/Grep/Glob
    for key in ("file_path", "path", "pattern", "notebook_path"):
        v = input_dict.get(key)
        if isinstance(v, str) and v:
            return hash_short(v, 8)
    # Grep can target a directory via `path`
    return ""


def _parse_ts(ts_str: Any) -> int:
    """Parse ISO timestamp to unix ms, 0 on failure."""
    if not isinstance(ts_str, str) or not ts_str:
        return 0
    try:
        from datetime import datetime
        dt = datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
        return int(dt.timestamp() * 1000)
    except (ValueError, TypeError):
        return 0


def iter_tool_results(jsonl_path: Path) -> Iterator[dict]:
    """Yield dict with rich metadata for each tool_result event."""
    # meta store per use_id (tool_use arrives before tool_result)
    tool_meta_by_use_id: dict[str, dict] = {}
    session_hash = hash_short(jsonl_path.stem)

    event_idx = 0               # every JSONL entry in this file
    tool_result_ordinal = 0     # N-th tool result emitted in this session
    compaction_count = 0        # number of compactions seen so far
    last_compact_event = -1     # event_idx of last compaction boundary
    context_partition = 0       # increments on each compaction

    with jsonl_path.open("r", encoding="utf-8", errors="ignore") as f:
        for line in f:
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            event_idx += 1

            # Detect compaction markers (at top-level or under message)
            msg = obj.get("message") or {}
            subtype = obj.get("subtype") or msg.get("subtype") or ""
            is_compact = (
                bool(obj.get("isCompactSummary"))
                or bool(msg.get("isCompactSummary"))
                or subtype == "compact_boundary"
            )
            if is_compact:
                compaction_count += 1
                last_compact_event = event_idx
                context_partition += 1

            is_sidechain = bool(obj.get("isSidechain", False))
            ts_ms = _parse_ts(obj.get("timestamp"))
            role = msg.get("role") or obj.get("type")
            content = msg.get("content")
            if not content or isinstance(content, str):
                continue

            # Record tool_use invocations from assistant messages
            if role == "assistant" or obj.get("type") == "assistant":
                for block in content:
                    if not isinstance(block, dict):
                        continue
                    if block.get("type") == "tool_use":
                        use_id = block.get("id", "")
                        if use_id:
                            tool_meta_by_use_id[use_id] = {
                                "name": block.get("name", ""),
                                "input": block.get("input", {}) or {},
                                "event_idx": event_idx,
                                "ts_ms": ts_ms,
                            }
                continue

            # Extract tool_result blocks from user messages
            if role == "user" or obj.get("type") == "user":
                for block in content:
                    if not isinstance(block, dict):
                        continue
                    if block.get("type") != "tool_result":
                        continue
                    use_id = block.get("tool_use_id", "")
                    inner = block.get("content")
                    text = ""
                    if isinstance(inner, str):
                        text = inner
                    elif isinstance(inner, list):
                        parts = []
                        for part in inner:
                            if isinstance(part, dict) and part.get("type") == "text":
                                parts.append(part.get("text", ""))
                        text = "\n".join(parts)
                    else:
                        continue

                    tool_result_ordinal += 1
                    meta = tool_meta_by_use_id.get(use_id, {})
                    raw_name = meta.get("name", "")
                    tool_name_anon = anonymize_tool(raw_name)
                    input_dict = meta.get("input", {}) or {}
                    endpoint_class = derive_endpoint_class(raw_name, input_dict)
                    input_size = len(json.dumps(input_dict, ensure_ascii=False))

                    events_since_compact = (
                        event_idx - last_compact_event if last_compact_event > 0 else -1
                    )

                    yield {
                        "session": session_hash,
                        "tool_use_id_hash": hash_short(use_id, 8) if use_id else "",
                        "tool_name_anon": tool_name_anon,
                        "endpoint_class": endpoint_class,
                        "text": text,
                        "content_sha256": _content_hash(text),
                        "file_path_hash": _file_path_hash(raw_name, input_dict),
                        "event_idx_in_session": event_idx,
                        "tool_result_ordinal": tool_result_ordinal,
                        "ts_ms": ts_ms,
                        "is_sidechain": is_sidechain,
                        "compaction_count_before": compaction_count,
                        "events_since_last_compaction": events_since_compact,
                        "context_partition": context_partition,
                        "input_size_chars": input_size,
                    }


# ─── Checkpoint + write ────────────────────────────────────────────────────

def write_parquet(rows: list[dict], path: Path):
    if not rows:
        return 0
    table = pa.Table.from_pylist(rows)
    path.parent.mkdir(parents=True, exist_ok=True)
    pq.write_table(table, path, compression="zstd")
    return len(rows)


# ─── Main ──────────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(
        description="Extract Paper 2 (format-adaptive) tool-result shape features."
    )
    ap.add_argument("--root", default=str(Path.home() / ".claude/projects"))
    ap.add_argument("--out", default="/tmp/claude_analysis/paper2_format_events.parquet")
    ap.add_argument("--min-chars", type=int, default=200,
                    help="Skip tool_results shorter than this many chars (tiny = no point)")
    ap.add_argument("--savings-threshold", type=float, default=20.0,
                    help="Minimum %% savings vs JSON to mark paper2_applicable")
    ap.add_argument("--checkpoint-every", type=int, default=5000)
    ap.add_argument("--resume", action="store_true",
                    help="Skip already-written parquet; do not re-process")
    args = ap.parse_args()

    root = Path(args.root)
    out = Path(args.out)

    if args.resume and out.exists():
        print(f"# --resume: {out} exists, exiting without work", file=sys.stderr)
        return

    files = sorted(root.rglob("*.jsonl"))
    print(f"# scanning {len(files)} JSONL files under {root}", file=sys.stderr)

    rows: list[dict] = []
    t0 = time.time()
    kept = 0
    skipped_short = 0
    shape_counter: Counter = Counter()
    applicable = 0

    for idx, f in enumerate(files):
        for event in iter_tool_results(f):
            text = event["text"]
            if len(text) < args.min_chars:
                skipped_short += 1
                continue
            feats = analyze_tool_result(text, event["tool_name_anon"])
            if feats is None:
                continue
            # Copy all event metadata fields (except text) into the row
            for k in ("session", "tool_use_id_hash", "endpoint_class",
                      "content_sha256", "file_path_hash",
                      "event_idx_in_session", "tool_result_ordinal", "ts_ms",
                      "is_sidechain", "compaction_count_before",
                      "events_since_last_compaction", "context_partition",
                      "input_size_chars"):
                feats[k] = event[k]
            feats["file_idx"] = idx
            feats["paper2_applicable"] = feats["savings_pct_vs_json"] >= args.savings_threshold
            rows.append(feats)
            kept += 1
            shape_counter[feats["shape"]] += 1
            if feats["paper2_applicable"]:
                applicable += 1

        if len(rows) >= args.checkpoint_every:
            # Flush partial
            part = out.with_suffix(".partial.parquet")
            write_parquet(rows, part)
            print(
                f"# checkpoint: files={idx+1}/{len(files)} kept={kept} "
                f"applicable={applicable} ({100.0*applicable/max(kept,1):.1f}%) "
                f"elapsed={time.time()-t0:.0f}s",
                file=sys.stderr,
            )

    n = write_parquet(rows, out)
    elapsed = time.time() - t0
    print(f"\n# done in {elapsed:.0f}s", file=sys.stderr)
    print(f"# wrote {n:,} rows → {out}", file=sys.stderr)
    print(f"# skipped (too short, <{args.min_chars} chars): {skipped_short:,}",
          file=sys.stderr)
    print(f"# paper2_applicable (savings ≥ {args.savings_threshold}%): "
          f"{applicable:,} ({100.0*applicable/max(kept,1):.1f}%)",
          file=sys.stderr)
    print("\n# shape distribution:", file=sys.stderr)
    for shape, count in sorted(shape_counter.items(), key=lambda t: -t[1]):
        pct = 100.0 * count / max(kept, 1)
        print(f"#   {shape:25s} {count:>8,} ({pct:5.1f}%)", file=sys.stderr)

    # Partial can be removed once final is written
    part = out.with_suffix(".partial.parquet")
    if part.exists() and part != out:
        part.unlink()


if __name__ == "__main__":
    main()
