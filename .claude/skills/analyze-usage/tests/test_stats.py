"""Unit tests for lib/stats.py."""
from __future__ import annotations

import math
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.stats import (
    cv, gini, shannon_entropy, milestone_at, pearson, percentile, median,
    power_law_fit, acceleration,
)


def test_cv_empty():
    assert cv([]) == 0
    assert cv([0, 0, 0]) == 0


def test_cv_known():
    # mean=10, std=0 → cv=0
    assert cv([10, 10, 10]) == 0
    # cv known: std/mean
    xs = [1.0, 2.0, 3.0]
    mean = 2.0
    var = ((1-2)**2 + (2-2)**2 + (3-2)**2) / 3
    expected = math.sqrt(var) / mean
    assert abs(cv(xs) - expected) < 1e-6


def test_gini_extremes():
    # Equal → 0
    assert gini([1, 1, 1, 1]) < 0.01
    # All on one → near 1
    assert gini([0, 0, 0, 100]) > 0.7


def test_shannon_entropy():
    assert shannon_entropy({}) == 0
    # Equal 2-class: log2(2) = 1
    assert abs(shannon_entropy({"a": 5, "b": 5}) - 1.0) < 1e-6
    # Single class: 0
    assert shannon_entropy({"a": 10}) == 0


def test_milestone_at():
    cum = [1, 5, 10, 15, 30]
    assert milestone_at(cum, 10) == 3
    assert milestone_at(cum, 30) == 5
    assert milestone_at(cum, 100) is None
    assert milestone_at([], 10) is None


def test_pearson_perfect():
    assert abs(pearson([1, 2, 3], [2, 4, 6]) - 1.0) < 1e-6
    assert abs(pearson([1, 2, 3], [3, 2, 1]) + 1.0) < 1e-6


def test_pearson_zero():
    assert pearson([], []) == 0
    assert pearson([1], [1]) == 0
    assert pearson([1, 1, 1], [1, 2, 3]) == 0  # zero std


def test_percentile():
    xs = [1, 2, 3, 4, 5]
    assert median(xs) == 3
    assert percentile(xs, 0) == 1
    assert percentile(xs, 100) == 5
    assert abs(percentile(xs, 50) - 3) < 1e-6
    assert percentile([], 50) == 0


def test_power_law_fit_known():
    # y = 2 * x^2 → log y = log2 + 2 log x → slope=2
    xs = [1, 2, 3, 4, 5, 10, 20]
    ys = [2 * x**2 for x in xs]
    fit = power_law_fit(xs, ys)
    assert abs(fit["slope"] - 2.0) < 0.01
    assert fit["r"] > 0.99
    assert fit["n"] == len(xs)


def test_acceleration():
    # Linear const slope → acceleration ~0
    xs = [1, 2, 3, 4, 5, 6]
    assert abs(acceleration(xs)) < 1e-6
    # Accelerating (steeper second half): positive
    xs2 = [1, 2, 3, 5, 8, 13]
    assert acceleration(xs2) > 0
    # Decelerating
    xs3 = [1, 5, 10, 11, 11.5, 12]
    assert acceleration(xs3) < 0


if __name__ == "__main__":
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
