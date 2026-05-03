"""Share-block composer tests.

Verifies that `share_oneline` / `share_paragraph` produce stable,
copy-pasteable output from a synthetic period aggregate, and that the
DORA level / top-N rollups respond to the right input fields.
"""
from __future__ import annotations

import sys
from collections import Counter
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SKILL = _HERE.parent
sys.path.insert(0, str(_SKILL))

from lib.share import share_oneline, share_paragraph  # noqa: E402


def _agg(**overrides) -> dict:
    """Synthetic period aggregate with sensible defaults."""
    base = {
        "n": 142,
        "lines_added": 18420,
        "wall_min": 168 * 60,
        "active_min": 31 * 60,
        "biomes": Counter({"Whale": 3, "Shark": 5, "Dolphin": 24, "Fish": 62,
                           "Shrimp": 38, "Plankton": 10}),
        "archs": Counter({"Constructor": 54, "Researcher": 31, "Polymath": 20,
                          "Builder": 12, "Operator": 25}),
        "rhythms": Counter({"Phased": 62, "Mono": 30, "Mixed": 35, "Chaos": 15}),
        "stack_loc": Counter({"backend": 10000, "frontend": 4000, "infra": 2000}),
        "feat": 18, "fix": 7, "review_fix": 3,
        "pushes": 47, "pr_mr": 23, "compacts": 89, "pivots": 12, "subs": 340,
    }
    base.update(overrides)
    return base


# --- share_oneline ---------------------------------------------------------


def test_oneline_includes_all_keys():
    line = share_oneline("April 2026", _agg())
    # Every callout must show up
    assert "April 2026" in line
    assert "142 sessions" in line
    assert "+18.4k LOC" in line
    assert "DORA" in line
    assert "Elite" in line
    assert "0.22" in line  # CFR (7-3)/18 = 0.22
    assert "47🚀" in line
    assert "23PR/MR" in line
    assert "🏗" in line
    assert "Constructor" in line
    assert "rhythm" in line


def test_oneline_is_short_enough_for_twitter():
    """A monthly digest should fit in 280 chars by default."""
    line = share_oneline("April 2026", _agg())
    assert len(line) <= 280, f"line is {len(line)} chars: {line}"


def test_oneline_dora_levels_thresholds():
    # CFR < 0.30 → Elite 🟢
    elite = share_oneline("M", _agg(feat=10, fix=2, review_fix=0))  # 0.20
    assert "🟢" in elite and "Elite" in elite
    # 0.30 ≤ CFR < 1.0 → High 🟡
    high = share_oneline("M", _agg(feat=10, fix=5, review_fix=0))  # 0.50
    assert "🟡" in high and "High" in high
    # 1.0 ≤ CFR < 3.0 → Medium 🟠
    medium = share_oneline("M", _agg(feat=10, fix=15, review_fix=0))  # 1.50
    assert "🟠" in medium and "Medium" in medium
    # CFR ≥ 3.0 → Low 🔴
    low = share_oneline("M", _agg(feat=2, fix=10, review_fix=0))  # 5.00
    assert "🔴" in low and "Low" in low


def test_oneline_review_fix_excluded_from_cfr():
    # 5 fix - 4 review = 1 prod_fix; over 10 feat → CFR 0.10 → Elite
    line = share_oneline("M", _agg(feat=10, fix=5, review_fix=4))
    assert "0.10" in line and "Elite" in line


def test_oneline_zero_feat_does_not_divide_by_zero():
    line = share_oneline("M", _agg(feat=0, fix=3, review_fix=0))
    assert "0.00" in line  # CFR defaults to 0.0


def test_oneline_loc_formatting_thresholds():
    """`_format_loc` rounds to k / M with one decimal."""
    assert "+420 LOC" in share_oneline("M", _agg(lines_added=420))
    assert "+1.5k LOC" in share_oneline("M", _agg(lines_added=1500))
    assert "+2.5M LOC" in share_oneline("M", _agg(lines_added=2_500_000))


def test_oneline_handles_empty_agg():
    """No archetypes / rhythms / sessions should not raise."""
    empty = {
        "n": 0, "lines_added": 0, "wall_min": 0, "active_min": 0,
        "biomes": Counter(), "archs": Counter(), "rhythms": Counter(),
        "stack_loc": Counter(),
        "feat": 0, "fix": 0, "review_fix": 0,
        "pushes": 0, "pr_mr": 0, "compacts": 0, "pivots": 0, "subs": 0,
    }
    line = share_oneline("Q", empty)
    assert "0 sessions" in line
    assert "+0 LOC" in line
    assert "Elite" in line  # 0/0 → 0 → Elite


# --- share_paragraph -------------------------------------------------------


def test_paragraph_mentions_every_metric_family():
    para = share_paragraph("April 2026", _agg())
    assert "**April 2026**" in para
    assert "Claude Code sessions" in para
    assert "wall 168h" in para and "active 31h" in para
    assert "(18%)" in para  # 31/168 ≈ 18%
    assert "🐋×3" in para and "🦈×5" in para  # biome counts
    assert "Constructor" in para
    assert "Phased" in para
    assert "Elite" in para
    assert "True CFR" in para
    assert "47 pushes" in para
    assert "23 PR/MR" in para
    assert "89🌀" in para
    assert "12🚧" in para


def test_paragraph_is_one_paragraph_no_newlines():
    """Must be a single line so it pastes cleanly into a Slack/Twitter box."""
    para = share_paragraph("April 2026", _agg())
    assert "\n" not in para


def test_paragraph_biome_summary_skips_zero_counts():
    para = share_paragraph(
        "Q",
        _agg(biomes=Counter({"Whale": 1, "Shark": 0, "Dolphin": 4})),
    )
    assert "🐋×1" in para
    assert "🐬×4" in para
    assert "🦈" not in para  # zero-count biome must not appear
