//! Integration tests for MCP tool filtering based on provider configuration.
//!
//! These tests verify that the MCP server correctly filters tools based on
//! which providers are configured:
//! - No providers: only context management tools available
//! - Issue-only providers (ClickUp, Jira): issue tools + context tools
//! - Git providers (GitHub, GitLab): issue tools + MR tools + context tools

use std::sync::Arc;

use async_trait::async_trait;
use devboy_core::{
    Comment, CreateCommentInput, CreateIssueInput, Discussion, FileDiff, Issue, IssueFilter,
    IssueProvider, MergeRequest, MergeRequestProvider, MrFilter, Provider, Result,
    UpdateIssueInput, User,
};
use devboy_mcp::McpServer;

// =============================================================================
// Test Providers
// =============================================================================

/// ClickUp-like provider that only supports issues, not merge requests.
struct ClickUpTestProvider;

#[async_trait]
impl IssueProvider for ClickUpTestProvider {
    async fn get_issues(&self, _filter: IssueFilter) -> Result<devboy_core::ProviderResult<Issue>> {
        Ok(vec![Issue {
            key: "CU-123".to_string(),
            title: "Test Issue".to_string(),
            description: None,
            state: "open".to_string(),
            source: "clickup".to_string(),
            labels: vec![],
            assignees: vec![],
            author: None,
            priority: None,
            url: None,
            created_at: None,
            updated_at: None,
            parent: None,
            subtasks: vec![],
        }]
        .into())
    }

    async fn get_issue(&self, _key: &str) -> Result<Issue> {
        Err(devboy_core::Error::NotFound("not implemented".into()))
    }

    async fn create_issue(&self, _input: CreateIssueInput) -> Result<Issue> {
        Err(devboy_core::Error::NotFound("not implemented".into()))
    }

    async fn update_issue(&self, _key: &str, _input: UpdateIssueInput) -> Result<Issue> {
        Err(devboy_core::Error::NotFound("not implemented".into()))
    }

    async fn get_comments(&self, _issue_key: &str) -> Result<devboy_core::ProviderResult<Comment>> {
        Ok(vec![].into())
    }

    async fn add_comment(&self, _issue_key: &str, _body: &str) -> Result<Comment> {
        Err(devboy_core::Error::NotFound("not implemented".into()))
    }

    fn provider_name(&self) -> &'static str {
        "clickup"
    }
}

#[async_trait]
impl MergeRequestProvider for ClickUpTestProvider {
    fn provider_name(&self) -> &'static str {
        "clickup"
    }
    // Default implementations return ProviderUnsupported
}

#[async_trait]
impl devboy_core::PipelineProvider for ClickUpTestProvider {
    fn provider_name(&self) -> &'static str {
        "test"
    }
}

#[async_trait]
impl Provider for ClickUpTestProvider {
    async fn get_current_user(&self) -> Result<User> {
        Ok(User {
            id: "1".to_string(),
            username: "clickup-user".to_string(),
            name: None,
            email: None,
            avatar_url: None,
        })
    }
}

/// GitLab-like provider that supports both issues and merge requests.
struct GitLabTestProvider;

#[async_trait]
impl IssueProvider for GitLabTestProvider {
    async fn get_issues(&self, _filter: IssueFilter) -> Result<devboy_core::ProviderResult<Issue>> {
        Ok(vec![Issue {
            key: "gitlab#123".to_string(),
            title: "Test Issue".to_string(),
            description: None,
            state: "opened".to_string(),
            source: "gitlab".to_string(),
            labels: vec![],
            assignees: vec![],
            author: None,
            priority: None,
            url: None,
            created_at: None,
            updated_at: None,
            parent: None,
            subtasks: vec![],
        }]
        .into())
    }

    async fn get_issue(&self, _key: &str) -> Result<Issue> {
        Err(devboy_core::Error::NotFound("not implemented".into()))
    }

