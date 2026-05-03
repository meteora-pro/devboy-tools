"""Period-report renderers: terminal / markdown / HTML.

All three consume the same `aggregate()` data dict (from period_report.py)
and produce a string. The terminal renderer mirrors what we used to print
inline; markdown is GitHub/Slack-friendly; HTML is a single-file page with
inline CSS (dark theme, biome colours).
"""

from __future__ import annotations

from collections import Counter
from datetime import datetime, timedelta
from html import escape as h

from .classifiers import (
    ARCHETYPE_EMOJI,
    BIOME_EMOJI,
    RHYTHM_EMOJI,
)
from .hints import hint
from .share import share_oneline, share_paragraph


# Toggled off by `period_report.py --no-hints`. Module-level state keeps
# the section-render helpers' signatures unchanged.
SHOW_HINTS = True


def _hint_terminal(section: str) -> list[str]:
    text = hint(section) if SHOW_HINTS else ""
    return [f"     ↳ {text}"] if text else []


def _hint_markdown(section: str) -> list[str]:
    text = hint(section) if SHOW_HINTS else ""
    return [f"> _{text}_", ""] if text else []


def _hint_html(section: str) -> str:
    text = hint(section) if SHOW_HINTS else ""
    return f'<p class="hint">{h(text)}</p>' if text else ""


# ─── Common helpers ───────────────────────────────────────────────────────


_BIOMES_ORDER = ["Whale", "Shark", "Dolphin", "Fish", "Shrimp", "Plankton"]
_ARCHS_ORDER = ["Operator", "Researcher", "Polymath", "Inspector", "Builder",
                "Scholar", "Constructor", "Discusser"]
_RHYTHMS_ORDER = ["Mono", "Phased", "Mixed", "Chaos", "—"]
_STACKS_ORDER = ["backend", "frontend", "infra", "docs", "config", "other"]
_STACK_EMOJI = {
    "frontend": "🎨", "backend": "⚡", "infra": "☁️",
    "docs": "📖", "config": "⚙", "other": "📦",
}
# Biome colours for HTML
_BIOME_COLOR = {
    "Whale":    "#1e3a8a",  # deep navy
    "Shark":    "#0369a1",  # steel blue
    "Dolphin":  "#0891b2",  # sky / cyan
    "Fish":     "#0d9488",  # teal
    "Shrimp":   "#ea580c",  # coral
    "Plankton": "#a3a3a3",  # neutral grey
}


def cfr_level(cfr: float) -> tuple[str, str]:
    """Return (emoji, level)."""
    if cfr < 0.3:
        return ("🟢", "Elite")
    if cfr < 1:
        return ("🟡", "High")
    if cfr < 3:
        return ("🟠", "Medium")
    return ("🔴", "Low")


# ─── Terminal renderer ────────────────────────────────────────────────────


def _term_aquarium(biomes: Counter) -> list[str]:
    out = ["  🌊 AQUARIUM:"]
    out.extend(_hint_terminal("aquarium"))
    for b in _BIOMES_ORDER:
        n = biomes.get(b, 0)
        if n == 0:
            continue
        em = BIOME_EMOJI[b]
        visual = em * n if n <= 30 else em * 5 + f"·×{n}"
        out.append(f"     {b:<10s}  {visual}")
    return out


def _term_archs(archs: Counter, total: int) -> list[str]:
    out = ["  🎭 ARCHETYPES:"]
    out.extend(_hint_terminal("archetypes"))
    if total == 0:
        return out
    max_a = max(archs.values()) if archs else 1
    width = 30
    for a in _ARCHS_ORDER:
        n = archs.get(a, 0)
        if n == 0:
            continue
        bar_n = int(n * width / max_a)
        bars = "█" * bar_n + "░" * (width - bar_n)
        out.append(f"    {ARCHETYPE_EMOJI[a]} {a:<14s} {n:>3d}  │{bars}│ {n*100/total:>4.1f}%")
    return out


def _term_rhythm(rhythms: Counter, total: int) -> list[str]:
    out = ["  🎵 RHYTHMS:"]
    out.extend(_hint_terminal("rhythms"))
    if total == 0:
        return out
    for r in _RHYTHMS_ORDER:
        n = rhythms.get(r, 0)
        if n == 0:
            continue
        em = RHYTHM_EMOJI[r]
        bars = em * int(n * 20 / total)
        out.append(f"     {em} {r:<8s} {n:>3d}  {bars}")
    return out


