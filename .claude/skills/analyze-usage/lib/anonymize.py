"""Anonymization helpers for outputs/anon/ bundle.

Anonymity contract (mirrors SKILL.md):

- session_id  → sequential s0001 (main) / a0001 (subagent)
- project_path → p<6-hex> SHA-1 truncation
- file_path    → drop, keep file_ext + stack_class only
- bash_command → drop, keep category + structural flags only
- prompt_text  → drop, keep chars + intent flags + token_hash BoW
- branch_name  → drop, keep 6-hex branch_hash only
- tool_use_id  → drop
- mcp slug     → 6-hex hash, verb intact
- aggregate with N<5 distinct sessions → suppress (K=5)

The anonymizer keeps a sidecar dictionary (raw_dir/_token_dict.json) so the
owner can look up hashes back to original strings.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path

K_THRESHOLD = 5


def hash_path(p: str, length: int = 6) -> str:
    """SHA-1 truncation of a path or string. Stable across runs."""
    if not p:
        return ""
    h = hashlib.sha1(p.encode("utf-8")).hexdigest()
    return "p" + h[:length]


def hash_token(word: str, length: int = 6) -> str:
    """Deterministic 6-hex hash of a token (for prompt BoW)."""
    if not word:
        return ""
    h = hashlib.sha1(word.lower().encode("utf-8")).hexdigest()
    return h[:length]


def hash_branch(branch: str, length: int = 6) -> str:
    if not branch:
        return ""
    return "b" + hashlib.sha1(branch.encode("utf-8")).hexdigest()[:length]


def hash_mcp_slug(name: str) -> str:
    """`mcp__<server>__<verb>` → `mcp__<6-hex>__<verb>` (server hashed, verb kept)."""
    if not name.startswith("mcp__"):
        return name
    parts = name.split("__")
    if len(parts) < 3:
        return name
    server = parts[1]
    verb = "__".join(parts[2:])
    return f"mcp__{hash_token(server)}__{verb}"


class SidProjector:
    """Deterministic mapping `full_uuid → s0001 / a0001` for anon output."""

    def __init__(self, prefix: str = "s"):
        self.prefix = prefix
        self._map: dict[str, str] = {}

    def get(self, full_id: str) -> str:
        if full_id not in self._map:
            self._map[full_id] = f"{self.prefix}{len(self._map) + 1:04d}"
        return self._map[full_id]

    def dump(self, path: Path) -> None:
        path.write_text(json.dumps(self._map, indent=2))


def file_ext(path: str) -> str:
    """Extension only, lowercase, leading dot stripped. '' for no extension."""
    if not path:
        return ""
    base = os.path.basename(path)
    if "." not in base:
        return ""
    return base.rsplit(".", 1)[1].lower()


def k_anon_filter(rows: list[dict], group_key: str | tuple[str, ...]) -> list[dict]:
    """Drop rows whose grouping key occurs <K_THRESHOLD times.

    group_key may be a single column name or a tuple of column names.
    """
    keys = (group_key,) if isinstance(group_key, str) else tuple(group_key)
    counts: dict[tuple, int] = {}
    for r in rows:
        k = tuple(r.get(c) for c in keys)
        counts[k] = counts.get(k, 0) + 1
    return [r for r in rows if counts[tuple(r.get(c) for c in keys)] >= K_THRESHOLD]


def write_token_sidecar(token_to_hash: dict, raw_dir: Path) -> None:
    """Persist the prompt-token reverse-lookup (raw → hash) dictionary.

    Lives in `outputs/raw/` only, never copied to anon.
    """
    raw_dir.mkdir(parents=True, exist_ok=True)
    (raw_dir / "_token_dict.json").write_text(json.dumps(token_to_hash, indent=2, ensure_ascii=False))
