"""Short one-line explainers for each report section.

The period digest renders these as a dim/italic line right after the
section header so a reader who doesn't know our jargon can decode the
emoji/labels without reading GLOSSARY.md. Keep each entry under 280
chars (the same upper bound `tests/test_hints_and_renders.py` asserts);
shorter is better, but several sections legitimately need to enumerate
their emoji legends (archetypes, biomes).

Keys map onto the section identifiers used by `lib.render`. The
period_report orchestrator opts the whole thing in or out via
`--no-hints`.
"""

from __future__ import annotations


HINTS: dict[str, str] = {
    "aquarium": (
        "Sessions bucketed by real-prompt count: "
        "🐋 Whale ≥500 · 🦈 Shark 100–499 · 🐬 Dolphin 30–99 · "
        "🐟 Fish 10–29 · 🦐 Shrimp 3–9 · 🦠 Plankton 0–2."
    ),
    "archetypes": (
        "Dominant tool mix per session: "
        "⚙ Operator (Bash-heavy) · 🔬 Researcher (Read-heavy) · "
        "🌐 Polymath (balanced) · 🛠 Builder (Edit + Bash) · "
        "📝 Scholar (Read + Edit) · 🔍 Inspector (Read + Bash) · "
        "🏗 Constructor (Edit-heavy) · 💬 Discusser (text only)."
    ),
    "rhythms": (
        "Order of tool use within a session: "
        "🎼 Mono (one phase) · 📊 Phased (clear phases) · "
        "🎲 Mixed (some structure) · 🌪 Chaos (high entropy)."
    ),
    "stack": (
        "Lines added grouped by file path: "
        "⚡ backend · 🎨 frontend · ☁️ infra · 📖 docs · ⚙ config · 📦 other."
    ),
    "dora": (
        "True CFR = (fix − review_fix) / feat — review-fix commits are "
        "excluded from the fix count. Levels: 🟢 Elite < 0.30 · "
        "🟡 High < 1.00 · 🟠 Medium < 3.00 · 🔴 Low ≥ 3.00."
    ),
    "friction": (
        "Hidden cost markers: 🌀 compact = the context window overflowed and "
        "Claude Code auto-summarised history; 🚧 pivot = `[Request "
        "interrupted by user]`; 🤖 subagent = a Task / Agent spawn."
    ),
    "weekly_table": (
        "Per-ISO-week breakdown of activity. CFR uses the same Elite/High/"
        "Medium/Low colour scheme as the DORA radar."
    ),
    "trends": (
        "Across-period view (≥ 2 months). 🚀↑↑↑ / 📉↓↓↓ mark the magnitude "
        "of the change; 🌋 marks a peak month, 🪂 a dip."
    ),
}


def hint(section: str) -> str:
    """Return the hint string for a section id, or `''` if none."""
    return HINTS.get(section, "")
