# Release procedure

Authoritative checklist for cutting a `devboy-tools` release. Reflects [ADR-022](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-022-crates-io-publishing.md) — the workspace ships through **two** channels:

- **npm** — `@devboy-tools/cli` and per-platform binary subpackages. Primary user-facing channel; this is what `devboy onboard` and the agent plugins assume.
- **crates.io** — every workspace library + the `devboy-cli` binary (`cargo install devboy-cli`). Secondary channel for downstream Rust projects that want to embed devboy components without vendoring source.

Both channels publish from the **same `v*` git tag**, in parallel: pushing the tag fans out to two GitHub Actions workflows (`.github/workflows/release.yml` for npm, `.github/workflows/release-crates-io.yml` for crates.io).

## CI tokens

Two repo secrets drive the two channels. Set both at https://github.com/meteora-pro/devboy-tools/settings/secrets/actions.

| Secret | Channel | What it is | Scopes |
|---|---|---|---|
| `NPM_TOKEN` | npm | npm automation token | `automation` |
| `CARGO_REGISTRY_TOKEN` | crates.io | crates.io API token | `publish-update` (every release after first); add `publish-new` until each crate has had its first publish |

The `CARGO_REGISTRY_TOKEN` env var is read by `cargo publish` natively — no `cargo login` step is needed in CI. Generate the token at https://crates.io/settings/tokens, scope it to "All crates" (or an explicit `devboy-*` allowlist), pick an expiry you're willing to rotate, and paste it into the repo secret.

## Before you start

- Decide the target version (workspace-wide, single bump in `[workspace.package].version`).
- Confirm `main` is green: CI, tests, plugin manifest drift check, and `cargo publish --dry-run -p devboy-core` all passed on the merge commit.
- Confirm you have:
  - Push access to the `meteora-pro/devboy-tools` git remote (both release pipelines trigger from `v*` tags).
  - Both `NPM_TOKEN` and `CARGO_REGISTRY_TOKEN` set as repo secrets (see the table at the top of this doc).

## Step 1 — Bump the version

1. Update `[workspace.package].version` in the root `Cargo.toml`. Every member crate inherits it.
2. Update every `[workspace.dependencies] devboy-* = { version = "X.Y.Z", path = "..." }` to the new version. Local builds keep resolving via `path`; published consumers resolve via `version`.
3. Run `cargo check --workspace --all-targets` and `cargo test --workspace` locally.
4. Commit: `chore(release): bump workspace to X.Y.Z`.
5. Open a PR. Wait for CI. Merge.

## Step 2 — Tag

```bash
git checkout main
git pull
git tag -a vX.Y.Z -m "release X.Y.Z"
git push origin vX.Y.Z
```

Pushing the tag fans out to **two parallel GitHub Actions workflows**:

- `.github/workflows/release.yml` → builds platform binaries, signs them, and publishes the npm package + GitHub Release.
- `.github/workflows/release-crates-io.yml` → publishes every workspace crate to crates.io in topological order using `CARGO_REGISTRY_TOKEN`.

No further manual steps. The workflows are idempotent on retry-after-failure (re-runs from the failed step), but **not** on already-published versions — see "Recovery" below.

## Step 3 — Verify

Once both workflows are green:

- [npm page](https://www.npmjs.com/package/@devboy-tools/cli) — new version visible
- GitHub Releases — tag with platform binaries attached
- `https://crates.io/crates/<name>` — every devboy-* crate at the new version
- `https://docs.rs/<name>` — docs build green (5–10 min)

If a docs.rs build is red, ship a patch version with the doc fix (you can't re-upload the same version).

## Manual fallback (rare)

The first ever crates.io release (0.27.0) was run by hand because each crate had to claim its name. From 0.27.x onwards CI handles it; this section is here for break-glass scenarios.

```bash
git checkout vX.Y.Z

# Layer 1 — leaf
cargo publish -p devboy-core

# Layer 2 — depend only on devboy-core (publish in any order)
cargo publish -p devboy-storage
cargo publish -p devboy-assets
cargo publish -p devboy-format-pipeline
cargo publish -p devboy-gitlab
cargo publish -p devboy-github
cargo publish -p devboy-jira
cargo publish -p devboy-clickup
cargo publish -p devboy-confluence
cargo publish -p devboy-fireflies
cargo publish -p devboy-slack

# Layer 3 — depends on layer 1 + 2
cargo publish -p devboy-executor

# Layer 4 — depends on layer 3
cargo publish -p devboy-mcp

# Layer 5 — depends on devboy-core (independent of the executor/mcp chain)
cargo publish -p devboy-skills

# Layer 6 — binary, depends on everything above
cargo publish -p devboy-cli
```

`cargo login` once with a token that has `publish-new` + `publish-update` (or just rely on `CARGO_REGISTRY_TOKEN` env var if you'd rather not persist the token).

## Recovery

If a step fails partway through the wave, **stop** and investigate. crates.io rejects re-uploads of the same version, so:

1. Fix the underlying issue on `main`.
2. Bump the patch version to `X.Y.(Z+1)` in `[workspace.package].version` and every `[workspace.dependencies]` entry.
3. Retag as `vX.Y.(Z+1)` and push.
4. Re-running CI publishes from the failed crate onwards — the earlier ones already on crates.io are skipped automatically when they hit "no token" or "already exists" with `--no-verify`. (If they fail loudly, edit the workflow to comment out the already-published layers temporarily.)

> **Settling delay.** crates.io's index sometimes needs ~30 s before a freshly-published crate becomes resolvable as a dependency. The CI workflow has explicit `sleep` calls between layers to handle this; if you're publishing manually and hit `no matching package named …`, wait 30 seconds and retry.

## Step 4 — Verify the wave landed

For each crate that was just published:

- `https://crates.io/crates/<name>` — version page exists, README rendered.
- `https://docs.rs/<name>` — docs build is green (docs.rs typically completes within 5–10 minutes).

If a docs.rs build is red, fix the docs and publish a **patch** version (you cannot re-upload the same version on crates.io).

## Step 5 — Post-release hygiene

- Update the "Use as a library" section of the root `README.md` if new crates joined the wave.
- Close the release issue if there is one.
- Open a milestone for the next version.

## Smoke-test snippets

```bash
# Verify package metadata before publishing
cargo publish --dry-run -p devboy-core

# Inspect the tarball Cargo would upload
cargo package -p devboy-core --list

# Confirm the published version once the upload finishes
cargo search devboy-core --limit 1
```

## Failure modes seen so far

| Symptom | Cause | Fix |
|---|---|---|
| `error: failed to verify package tarball … couldn't read … no such file` | An `include_str!`/`rust-embed`/`include_bytes!` path resolves outside the crate root. Cargo packaged only the crate dir, so the file is missing in the tarball. | Move the data inside the crate, or add a `build.rs` that mirrors it into `OUT_DIR`. |
| `error: no matching package named devboy-core found` (running dry-run) | Cargo resolves deps through the registry. The dependency is not on crates.io yet (or hasn't propagated). | Publish the dependency first, or wait ~30s for index propagation. |
| `error: 1 files in the working directory contain changes that were not yet committed into git` | You're publishing from a dirty tree. | Commit first. `--allow-dirty` exists for emergencies but should not be the default. |
| docs.rs build is red on `https://docs.rs/<crate>/<version>/builds` | A doctest or feature-gated path didn't compile in the docs.rs sandbox. | Fix locally with `cargo doc --no-deps -p <crate>`, then publish a patch version. |
