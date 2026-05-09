# devboy-core public API baseline

`baseline.txt` is the snapshot of `devboy-core`'s public Rust surface
captured with [`cargo-public-api`](https://github.com/cargo-public-api/cargo-public-api).
It exists to make breaking-change diffs visible in code review.

## Regenerate

```bash
rustup toolchain install nightly --profile minimal     # one-time
cargo install cargo-public-api --locked                # one-time
cargo public-api --simplified -p devboy-core \
  > crates/devboy-core/.public-api/baseline.txt
```

## Diff against the baseline

```bash
cargo public-api --simplified -p devboy-core diff \
  crates/devboy-core/.public-api/baseline.txt
```

Empty output = no surface change. Anything else is a candidate for the
release notes (or a `pub(crate)` lockdown if it leaked unintentionally).