    async fn create_issue(&self, _input: CreateIssueInput) -> Result<Issue> {
        Err(devboy_core::Error::NotFound("not implemented".into()))
    }

    async fn update_issue(&self, _key: &str, _input: UpdateIssueInput) -> Result<Issue> {
        Err(devboy_core::Error::NotFound("not implemented".into()))
    }

    async fn get_comments(&self, _issue_key: &str) -> Result<devboy_core::ProviderResult<Comment>> {
        Ok(vec![].into())
    }

    async fn add_comment(&self, _issue_key: &str, _body: &str) -> Result<Comment> {
        Err(devboy_core::Error::NotFound("not implemented".into()))
    }

    fn provider_name(&self) -> &'static str {
        "gitlab"
    }
}

#[async_trait]
impl MergeRequestProvider for GitLabTestProvider {
    async fn get_merge_requests(
        &self,
        _filter: MrFilter,
    ) -> Result<devboy_core::ProviderResult<MergeRequest>> {
        Ok(vec![MergeRequest {
            key: "mr#123".to_string(),
            title: "Test MR".to_string(),
            description: None,
            state: "opened".to_string(),
            source: "gitlab".to_string(),
            source_branch: "feature".to_string(),
            target_branch: "main".to_string(),
            author: None,
            assignees: vec![],
            reviewers: vec![],
            labels: vec![],
            draft: false,
            url: None,
            created_at: None,
            updated_at: None,
        }]
        .into())
    }

    async fn get_merge_request(&self, _key: &str) -> Result<MergeRequest> {
        Err(devboy_core::Error::NotFound("not implemented".into()))
    }

    async fn get_discussions(
        &self,
        _mr_key: &str,
    ) -> Result<devboy_core::ProviderResult<Discussion>> {
        Ok(vec![].into())
    }

    async fn get_diffs(&self, _mr_key: &str) -> Result<devboy_core::ProviderResult<FileDiff>> {
        Ok(vec![].into())
    }

    async fn add_comment(&self, _mr_key: &str, _input: CreateCommentInput) -> Result<Comment> {
        Err(devboy_core::Error::NotFound("not implemented".into()))
    }

    fn provider_name(&self) -> &'static str {
        "gitlab"
    }
}

#[async_trait]
impl devboy_core::PipelineProvider for GitLabTestProvider {
    fn provider_name(&self) -> &'static str {
        "test"
    }
}

#[async_trait]
impl Provider for GitLabTestProvider {
    async fn get_current_user(&self) -> Result<User> {
        Ok(User {
            id: "1".to_string(),
            username: "gitlab-user".to_string(),
            name: None,
            email: None,
            avatar_url: None,
        })
    }
}

// =============================================================================
// Integration Tests
// =============================================================================

/// Context management tools that should always be available.
const CONTEXT_TOOLS: &[&str] = &["list_contexts", "use_context", "get_current_context"];

/// Issue-related tools.
const ISSUE_TOOLS: &[&str] = &[
    "get_issues",
    "get_issue",
    "create_issue",
    "update_issue",
    "get_issue_comments",
    "get_issue_relations",
    "add_issue_comment",
];

/// Merge request tools.
const MR_TOOLS: &[&str] = &[
    "get_merge_requests",
    "get_merge_request",
    "get_merge_request_discussions",
    "get_merge_request_diffs",
    "create_merge_request_comment",
];

/// Helper to get tool names from server.
fn get_tool_names(server: &McpServer) -> Vec<String> {
    use devboy_mcp::RequestId;
    use devboy_mcp::protocol::ToolsListResult;

    let resp = server.handle_tools_list(RequestId::Number(1));
    let result: ToolsListResult = serde_json::from_value(resp.result.unwrap()).unwrap();
    result.tools.into_iter().map(|t| t.name).collect()
}

