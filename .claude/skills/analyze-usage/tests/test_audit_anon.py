"""Tests for pattern_detector.py --audit-anon.

(1) Audit on a clean fixture passes.
(2) Audit on a poisoned fixture fails (returns 1) for each leak kind.
"""
from __future__ import annotations

import shutil
import sys
import tempfile
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

_HERE = Path(__file__).resolve().parent
_SKILL = _HERE.parent
sys.path.insert(0, str(_SKILL))
sys.path.insert(0, str(_SKILL / "scripts"))


def _fresh_outputs_dir() -> Path:
    return Path(tempfile.mkdtemp(prefix="audit_anon_test_"))


def _write_anon(anon_dir: Path, name: str, rows: list[dict]) -> None:
    anon_dir.mkdir(parents=True, exist_ok=True)
    pq.write_table(pa.Table.from_pylist(rows), str(anon_dir / name))


def _run_audit(outputs_dir: Path) -> int:
    """Reload the module and run main with --audit-anon."""
    if "pattern_detector" in sys.modules:
        del sys.modules["pattern_detector"]
    import pattern_detector  # noqa: WPS433
    return pattern_detector.main(["--outputs", str(outputs_dir), "--audit-anon"])


def test_clean_anon_passes():
    out = _fresh_outputs_dir()
    try:
        anon = out / "anon"
        # Minimal valid anon biome.parquet
        _write_anon(anon, "biome.parquet", [
            {"sid": "s0001", "project": "pa1b2c3", "biome": "Fish", "real_prompts": 12},
            {"sid": "a0001", "project": "pa1b2c3", "biome": "Shrimp", "real_prompts": 5},
        ])
        # Some other parquet with safe enum strings
        _write_anon(anon, "archetype.parquet", [
            {"sid": "s0001", "archetype": "Operator", "rhythm": "Mono"},
        ])
        rc = _run_audit(out)
        assert rc == 0, "clean audit should return 0"
    finally:
        shutil.rmtree(out, ignore_errors=True)


def test_uuid_leak_fails():
    out = _fresh_outputs_dir()
    try:
        anon = out / "anon"
        _write_anon(anon, "biome.parquet", [
            # Real UUID accidentally left in `sid`
            {"sid": "2c052d83-b765-440b-923d-72b7be0f7b30", "project": "pa1b2c3",
             "biome": "Whale", "real_prompts": 700},
        ])
        rc = _run_audit(out)
        assert rc == 1, "UUID leak must fail audit"
    finally:
        shutil.rmtree(out, ignore_errors=True)


def test_abs_path_leak_fails():
    out = _fresh_outputs_dir()
    try:
        anon = out / "anon"
        _write_anon(anon, "biome.parquet", [{"sid": "s0001", "project": "pa1b2c3", "biome": "Fish"}])
        _write_anon(anon, "outputs.parquet", [
            {"sid": "s0001", "project": "pa1b2c3",
             "leaked_path": "/Users/andreymaznyak/projects/foo/bar.rs"},
        ])
        rc = _run_audit(out)
        assert rc == 1, "absolute path leak must fail audit"
    finally:
        shutil.rmtree(out, ignore_errors=True)


def test_raw_bash_leak_fails():
    out = _fresh_outputs_dir()
    try:
        anon = out / "anon"
        _write_anon(anon, "biome.parquet", [{"sid": "s0001", "project": "pa1b2c3", "biome": "Fish"}])
        _write_anon(anon, "outputs.parquet", [
            {"sid": "s0001", "leaked_cmd": "git push origin feat/foo"},
        ])
        rc = _run_audit(out)
        assert rc == 1, "raw bash command must fail audit"
    finally:
        shutil.rmtree(out, ignore_errors=True)


def test_bad_sid_format_fails():
    out = _fresh_outputs_dir()
    try:
        anon = out / "anon"
        # sid not matching s/a/sub_ prefix
        _write_anon(anon, "biome.parquet", [
            {"sid": "wrong-format-sid", "project": "pa1b2c3", "biome": "Fish"},
        ])
        rc = _run_audit(out)
        assert rc == 1, "bad sid format must fail audit"
    finally:
        shutil.rmtree(out, ignore_errors=True)


def test_raw_branch_in_list_field_fails():
    out = _fresh_outputs_dir()
    try:
        anon = out / "anon"
        _write_anon(anon, "biome.parquet", [{"sid": "s0001", "project": "pa1b2c3", "biome": "Fish"}])
        # Branch hidden inside a list-of-strings column
        _write_anon(anon, "topic_signatures.parquet", [
            {"sid": "s0001", "project": "pa1b2c3",
             "token_hashes": ["abc123", "feat/DEV-100-foo"]},
        ])
        rc = _run_audit(out)
        assert rc == 1, "raw branch in nested list must fail audit"
    finally:
        shutil.rmtree(out, ignore_errors=True)


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