def _term_stack(stack_loc: Counter) -> list[str]:
    out = ["  🎨 STACK PALETTE (LOC):"]
    out.extend(_hint_terminal("stack"))
    total = sum(stack_loc.values()) or 1
    for st in _STACKS_ORDER:
        v = stack_loc.get(st, 0)
        if v == 0:
            continue
        pct = v * 100 / total
        n_icons = max(1, int(pct / 2))
        bar = (_STACK_EMOJI[st] * n_icons)[:50]
        out.append(f"     {st:<10s} {pct:>4.1f}%  {bar}")
    return out


def _term_dora(agg: dict) -> list[str]:
    cfr = (agg["fix"] - agg["review_fix"]) / agg["feat"] if agg["feat"] else 0
    em, level = cfr_level(cfr)
    out = ["  🚀 DORA RADAR:"]
    out.extend(_hint_terminal("dora"))
    out += [
        "     ┌────────────────────────────────────────────┐",
        f"     │  📤 Pushes:       {agg['pushes']:>4d}                       │",
        f"     │  🔀 PR/MR:        {agg['pr_mr']:>4d}                       │",
        f"     │  ✨ feat:         {agg['feat']:>4d}                       │",
        f"     │  🐛 fix:          {agg['fix']:>4d}  (review {agg['review_fix']}, prod {agg['fix']-agg['review_fix']})  │",
        f"     │  {em} CFR:          {cfr:>4.2f}  [{level:<6s}]            │",
        "     └────────────────────────────────────────────┘",
    ]
    return out


def _term_friction(agg: dict) -> list[str]:
    out = ["  ⚡ FRICTION:"]
    out.extend(_hint_terminal("friction"))
    out += [
        f"     compacts ({agg['compacts']})    {'🌀' * min(agg['compacts'] // 10, 30)}",
        f"     pivots   ({agg['pivots']})    {'🚧' * min(agg['pivots'] // 5, 30)}",
        f"     subagents({agg['subs']})    {'🤖' * min(agg['subs'] // 20, 30)}",
    ]
    return out


def render_period_terminal(label: str, agg: dict) -> list[str]:
    out: list[str] = []
    out.append("")
    out.append("▓" * 100)
    out.append(f"  {label}")
    out.append("▓" * 100)
    wall_h = int(agg["wall_min"] / 60)
    act_h = int(agg["active_min"] / 60)
    ratio = act_h * 100 / wall_h if wall_h else 0
    out.append(
        f"  📊 {agg['n']} sessions | +{agg['lines_added']:,} LOC | "
        f"wall {wall_h}h | active {act_h}h ({ratio:.0f}%)"
    )
    out.append("")
    out.extend(_term_aquarium(agg["biomes"]))
    out.append("")
    out.extend(_term_archs(agg["archs"], agg["n"]))
    out.append("")
    out.extend(_term_rhythm(agg["rhythms"], agg["n"]))
    out.append("")
    out.extend(_term_stack(agg["stack_loc"]))
    out.append("")
    out.extend(_term_dora(agg))
    out.append("")
    out.extend(_term_friction(agg))
    return out


# ─── Markdown renderer ───────────────────────────────────────────────────


def _md_aquarium(biomes: Counter) -> list[str]:
    out = ["### 🌊 Aquarium", ""]
    out.extend(_hint_markdown("aquarium"))
    for b in _BIOMES_ORDER:
        n = biomes.get(b, 0)
        if n == 0:
            continue
        em = BIOME_EMOJI[b]
        visual = em * n if n <= 30 else em * 5 + f"·×{n}"
        out.append(f"- {em} **{b}** — {visual}")
    out.append("")
    return out


def _md_archs(archs: Counter, total: int) -> list[str]:
    if total == 0:
        return []
    out = ["### 🎭 Archetypes", ""]
    out.extend(_hint_markdown("archetypes"))
    out += ["| | Archetype | N | % | |", "|--|--|--:|--:|--|"]
    for a in _ARCHS_ORDER:
        n = archs.get(a, 0)
        if n == 0:
            continue
        bar_n = int(n * 20 / max(1, max(archs.values())))
        bar = "█" * bar_n + "░" * (20 - bar_n)
        out.append(f"| {ARCHETYPE_EMOJI[a]} | {a} | {n} | {n*100/total:.1f}% | `{bar}` |")
    out.append("")
    return out


def _md_rhythm(rhythms: Counter, total: int) -> list[str]:
    if total == 0:
        return []
    out = ["### 🎵 Rhythms", ""]
    out.extend(_hint_markdown("rhythms"))
    out += ["| Rhythm | N | % |", "|--|--:|--:|"]
    for r in _RHYTHMS_ORDER:
        n = rhythms.get(r, 0)
        if n == 0:
            continue
        out.append(f"| {RHYTHM_EMOJI[r]} {r} | {n} | {n*100/total:.1f}% |")
    out.append("")
    return out


