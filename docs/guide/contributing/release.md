# Release procedure

Authoritative checklist for cutting a `devboy-tools` release. Reflects [ADR-022](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-022-crates-io-publishing.md) — the workspace ships through **two** channels:

- **npm** — `@devboy-tools/cli` and per-platform binary subpackages. Primary user-facing channel; this is what `devboy onboard` and the agent plugins assume.
- **crates.io** — every workspace library plus the `devboy-cli` binary. Secondary channel: lets downstream Rust projects embed devboy components and lets people install the CLI through `cargo install` without Node.

Both channels publish from the **same git tag**.

## Before you start

- Decide the target version (workspace-wide, single bump in `[workspace.package].version`).
- Confirm `main` is green: CI, tests, plugin manifest drift check, and `cargo publish --dry-run -p devboy-core` all passed on the merge commit.
- Confirm you have:
  - A crates.io API token with publish permission on every `devboy-*` crate. `cargo login` once per machine.
  - Push access to the `meteora-pro/devboy-tools` git remote (the npm release pipeline triggers from a `v*` tag).
  - The local toolchain matches CI (`rustup show` → stable, `rust-version = 1.85`).

## Step 1 — Bump the version

1. Update `[workspace.package].version` in the root `Cargo.toml`. Every member crate inherits it.
2. Update every `[workspace.dependencies] devboy-* = { version = "X.Y.Z", path = "..." }` to the new version. Local builds keep resolving via `path`; published consumers resolve via `version`.
3. Run `cargo check --workspace --all-targets` and `cargo test --workspace` locally.
4. Commit: `chore(release): bump workspace to X.Y.Z`.
5. Open a PR. Wait for CI. Merge.

## Step 2 — Tag and publish to npm

The existing `.github/workflows/release.yml` triggers on `v*` tags and handles npm publication, signing, and GitHub Release creation.

```bash
git checkout main
git pull
git tag -a vX.Y.Z -m "release X.Y.Z"
git push origin vX.Y.Z
```

Wait for the workflow to finish. Verify on:

- [crates page on npm](https://www.npmjs.com/package/@devboy-tools/cli) — new version visible
- GitHub Releases — new tag with platform binaries attached

## Step 3 — Publish to crates.io (first wave)

Order matters: each crate's deps must already be on crates.io before its own `cargo publish` runs. Publish in topological order from leaves up.

> **Smoke-test first.** Before you start, run `cargo publish --dry-run -p devboy-core` from a clean checkout of the tagged commit. If it fails, stop — fix the underlying issue, retag, retry.

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
```

Each `cargo publish` call:

1. Packages the crate.
2. Re-builds it from the tarball (verify step) — this is what catches "files outside the package" issues.
3. Uploads to crates.io and waits for the registry to acknowledge.

If a step fails, **stop**. Investigate, fix on `main`, cut a patch tag (`vX.Y.Z+1`), and resume from the failed crate.

> **Settling delay.** crates.io's index sometimes needs a few seconds before a freshly-published crate becomes resolvable as a dependency. If the next `cargo publish` errors with `no matching package named …`, wait 30 seconds and retry.

## Step 4 — Verify the wave landed

For each crate that was just published:

- `https://crates.io/crates/<name>` — version page exists, README rendered.
- `https://docs.rs/<name>` — docs build is green (docs.rs typically completes within 5–10 minutes).

If a docs.rs build is red, fix the docs and publish a **patch** version (you cannot re-upload the same version on crates.io).

## Step 5 — Post-release hygiene

- Update the "Use as a library" section of the root `README.md` if new crates joined the wave.
- Close the release issue if there is one.
- Open a milestone for the next version.

## Second wave: `devboy-skills` and `devboy-cli`

`devboy-skills` embeds the workspace-root `skills/` tree via `rust-embed`, and `cargo publish` rejects files outside the crate root. The fix (move `skills/` inside `devboy-skills` or wire a `build.rs` sync) ripples into plugin symlinks, release scripts, and several ADRs — large enough to warrant its own PR. `devboy-cli` is blocked behind it because it depends on `devboy-skills`.

When the second wave lands, this document grows two more `cargo publish` invocations at the bottom of step 3.

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
