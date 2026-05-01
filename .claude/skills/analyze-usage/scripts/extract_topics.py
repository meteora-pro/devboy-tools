#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow>=15"]
# ///
"""Per-session topic signature: BoW from real-human prompts.

raw/topic_signatures.parquet keeps original tokens + counts (sidecar dict
not separate; tokens themselves are the keys).
anon/topic_signatures.parquet replaces tokens with 6-hex hashes.
"""
from __future__ import annotations

import argparse
import re
import sys
from collections import Counter
from datetime import datetime
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE.parent))

from lib.parsers import find_session_files, load_session  # noqa: E402
from lib.anonymize import hash_path, hash_token, SidProjector, write_token_sidecar  # noqa: E402
from lib.io import outputs_dirs, write_parquet  # noqa: E402

WORD_RX = re.compile(r"[a-zа-я0-9_\-]{3,}", re.I)
STOPWORDS = {
    "the","and","that","with","this","from","have","not","you","for","but",
    "и","в","на","с","что","это","как","но","для","или","же","ли",
    "the","a","an","of","to","is","are","be","was","were","by","at","or",
}

TOP_K = 20


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--since", default=None)
    p.add_argument("--outputs", default=None)
    args = p.parse_args(argv)
    since = datetime.fromisoformat(args.since) if args.since else None
    raw_dir, anon_dir = outputs_dirs(Path(args.outputs) if args.outputs else None)
    sid_main = SidProjector("s")

    raw_rows = []
    anon_rows = []
    token_to_hash: dict[str, str] = {}

    for jf in find_session_files():
        try:
            sess = load_session(jf, include_subagents=False, since=since)
        except Exception:
            continue
        bow: Counter[str] = Counter()
        for ev in sess.events:
            if ev.type == "user" and ev.user_kind == "real_human":
                for w in WORD_RX.findall(ev.user_text.lower()):
                    if w in STOPWORDS:
                        continue
                    bow[w] += 1
        if not bow:
            continue
        top = bow.most_common(TOP_K)
        raw_rows.append({
            "sid": sess.sid, "project": sess.project,
            "tokens": [t for t, _ in top],
            "counts": [c for _, c in top],
        })
        # anon: hash tokens
        anon_tokens = []
        for t, _ in top:
            if t not in token_to_hash:
                token_to_hash[t] = hash_token(t)
            anon_tokens.append(token_to_hash[t])
        anon_rows.append({
            "sid": sid_main.get(sess.sid),
            "project": hash_path(sess.project),
            "token_hashes": anon_tokens,
            "counts": [c for _, c in top],
        })

    n_raw = write_parquet(raw_rows, raw_dir / "topic_signatures.parquet")
    n_anon = write_parquet(anon_rows, anon_dir / "topic_signatures.parquet")
    write_token_sidecar(token_to_hash, raw_dir)
    print(f"topic_signatures: raw {n_raw}, anon {n_anon}, sidecar {len(token_to_hash)} tokens", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