def _md_stack(stack_loc: Counter) -> list[str]:
    total = sum(stack_loc.values())
    if total == 0:
        return []
    out = ["### 🎨 Stack palette (LOC)", ""]
    out.extend(_hint_markdown("stack"))
    out += ["| Layer | LOC | % |", "|--|--:|--:|"]
    for st in _STACKS_ORDER:
        v = stack_loc.get(st, 0)
        if v == 0:
            continue
        pct = v * 100 / total
        out.append(f"| {_STACK_EMOJI[st]} {st} | {v:,} | {pct:.1f}% |")
    out.append("")
    return out


def _md_dora(agg: dict) -> list[str]:
    cfr = (agg["fix"] - agg["review_fix"]) / agg["feat"] if agg["feat"] else 0
    em, level = cfr_level(cfr)
    out = ["### 🚀 DORA", ""]
    out.extend(_hint_markdown("dora"))
    out += [
        "| Metric | Value |",
        "|--|--:|",
        f"| 📤 Pushes | {agg['pushes']} |",
        f"| 🔀 PR/MR | {agg['pr_mr']} |",
        f"| ✨ feat | {agg['feat']} |",
        f"| 🐛 fix | {agg['fix']} (review {agg['review_fix']}, prod {agg['fix']-agg['review_fix']}) |",
        f"| {em} **True CFR** | **{cfr:.2f}** ({level}) |",
        "",
    ]
    return out


def _md_friction(agg: dict) -> list[str]:
    out = ["### ⚡ Friction", ""]
    out.extend(_hint_markdown("friction"))
    out += [
        f"- 🌀 compacts: **{agg['compacts']}**",
        f"- 🚧 pivots: **{agg['pivots']}**",
        f"- 🤖 subagent spawns: **{agg['subs']}**",
        "",
    ]
    return out


def render_period_markdown(label: str, agg: dict) -> list[str]:
    out: list[str] = ["", f"## {label}", ""]
    wall_h = int(agg["wall_min"] / 60)
    act_h = int(agg["active_min"] / 60)
    ratio = act_h * 100 / wall_h if wall_h else 0
    out.append(
        f"**{agg['n']} sessions** · +{agg['lines_added']:,} LOC · "
        f"wall {wall_h}h · active {act_h}h ({ratio:.0f}%)"
    )
    out.append("")
    out.extend(_md_aquarium(agg["biomes"]))
    out.extend(_md_archs(agg["archs"], agg["n"]))
    out.extend(_md_rhythm(agg["rhythms"], agg["n"]))
    out.extend(_md_stack(agg["stack_loc"]))
    out.extend(_md_dora(agg))
    out.extend(_md_friction(agg))
    return out


# ─── HTML renderer ───────────────────────────────────────────────────────


