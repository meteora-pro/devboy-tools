"""Classifiers for sessions: biome, archetype, rhythm, stack, bash sub.

Each function takes minimal aggregated data and returns a label. Pure
functions, no I/O.
"""

from __future__ import annotations

import os
import re
from collections import Counter


# ─── Biome (size class) ───────────────────────────────────────────────────


BIOME_THRESHOLDS = [
    (500, "Whale"),
    (100, "Shark"),
    (30,  "Dolphin"),
    (10,  "Fish"),
    (3,   "Shrimp"),
]

BIOME_EMOJI = {
    "Whale":    "🐋",
    "Shark":    "🦈",
    "Dolphin":  "🐬",
    "Fish":     "🐟",
    "Shrimp":   "🦐",
    "Plankton": "🦠",
}


def biome_of(real_prompts: int) -> str:
    """Map real-human prompt count to biome label."""
    for thr, name in BIOME_THRESHOLDS:
        if real_prompts >= thr:
            return name
    return "Plankton"


# ─── Archetype (Variant B: pure ≥45%, hybrid each ≥25%) ───────────────────


ARCHETYPE_EMOJI = {
    "Constructor": "🏗",
    "Operator":    "⚙️",
    "Researcher":  "🔬",
    "Builder":     "🛠",
    "Scholar":     "📝",
    "Inspector":   "🔍",
    "Polymath":    "🌐",
    "Discusser":   "💬",
}


def archetype_of(edit: int, bash: int, read: int) -> str:
    """Variant B archetype classifier.

    Pure single-axis ≥45% → Constructor / Operator / Researcher.
    Hybrid two-axis ≥25% each → Builder (E+B) / Scholar (E+R) / Inspector (B+R).
    All three ≥25% → Polymath.
    Else → Discusser.
    """
    total = edit + bash + read
    if total < 5:
        return "Discusser"
    e, b, r = edit / total, bash / total, read / total
    if e >= 0.45:
        return "Constructor"
    if b >= 0.45:
        return "Operator"
    if r >= 0.45:
        return "Researcher"
    if e >= 0.25 and b >= 0.25 and r >= 0.25:
        return "Polymath"
    if e >= 0.25 and b >= 0.25:
        return "Builder"
    if e >= 0.25 and r >= 0.25:
        return "Scholar"
    if b >= 0.25 and r >= 0.25:
        return "Inspector"
    return "Discusser"


# ─── Rhythm (10-window dominant tool) ─────────────────────────────────────


RHYTHM_EMOJI = {
    "Mono":   "🎼",
    "Phased": "📊",
    "Mixed":  "🎲",
    "Chaos":  "🌪",
    "—":      "·",
}


