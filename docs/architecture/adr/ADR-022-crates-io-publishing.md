---
id: ADR-022
title: Publish workspace library crates on crates.io alongside the npm CLI binary
status: proposed
date: 2026-05-08
deciders: ["Andrei Mazniak"]
tags: ["distribution", "rust", "crates.io"]
supersedes: null
superseded_by: null
---

# ADR-022: Publish workspace library crates on crates.io alongside the npm CLI binary

## Status

**proposed** — operational checklist tracked in [#250](https://github.com/meteora-pro/devboy-tools/issues/250).

## Context

[ADR-002](./ADR-002-rust-architecture.md) decided to ship `devboy-tools` as a Cargo workspace whose **CLI binary** is distributed through an npm wrapper. That decision optimised for a single user-facing channel: end users who type `npm install -g @devboy-tools/cli` and never touch Cargo.

ADR-002 did **not** address the library surface. The workspace already exposes a meaningful set of reusable building blocks:

- `devboy-core` — provider traits (`IssueProvider`, `MergeRequestProvider`, …), unified types, error model, configuration
- `devboy-mcp` — an MCP server (proxy client + transport) that downstream agents could embed directly
- `devboy-skills` — SKILL.md / frontmatter parser + manifest model
- `devboy-storage` — keychain-backed credential store
- `devboy-assets` — on-disk asset cache with rotation (ADR-010)
- `devboy-executor` — tool execution engine + enrichment pipeline
- `devboy-format-pipeline` — TOON encoding, MCKP-budget trimming, cursor pagination (papers 1, 2)
- API plugins (`devboy-gitlab`, `devboy-github`, `devboy-jira`, `devboy-clickup`, `devboy-confluence`, `devboy-fireflies`, `devboy-slack`) — concrete provider implementations against the `devboy-core` traits

Today none of these crates is consumable as a Cargo dependency outside this repo: there is no `version` for any inter-crate dependency, no `description`/`license`/`repository` metadata propagated to crates.io, no per-crate README, and no `[package.metadata.docs.rs]` block. A downstream project that wanted to embed (for example) the format pipeline or the MCP proxy client would have to vendor source.

We want to change that without disturbing the npm distribution channel.

## Decision

> **Decision:** Publish every workspace crate that has a stable public surface as a **library on crates.io**, publish the CLI binary as a **secondary** `cargo install`-target, and explicitly mark internal-only crates `publish = false`. The npm CLI distribution remains the primary user-facing channel.

### Per-crate publish decisions

| Crate | Path | Kind | `publish` | Notes |
|---|---|---|---|---|
| `devboy-core` | `crates/devboy-core` | library | `true` | Foundation. Publish first in any release. |
| `devboy-storage` | `crates/devboy-storage` | library | `true` | Depends on `devboy-core`. |
| `devboy-assets` | `crates/devboy-assets` | library | `true` | Depends on `devboy-core`. |
| `devboy-format-pipeline` | `crates/plugins/format-pipeline` | library | `true` | Depends on `devboy-core`. |
| `devboy-gitlab` | `crates/plugins/api/gitlab` | library | `true` | Depends on `devboy-core`. |
| `devboy-github` | `crates/plugins/api/github` | library | `true` | Depends on `devboy-core`. |
| `devboy-jira` | `crates/plugins/api/jira` | library | `true` | Depends on `devboy-core`. |
| `devboy-clickup` | `crates/plugins/api/clickup` | library | `true` | Depends on `devboy-core`. |
| `devboy-confluence` | `crates/plugins/api/confluence` | library | `true` | Depends on `devboy-core`. |
| `devboy-fireflies` | `crates/plugins/api/fireflies` | library | `true` | Depends on `devboy-core`. |
| `devboy-slack` | `crates/plugins/api/slack` | library | `true` | Depends on `devboy-core`. |
| `devboy-executor` | `crates/devboy-executor` | library | `true` | Depends on `devboy-core`, `devboy-assets`, all API plugins, format-pipeline. |
| `devboy-mcp` | `crates/devboy-mcp` | library | `true` | Depends on `devboy-core`, `devboy-assets`, `devboy-executor`, `devboy-confluence`, format-pipeline. |
| `devboy-skills` | `crates/devboy-skills` | library | `true` | Depends on `devboy-core`. |
| `devboy-cli` | `crates/devboy-cli` | binary | `true` | Secondary: `cargo install devboy-cli` works alongside the npm channel. |
| `llm-eval` | `crates/llm-eval` | binary | `false` | Internal research tool. Not for public consumption. |

### Versioning

- Workspace stays on a **single shared version** (`workspace.package.version`). Bump once, all crates move together.
- First wave ships from the current `0.26.x` line — pre-1.0 semver permits breaking changes when needed.
- Internal dependencies are pinned through `[workspace.dependencies]` with both a `version =` and a `path =`. Local builds resolve via path; crates.io builds resolve via version.

### Release order

Topological order from leaves of the internal dep graph:

1. `devboy-core` (no internal deps)
2. `devboy-storage`, `devboy-assets`, `devboy-format-pipeline`, all 7 API plugins (depend only on `devboy-core`)
3. `devboy-executor` (depends on core + assets + plugins + format-pipeline)
4. `devboy-mcp` (depends on core + assets + executor + confluence + format-pipeline)
5. `devboy-skills` (depends on core)
6. `devboy-cli` (depends on everything)

The release procedure lives under [`docs/guide/contributing/release.mdx`](../../guide/contributing/release.mdx).

### CI guard

A `publish-dry-run` job runs `cargo publish --dry-run -p <crate>` for every publishable crate on PRs that touch a `Cargo.toml` or sources beneath `crates/`. Drift surfaces before a release attempt, not during one.

### Documentation

- Every publishable crate has a per-crate `README.md` and a crate-level `//!` doc comment with a minimal usage example.
- `[package.metadata.docs.rs]` is configured to enable `all-features` and pin the docs.rs build target.
- The root README has a "Use as a library" section that lists the published crates with version and docs.rs badges.

## Consequences

### Positive

- ✅ Downstream Rust projects can embed devboy components without vendoring.
- ✅ Provider plugins (`devboy-gitlab`, `devboy-jira`, …) become directly reusable — small, focused, well-tested crates with a stable trait contract.
- ✅ docs.rs renders the public surface, which doubles as authoritative API documentation.
- ✅ The npm distribution keeps working unchanged for end users who do not touch Cargo.

### Negative

- ❌ A second release channel to maintain. Each release now performs `cargo publish` on ~15 crates in topological order, in addition to the npm release.
- ❌ Public-API discipline becomes a hard constraint: anything `pub` outside a publishable crate's intended surface is a breaking-change risk. We mitigate this by collapsing internals to `pub(crate)` and adding `#![warn(missing_docs)]`, but it adds review surface.

### Risks

- ⚠️ **Name squatters.** All 15 names are free on crates.io as of 2026-05-08 (verified via `GET /api/v1/crates/<name>`). Mitigation: publish a `0.0.0` placeholder for each name from the same release that flips `publish = false → true` to claim ownership before the formal first wave.
- ⚠️ **Internal coupling leaking through public types.** A `devboy-mcp` function that returns a private `devboy-executor` type silently makes that executor type part of the MCP public surface. Mitigation: Phase 4 (public API audit, [#250](https://github.com/meteora-pro/devboy-tools/issues/250)) runs `cargo public-api` and locks down boundaries before the first wave.
- ⚠️ **CI cost.** Running 15 `cargo publish --dry-run` invocations per PR is non-trivial. Mitigation: gate the job on `paths:` filters (only run when a Cargo.toml or `crates/**` file changes) and reuse `Swatinem/rust-cache`.

## Alternatives Considered

### Alternative 1: Keep npm-only — do not publish on crates.io

**Description:** Leave the workspace as-is. End users get the CLI through npm; downstream Rust projects that want to embed pieces of devboy fork or vendor.

**Why rejected:** The library surface (`devboy-core` traits, format pipeline, MCP proxy client, skill manifest parser) is genuinely reusable and we already get external interest. Forcing every consumer to vendor is friction for negligible savings — the operational cost of `cargo publish` is small once the metadata is in place.

### Alternative 2: Publish a single mega-crate `devboy-tools`

**Description:** Collapse the workspace into a single published crate with feature flags for each piece (`features = ["mcp", "gitlab", "format-pipeline"]`).

**Why rejected:** Loses the layering. A consumer who only wants the `devboy-jira` provider would have to pull every transitive dep behind feature flags, and our internal architecture would have to absorb the public-surface implications of every cross-crate symbol. Multi-crate publish keeps each surface narrow.

### Alternative 3: Publish only `devboy-core` and freeze the rest

**Description:** Publish just the trait crate, keep everything else internal.

**Why rejected:** `devboy-core` alone is not actionable — a consumer who imports the trait still needs a concrete `IssueProvider` implementation, which is exactly what the API plugin crates are. The plugins are also where most of the reuse value lives.

## Implementation

- **Issues:** [#250](https://github.com/meteora-pro/devboy-tools/issues/250)
- **PR:** _this PR_
- **Code:** workspace `Cargo.toml`, every crate's `Cargo.toml`, `.github/workflows/ci.yml`, `docs/guide/contributing/release.mdx`

The implementation is staged across the phases listed in [#250](https://github.com/meteora-pro/devboy-tools/issues/250). This ADR captures the architectural decision; the issue tracks the per-phase checklist.

## References

- [ADR-001](./ADR-001-apache-license.md) — Apache 2.0 license (the license field for every published crate)
- [ADR-002](./ADR-002-rust-architecture.md) — Rust workspace + npm distribution (this ADR extends it)
- [Cargo: publishing on crates.io](https://doc.rust-lang.org/cargo/reference/publishing.html)
- [docs.rs: about builds](https://docs.rs/about/builds)
- [`cargo public-api`](https://github.com/cargo-public-api/cargo-public-api)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-08 | Andrei Mazniak | Initial version |