_HTML_HEAD = """<!DOCTYPE html>
<html lang="ru"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{
  --bg: #0f172a;
  --panel: #1e293b;
  --panel-2: #334155;
  --text: #e2e8f0;
  --text-dim: #94a3b8;
  --accent: #06b6d4;
  --accent-dim: #0891b2;
  --success: #10b981;
  --warn: #f59e0b;
  --danger: #ef4444;
}}
* {{ box-sizing: border-box; }}
body {{
  margin: 0; padding: 24px;
  font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif;
  background: linear-gradient(180deg, #0c1828 0%, #0f172a 100%);
  color: var(--text);
  line-height: 1.5;
}}
.container {{ max-width: 1100px; margin: 0 auto; }}
h1 {{
  font-size: 2.2rem;
  background: linear-gradient(135deg, #06b6d4, #0891b2, #0ea5e9);
  -webkit-background-clip: text;
  background-clip: text;
  color: transparent;
  margin: 0 0 8px;
}}
h2 {{ font-size: 1.6rem; margin: 0 0 12px; }}
h3 {{ font-size: 1.1rem; margin: 24px 0 12px; color: var(--accent); }}
.subtitle {{ color: var(--text-dim); margin-bottom: 32px; }}
.period {{
  background: var(--panel);
  border-radius: 12px;
  padding: 24px;
  margin-bottom: 24px;
  border: 1px solid var(--panel-2);
}}
.meta {{
  color: var(--text-dim);
  font-size: 0.95rem;
  margin-bottom: 16px;
}}
.meta strong {{ color: var(--text); font-size: 1.2rem; }}
.aquarium {{ display: flex; flex-wrap: wrap; gap: 8px; }}
.creature {{
  display: inline-flex; align-items: center; gap: 8px;
  padding: 6px 12px;
  border-radius: 999px;
  font-weight: 500;
  color: white;
}}
.bar-row {{
  display: grid;
  grid-template-columns: 180px 1fr 60px;
  gap: 12px; align-items: center;
  padding: 4px 0;
}}
.bar-label {{ font-weight: 500; }}
.bar-track {{
  background: var(--panel-2);
  border-radius: 4px;
  overflow: hidden;
  height: 22px; position: relative;
}}
.bar-fill {{
  height: 100%;
  background: linear-gradient(90deg, var(--accent-dim), var(--accent));
  display: flex; align-items: center;
  padding: 0 8px;
  color: white; font-size: 0.85rem; font-weight: 500;
  white-space: nowrap;
}}
.bar-value {{ text-align: right; color: var(--text-dim); font-variant-numeric: tabular-nums; }}
.dora {{
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 12px;
  margin-top: 12px;
}}
.dora-card {{
  background: var(--panel-2);
  padding: 12px 16px;
  border-radius: 8px;
}}
.dora-card .label {{ color: var(--text-dim); font-size: 0.85rem; }}
.dora-card .value {{ font-size: 1.6rem; font-weight: 600; margin-top: 4px; }}
.dora-card.cfr-elite  {{ border-left: 4px solid var(--success); }}
.dora-card.cfr-high   {{ border-left: 4px solid var(--warn); }}
.dora-card.cfr-medium {{ border-left: 4px solid #fb923c; }}
.dora-card.cfr-low    {{ border-left: 4px solid var(--danger); }}
.friction {{
  display: flex; gap: 24px; flex-wrap: wrap;
  margin-top: 12px;
}}
.friction div {{
  background: var(--panel-2);
  padding: 8px 14px; border-radius: 6px;
  font-size: 0.95rem;
}}
.weeks-table {{
  width: 100%;
  border-collapse: collapse;
  margin-top: 12px;
  font-size: 0.92rem;
  font-variant-numeric: tabular-nums;
}}
.weeks-table th, .weeks-table td {{
  padding: 8px 10px;
  border-bottom: 1px solid var(--panel-2);
  text-align: left;
}}
.weeks-table th {{ color: var(--text-dim); font-weight: 500; }}
.weeks-table td.num {{ text-align: right; }}
.bar-mini {{
  display: inline-block;
  height: 8px; border-radius: 4px;
  background: var(--accent);
  vertical-align: middle;
}}
.trends-table {{
  width: 100%; border-collapse: collapse; margin-top: 12px;
  font-variant-numeric: tabular-nums;
}}
.trends-table th, .trends-table td {{
  padding: 6px 10px;
  border-bottom: 1px solid var(--panel-2);
}}
.trends-table th {{ text-align: left; color: var(--text-dim); }}
.trends-table td.num {{ text-align: right; }}
.trend-arrow {{ font-size: 0.85rem; color: var(--text-dim); }}
.hint {{
  color: var(--text-dim);
  font-size: 0.82rem;
  font-style: italic;
  margin: -6px 0 12px;
  line-height: 1.45;
}}
.share-card {{
  background: linear-gradient(135deg, #0e7490 0%, #155e75 100%);
  border-radius: 12px;
  padding: 16px 18px;
  margin: 0 0 18px;
  border: 1px solid #0891b2;
  position: relative;
}}
.share-card .share-title {{
  font-size: 0.78rem;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: #cffafe;
  margin: 0 0 10px;
  font-weight: 600;
}}
.share-card .share-line,
.share-card .share-paragraph {{
  background: rgba(0, 0, 0, 0.18);
  border-radius: 6px;
  padding: 10px 12px;
  margin: 6px 0 10px;
  font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  font-size: 0.92rem;
  color: #ffffff;
  white-space: pre-wrap;
  word-break: break-word;
}}
.share-card .share-paragraph {{
  font-family: inherit;
  font-size: 0.95rem;
  line-height: 1.55;
}}
.copy-btn {{
  background: #06b6d4;
  border: none;
  color: white;
  font-size: 0.78rem;
  font-weight: 600;
  padding: 5px 11px;
  border-radius: 5px;
  cursor: pointer;
  margin-left: 8px;
  vertical-align: middle;
  transition: background 0.15s;
}}
.copy-btn:hover {{ background: #0891b2; }}
.copy-btn.copied {{ background: var(--success); }}
footer {{ color: var(--text-dim); font-size: 0.85rem; margin-top: 32px; text-align: center; }}
</style></head><body>
<div class="container">
"""