def rhythm_of(event_seq: list[str]) -> str:
    """Classify rhythm based on dominant-tool sequence over 10 windows.

    Mono   = top tool ≥80% of windows.
    Chaos  = ≥6 transitions and ≥4 unique tops.
    Phased = ≤4 transitions and ≤3 unique tops.
    Mixed  = everything else.
    "—" if too few events (<10).
    """
    if not event_seq or len(event_seq) < 10:
        return "—"
    n = len(event_seq)
    bsize = max(1, n // 10)
    top_seq: list[str] = []
    for i in range(10):
        end = (i + 1) * bsize if i < 9 else n
        win = event_seq[i * bsize:end]
        if not win:
            continue
        top_seq.append(Counter(win).most_common(1)[0][0])
    if not top_seq:
        return "—"
    mode_share = Counter(top_seq).most_common(1)[0][1] / len(top_seq)
    n_unique = len(set(top_seq))
    switches = sum(1 for i in range(1, len(top_seq)) if top_seq[i] != top_seq[i - 1])
    if mode_share >= 0.80:
        return "Mono"
    if switches >= 6 and n_unique >= 4:
        return "Chaos"
    if switches <= 4 and n_unique <= 3:
        return "Phased"
    return "Mixed"


# ─── Stack (file path → frontend/backend/infra/docs/config/other) ─────────


_INFRA_K8S = re.compile(
    r"(values\.yaml|chart\.yaml|deployment\.yaml|service\.yaml|ingress\.yaml|"
    r"configmap\.yaml|hpa\.yaml|statefulset\.yaml|/charts/|/templates/|"
    r"/helm/|/k8s/|/kubernetes/|deckhouse|werf\.yaml)",
    re.I,
)
_INFRA_CI = re.compile(r"(\.github/workflows/|\.gitlab-ci|/ci/|/pipelines/|jenkinsfile)", re.I)
_FRONT_DIRS = re.compile(
    r"(/(frontend|ui|web|client|dashboard|landing|admin|wwwroot|public|static|"
    r"pages|components|views)(/|$))",
    re.I,
)
_FRONT_DASH = re.compile(
    r"(-ui(/|$|-)|-web(/|$|-)|-frontend(/|$|-)|-dashboard(/|$|-)|-landing(/|$|-)"
    r"|-admin(/|$|-)|-app(/|$|-)|-client(/|$|-))",
    re.I,
)
_BACK_DIRS = re.compile(
    r"(/(api|backend|server|worker|services|service|cron|jobs|consumer|producer|graphql)(/|$))",
    re.I,
)
_BACK_DASH = re.compile(
    r"(-api(/|$|-)|-service(/|$|-)|-worker(/|$|-)|-server(/|$|-)|-bot(/|$|-)|"
    r"-consumer(/|$|-)|-cron(/|$|-)|-mcp(/|$|-))",
    re.I,
)


def stack_of(file_path: str) -> str:
    """Classify a file path into frontend / backend / infra / docs / config / other."""
    if not file_path:
        return "other"
    p = file_path.lower()
    fname = os.path.basename(p)
    ext = "." + fname.rsplit(".", 1)[1] if "." in fname else ""

    # Skip lockfiles and binaries
    if ext in (".lock", ".lockb"):
        return "other"
    if fname.endswith((".png", ".jpg", ".gif", ".ico", ".svg", ".bin", ".so", ".dll",
                       ".exe", ".zip", ".tar", ".gz")):
        return "other"

    # Infra
    if ext in (".tf", ".tfvars", ".hcl"):
        return "infra"
    if "dockerfile" in fname or "compose.yml" in fname or "compose.yaml" in fname:
        return "infra"
    if _INFRA_CI.search(p):
        return "infra"
    if ext in (".yaml", ".yml"):
        if _INFRA_K8S.search(p):
            return "infra"
        return "config"
    if ext in (".sh", ".bash", ".zsh", ".fish"):
        return "infra"

    # Docs
    if ext in (".md", ".mdx", ".rst", ".adoc"):
        return "docs"
    if ext == ".txt":
        return "docs"

    # Frontend explicit
    if ext in (".tsx", ".jsx"):
        return "frontend"
    if ext in (".vue", ".svelte"):
        return "frontend"
    if ext in (".css", ".scss", ".sass", ".less", ".styl", ".pcss"):
        return "frontend"
    if ext == ".html":
        return "frontend"

    # Path-based
    is_front = bool(_FRONT_DIRS.search(p) or _FRONT_DASH.search(p))
    is_back = bool(_BACK_DIRS.search(p) or _BACK_DASH.search(p))

    if ext in (".ts", ".js", ".mjs", ".cjs"):
        if is_front and not is_back:
            return "frontend"
        return "backend"
    if ext in (".py", ".rs", ".go", ".sql", ".proto", ".graphql"):
        return "backend"
    if ext in (".java", ".kt", ".kts", ".rb", ".php", ".cs"):
        return "backend"
    if ext == ".swift":
        return "frontend"

    if ext == ".toml":
        return "config"
    if ext in (".ini",):
        return "config"
    if ext == ".json":
        return "config"

    return "other"


# ─── Bash subcategory ─────────────────────────────────────────────────────


_BASH_RX = [
    ("git",     re.compile(r"\b(git\s+|gh\s+|glab\s+)", re.I)),
    ("test",    re.compile(r"\b(cargo\s+test|cargo\s+nextest|pytest|jest|vitest|"
                           r"nx\s+\S+\s+test|playwright(?!-cli)|npm\s+test|pnpm\s+test)\b", re.I)),
    ("build",   re.compile(r"\b(cargo\s+(?:build|check)|tsc|vite\s+build|webpack|"
                           r"nx\s+\S+\s+build|cargo\s+ndk)\b", re.I)),
    ("run",     re.compile(r"\b(cargo\s+run|npm\s+(?:start|run)|pnpm\s+(?:start|run|dev)|"
                           r"node\s+|deno\s+(?:run|task)|python[23]?\s+|uv\s+run|bun\s+run)\b", re.I)),
    ("device",  re.compile(r"\b(adb\s+|simctl\s+|fastboot|ios-deploy)\b", re.I)),
    ("deploy",  re.compile(r"\b(kubectl|helm\s+|terraform\s+|werf\s+|"
                           r"docker\s+(?:compose|run|build)|docker-compose|skopeo|podman)\b", re.I)),
    ("format",  re.compile(r"\b(prettier|black\s+|rustfmt|cargo\s+fmt|eslint|ruff|clippy)\b", re.I)),
    ("net",     re.compile(r"\b(curl\s+|wget\s+|ping\s+|nc\s+)", re.I)),
    ("playwright_cli", re.compile(r"playwright-cli", re.I)),
    ("shell",   re.compile(r"^\s*(?:cd|ls|cat|head|tail|mv|cp|mkdir|find|grep|sed|"
                           r"awk|rg|tree|chmod|touch|rm|pwd|tar|wc|sort|uniq|tee|jq|yq|echo)\b")),
]


def bash_sub_of(cmd: str) -> str:
    """Classify a bash command into git / test / build / run / device / deploy /
    format / net / playwright_cli / shell / other."""
    if not cmd:
        return "other"
    c = cmd.strip()
    for label, rx in _BASH_RX:
        if rx.search(c):
            return label
    return "other"


# ─── Conventional commits ─────────────────────────────────────────────────


COMMIT_HEAD_RX = re.compile(
    r"\b(feat|fix|refactor|chore|docs|test|ci|perf|style|build|revert)"
    r"(\([\w\-,/]+\))?(!)?\s*:\s*(.*)",
    re.I | re.DOTALL,
)
COMMIT_REVIEW_RX = re.compile(
    r"(review|review[- ]?comment|комментар|обратн[ыая]|copilot|artslob|cr[\s-]|"
    r"nitpick|address|reviewer|по\s+ревью|fix\s+review|after\s+review)",
    re.I,
)


def commit_type_of(message_first_line: str) -> str:
    """Extract conventional commit type, or 'uncategorized'."""
    m = COMMIT_HEAD_RX.match(message_first_line)
    return m.group(1).lower() if m else "uncategorized"


def is_review_fix(message_body: str) -> bool:
    """Heuristic: does this fix-commit reference review feedback?"""
    return bool(COMMIT_REVIEW_RX.search(message_body))
