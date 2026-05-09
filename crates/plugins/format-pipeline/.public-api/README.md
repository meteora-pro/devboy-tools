# devboy-format-pipeline public API baseline

Snapshot captured with [`cargo-public-api`](https://github.com/cargo-public-api/cargo-public-api). Drift surfaces in code review by diffing against `baseline.txt`.

## Regenerate

```bash
cargo public-api --simplified -p devboy-format-pipeline \
  > crates/plugins/format-pipeline/.public-api/baseline.txt
```

## Diff against the baseline

```bash
cargo public-api --simplified -p devboy-format-pipeline diff \
  crates/plugins/format-pipeline/.public-api/baseline.txt
```

Empty output = no surface change. See [crates/devboy-core/.public-api/README.md](../../../devboy-core/.public-api/README.md) for the rationale and one-time setup (nightly toolchain + `cargo install cargo-public-api`).
