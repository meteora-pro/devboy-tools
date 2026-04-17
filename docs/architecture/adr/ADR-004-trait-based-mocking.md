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

`devboy-tools` integrates with many external services: issue trackers (GitLab, GitHub, ClickUp, Jira), messengers (Slack, Telegram), CI systems (GitLab Pipelines, GitHub Actions), meeting sources (Fireflies). We need an abstraction that:

1. Lets us mock providers cleanly in tests
2. Lets contributors add a new provider by writing a single crate without touching the core executor
3. Uses a uniform interface across providers so the MCP tool layer doesn't care which provider it's talking to
4. Gets us compile-time safety that a provider actually implements the expected surface

## Decision

Use **Rust traits** (with `async_trait`) for provider abstractions. Each provider crate implements the trait for its concrete client.

### Core traits

```rust
// crates/devboy-core/src/traits.rs
use async_trait::async_trait;

#[async_trait]
pub trait IssueProvider: Send + Sync {
    async fn get_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>>;
    async fn get_issue(&self, key: &str) -> Result<Issue>;
    async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue>;
    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue>;
    async fn get_comments(&self, issue_key: &str) -> Result<Vec<Comment>>;
    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment>;
    fn provider_name(&self) -> &'static str;
}

#[async_trait]
pub trait MergeRequestProvider: Send + Sync {
    async fn get_merge_requests(&self, filter: MrFilter) -> Result<Vec<MergeRequest>>;
    async fn get_merge_request(&self, key: &str) -> Result<MergeRequest>;
    async fn get_discussions(&self, mr_key: &str) -> Result<Vec<Discussion>>;
    async fn get_diffs(&self, mr_key: &str) -> Result<Vec<FileDiff>>;
    async fn add_comment(
        &self,
        mr_key: &str,
        body: &str,
        position: Option<CodePosition>,
    ) -> Result<Comment>;
    async fn get_pipeline(&self, mr_key: &str) -> Result<Pipeline>;
    fn provider_name(&self) -> &'static str;
}

#[async_trait]
pub trait MessengerProvider: Send + Sync {
    async fn get_messages(&self, chat_id: &str, filter: MessageFilter) -> Result<Vec<Message>>;
    async fn send_message(&self, chat_id: &str, text: &str) -> Result<Message>;
    async fn search_messages(&self, query: &str) -> Result<Vec<Message>>;
    fn provider_name(&self) -> &'static str;
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
    async fn get_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
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

### Mock implementations for tests

```rust
// tests/common/mock_provider.rs
pub struct MockIssueProvider { issues: Vec<Issue>, /* ... */ }

impl MockIssueProvider {
    pub fn from_fixtures(path: &str) -> Result<Self> { /* load JSON */ }
    pub fn with_issue(mut self, issue: Issue) -> Self { /* builder */ }
}

#[async_trait]
impl IssueProvider for MockIssueProvider {
    async fn get_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        // In-memory filter + limit
    }
    // ...
    fn provider_name(&self) -> &'static str { "mock" }
}
```

The mock can be hydrated from fixture files (for Record-and-Replay tests per ADR-003) or built inline per test with `.with_issue(...)`.

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

- **Traits:** `crates/devboy-core/src/traits.rs` (and neighbouring modules for specific domains)
- **Real providers:** `crates/plugins/api/<provider>/`
- **HTTP-level mocks:** `crates/plugins/api/<provider>/tests/` using `httpmock`
- **Trait mocks:** `crates/devboy-core/src/mock.rs` or per-crate `tests/common/`

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
