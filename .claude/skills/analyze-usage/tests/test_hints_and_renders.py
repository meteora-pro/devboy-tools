"""Hint dictionary + render-share-block coverage tests."""
from __future__ import annotations

import sys
from collections import Counter
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SKILL = _HERE.parent
sys.path.insert(0, str(_SKILL))

from lib import hints, render  # noqa: E402


def _agg() -> dict:
    return {
        "n": 30, "lines_added": 4200,
        "wall_min": 90 * 60, "active_min": 18 * 60,
        "biomes": Counter({"Whale": 1, "Dolphin": 5, "Fish": 24}),
        "archs": Counter({"Constructor": 12, "Researcher": 8, "Operator": 10}),
        "rhythms": Counter({"Phased": 15, "Mono": 10, "Mixed": 5}),
        "stack_loc": Counter({"backend": 3000, "frontend": 1200}),
        "feat": 6, "fix": 2, "review_fix": 0,
        "pushes": 14, "pr_mr": 9, "compacts": 12, "pivots": 3, "subs": 40,
    }


# --- hints -----------------------------------------------------------------


def test_every_section_has_a_hint():
    for section in ["aquarium", "archetypes", "rhythms", "stack",
                    "dora", "friction", "weekly_table", "trends"]:
        text = hints.hint(section)
        assert text, f"missing hint for section: {section}"
        assert len(text) < 280, f"hint for {section} is too long: {len(text)}"


def test_unknown_section_returns_empty_string():
    assert hints.hint("does-not-exist") == ""


def test_hints_module_constant_is_subset_of_function_lookup():
    # Every key in HINTS must be reachable through `hint()`.
    for k, v in hints.HINTS.items():
        assert hints.hint(k) == v


# --- render share blocks ---------------------------------------------------


def test_terminal_share_block_emits_box_drawing_frame():
    lines = render.render_share_block_terminal("April 2026", _agg())
    text = "\n".join(lines)
    assert "SHARE" in text
    assert "April 2026" in text
    assert "│" in text and "└" in text  # box-drawing frame
    # The rendered block must contain the one-liner content
    assert "DORA" in text


def test_markdown_share_block_has_code_fences_and_blockquote():
    lines = render.render_share_block_markdown("April 2026", _agg())
    text = "\n".join(lines)
    assert "### 🔗 Share" in text
    assert "```" in text  # fenced one-liner
    assert text.count("```") == 2  # opening + closing
    assert "> " in text  # blockquoted paragraph


def test_html_share_block_has_copy_buttons_and_unique_ids():
    # Reset the global id counter so test runs are deterministic
    render._share_html_id = 0
    a = render.render_share_block_html("April 2026", _agg())
    b = render.render_share_block_html("May 2026", _agg())
    assert 'class="share-card"' in a
    assert 'class="copy-btn"' in a
    assert 'data-target="share-line-1"' in a
    assert 'data-target="share-para-1"' in a
    # second render must use a different id
    assert 'data-target="share-line-2"' in b
    assert 'data-target="share-para-2"' in b


def test_html_share_block_escapes_html_special_chars():
    agg = _agg()
    out = render.render_share_block_html("Q1 <test>", agg)
    assert "<test>" not in out  # raw < must be escaped
    assert "&lt;test&gt;" in out


# --- SHOW_HINTS toggle propagates --------------------------------------------


def test_terminal_section_includes_hint_when_enabled():
    render.SHOW_HINTS = True
    out = render._term_aquarium(Counter({"Whale": 2}))
    text = "\n".join(out)
    assert "↳" in text  # the hint marker
    assert "Whale" in text  # explainer mentions Whale


def test_terminal_section_omits_hint_when_disabled():
    render.SHOW_HINTS = False
    try:
        out = render._term_aquarium(Counter({"Whale": 2}))
    finally:
        render.SHOW_HINTS = True
    text = "\n".join(out)
    assert "↳" not in text


def test_markdown_section_includes_hint_when_enabled():
    render.SHOW_HINTS = True
    out = render._md_aquarium(Counter({"Whale": 2}))
    text = "\n".join(out)
    assert "> _" in text  # blockquote italic hint marker


def test_html_section_includes_hint_div_when_enabled():
    render.SHOW_HINTS = True
    out = render._html_aquarium(Counter({"Whale": 2}))
    assert 'class="hint"' in out
