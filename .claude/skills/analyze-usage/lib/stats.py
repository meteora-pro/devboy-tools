"""Pure statistical helpers used by extractors.

No I/O, no pandas. Plain Python lists / counters. Each function safe for
empty input (returns 0 or empty result, not raises).
"""

from __future__ import annotations

import math
from collections import Counter
from typing import Iterable, Sequence


def cv(xs: Sequence[float]) -> float:
    """Coefficient of variation = std_dev / mean. Returns 0 if mean == 0."""
    if not xs:
        return 0.0
    n = len(xs)
    mean = sum(xs) / n
    if mean == 0:
        return 0.0
    var = sum((x - mean) ** 2 for x in xs) / n
    return math.sqrt(var) / mean


def gini(xs: Sequence[float]) -> float:
    """Gini coefficient on a non-negative numeric sequence.

    0 = perfectly equal, ~1 = max concentration (one value holds everything).
    Treats negatives as 0. Empty input → 0.
    """
    if not xs:
        return 0.0
    vals = sorted(max(x, 0) for x in xs)
    n = len(vals)
    total = sum(vals)
    if total == 0:
        return 0.0
    cum = 0.0
    weighted = 0.0
    for i, v in enumerate(vals, start=1):
        cum += v
        weighted += i * v
    return (2 * weighted) / (n * total) - (n + 1) / n


def shannon_entropy(counter: Counter | dict) -> float:
    """Shannon entropy in bits (log2)."""
    total = sum(counter.values())
    if total == 0:
        return 0.0
    h = 0.0
    for v in counter.values():
        if v <= 0:
            continue
        p = v / total
        h -= p * math.log2(p)
    return h


def milestone_at(cum_R: Sequence[int], threshold: int) -> int | None:
    """Index (1-based burst position) where cumulative R first crosses threshold.

    Returns None if never reached.
    """
    for i, c in enumerate(cum_R, start=1):
        if c >= threshold:
            return i
    return None


def acceleration(daily_xs: Sequence[float]) -> float:
    """Slope of the second-half — slope of the first-half (linear-fit comparison).

    Positive value = output rate increasing over time (accelerating).
    Negative = decelerating. Zero (or None for too-short) otherwise.
    """
    n = len(daily_xs)
    if n < 4:
        return 0.0
    half = n // 2
    first = daily_xs[:half]
    second = daily_xs[half:]

    def _slope(seq: Sequence[float]) -> float:
        m = len(seq)
        if m < 2:
            return 0.0
        mean_x = (m - 1) / 2
        mean_y = sum(seq) / m
        num = sum((i - mean_x) * (seq[i] - mean_y) for i in range(m))
        den = sum((i - mean_x) ** 2 for i in range(m))
        return num / den if den else 0.0

    return _slope(second) - _slope(first)


def pearson(xs: Sequence[float], ys: Sequence[float]) -> float:
    """Pearson correlation coefficient. 0 for empty / degenerate."""
    n = min(len(xs), len(ys))
    if n < 2:
        return 0.0
    mx = sum(xs) / n
    my = sum(ys) / n
    num = sum((xs[i] - mx) * (ys[i] - my) for i in range(n))
    dx = math.sqrt(sum((xs[i] - mx) ** 2 for i in range(n)))
    dy = math.sqrt(sum((ys[i] - my) ** 2 for i in range(n)))
    if dx == 0 or dy == 0:
        return 0.0
    return num / (dx * dy)


def percentile(xs: Sequence[float], p: float) -> float:
    """p-th percentile (p in [0, 100]). Linear interpolation between ranks."""
    if not xs:
        return 0.0
    s = sorted(xs)
    n = len(s)
    if p <= 0:
        return s[0]
    if p >= 100:
        return s[-1]
    rank = (p / 100) * (n - 1)
    lo = int(rank)
    hi = min(lo + 1, n - 1)
    frac = rank - lo
    return s[lo] * (1 - frac) + s[hi] * frac


def median(xs: Sequence[float]) -> float:
    return percentile(xs, 50)


def power_law_fit(xs: Sequence[float], ys: Sequence[float]) -> dict:
    """Log-log linear fit: y ≈ a * x^k.

    Returns {"slope": k, "intercept_log": log(a), "scale": a, "r": pearson_r}.
    Drops pairs where x or y ≤ 0.
    """
    pairs = [(x, y) for x, y in zip(xs, ys) if x > 0 and y > 0]
    if len(pairs) < 3:
        return {"slope": 0.0, "intercept_log": 0.0, "scale": 0.0, "r": 0.0, "n": len(pairs)}
    log_x = [math.log(p[0]) for p in pairs]
    log_y = [math.log(p[1]) for p in pairs]
    n = len(pairs)
    mx = sum(log_x) / n
    my = sum(log_y) / n
    num = sum((log_x[i] - mx) * (log_y[i] - my) for i in range(n))
    den = sum((log_x[i] - mx) ** 2 for i in range(n))
    slope = num / den if den else 0.0
    intercept = my - slope * mx
    return {
        "slope": slope,
        "intercept_log": intercept,
        "scale": math.exp(intercept),
        "r": pearson(log_x, log_y),
        "n": n,
    }