_HTML_TAIL = """
<footer>Generated by analyze-usage skill · {ts}</footer>
</div>
<script>
function flashCopyButton(btn, label) {
  const orig = btn.textContent;
  btn.textContent = label;
  btn.classList.add('copied');
  setTimeout(() => {
    btn.textContent = orig;
    btn.classList.remove('copied');
  }, 1600);
}
function selectionFallback(el) {
  // Used when navigator.clipboard is unavailable (file:// URLs, http
  // pages without secure context, Firefox with permissions denied) or
  // its writeText() promise rejects. Selects the element's text so the
  // user can hit ⌘/Ctrl+C as a manual fallback.
  const range = document.createRange();
  range.selectNodeContents(el);
  const sel = window.getSelection();
  sel.removeAllRanges();
  sel.addRange(range);
}
document.querySelectorAll('.copy-btn').forEach(btn => {
  btn.addEventListener('click', () => {
    const id = btn.getAttribute('data-target');
    const el = document.getElementById(id);
    if (!el) return;
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(el.textContent).then(
        () => flashCopyButton(btn, '✓ Copied'),
        () => {
          // Permission denied or insecure context. Select the text and
          // tell the user how to copy manually instead of failing
          // silently with an unhandled promise rejection.
          selectionFallback(el);
          flashCopyButton(btn, '⌘/Ctrl+C');
        }
      );
    } else {
      selectionFallback(el);
      flashCopyButton(btn, '⌘/Ctrl+C');
    }
  });
});
</script>
</body></html>
"""


# ─── Share block renderers (Issue: hints + share PR) ──────────────────────


def render_share_block_terminal(label: str, agg: dict) -> list[str]:
    """Single line + paragraph, plain text, no markup. Easy to scrape."""
    line = share_oneline(label, agg)
    para = share_paragraph(label, agg)
    return [
        "  ┌─ SHARE ─────────────────────────────────────────────────────────",
        f"  │ {line}",
        "  │",
        "  │ " + para.replace("\n", "\n  │ "),
        "  └─────────────────────────────────────────────────────────────────",
    ]


def render_share_block_markdown(label: str, agg: dict) -> list[str]:
    """Two fenced blocks so users can copy either format from a Markdown viewer."""
    line = share_oneline(label, agg)
    para = share_paragraph(label, agg)
    return [
        "### 🔗 Share",
        "",
        "**One-liner:**",
        "",
        "```",
        line,
        "```",
        "",
        "**Paragraph:**",
        "",
        "> " + para,
        "",
    ]


# Counter for unique element ids inside one HTML doc — share block can
# render multiple times (per period / per week) so ids must not clash.
_share_html_id = 0


def render_share_block_html(label: str, agg: dict) -> str:
    """Card-style block with a copy-to-clipboard button on each artefact."""
    global _share_html_id
    _share_html_id += 1
    line_id = f"share-line-{_share_html_id}"
    para_id = f"share-para-{_share_html_id}"
    line = share_oneline(label, agg)
    para = share_paragraph(label, agg)
    return (
        '<div class="share-card">'
        f'<div class="share-title">🔗 Share — {h(label)}'
        f'<button class="copy-btn" data-target="{line_id}">📋 Copy line</button>'
        f'<button class="copy-btn" data-target="{para_id}">📋 Copy paragraph</button>'
        '</div>'
        f'<div class="share-line" id="{line_id}">{h(line)}</div>'
        f'<div class="share-paragraph" id="{para_id}">{h(para)}</div>'
        '</div>'
    )


def _html_aquarium(biomes: Counter) -> str:
    out = ['<h3>🌊 Aquarium</h3>', _hint_html("aquarium"), '<div class="aquarium">']
    for b in _BIOMES_ORDER:
        n = biomes.get(b, 0)
        if n == 0:
            continue
        em = BIOME_EMOJI[b]
        color = _BIOME_COLOR[b]
        out.append(
            f'<div class="creature" style="background:{color}">'
            f'{em} {h(b)} × {n}</div>'
        )
    out.append("</div>")
    return "\n".join(out)


def _html_bar_chart(title: str, items: list[tuple[str, str, int]], total: int,
                    hint_section: str = "") -> str:
    """items: list of (emoji, label, count). `hint_section` adds an
    explainer line above the chart when set and SHOW_HINTS is true."""
    if total == 0:
        return ""
    max_v = max((c for _, _, c in items), default=1)
    out = [f"<h3>{h(title)}</h3>"]
    if hint_section:
        out.append(_hint_html(hint_section))
    out.append('<div class="bar-chart">')
    for em, label, n in items:
        if n == 0:
            continue
        pct = n * 100 / total
        width = max(2, int(n * 100 / max_v))
        out.append(
            '<div class="bar-row">'
            f'<span class="bar-label">{em} {h(label)}</span>'
            f'<div class="bar-track"><div class="bar-fill" style="width:{width}%">{pct:.1f}%</div></div>'
            f'<span class="bar-value">{n}</span>'
            '</div>'
        )
    out.append("</div>")
    return "\n".join(out)


