"""Extractor smoke test on a synthetic fixture.

Builds a tiny ~/.claude/projects/ structure under a temp dir, runs each
extractor pointing at it, and asserts:
  - parquet file exists in raw/ and anon/
  - row count > 0 (when fixture is non-empty)
  - schema has the expected columns
  - sample values make sense
"""
from __future__ import annotations

import json
import shutil
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pyarrow.parquet as pq

_HERE = Path(__file__).resolve().parent
_SKILL = _HERE.parent
sys.path.insert(0, str(_SKILL))


def _ev(typ: str, ts: datetime, **extra) -> dict:
    rec = {"type": typ, "timestamp": ts.isoformat()}
    rec.update(extra)
    return rec


def _user_text(ts: datetime, text: str) -> dict:
    return _ev(
        "user", ts,
        message={"content": [{"type": "text", "text": text}]},
    )


def _assistant(ts: datetime, *, text: str = "", tool_uses: list[dict] | None = None,
               in_tokens: int = 0, cache_read: int = 0) -> dict:
    content = []
    if text:
        content.append({"type": "text", "text": text})
    for tu in (tool_uses or []):
        content.append({"type": "tool_use", "id": f"toolu_{tu.get('name')}_{ts.microsecond}",
                        "name": tu["name"], "input": tu.get("input", {})})
    return {
        "type": "assistant",
        "timestamp": ts.isoformat(),
        "message": {
            "content": content,
            "usage": {"input_tokens": in_tokens, "cache_read_input_tokens": cache_read,
                      "output_tokens": 50},
        },
    }


def build_fixture_root() -> Path:
    """Create a synthetic projects dir with 3 controlled sessions."""
    tmpdir = Path(tempfile.mkdtemp(prefix="analyze_usage_test_"))
    base = tmpdir / "projects"
    proj = base / "-tmp-fixture-project"
    proj.mkdir(parents=True)

    t0 = datetime(2026, 4, 1, 10, 0, tzinfo=timezone.utc)

    # SESSION 1: Operator-Mono whale-ish (lots of bash, R=15 → Fish)
    s1 = proj / "11111111-1111-1111-1111-111111111111.jsonl"
    with s1.open("w") as f:
        for i in range(15):
            ts = t0 + timedelta(minutes=i * 5)
            f.write(json.dumps(_user_text(ts, f"do task {i}")) + "\n")
            for k in range(8):
                ts2 = ts + timedelta(seconds=k * 10)
                f.write(json.dumps(_assistant(
                    ts2,
                    tool_uses=[{"name": "Bash", "input": {"command": "git status"}}],
                )) + "\n")

    # SESSION 2: Constructor (Edit-heavy), commits with feat + fix
    s2 = proj / "22222222-2222-2222-2222-222222222222.jsonl"
    with s2.open("w") as f:
        ts = t0 + timedelta(hours=1)
        f.write(json.dumps(_user_text(ts, "build me a feature")) + "\n")
        for k in range(10):
            f.write(json.dumps(_assistant(
                ts + timedelta(seconds=k),
                tool_uses=[{"name": "Write",
                            "input": {"file_path": "/tmp/fix/api/svc.rs",
                                      "content": "pub fn x() {\n    1\n}\n"}}],
            )) + "\n")
        # commit feat
        f.write(json.dumps(_assistant(
            ts + timedelta(seconds=20),
            tool_uses=[{"name": "Bash", "input": {"command": "git commit -m 'feat: add svc'"}}],
        )) + "\n")
        # commit fix prod
        f.write(json.dumps(_assistant(
            ts + timedelta(seconds=30),
            tool_uses=[{"name": "Bash", "input": {"command": "git commit -m 'fix: null pointer'"}}],
        )) + "\n")
        # commit fix review
        f.write(json.dumps(_assistant(
            ts + timedelta(seconds=40),
            tool_uses=[{"name": "Bash",
                        "input": {"command": "git commit -m 'fix: address review comments'"}}],
        )) + "\n")
        # push
        f.write(json.dumps(_assistant(
            ts + timedelta(seconds=50),
            tool_uses=[{"name": "Bash", "input": {"command": "git push origin feat/x"}}],
        )) + "\n")

    # SESSION 3: ghost (R=0)
    s3 = proj / "33333333-3333-3333-3333-333333333333.jsonl"
    with s3.open("w") as f:
        f.write(json.dumps({"type": "file-history-snapshot", "timestamp": t0.isoformat()}) + "\n")

    return tmpdir


