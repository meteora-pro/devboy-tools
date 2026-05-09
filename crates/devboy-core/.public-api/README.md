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

`--simplified` (a.k.a. `-s`) drops generic-parameter clutter from
generated `impl` blocks but **keeps** auto-derived trait impls
(`Clone`, `Debug`, `PartialEq`, …) in the snapshot. That is
deliberate: those impls are part of the observable public surface
and a `derive` change should show up as a baseline diff. If a
particular crate's baseline is dominated by derive churn and the
trade-off no longer pays its way, switch that crate to `-sss`
(everything `--simplified` does + collapse auto-trait impls) and
note the choice here.

## Diff against the baseline

```bash
cargo public-api --simplified -p devboy-core diff \
  crates/devboy-core/.public-api/baseline.txt
```

Empty output = no surface change. Anything else is a candidate for the
release notes (or a `pub(crate)` lockdown if it leaked unintentionally).

## Audit log

### 2026-05-09 — initial baseline (Phase 4, [#250](https://github.com/meteora-pro/devboy-tools/issues/250))

Walked every `pub mod` and `pub use` in `lib.rs` and grepped the rest
of the workspace for usage. **No items can be safely demoted to
`pub(crate)`**:

- `pub mod sentry_integration` — called from `devboy-cli/src/main.rs`
  (`init_sentry` lifecycle).
- `pub mod remote_config` — called from `devboy-cli/src/main.rs` and
  `devboy-cli/src/doctor/checks/config.rs` (`resolve_url`,
  `redact_url_for_display`, `fetch_and_merge`).
- `pub mod asset` — used by `devboy-assets`, the API plugins, and
  `devboy-executor` (asset metadata, markdown attachment parsing).
- `pub mod agents` — used by `devboy-cli` (onboard / agent detection)
  and `devboy-mcp` (bundle resolution).
- `pub mod enricher` — used by `devboy-format-pipeline` (knapsack
  planner) and every API plugin (custom-field hints).
- Provider traits, types, config — primary public surface; consumed
  by every plugin crate and the executor.

Conclusion: the broad surface is load-bearing for the workspace. The
baseline is the line; further demotion needs per-symbol justification
and a downstream migration plan. Future PRs that add `pub` items will
show up as a positive diff against `baseline.txt` and have to be
either justified or shrunk before merge — that's the actual lockdown.
