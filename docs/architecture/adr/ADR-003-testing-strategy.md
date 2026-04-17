---
id: ADR-003
title: Testing strategy — layered mocking with optional record-and-replay for real APIs
status: accepted
date: 2026-01-13
deciders: ["Andrei Mazniak"]
tags: ["testing", "ci-cd", "snapshots", "mocks"]
supersedes: null
superseded_by: null
---

# ADR-003: Testing strategy

## Status

**accepted** — the Rust test layout, GitHub Actions CI, Codecov coverage, and per-provider HTTP mocks are in place. The Record-and-Replay layer for real-API tests is intentionally optional.

## Context

The testing strategy must satisfy several competing constraints:

1. **Forks and drive-by contributors must be able to run `cargo test` with no secrets.** A test suite that requires every provider's API token is a non-starter for an open-source project.
2. **The tree must also be covered by real-API tests** somewhere, so that provider drift (breaking changes in GitLab/GitHub/ClickUp/Jira) is caught before it reaches users.
3. **Trunk-based development** — PRs land into `main`, releases are cut via git tags, branch protection enforces green CI and reviews.
4. **Graceful degradation** when an external API is down — test runs should not fail just because a third-party service is momentarily unreachable.

## Decision

### Branching model: trunk-based

```
main ────●────●────●────●────●────●────●────►
         │         │              │
         v1.0.0    v1.1.0         v1.2.0  (git tags → releases)

PRs ─────┴─────────┴──────────────┴─────────►
```

- PRs merge directly into `main`
- Releases are git tags (`vX.Y.Z`) on `main`
- Branch protection: required reviews + green CI

### Test pyramid

```
┌─────────────────────────────────────────────────────────────────┐
│                    Testing Pyramid                               │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│    ┌─────────────────────────────────────────┐                  │
│    │  Real-API tests (opt-in, record mode)   │  with secrets    │
│    │  Record & Replay, fallback to fixtures  │                  │
│    └─────────────────────────────────────────┘                  │
│                        │                                         │
│    ┌─────────────────────────────────────────┐                  │
│    │  Integration tests (trait mocks)        │  no secrets      │
│    │  MockIssueProvider, MockMrProvider, …   │                  │
│    └─────────────────────────────────────────┘                  │
│                        │                                         │
│    ┌─────────────────────────────────────────┐                  │
│    │  Unit tests (HTTP mocks via httpmock)   │  no secrets      │
│    │  Per-provider: URLs, query params, …     │                  │
│    └─────────────────────────────────────────┘                  │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Layer 1: HTTP-level mocks (unit tests)

Per-provider HTTP tests use [`httpmock`](https://docs.rs/httpmock/) to spin up a local mock server, configure expected request/response pairs, and assert:

- The correct HTTP method, URL, headers, and query parameters are sent
- Response parsing produces the expected typed `Issue`/`MergeRequest`/etc. values
- Error cases (401, 403, 5xx, network timeout) are handled correctly

These tests require no secrets and run in every CI job. They live in `crates/plugins/api/<provider>/tests/`.

### Layer 2: Trait-mocked integration tests

`IssueProvider`/`MergeRequestProvider`/etc. are Rust traits (see ADR-004). Mock implementations (`MockIssueProvider`, etc.) sit in `tests/common/` and power integration tests that exercise the MCP tool layer and executor pipeline without touching any network.

### Layer 3: Record-and-Replay for real-API tests (opt-in)

For tests that want to exercise the real provider (drift detection, regression testing against third-party changes), the harness supports two modes:

```
ENV token present?
   │
   ├── YES → Try real API call
   │         ├── 200 OK         → save/update snapshot, return live data
   │         ├── 401/403        → FAIL (bad credentials, surface to user)
   │         ├── 5xx            → warn + fall back to snapshot
   │         └── Network error  → warn + fall back to snapshot
   │
   └── NO  → Load snapshot → use as mock
             (forks, contributors without .env)
```

Snapshot files live under `tests/fixtures/<provider>/` and are committed to the repo. This means:

- Forks run `cargo test` with no secrets and get Replay mode automatically
- In the main repository, a scheduled job with credentials runs in Record mode, refreshes snapshots, and commits drift back as `chore: update test fixtures [skip ci]`

### Error handling sketch

```rust
pub enum ApiResult<T> {
    Ok(T),
    Fallback { data: T, reason: String },   // 5xx / network → snapshot
    ConfigError { message: String },         // 401/403 → fail the test
}

async fn call_api_with_fallback<T>(
    api_call: impl Future<Output = Result<T, ApiError>>,
    fixture_path: &str,
) -> ApiResult<T> { /* ... */ }
```

Authentication failures are never silently masked — they indicate a misconfigured test environment and should fail loudly.

### CI workflows

- `.github/workflows/ci.yml` — jobs: `fmt`, `docs` (build), `clippy`, `test` (matrix over 5 platforms), `build` (matrix over 5 platforms), `coverage`
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features` (with `RUSTFLAGS: -Dwarnings`)
  - `cargo test --all-features` per target