def _html_dora(agg: dict) -> str:
    cfr = (agg["fix"] - agg["review_fix"]) / agg["feat"] if agg["feat"] else 0
    _, level = cfr_level(cfr)
    cfr_class = f"cfr-{level.lower()}"
    cards = [
        ("📤 Pushes", agg["pushes"], ""),
        ("🔀 PR/MR", agg["pr_mr"], ""),
        ("✨ feat", agg["feat"], ""),
        ("🐛 fix", f"{agg['fix']} ({agg['fix']-agg['review_fix']} prod, {agg['review_fix']} review)", ""),
        (f"True CFR [{level}]", f"{cfr:.2f}", cfr_class),
    ]
    out = ['<h3>🚀 DORA</h3>', _hint_html("dora"), '<div class="dora">']
    for label, val, cls in cards:
        out.append(
            f'<div class="dora-card {cls}">'
            f'<div class="label">{h(str(label))}</div>'
            f'<div class="value">{h(str(val))}</div>'
            '</div>'
        )
    out.append("</div>")
    return "\n".join(out)


def _html_friction(agg: dict) -> str:
    return (
        '<h3>⚡ Friction</h3>'
        + _hint_html("friction")
        + '<div class="friction">'
        f'<div>🌀 compacts: <strong>{agg["compacts"]}</strong></div>'
        f'<div>🚧 pivots: <strong>{agg["pivots"]}</strong></div>'
        f'<div>🤖 subagents: <strong>{agg["subs"]}</strong></div>'
        '</div>'
    )


def _html_stack(stack_loc: Counter) -> str:
    total = sum(stack_loc.values())
    if total == 0:
        return ""
    items = []
    for st in _STACKS_ORDER:
        v = stack_loc.get(st, 0)
        if v == 0:
            continue
        items.append((_STACK_EMOJI[st], st, v))
    return _html_bar_chart("🎨 Stack palette (LOC)", items, total, hint_section="stack")


def render_period_html_section(label: str, agg: dict) -> str:
    wall_h = int(agg["wall_min"] / 60)
    act_h = int(agg["active_min"] / 60)
    ratio = act_h * 100 / wall_h if wall_h else 0

    out = [f'<section class="period"><h2>{h(label)}</h2>']
    out.append(
        '<div class="meta">'
        f'<strong>{agg["n"]} sessions</strong> · '
        f'+{agg["lines_added"]:,} LOC · '
        f'wall {wall_h}h · active {act_h}h ({ratio:.0f}%)'
        '</div>'
    )
    out.append(_html_aquarium(agg["biomes"]))

    arch_items = [(ARCHETYPE_EMOJI[a], a, agg["archs"].get(a, 0)) for a in _ARCHS_ORDER]
    out.append(_html_bar_chart("🎭 Archetypes", arch_items, agg["n"], hint_section="archetypes"))

    rhy_items = [(RHYTHM_EMOJI[r], r, agg["rhythms"].get(r, 0)) for r in _RHYTHMS_ORDER]
    out.append(_html_bar_chart("🎵 Rhythms", rhy_items, agg["n"], hint_section="rhythms"))

    out.append(_html_stack(agg["stack_loc"]))
    out.append(_html_dora(agg))
    out.append(_html_friction(agg))
    out.append("</section>")
    return "\n".join(out)


def render_html_doc(title: str, body_sections: list[str]) -> str:
    head = _HTML_HEAD.format(title=h(title))
    header = (
        f'<h1>🌊 {h(title)}</h1>'
        '<div class="subtitle">'
        '<em>aquarium of biomes · archetypes · rhythm · stack · DORA</em>'
        '</div>'
    )
    tail = _HTML_TAIL.format(ts=datetime.now().isoformat(timespec="seconds"))
    return head + header + "\n".join(body_sections) + tail


# ─── Weekly table & bars ─────────────────────────────────────────────────


