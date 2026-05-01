"""I/O helpers for extractors: parquet write + output paths."""

from __future__ import annotations

import os
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq


DEFAULT_OUTPUTS = Path(__file__).resolve().parent.parent / "outputs"


def outputs_dirs(base: Path | None = None) -> tuple[Path, Path]:
    """Return (raw_dir, anon_dir), creating them if needed.

    Backwards-compatible: returns just (raw, anon). Use `outputs_dirs_v2()`
    if you also need the LLM dir.
    """
    raw, anon, _ = outputs_dirs_v2(base)
    return raw, anon


def outputs_dirs_v2(base: Path | None = None) -> tuple[Path, Path, Path]:
    """Three-tier output directories.

    raw/   — Tier 1 stat extraction with original identifiers (owner-only)
    anon/  — Tier 1 anonymized version (shareable)
    llm/   — Tier 2 LLM-augmented features (session names, phase narratives,
             pattern interpretations). Owner-only because narrative content
             may include real session/project names.
    """
    base = Path(base) if base else DEFAULT_OUTPUTS
    raw = base / "raw"
    anon = base / "anon"
    llm = base / "llm"
    for d in (raw, anon, llm):
        d.mkdir(parents=True, exist_ok=True)
    return raw, anon, llm


def write_parquet(
    rows: list[dict], path: Path, schema: pa.Schema | None = None
) -> int:
    """Write rows to parquet. Returns number of rows written.

    When `rows` is empty and `schema` is provided, writes an empty table
    with the given schema so downstream consumers see a stable column set
    (matters for Tier-2 stubs like `session_names.parquet` where the
    file may be queried before any rows are populated).
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    if not rows:
        # Write an empty marker so the pipeline can detect the run happened.
        if schema is not None:
            empty = pa.Table.from_arrays(
                [pa.array([], type=field.type) for field in schema],
                schema=schema,
            )
        else:
            empty = pa.Table.from_pylist([])
        pq.write_table(empty, str(path))
        return 0
    table = pa.Table.from_pylist(rows)
    pq.write_table(table, str(path), compression="snappy")
    return len(rows)
