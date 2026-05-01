#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Markdown report skeleton from parquet bundles.

Reads `outputs/raw/*.parquet` and emits `outputs/reports/<date>.md`.
"""
from __future__ import annotations

import argparse
import sys
from collections import Counter
from datetime import date
from pathlib import Path

import pyarrow.parquet as pq

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.io import outputs_dirs  # noqa: E402
from lib.classifiers import BIOME_EMOJI, ARCHETYPE_EMOJI  # noqa: E402


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--outputs", default=None)
    p.add_argument("--out", default=None, help="Output markdown path")
    args = p.parse_args(argv)
    raw_dir, _ = outputs_dirs(Path(args.outputs) if args.outputs else None)
    reports_dir = raw_dir.parent / "reports"
    reports_dir.mkdir(parents=True, exist_ok=True)

    biome_rows = pq.read_table(raw_dir / "biome.parquet").to_pylist()
    arch_rows = pq.read_table(raw_dir / "archetype.parquet").to_pylist()
    out_rows = pq.read_table(raw_dir / "outputs.parquet").to_pylist()

    biomes = Counter(r["biome"] for r in biome_rows)
    archs = Counter(r["archetype"] for r in arch_rows)
    total_loc = sum(r["lines_added"] for r in out_rows)
    total_feat = sum(r["feat"] for r in out_rows)
    total_fix = sum(r["fix"] for r in out_rows)
    total_review = sum(r["review_fix"] for r in out_rows)
    total_pushes = sum(r["pushes"] for r in out_rows)
    cfr = (total_fix - total_review) / (total_feat + 1) if total_feat else 0

    md = []
    md.append(f"# Usage report — {date.today().isoformat()}\n")
    md.append(f"## 🌊 Aquarium ({len(biome_rows)} entities)\n")
    for b in ("Whale", "Shark", "Dolphin", "Fish", "Shrimp", "Plankton"):
        n = biomes.get(b, 0)
        if n == 0:
            continue
        md.append(f"- {BIOME_EMOJI[b]} **{b}**: {n}")
    md.append("")

    md.append(f"## 🎭 Archetypes ({len(arch_rows)} sessions)\n")
    for a, n in archs.most_common():
        md.append(f"- {ARCHETYPE_EMOJI.get(a, '·')} **{a}**: {n}")
    md.append("")

    md.append("## 🚀 DORA aggregates\n")
    md.append(f"- +LOC: **{total_loc:,}**")
    md.append(f"- feat: {total_feat}, fix: {total_fix} (review {total_review}, prod {total_fix-total_review})")
    md.append(f"- pushes: {total_pushes}")
    cfr_em = "🟢" if cfr < 0.3 else "🟡" if cfr < 1 else "🟠" if cfr < 3 else "🔴"
    cfr_lvl = "Elite" if cfr < 0.3 else "High" if cfr < 1 else "Medium" if cfr < 3 else "Low"
    md.append(f"- True CFR: {cfr_em} **{cfr:.2f}** ({cfr_lvl})")
    md.append("")

    md.append("## 🐋 Top sessions\n")
    sorted_b = sorted(biome_rows, key=lambda r: -r["real_prompts"])[:10]
    for r in sorted_b:
        md.append(f"- {BIOME_EMOJI[r['biome']]} {r['sid'][:8]} R={r['real_prompts']} project={r['project']}")
    md.append("")

    out_path = Path(args.out) if args.out else (reports_dir / f"{date.today().isoformat()}.md")
    out_path.write_text("\n".join(md))
    print(f"report → {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