def render_weekly_table_terminal(weeks: list[tuple[tuple[int, int], dict]]) -> list[str]:
    out = []
    out.append("")
    out.append("═" * 100)
    out.append("  📅 WEEKLY TEMPERATURE")
    out.append("═" * 100)
    out.extend(_hint_terminal("weekly_table"))
    out.append("")
    out.append(f"  {'week':<22s}  {'sess':>4s}  {'+LOC':>7s}  {'feat':>4s}  "
               f"{'fix':>4s}  {'CFR':>4s}  {'biomes':<32s}  {'top arch':<14s}")
    out.append(f"  {'─'*22}  {'─'*4}  {'─'*7}  {'─'*4}  {'─'*4}  {'─'*4}  {'─'*32}  {'─'*14}")
    for (yr, wk), a in weeks:
        d_start = datetime.fromisocalendar(yr, wk, 1)
        d_end = d_start + timedelta(days=6)
        label = f"W{wk:02d} {d_start.strftime('%b %d')}-{d_end.strftime('%b %d')}"
        biomes_str = ""
        for b in _BIOMES_ORDER:
            n = a["biomes"].get(b, 0)
            if n > 0:
                biomes_str += BIOME_EMOJI[b] + f"{n} "
        top_arch = a["archs"].most_common(1)[0] if a["archs"] else ("—", 0)
        cfr = (a["fix"] - a["review_fix"]) / a["feat"] if a["feat"] else 0
        em, _ = cfr_level(cfr)
        out.append(
            f"  {label:<22s}  {a['n']:>4d}  {a['lines_added']:>7,d}  "
            f"{a['feat']:>4d}  {a['fix']:>4d}  {em}{cfr:>3.1f}  "
            f"{biomes_str[:32]:<32s}  "
            f"{ARCHETYPE_EMOJI.get(top_arch[0], '·')+top_arch[0][:11]:<14s}"
        )

    # Bar charts
    out.append("")
    out.append("  📊 +LOC by week:")
    max_loc = max((a["lines_added"] for _, a in weeks), default=1) or 1
    for (yr, wk), a in weeks:
        d_start = datetime.fromisocalendar(yr, wk, 1)
        d_end = d_start + timedelta(days=6)
        label = f"W{wk:02d} {d_start.strftime('%b %d')}-{d_end.strftime('%b %d')}"
        bar_n = int(a["lines_added"] * 40 / max_loc)
        out.append(f"  {label:<22s} │{'█'*bar_n}{'░'*(40-bar_n)}│ {a['lines_added']:>7,d}")
    return out


def render_weekly_table_markdown(weeks: list[tuple[tuple[int, int], dict]]) -> list[str]:
    out = ["", "## 📅 Weeks", ""]
    out.extend(_hint_markdown("weekly_table"))
    out.append("| Week | sess | +LOC | feat | fix | CFR | biomes | top archetype |")
    out.append("|--|--:|--:|--:|--:|--:|--|--|")
    for (yr, wk), a in weeks:
        d_start = datetime.fromisocalendar(yr, wk, 1)
        d_end = d_start + timedelta(days=6)
        label = f"W{wk:02d} {d_start.strftime('%b %d')}-{d_end.strftime('%b %d')}"
        biomes_str = ""
        for b in _BIOMES_ORDER:
            n = a["biomes"].get(b, 0)
            if n > 0:
                biomes_str += BIOME_EMOJI[b] + f"{n} "
        top_arch = a["archs"].most_common(1)[0] if a["archs"] else ("—", 0)
        cfr = (a["fix"] - a["review_fix"]) / a["feat"] if a["feat"] else 0
        em, _ = cfr_level(cfr)
        out.append(
            f"| {label} | {a['n']} | {a['lines_added']:,} | {a['feat']} | {a['fix']} | "
            f"{em}{cfr:.1f} | {biomes_str.strip()} | "
            f"{ARCHETYPE_EMOJI.get(top_arch[0], '·')} {top_arch[0]} |"
        )
    out.append("")
    return out


def render_weekly_table_html(weeks: list[tuple[tuple[int, int], dict]]) -> str:
    if not weeks:
        return ""
    max_loc = max((a["lines_added"] for _, a in weeks), default=1) or 1
    out = ['<section class="period"><h2>📅 Weeks</h2>']
    out.append(_hint_html("weekly_table"))
    out.append('<table class="weeks-table">')
    out.append(
        '<thead><tr><th>Week</th><th class="num">sess</th>'
        '<th class="num">+LOC</th><th class="num">feat</th><th class="num">fix</th>'
        '<th class="num">CFR</th><th>biomes</th><th>top arch</th><th>+LOC bar</th></tr></thead>'
    )
    out.append("<tbody>")
    for (yr, wk), a in weeks:
        d_start = datetime.fromisocalendar(yr, wk, 1)
        d_end = d_start + timedelta(days=6)
        label = f"W{wk:02d} {d_start.strftime('%b %d')}-{d_end.strftime('%b %d')}"
        biomes_str = ""
        for b in _BIOMES_ORDER:
            n = a["biomes"].get(b, 0)
            if n > 0:
                biomes_str += BIOME_EMOJI[b] + f"{n} "
        top_arch = a["archs"].most_common(1)[0] if a["archs"] else ("—", 0)
        cfr = (a["fix"] - a["review_fix"]) / a["feat"] if a["feat"] else 0
        em, _ = cfr_level(cfr)
        bar_w = int(a["lines_added"] * 200 / max_loc)
        out.append(
            f'<tr><td>{h(label)}</td>'
            f'<td class="num">{a["n"]}</td>'
            f'<td class="num">{a["lines_added"]:,}</td>'
            f'<td class="num">{a["feat"]}</td>'
            f'<td class="num">{a["fix"]}</td>'
            f'<td class="num">{em} {cfr:.1f}</td>'
            f'<td>{h(biomes_str.strip())}</td>'
            f'<td>{ARCHETYPE_EMOJI.get(top_arch[0], "·")} {h(top_arch[0])}</td>'
            f'<td><span class="bar-mini" style="width:{bar_w}px"></span></td>'
            "</tr>"
        )
    out.append("</tbody></table></section>")
    return "\n".join(out)


