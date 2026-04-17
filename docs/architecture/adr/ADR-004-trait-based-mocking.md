---
id: ADR-004
title: Trait-based provider abstraction for testability and extensibility
status: accepted
date: 2026-01-13
deciders: ["Andrei Mazniak"]
tags: ["rust", "testing", "architecture", "traits"]
supersedes: null
superseded_by: null
---

# ADR-004: Trait-based provider abstraction

## Status

**accepted** — the core traits (`IssueProvider`, `MergeRequestProvider`, `MessengerProvider`, etc.) and their real/mock implementations are in place in `crates/devboy-core/` and `crates/plugins/api/*`.

## Context

`devboy-tools` integrates with many external services — issue trackers (GitLab, GitHub, ClickUp, Jira), a messenger (Slack), a meeting-notes source (Fireflies), and CI pipelines on top of GitLab/GitHub. We need an abstraction that:

1. Lets us mock providers cleanly in tests
2. Lets contributors add a new provider by writing a single crate without touching the core executor
3. Uses a uniform interface across providers so the MCP tool layer doesn't care which provider it's talking to
4. Gets us compile-time safety that a provider actually implements the expected surface

## Decision

Use **Rust traits** (with `async_trait`) for provider abstractions. Each provider crate implements one or more of these traits for its concrete client.

### Core traits

All traits live in `crates/devboy-core/src/provider.rs` and are re-exported from `devboy_core`:

- **`Provider`** — base trait every provider implements. Carries identity and capability declarations (see ADR-007 for the `ToolEnricher` / `ToolCategory` surface).
- **`IssueProvider`** — issues, comments, statuses, links. Implemented by GitLab, GitHub, ClickUp, Jira.
- **`MergeRequestProvider`** — merge/pull requests, discussions, diffs, pipelines. Implemented by GitLab, GitHub.
- **`PipelineProvider`** — CI pipelines, jobs, job logs. Implemented by GitLab, GitHub.
- **`MessengerProvider`** — chats, messages, search, sending. Implemented by Slack.
- **`MeetingNotesProvider`** — meeting notes, transcripts, search. Implemented by Fireflies.

A provider crate can implement any subset of these traits. For example, `devboy-slack` implements `Provider` + `MessengerProvider` only; `devboy-gitlab` implements `Provider` + `IssueProvider` + `MergeRequestProvider` + `PipelineProvider`.

Shape of a typical trait (simplified):

```rust
#[async_trait]
pub trait IssueProvider: Provider {
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>>;
    async fn get_issue(&self, key: &str) -> Result<Issue>;
    async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue>;
    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue>;
    async fn get_comments(&self, issue_key: &str) -> Result<ProviderResult<Comment>>;
    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment>;
    // ... asset methods (see ADR-010), plus optional link/relation methods
}
```

### Real implementations

Each real provider lives in its own crate under `crates/plugins/api/<provider>/`. Example shape:

```rust
// crates/plugins/api/gitlab/src/lib.rs
pub struct GitLabProvider {
    client: reqwest::Client,
    base_url: String,
    token: String,
    project_id: String,
}

#[async_trait]
impl IssueProvider for GitLabProvider {
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>> {
        let url = format!("{}/api/v4/projects/{}/issues", self.base_url, self.project_id);
        let response = self.client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .query(&filter.to_gitlab_params())
            .send().await?
            .error_for_status()?
            .json::<Vec<GitLabIssue>>().await?;
        Ok(response.into_iter().map(Issue::from).collect())
    }
    // ...
    fn provider_name(&self) -> &'static str { "gitlab" }
}
```

### Test harness for Record-and-Replay

Integration tests use the Record-and-Replay pattern from ADR-003 through a small harness under `crates/devboy-cli/tests/common/`:

- **`TestProvider`** (`test_provider.rs`) — wraps a concrete real client and a `FixtureProvider`. Selects Record mode when the relevant provider env vars are set; otherwise selects Replay mode.
- **`FixtureProvider`** — implements the provider traits by loading committed fixture files under `crates/devboy-cli/tests/fixtures/<provider>/`.
- **`ApiResult<T>`** (`api_result.rs`) — the `Ok` / `Fallback` / `ConfigError` variant from ADR-003 that threads through live-call outcomes.

```rust
// Sketch — see crates/devboy-cli/tests/common/test_provider.rs for the real thing
pub struct TestProvider { /* mode + real client + fixture fallback */ }
impl TestProvider {
    pub fn github() -> Self { /* detects GITHUB_TEST_TOKEN */ }
}
```

Unit tests in provider crates use `httpmock` instead (see the next section) and don't need this harness.

