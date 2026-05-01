#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Heuristic pattern detector + anonymization audit.

Default mode reads `outputs/raw/*.parquet` and flags sessions matching
patterns from our hypothesis loop:

  needs_split         ≥2 idle gaps >8h
  pure_constructor    archetype=Constructor AND rhythm=Phased
  feature_with_debug  Inspector + compacts ≥2 (proxy for retry-loop)
  pipeline_rescue     compacts ≥3 AND lines_added <500 AND commits >0
  ghost_session       Plankton + 0 tools

`--audit-anon` mode walks `outputs/anon/*.parquet` and asserts every
column is free of:

  - raw session UUIDs (8-4-4-4-12 hex pattern)
  - absolute filesystem paths (/Users/, /home/, /tmp/, ~)
  - raw bash commands (`git push`, `kubectl …`, `cargo …`, etc.)
  - raw branch names (`feat/…`, `fix/…`, …)
  - raw mcp slugs (only `mcp__<6hex>__<verb>` allowed)

Exit code is 0 if clean, 1 if any leak found.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

import pyarrow.parquet as pq

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.io import outputs_dirs  # noqa: E402


# ─── Anon-audit patterns ──────────────────────────────────────────────────

# Full-form session UUID like "2c052d83-b765-440b-923d-72b7be0f7b30"
RX_UUID = re.compile(
    r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
    re.I,
)
# Absolute filesystem paths
RX_ABS_PATH = re.compile(r"(?:^|[\s'\"])(?:/Users/|/home/|/tmp/|/var/|~/)", re.I)
# Raw bash commands (smell test)
RX_RAW_BASH = re.compile(
    r"\b(git\s+(?:push|commit|status|checkout|switch)|gh\s+(?:pr|issue)|"
    r"glab\s+(?:mr|api)|kubectl\s+\w+|helm\s+\w+|terraform\s+\w+|"
    r"cargo\s+(?:build|test|check|run|fmt)|docker\s+(?:run|build|compose)|"
    r"npm\s+(?:install|test|run)|pnpm\s+\w+)",
    re.I,
)
# Raw branch names like feat/DEV-123-foo, fix/something, chore/cleanup
RX_RAW_BRANCH = re.compile(r"\b(feat|fix|chore|docs|refactor|test|ci)/[a-zA-Z0-9_\-]{3,}", re.I)
# Raw MCP slug: mcp__server__verb where server is NOT a 6-hex hash
# Allowed form: mcp__[0-9a-f]{6}__verb
RX_MCP_RAW = re.compile(r"mcp__(?![0-9a-f]{6}__)[a-zA-Z][\w\-]*__[\w\-]+")


def _is_string_col(table: pq.lib.Table, col: str) -> bool:  # type: ignore[name-defined]
    t = table.schema.field(col).type
    # arrow types: string, large_string, list<string>, etc.
    return "string" in str(t).lower()


def _walk_strings(value):
    """Yield every string nested anywhere in `value` (lists / tuples / dicts)."""
    if value is None:
        return
    if isinstance(value, str):
        yield value
    elif isinstance(value, (list, tuple)):
        for v in value:
            yield from _walk_strings(v)
    elif isinstance(value, dict):
        for v in value.values():
            yield from _walk_strings(v)


