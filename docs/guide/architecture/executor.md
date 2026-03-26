# Executor

The `devboy-executor` crate separates tool execution logic from transport (MCP stdio, HTTP, NAPI). It provides a stateless execution engine that creates providers on-the-fly from runtime context.

## Core Concepts

### AdditionalContext

Runtime context for each tool call. Contains everything needed to create a provider:

```rust
pub struct AdditionalContext {
    pub provider: ProviderConfig,      // WHO: connection + scope
    pub proxy: Option<ProxyConfig>,    // optional proxy
    pub metadata: Option<ProviderMetadata>, // for dynamic enrichment
    pub extra: HashMap<String, Value>, // extensibility
}
```

### ProviderConfig

Typed enum with provider-specific scopes. Compiler prevents invalid combinations:

```rust
pub enum ProviderConfig {
    GitLab {
        base_url: String,
        access_token: String,
        scope: GitLabScope,          // Project | Group | Global
        extra: HashMap<String, Value>,
    },
    GitHub {
        base_url: String,
        access_token: String,
        scope: GitHubScope,          // Repository | Organization | Global
        extra: HashMap<String, Value>,
    },
    ClickUp { ... },
    Jira { ... },
    Custom { name: String, config: HashMap<String, Value> },
}
```

Each scope determines which API endpoint prefix to use:
- `GitLabScope::Project { id }` → `/api/v4/projects/{id}/...`
- `GitHubScope::Repository { owner, repo }` → `/repos/{owner}/{repo}/...`

### ToolOutput

Typed enum for structured results. The caller decides how to format:

```rust
pub enum ToolOutput {
    Issues(Vec<Issue>),
    SingleIssue(Box<Issue>),
    MergeRequests(Vec<MergeRequest>),
    SingleMergeRequest(Box<MergeRequest>),
    Discussions(Vec<Discussion>),
    Diffs(Vec<FileDiff>),
    Comments(Vec<Comment>),
    Text(String),
}
```

## Usage

### Direct execution

```rust
use devboy_executor::{Executor, AdditionalContext, ProviderConfig, GitLabScope};

let executor = Executor::new();
let ctx = AdditionalContext {
    provider: ProviderConfig::GitLab {
        base_url: "https://gitlab.com".into(),
        access_token: "glpat-xxx".into(),
        scope: GitLabScope::Project { id: "12345".into() },
        extra: HashMap::new(),
    },
    proxy: None,
    metadata: None,
    extra: HashMap::new(),
};

let output = executor.execute("get_issues", json!({"state": "open"}), &ctx).await?;
```

### With formatting

```rust
use devboy_executor::execute_and_format;

let text = execute_and_format(&executor, "get_issues", args, &ctx, None).await?;
// Returns formatted markdown/compact/json text
```

### With enrichers

```rust
use devboy_executor::{Executor, PipelineFormatEnricher, create_enricher};

let mut executor = Executor::new();
executor.add_enricher(Box::new(PipelineFormatEnricher));

// Provider-specific enricher from metadata
if let Some(enricher) = create_enricher(&ctx.provider, ctx.metadata.as_ref()) {
    executor.add_enricher(enricher);
}
```

## Supported Tools

| Category | Tools |
|----------|-------|
| Issues | `get_issues`, `get_issue`, `get_issue_comments`, `create_issue`, `update_issue`, `add_issue_comment` |
| Merge Requests | `get_merge_requests`, `get_merge_request`, `get_merge_request_discussions`, `get_merge_request_diffs`, `create_merge_request`, `create_merge_request_comment` |

## Provider Factory

`factory::create_provider()` creates a provider from `ProviderConfig`. Provider instances are cheap and stateless — they hold a `reqwest::Client` and connection parameters, with no persistent connections.

`factory::create_enricher()` creates the matching enricher:
- GitLab/GitHub: static enrichers (no metadata needed)
- ClickUp/Jira: dynamic enrichers (require metadata for enum population)
