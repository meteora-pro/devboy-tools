"""Unit tests for lib/classifiers.py — pure functions, no I/O."""
from __future__ import annotations

import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.classifiers import (
    biome_of, archetype_of, rhythm_of, stack_of, bash_sub_of,
    commit_type_of, is_review_fix,
)


def test_biome_thresholds():
    assert biome_of(0) == "Plankton"
    assert biome_of(2) == "Plankton"
    assert biome_of(3) == "Shrimp"
    assert biome_of(9) == "Shrimp"
    assert biome_of(10) == "Fish"
    assert biome_of(29) == "Fish"
    assert biome_of(30) == "Dolphin"
    assert biome_of(99) == "Dolphin"
    assert biome_of(100) == "Shark"
    assert biome_of(499) == "Shark"
    assert biome_of(500) == "Whale"
    assert biome_of(10_000) == "Whale"


def test_archetype_too_few():
    assert archetype_of(0, 0, 0) == "Discusser"
    assert archetype_of(2, 1, 1) == "Discusser"  # total<5


def test_archetype_pure():
    # Edit ≥45%
    assert archetype_of(50, 25, 25) == "Constructor"
    # Bash ≥45%
    assert archetype_of(25, 50, 25) == "Operator"
    # Read ≥45%
    assert archetype_of(25, 25, 50) == "Researcher"


def test_archetype_hybrid():
    # All three ≥25 → Polymath
    assert archetype_of(30, 30, 40) == "Polymath"  # 30/30/40
    # Edit+Bash ≥25, Read <25 → Builder
    assert archetype_of(40, 40, 20) == "Builder"
    # Edit+Read ≥25 → Scholar
    assert archetype_of(40, 20, 40) == "Scholar"
    # Bash+Read ≥25 → Inspector
    assert archetype_of(20, 40, 40) == "Inspector"


def test_rhythm_too_short():
    assert rhythm_of([]) == "—"
    assert rhythm_of(["Edit"] * 5) == "—"


def test_rhythm_mono():
    # All Edit → Mono
    assert rhythm_of(["Edit"] * 100) == "Mono"


def test_rhythm_chaos():
    # Highly varied seq where each of the 10 windows gets a different dominant tool.
    # 10 windows × 4 events = 40 events, each window uniformly different top.
    tools_per_window = ["Edit", "Bash:test", "Read", "Bash:git",
                        "Edit", "Read", "Bash:build", "Bash:deploy",
                        "Edit", "Read"]
    seq = []
    for t in tools_per_window:
        seq.extend([t] * 4)  # window dominated by t
    cls = rhythm_of(seq)
    # Expected: 10 unique tops, 9 switches → fits Chaos
    assert cls == "Chaos", cls


def test_stack_basic():
    assert stack_of("apps/api/src/main.rs") == "backend"
    assert stack_of("apps/dashboard-ui/src/App.tsx") == "frontend"
    assert stack_of("infra/k8s/deployment.yaml") == "infra"
    assert stack_of("docs/README.md") == "docs"
    assert stack_of("package.json") == "config"
    assert stack_of("") == "other"
    assert stack_of("infra/main.tf") == "infra"
    assert stack_of("apps/web/styles.css") == "frontend"


def test_bash_sub():
    assert bash_sub_of("git status") == "git"
    assert bash_sub_of("cargo test --all") == "test"
    assert bash_sub_of("cargo build") == "build"
    assert bash_sub_of("kubectl get pods") == "deploy"
    assert bash_sub_of("ls -la") == "shell"
    assert bash_sub_of("curl https://example.com") == "net"
    assert bash_sub_of("playwright-cli snapshot") == "playwright_cli"
    assert bash_sub_of("") == "other"
    assert bash_sub_of("python3 setup.py") == "run"


def test_commit_type():
    assert commit_type_of("feat: add login") == "feat"
    assert commit_type_of("feat(api): something") == "feat"
    assert commit_type_of("fix: bug") == "fix"
    assert commit_type_of("not a conventional commit") == "uncategorized"
    assert commit_type_of("") == "uncategorized"


def test_review_fix_detection():
    assert is_review_fix("fix: address review comments")
    assert is_review_fix("fix: copilot suggestion")
    assert is_review_fix("fix: artslob review feedback")
    assert not is_review_fix("fix: null pointer in user service")


if __name__ == "__main__":
    # Lightweight inline runner
    fns = [v for k, v in dict(globals()).items() if k.startswith("test_") and callable(v)]
    failed = 0
    for fn in fns:
        try:
            fn()
            print(f"  ✓ {fn.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"  ✗ {fn.__name__}: {e}")
    if failed:
        raise SystemExit(f"{failed}/{len(fns)} failed")
    print(f"\n  {len(fns)}/{len(fns)} passed")