def audit_anon(anon_dir: Path) -> int:
    """Walk every parquet in anon/ and assert no leaks. Returns leak count."""
    leaks: list[tuple[str, str, str, str]] = []  # (file, col, kind, sample)
    parquets = sorted(anon_dir.glob("*.parquet"))
    if not parquets:
        print(f"⚠  no parquet files in {anon_dir}", file=sys.stderr)
        return 0

    for pf in parquets:
        try:
            table = pq.read_table(pf)
        except Exception as e:
            print(f"  ⚠  cannot read {pf.name}: {e}", file=sys.stderr)
            continue
        for col in table.column_names:
            if not _is_string_col(table, col):
                continue
            col_data = table.column(col).to_pylist()
            for value in col_data:
                for s in _walk_strings(value):
                    if not s:
                        continue
                    if RX_UUID.search(s):
                        leaks.append((pf.name, col, "raw_uuid", s[:80]))
                    if RX_ABS_PATH.search(s):
                        leaks.append((pf.name, col, "abs_path", s[:80]))
                    if RX_RAW_BASH.search(s):
                        leaks.append((pf.name, col, "raw_bash", s[:80]))
                    if RX_RAW_BRANCH.search(s):
                        leaks.append((pf.name, col, "raw_branch", s[:80]))
                    if RX_MCP_RAW.search(s):
                        leaks.append((pf.name, col, "raw_mcp_slug", s[:80]))

    # Specific schema rules
    biome_path = anon_dir / "biome.parquet"
    if biome_path.exists():
        rows = pq.read_table(biome_path).to_pylist()
        for r in rows:
            sid = r.get("sid", "")
            if not (
                re.fullmatch(r"s\d{4,}", sid)
                or re.fullmatch(r"a\d{4,}", sid)
                or sid.startswith("sub_")
                or sid == ""
            ):
                leaks.append(("biome.parquet", "sid", "bad_sid_format", sid[:80]))
            project = r.get("project", "")
            if project and not re.fullmatch(r"p[0-9a-f]{4,}", project):
                leaks.append(("biome.parquet", "project", "bad_project_format", project[:80]))

    # Print report
    print("Anon audit report:")
    print(f"  files scanned: {len(parquets)}")
    if not leaks:
        print(f"  ✓ no leaks found")
        return 0
    print(f"  ✗ {len(leaks)} leaks found")
    by_kind: dict[str, int] = {}
    for f, c, k, s in leaks:
        by_kind[k] = by_kind.get(k, 0) + 1
    for k, n in sorted(by_kind.items(), key=lambda x: -x[1]):
        print(f"    {k:<20s} {n:>4d}")
    print("  first 10 examples:")
    for f, c, k, s in leaks[:10]:
        print(f"    [{f}].{c} {k}: {s!r}")
    return 1


# ─── Heuristic pattern detection (default) ────────────────────────────────


def detect_patterns(raw_dir: Path) -> int:
    biome = pq.read_table(raw_dir / "biome.parquet").to_pylist()
    arch = {r["sid"]: r for r in pq.read_table(raw_dir / "archetype.parquet").to_pylist()}
    cmp = {r["sid"]: r for r in pq.read_table(raw_dir / "compact_pattern.parquet").to_pylist()}
    out = {r["sid"]: r for r in pq.read_table(raw_dir / "outputs.parquet").to_pylist()}
    gaps_per_sid: dict[str, int] = {}
    try:
        for r in pq.read_table(raw_dir / "idle_gaps.parquet").to_pylist():
            gaps_per_sid[r["sid"]] = gaps_per_sid.get(r["sid"], 0) + 1
    except Exception:
        pass

    flags = {
        "needs_split": [],
        "pure_constructor": [],
        "feature_with_debug": [],
        "pipeline_rescue": [],
        "ghost_session": [],
    }

    for r in biome:
        sid = r["sid"]
        a = arch.get(sid, {})
        c = cmp.get(sid, {})
        o = out.get(sid, {})
        if r["biome"] == "Plankton" and a.get("total_tools", 0) == 0:
            flags["ghost_session"].append(sid)
        if gaps_per_sid.get(sid, 0) >= 2:
            flags["needs_split"].append(sid)
        if a.get("archetype") == "Constructor" and a.get("rhythm") == "Phased":
            flags["pure_constructor"].append(sid)
        if a.get("archetype") == "Inspector" and c.get("compacts", 0) >= 2:
            flags["feature_with_debug"].append(sid)
        if c.get("compacts", 0) >= 3 and o.get("lines_added", 0) < 500 and o.get("commits_total", 0) > 0:
            flags["pipeline_rescue"].append(sid)

    print("Pattern detector report:")
    for k, sids in flags.items():
        print(f"  {k:<22s} {len(sids):>4d}  examples: {[s[:8] for s in sids[:5]]}")
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--outputs", default=None)
    p.add_argument("--audit-anon", action="store_true",
                   help="Walk anon/ parquets and check for leaks. Exit 1 if any.")
    args = p.parse_args(argv)
    raw_dir, anon_dir = outputs_dirs(Path(args.outputs) if args.outputs else None)
    if args.audit_anon:
        leak_count = audit_anon(anon_dir)
        return 1 if leak_count else 0
    return detect_patterns(raw_dir)


if __name__ == "__main__":
    raise SystemExit(main())