- `.github/workflows/deploy-docs.yml` — on `push` to `main` with changes under `docs/**`, deploys the Rspress site to GitHub Pages
- `.github/workflows/fixtures-update.yml` — refreshes fixtures against the real APIs on a schedule
- `.github/workflows/release.yml` — builds release artefacts on git tag
- Coverage via `cargo-llvm-cov` → Codecov (uses `codecov-action@v5`)
- Record-and-Replay real-API runs are gated on secrets being present; forks skip them and fall back to fixtures

### CI secrets

Only a minimal set of credentials is configured in the main repository; other providers run purely against fixtures:

| Secret | Used for |
|--------|----------|
| `GITLAB_URL`, `GITLAB_TOKEN`, `GITLAB_PROJECT_ID` | GitLab provider tests in Record mode |
| `GH_TEST_TOKEN` | GitHub provider tests in Record mode |
| `CODECOV_TOKEN` | Uploading coverage reports |

### Coverage thresholds

Enforced through `codecov.yml` at the repository root:

| Metric | Target | Threshold | Enforcement |
|--------|--------|-----------|-------------|
| Project coverage | 90% | 1% | status check on PR |
| Patch coverage | 90% | — | informational (reported but does not block merge — test code in `crates/*/tests/` is not instrumented and can unfairly lower patch coverage) |

The `crates/devboy-cli/src/main.rs` binary entry point is excluded from coverage (orchestrates subcommands, does little worth measuring).

### Documentation

The user guide is built with [Rspress](https://rspress.dev/) from `docs/` and deployed to GitHub Pages by `deploy-docs.yml`. Inline API documentation lives in Rust doc comments and is surfaced through `cargo doc`; we do not currently publish a separate auto-generated rustdoc site.

## Consequences

### Positive

- ✅ **Zero-config for contributors** — `cargo test` passes in a fresh clone with no environment variables
- ✅ **Provider drift is caught** — the scheduled record run refreshes fixtures and surfaces breaking changes
- ✅ **Resilient CI** — transient 5xx or network blips fall back to fixtures and don't spuriously fail the build
- ✅ **Fast feedback on misconfiguration** — 401/403 fail immediately with a clear message
- ✅ **Forks work out of the box** — committed fixtures are used as the mock source

### Negative

- ❌ **Repository size** — fixture files grow over time
- ❌ **Stale fixtures** — if the scheduled record run hasn't happened recently, fixtures may lag behind real API responses
- ❌ **PII risk** — real responses may contain personal data (user emails, issue titles); must be sanitised before commit

### Risks

| Risk | Mitigation |
|------|------------|
| PII in fixtures | A sanitiser function runs before each snapshot is saved — redact emails, names, custom-field values |
| Fixture bloat | Save only the fields the tests actually consume; use snapshot-by-snapshot review |
| API rate limits during record runs | Cache, retry with exponential backoff, schedule runs off-peak |

## Alternatives Considered

### Alternative 1: Only real-API tests (no mocks)

**Why rejected:** Forks can't run tests. Every contributor would need their own provider accounts and tokens. Rate limits become a real problem under parallel CI.

### Alternative 2: Only HTTP mocks (no record-and-replay)

**Why rejected:** Provider drift goes undetected. The first signal of a breaking API change would be users reporting breakage.

### Alternative 3: Only trait mocks (no HTTP-level tests)

**Why rejected:** Misses a whole class of bugs — wrong URLs, wrong headers, wrong query parameter names, deserialization mistakes. Trait mocks can't catch "we send `state=open` but the API expects `state=opened`".

## Implementation

- **Unit tests:** `crates/plugins/api/<provider>/tests/` using `httpmock`
- **Integration tests:** `crates/devboy-cli/tests/` + per-crate `tests/`
- **Fixtures:** `tests/fixtures/<provider>/` (committed)
- **CI:** `.github/workflows/ci.yml`

## References

- [insta — Rust snapshot testing](https://insta.rs/)
- [httpmock](https://docs.rs/httpmock/)
- [Trunk Based Development](https://trunkbaseddevelopment.com/)
- [VCR pattern](https://github.com/vcr/vcr) — the Ruby project that popularised record-and-replay

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-01-13 | Andrei Mazniak | Initial version |
| 2026-04-17 | Andrei Mazniak | Translated to English; trimmed inline code samples; marked accepted; clarified that Record-and-Replay is opt-in (not a blocker for contributors) |
| 2026-04-17 | Andrei Mazniak | Synced with current CI: Rspress (not rustdoc+mdBook) for docs, separate `deploy-docs.yml`, coverage thresholds from `codecov.yml` (90% / 90% informational), actual CI secrets list |