def run_extractors(fixture_root: Path, outputs_dir: Path) -> None:
    """Set CLAUDE_PROJECTS_ROOT env var so extractors find the fixture."""
    import os
    import importlib
    os.environ["CLAUDE_PROJECTS_ROOT"] = str(fixture_root / "projects")
    sys.path.insert(0, str(_SKILL / "scripts"))
    for name in [
        "extract_biome", "extract_archetype", "extract_outputs",
        "extract_compacts", "extract_growth", "extract_subagents",
        "extract_gaps", "extract_burst_classes", "extract_parallelism",
        "extract_topics",
    ]:
        if name in sys.modules:
            del sys.modules[name]
        mod = importlib.import_module(name)
        rc = mod.main(["--outputs", str(outputs_dir)])
        if rc != 0:
            raise AssertionError(f"{name} returned {rc}")


def assert_parquet(path: Path, expected_cols: set[str], min_rows: int = 1) -> None:
    if not path.exists():
        raise AssertionError(f"{path} not created")
    table = pq.read_table(path)
    rows = table.num_rows
    if rows < min_rows:
        raise AssertionError(f"{path} has {rows} rows, expected ≥{min_rows}")
    cols = set(table.column_names)
    missing = expected_cols - cols
    if missing:
        raise AssertionError(f"{path} missing columns: {missing}")


def test_full_pipeline_smoke():
    fixture = build_fixture_root()
    try:
        out = fixture / "outputs"
        run_extractors(fixture, out)
        # All raw parquets exist with sensible schema
        assert_parquet(out / "raw" / "biome.parquet", {"sid", "biome", "real_prompts"})
        assert_parquet(out / "raw" / "archetype.parquet", {"sid", "archetype", "rhythm"})
        assert_parquet(out / "raw" / "outputs.parquet",
                       {"sid", "feat", "fix", "review_fix", "prod_fix", "lines_added"})
        assert_parquet(out / "raw" / "compact_pattern.parquet", {"sid", "compacts"})
        assert_parquet(out / "raw" / "growth.parquet", {"sid", "real_prompts", "n_bursts"})
        # idle_gaps.parquet may be empty (no gaps in 1-hour fixture)
        assert (out / "raw" / "idle_gaps.parquet").exists()
        assert (out / "raw" / "burst_classes.parquet").exists()
        assert (out / "raw" / "parallelism.parquet").exists()
        assert (out / "raw" / "topic_signatures.parquet").exists()
        assert (out / "raw" / "subagent_pyramid.parquet").exists()

        # Anon mirrors
        for f in (out / "raw").glob("*.parquet"):
            anon = out / "anon" / f.name
            assert anon.exists(), f"anon mirror missing for {f.name}"

        # Specific value checks
        biomes = pq.read_table(out / "raw" / "biome.parquet").to_pylist()
        # session1 = R=15 → Fish; session2 R=1 → Plankton; session3 R=0 → Plankton (ghost)
        sids_to_biome = {r["sid"][:8]: r["biome"] for r in biomes}
        assert sids_to_biome.get("11111111") == "Fish", sids_to_biome
        # session2 has only 1 prompt → Plankton
        assert sids_to_biome.get("22222222") == "Plankton", sids_to_biome

        outputs = pq.read_table(out / "raw" / "outputs.parquet").to_pylist()
        s2 = next(r for r in outputs if r["sid"].startswith("22222222"))
        assert s2["feat"] == 1, s2
        assert s2["fix"] == 2, s2
        assert s2["review_fix"] == 1, s2
        assert s2["prod_fix"] == 1, s2
        assert s2["pushes"] == 1, s2

        archs = pq.read_table(out / "raw" / "archetype.parquet").to_pylist()
        s1_arch = next(r for r in archs if r["sid"].startswith("11111111"))
        assert s1_arch["archetype"] == "Operator", s1_arch
        s2_arch = next(r for r in archs if r["sid"].startswith("22222222"))
        # session2 has 10 Write + 4 Bash → Edit:Bash 71%:29% → Constructor (≥45% Edit)
        assert s2_arch["archetype"] == "Constructor", s2_arch
    finally:
        shutil.rmtree(fixture, ignore_errors=True)


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
        except Exception as e:
            failed += 1
            import traceback
            traceback.print_exc()
            print(f"  ✗ {fn.__name__}: {e}")
    if failed:
        raise SystemExit(f"{failed}/{len(fns)} failed")
    print(f"\n  {len(fns)}/{len(fns)} passed")