# ─── Trends ──────────────────────────────────────────────────────────────


def trend_arrow(vs: list[float]) -> str:
    if all(vs[i] > vs[i - 1] for i in range(1, len(vs))):
        return "🚀↑↑↑"
    if all(vs[i] < vs[i - 1] for i in range(1, len(vs))):
        return "📉↓↓↓"
    if len(vs) >= 3 and vs[len(vs) // 2] > vs[0] and vs[len(vs) // 2] > vs[-1]:
        return "🌋 peak mid"
    if len(vs) >= 3 and vs[len(vs) // 2] < vs[0] and vs[len(vs) // 2] < vs[-1]:
        return "🪂 dip mid"
    return "~ stable"


_TREND_METRICS = [
    ("Sessions", "n", "{:,.0f}"),
    ("+LOC", "lines_added", "{:,.0f}"),
    ("feat commits", "feat", "{:,.0f}"),
    ("fix commits", "fix", "{:,.0f}"),
    ("pushes", "pushes", "{:,.0f}"),
    ("PR/MR", "pr_mr", "{:,.0f}"),
    ("compacts", "compacts", "{:,.0f}"),
    ("pivots", "pivots", "{:,.0f}"),
    ("subagents", "subs", "{:,.0f}"),
    ("wall hours", "wall_min", "{:.0f}"),
    ("active hours", "active_min", "{:.0f}"),
]


def _trend_value(agg: dict, key: str, name: str) -> float:
    v = agg.get(key, 0)
    if "hour" in name:
        return v / 60
    return v


def render_trends_terminal(months_data: dict[int, dict], months_labels: dict[int, str]) -> list[str]:
    out = ["", "═" * 100, "  📈 TRENDS (by period)", "═" * 100]
    out.extend(_hint_terminal("trends"))
    out.append("")
    keys = sorted(months_data.keys())
    if len(keys) < 2:
        return out
    headers = "  ".join(f"{months_labels[k][:14]:>14s}" for k in keys)
    out.append(f"  {'metric':<24s}     {headers}    trend")
    out.append(f"  {'─'*24}     {'  '.join('─'*14 for _ in keys)}    ─────")
    for name, key, fmt in _TREND_METRICS:
        vs = [_trend_value(months_data[k], key, name) for k in keys]
        cells = "   ".join(f"{fmt.format(v):>14s}" for v in vs)
        out.append(f"  {name:<24s}     {cells}    {trend_arrow(vs)}")
    return out


def render_trends_markdown(months_data: dict[int, dict], months_labels: dict[int, str]) -> list[str]:
    out = ["", "## 📈 Trends", ""]
    out.extend(_hint_markdown("trends"))
    keys = sorted(months_data.keys())
    if len(keys) < 2:
        return out
    header = "| Metric | " + " | ".join(months_labels[k] for k in keys) + " | Trend |"
    sep = "|--|" + "|".join("--:" for _ in keys) + "|--|"
    out.append(header)
    out.append(sep)
    for name, key, fmt in _TREND_METRICS:
        vs = [_trend_value(months_data[k], key, name) for k in keys]
        cells = " | ".join(fmt.format(v) for v in vs)
        out.append(f"| {name} | {cells} | {trend_arrow(vs)} |")
    out.append("")
    return out


def render_trends_html(months_data: dict[int, dict], months_labels: dict[int, str]) -> str:
    keys = sorted(months_data.keys())
    if len(keys) < 2:
        return ""
    out = ['<section class="period"><h2>📈 Trends</h2>']
    out.append(_hint_html("trends"))
    out.append('<table class="trends-table"><thead><tr><th>Metric</th>')
    for k in keys:
        out.append(f'<th class="num">{h(months_labels[k])}</th>')
    out.append("<th>Trend</th></tr></thead><tbody>")
    for name, key, fmt in _TREND_METRICS:
        vs = [_trend_value(months_data[k], key, name) for k in keys]
        cells = "".join(f'<td class="num">{fmt.format(v)}</td>' for v in vs)
        out.append(
            f'<tr><td>{h(name)}</td>{cells}'
            f'<td class="trend-arrow">{trend_arrow(vs)}</td></tr>'
        )
    out.append("</tbody></table></section>")
    return "\n".join(out)