#[test]
fn test_no_providers_only_context_tools() {
    let server = McpServer::new();
    let tools = get_tool_names(&server);

    // Context tools should be available
    for tool in CONTEXT_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "Expected context tool '{}' to be available",
            tool
        );
    }

    // Issue tools should NOT be available
    for tool in ISSUE_TOOLS {
        assert!(
            !tools.contains(&tool.to_string()),
            "Issue tool '{}' should not be available without providers",
            tool
        );
    }

    // MR tools should NOT be available
    for tool in MR_TOOLS {
        assert!(
            !tools.contains(&tool.to_string()),
            "MR tool '{}' should not be available without providers",
            tool
        );
    }
}

#[test]
fn test_clickup_provider_has_issue_tools_but_no_mr_tools() {
    let mut server = McpServer::new();
    server.add_provider(Arc::new(ClickUpTestProvider));
    let tools = get_tool_names(&server);

    // Context tools should be available
    for tool in CONTEXT_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "Expected context tool '{}' to be available",
            tool
        );
    }

    // Issue tools should be available
    for tool in ISSUE_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "Expected issue tool '{}' to be available with ClickUp provider",
            tool
        );
    }

    // MR tools should NOT be available (ClickUp doesn't support MRs)
    for tool in MR_TOOLS {
        assert!(
            !tools.contains(&tool.to_string()),
            "MR tool '{}' should not be available with ClickUp-only provider",
            tool
        );
    }
}

#[test]
fn test_gitlab_provider_has_all_tools() {
    let mut server = McpServer::new();
    server.add_provider(Arc::new(GitLabTestProvider));
    let tools = get_tool_names(&server);

    // Context tools should be available
    for tool in CONTEXT_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "Expected context tool '{}' to be available",
            tool
        );
    }

    // Issue tools should be available
    for tool in ISSUE_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "Expected issue tool '{}' to be available with GitLab provider",
            tool
        );
    }

    // MR tools should be available (GitLab supports MRs)
    for tool in MR_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "Expected MR tool '{}' to be available with GitLab provider",
            tool
        );
    }
}

#[test]
fn test_mixed_providers_clickup_and_gitlab() {
    let mut server = McpServer::new();
    server.add_provider(Arc::new(ClickUpTestProvider));
    server.add_provider(Arc::new(GitLabTestProvider));
    let tools = get_tool_names(&server);

    // All tools should be available when at least one Git provider is configured
    for tool in CONTEXT_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "Expected context tool '{}' to be available",
            tool
        );
    }

    for tool in ISSUE_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "Expected issue tool '{}' to be available",
            tool
        );
    }

    for tool in MR_TOOLS {
        assert!(
            tools.contains(&tool.to_string()),
            "Expected MR tool '{}' to be available with GitLab in mix",
            tool
        );
    }
}

#[test]
fn test_context_switching_affects_tool_availability() {
    let mut server = McpServer::new();

    // Default context has no providers
    server.ensure_context("clickup-only");
    server.add_provider_to_context("clickup-only", Arc::new(ClickUpTestProvider));

    server.ensure_context("gitlab-context");
    server.add_provider_to_context("gitlab-context", Arc::new(GitLabTestProvider));

    // Default context (no providers) - only context tools
    let tools = get_tool_names(&server);
    assert!(!tools.contains(&"get_issues".to_string()));
    assert!(!tools.contains(&"get_merge_requests".to_string()));

    // Switch to ClickUp context - issue tools available, no MR tools
    server.set_active_context("clickup-only").unwrap();
    let tools = get_tool_names(&server);
    assert!(tools.contains(&"get_issues".to_string()));
    assert!(!tools.contains(&"get_merge_requests".to_string()));

    // Switch to GitLab context - all tools available
    server.set_active_context("gitlab-context").unwrap();
    let tools = get_tool_names(&server);
    assert!(tools.contains(&"get_issues".to_string()));
    assert!(tools.contains(&"get_merge_requests".to_string()));
}