### Unit-test mocks in provider crates

Every provider crate (e.g. `crates/plugins/api/github/`) has its own unit tests that spin up an `httpmock` server, configure expected requests and responses, and run the real client against the mock. These tests need no Record-and-Replay machinery because they exercise the HTTP shape, not the provider semantics.

### Two levels of mocking

Trait mocks are paired with HTTP-level mocks for a full picture:

| Level | Tool | Tests | When to use |
|-------|------|-------|-------------|
| Trait mocks | `MockIssueProvider` | MCP tool layer, executor, business logic | Integration tests |
| HTTP mocks | `httpmock` | HTTP request shape, response parsing | Per-provider unit tests |

HTTP-level mocks catch a different class of bug: wrong URLs, wrong query parameter names, missing headers, deserialization errors. Trait mocks can't catch those because they skip the HTTP layer entirely.

### Using the provider in MCP tools

```rust
// crates/devboy-mcp/src/tools/issues.rs
pub struct GetIssuesTool {
    provider: Arc<dyn IssueProvider>,
}

#[async_trait]
impl McpTool for GetIssuesTool {
    fn name(&self) -> &str { "get_issues" }

    async fn execute(&self, params: Value) -> Result<Value> {
        let filter: IssueFilter = serde_json::from_value(params)?;
        let issues = self.provider.get_issues(filter).await?;
        Ok(serde_json::to_value(issues)?)
    }
}
```

Tools hold `Arc<dyn IssueProvider>` — the concrete type is resolved at run-time based on configuration.

## Consequences

### Positive

- ✅ **Easy to mock** — any struct that implements the trait is a drop-in replacement in tests
- ✅ **Extensible** — adding a new provider is a new crate implementing the trait, no core changes
- ✅ **Compile-time safety** — Rust refuses to compile a provider that doesn't cover every required method
- ✅ **Dependency injection** — swap providers at construction time without rebuilding the world
- ✅ **Testable without network** — integration tests run on pure trait mocks

### Negative

- ❌ **Boilerplate** — every provider must impl every method (even ones it can't meaningfully support, returning `ProviderUnsupported`)
- ❌ **`async_trait` overhead** — small runtime cost per call (heap allocation for the returned future). Negligible for I/O-bound work like ours.
- ❌ **Object-safety constraints** — traits used through `dyn` can't have generic methods; a few API shapes are awkward because of this

## Alternatives Considered

### Alternative 1: Enum-based dispatch

```rust
enum Provider {
    GitLab(GitLabProvider),
    GitHub(GitHubProvider),
    // ...
}
```

**Why rejected:** The core enum has to change every time a new provider is added. This defeats the point of the plugin crate split (see ADR-007).

### Alternative 2: Generic parameters everywhere

```rust
struct Mcp<P: IssueProvider> { provider: P }
```

**Why rejected:** Makes run-time selection ("use GitLab if configured, else GitHub") awkward — you need `Box<dyn IssueProvider>` anyway for anything user-configurable. Generics would only help if the provider were chosen at compile time, which isn't the case.

## Implementation

- **Traits:** `crates/devboy-core/src/provider.rs` (re-exported from the `devboy_core` crate root)
- **Real providers:** `crates/plugins/api/<provider>/`
- **HTTP-level mocks:** per-provider tests using [`httpmock`](https://docs.rs/httpmock/) and per-provider `MockServer` setup
- **Test harness for Record-and-Replay:** `crates/devboy-cli/tests/common/` (`test_provider.rs`, `fixture_provider.rs`, `api_result.rs`, `mod.rs`)
- **Committed fixtures:** `crates/devboy-cli/tests/fixtures/<provider>/`

## References

- [`async-trait`](https://docs.rs/async-trait/)
- [`httpmock`](https://docs.rs/httpmock/)
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [ADR-003: Testing strategy](./ADR-003-testing-strategy.md)
- [ADR-007: Plugin architecture](./ADR-007-plugin-architecture.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-01-13 | Andrei Mazniak | Initial version |
| 2026-04-17 | Andrei Mazniak | Translated to English; marked accepted; trimmed code samples to the essentials |
| 2026-04-17 | Andrei Mazniak | Synced trait list with `devboy_core::provider` (added `Provider`, `PipelineProvider`, `MeetingNotesProvider`); replaced the fictional `MockIssueProvider` with the real test harness (`TestProvider`, `FixtureProvider`, `ApiResult` in `crates/devboy-cli/tests/common/`) |
| 2026-04-17 | Andrei Mazniak | Fixed return types in code sketches: provider methods return `Result<ProviderResult<T>>`, not `Result<Vec<T>>` |
