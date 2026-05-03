"""Compose copy-pasteable share artefacts from a period aggregate.

Two outputs:

- `share_oneline(label, agg)`  — single line, < 280 chars, social-media
  friendly. Suitable for Slack / Twitter / commit message.
- `share_paragraph(label, agg)` — one paragraph, ~5 sentences, fits a
  PR description or a README "what I did this month" note.

The data contract is the `agg` dict produced by
`scripts/period_report.aggregate()` — the same dict the renderers
consume, no extra plumbing.
"""

from __future__ import annotations

from collections import Counter

from .classifiers import ARCHETYPE_EMOJI, BIOME_EMOJI, RHYTHM_EMOJI


_BIOME_ORDER = ["Whale", "Shark", "Dolphin", "Fish", "Shrimp", "Plankton"]


def _cfr_and_level(agg: dict) -> tuple[float, str, str]:
    """Compute True CFR and its level + emoji. Mirrors `render.cfr_level`."""
    feat = agg.get("feat", 0)
    fix = agg.get("fix", 0)
    review_fix = agg.get("review_fix", 0)
    cfr = (fix - review_fix) / feat if feat else 0.0
    if cfr < 0.30:
        return cfr, "🟢", "Elite"
    if cfr < 1.00:
        return cfr, "🟡", "High"
    if cfr < 3.00:
        return cfr, "🟠", "Medium"
    return cfr, "🔴", "Low"


def _top_archetype(archs: Counter, total: int) -> tuple[str, str, float]:
    if not archs or total <= 0:
        return ("—", "—", 0.0)
    name, n = max(archs.items(), key=lambda kv: kv[1])
    return (ARCHETYPE_EMOJI.get(name, "•"), name, n * 100.0 / total)


def _top_rhythm(rhythms: Counter, total: int) -> tuple[str, str, float]:
    if not rhythms or total <= 0:
        return ("—", "—", 0.0)
    name, n = max(rhythms.items(), key=lambda kv: kv[1])
    return (RHYTHM_EMOJI.get(name, "•"), name, n * 100.0 / total)


def _biome_summary(biomes: Counter) -> str:
    parts: list[str] = []
    for name in _BIOME_ORDER:
        n = biomes.get(name, 0)
        if n > 0:
            parts.append(f"{BIOME_EMOJI[name]}×{n}")
    return " ".join(parts)


def _format_loc(loc: int) -> str:
    if loc >= 1_000_000:
        return f"{loc / 1_000_000:.1f}M"
    if loc >= 1_000:
        return f"{loc / 1_000:.1f}k"
    return str(loc)


def share_oneline(label: str, agg: dict) -> str:
    """Single line, ≤ 280 chars."""
    n = agg.get("n", 0)
    loc = agg.get("lines_added", 0)
    cfr, em, lvl = _cfr_and_level(agg)
    pushes = agg.get("pushes", 0)
    pr_mr = agg.get("pr_mr", 0)
    arch_em, arch_name, arch_pct = _top_archetype(agg.get("archs", Counter()), n)
    rhy_em, rhy_name, _rhy_pct = _top_rhythm(agg.get("rhythms", Counter()), n)
    rhythm_part = f" · rhythm {rhy_em} {rhy_name}" if rhy_name != "—" else ""
    return (
        f"{label} · {n} sessions · +{_format_loc(loc)} LOC · "
        f"DORA {em} {lvl} (CFR {cfr:.2f}, {pushes}🚀 {pr_mr}PR/MR) · "
        f"{arch_em} {arch_name} {arch_pct:.0f}%{rhythm_part}"
    )


def share_paragraph(label: str, agg: dict) -> str:
    """One paragraph, ~5 sentences. Markdown-friendly prose."""
    n = agg.get("n", 0)
    loc = agg.get("lines_added", 0)
    wall_h = int(agg.get("wall_min", 0) / 60)
    act_h = int(agg.get("active_min", 0) / 60)
    ratio = (act_h * 100 / wall_h) if wall_h else 0
    biomes_str = _biome_summary(agg.get("biomes", Counter())) or "no sessions"
    arch_em, arch_name, arch_pct = _top_archetype(agg.get("archs", Counter()), n)
    rhy_em, rhy_name, rhy_pct = _top_rhythm(agg.get("rhythms", Counter()), n)
    cfr, em, lvl = _cfr_and_level(agg)
    pushes = agg.get("pushes", 0)
    pr_mr = agg.get("pr_mr", 0)
    feat = agg.get("feat", 0)
    fix = agg.get("fix", 0)
    review_fix = agg.get("review_fix", 0)
    prod_fix = fix - review_fix
    compacts = agg.get("compacts", 0)
    pivots = agg.get("pivots", 0)

    sentences = [
        f"**{label}**: {n} live Claude Code sessions, +{_format_loc(loc)} LOC across "
        f"wall {wall_h}h / active {act_h}h ({ratio:.0f}%).",
        f"Biome mix — {biomes_str}.",
        f"Top archetype was {arch_em} {arch_name} ({arch_pct:.0f}%); "
        f"dominant rhythm {rhy_em} {rhy_name} ({rhy_pct:.0f}%).",
        f"DORA radar: {em} {lvl} — True CFR {cfr:.2f} "
        f"({prod_fix} prod_fix / {feat} feat, {review_fix} review-fix excluded), "
        f"{pushes} pushes and {pr_mr} PR/MR.",
        f"Friction: {compacts}🌀 compacts, {pivots}🚧 pivots.",
    ]
    return " ".join(sentences)
