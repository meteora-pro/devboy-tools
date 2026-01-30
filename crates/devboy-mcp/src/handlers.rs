//! Tool handlers for MCP server.
//!
//! This module implements the actual tool execution logic,
//! calling providers and transforming output through the pipeline.

use std::sync::Arc;

use devboy_core::{IssueFilter, IssueProvider, MrFilter, Provider};
use devboy_pipeline::{OutputFormat, Pipeline, PipelineConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{ToolCallResult, ToolDefinition};

/// Helper to get provider name without ambiguity.
fn get_provider_name(provider: &dyn Provider) -> &'static str {
    IssueProvider::provider_name(provider)
}

/// Tool handler that executes tools using providers.
pub struct ToolHandler {
    providers: Vec<Arc<dyn Provider>>,
    pipeline_config: PipelineConfig,
}

impl ToolHandler {
    /// Create a new tool handler with providers.
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        Self {
            providers,
            pipeline_config: PipelineConfig::default(),
        }
    }

    /// Create with custom pipeline configuration.
    pub fn with_pipeline_config(mut self, config: PipelineConfig) -> Self {
        self.pipeline_config = config;
        self
    }

    /// Get available tool definitions.
    pub fn available_tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "get_issues".to_string(),
                description: "Get issues from configured git providers (GitLab, GitHub)"
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "state": {
                            "type": "string",
                            "enum": ["open", "closed", "all"],
                            "description": "Filter by issue state (default: open)"
                        },
                        "search": {
                            "type": "string",
                            "description": "Search query for title and description"
                        },
                        "labels": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Filter by label names"
                        },
                        "assignee": {
                            "type": "string",
                            "description": "Filter by assignee username"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results (default: 20)",
                            "minimum": 1,
                            "maximum": 100
                        },
                        "offset": {
                            "type": "integer",
                            "description": "Number of results to skip for pagination (default: 0)",
                            "minimum": 0
                        },
                        "format": {
                            "type": "string",
                            "enum": ["markdown", "compact", "json"],
                            "description": "Output format (default: markdown)"
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "get_merge_requests".to_string(),
                description: "Get merge requests / pull requests from configured git providers"
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "state": {
                            "type": "string",
                            "enum": ["open", "closed", "merged", "all"],
                            "description": "Filter by MR/PR state (default: open)"
                        },
                        "author": {
                            "type": "string",
                            "description": "Filter by author username"
                        },
                        "labels": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Filter by label names"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results (default: 20)",
                            "minimum": 1,
                            "maximum": 100
                        },
                        "offset": {
                            "type": "integer",
                            "description": "Number of results to skip for pagination (default: 0)",
                            "minimum": 0
                        },
                        "format": {
                            "type": "string",
                            "enum": ["markdown", "compact", "json"],
                            "description": "Output format (default: markdown)"
                        }
                    }
                }),
            },
        ]
    }

    /// Execute a tool by name with arguments.
    pub async fn execute(&self, name: &str, arguments: Option<Value>) -> ToolCallResult {
        match name {
            "get_issues" => self.handle_get_issues(arguments).await,
            "get_merge_requests" => self.handle_get_merge_requests(arguments).await,
            _ => ToolCallResult::error(format!("Unknown tool: {}", name)),
        }
    }

    /// Handle get_issues tool call.
    async fn handle_get_issues(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: GetIssuesParams = arguments
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        let filter = IssueFilter {
            state: params.state,
            search: params.search,
            labels: params.labels,
            assignee: params.assignee,
            limit: Some(params.limit.unwrap_or(20) as u32),
            offset: Some(params.offset.unwrap_or(0) as u32),
            ..Default::default()
        };

        // Collect issues from all providers
        let mut all_issues = Vec::new();
        let mut errors = Vec::new();

        for provider in &self.providers {
            match provider.get_issues(filter.clone()).await {
                Ok(issues) => {
                    tracing::debug!(
                        "Got {} issues from {}",
                        issues.len(),
                        get_provider_name(provider.as_ref())
                    );
                    all_issues.extend(issues);
                }
                Err(e) => {
                    let name = get_provider_name(provider.as_ref());
                    tracing::warn!("Error from {}: {}", name, e);
                    errors.push(format!("{}: {}", name, e));
                }
            }
        }

        if all_issues.is_empty() && !errors.is_empty() {
            return ToolCallResult::error(format!("Failed to get issues: {}", errors.join(", ")));
        }

        // Transform through pipeline
        let pipeline = self.create_pipeline(&params.format);
        match pipeline.transform_issues(all_issues) {
            Ok(output) => ToolCallResult::text(output.to_string_with_hints()),
            Err(e) => ToolCallResult::error(format!("Pipeline error: {}", e)),
        }
    }

    /// Handle get_merge_requests tool call.
    async fn handle_get_merge_requests(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: GetMergeRequestsParams = arguments
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        let filter = MrFilter {
            state: params.state,
            author: params.author,
            labels: params.labels,
            limit: Some(params.limit.unwrap_or(20) as u32),
            ..Default::default()
        };

        // Collect MRs from all providers
        let mut all_mrs = Vec::new();
        let mut errors = Vec::new();

        for provider in &self.providers {
            match provider.get_merge_requests(filter.clone()).await {
                Ok(mrs) => {
                    tracing::debug!(
                        "Got {} MRs from {}",
                        mrs.len(),
                        get_provider_name(provider.as_ref())
                    );
                    all_mrs.extend(mrs);
                }
                Err(e) => {
                    let name = get_provider_name(provider.as_ref());
                    tracing::warn!("Error from {}: {}", name, e);
                    errors.push(format!("{}: {}", name, e));
                }
            }
        }

        if all_mrs.is_empty() && !errors.is_empty() {
            return ToolCallResult::error(format!(
                "Failed to get merge requests: {}",
                errors.join(", ")
            ));
        }

        // Transform through pipeline
        let pipeline = self.create_pipeline(&params.format);
        match pipeline.transform_merge_requests(all_mrs) {
            Ok(output) => ToolCallResult::text(output.to_string_with_hints()),
            Err(e) => ToolCallResult::error(format!("Pipeline error: {}", e)),
        }
    }

    /// Create a pipeline with the specified format.
    fn create_pipeline(&self, format: &Option<String>) -> Pipeline {
        let output_format = match format.as_deref() {
            Some("json") => OutputFormat::Json,
            Some("compact") => OutputFormat::Compact,
            _ => OutputFormat::Markdown,
        };

        Pipeline::with_config(PipelineConfig {
            format: output_format,
            ..self.pipeline_config.clone()
        })
    }
}

/// Parameters for get_issues tool.
#[derive(Debug, Default, Serialize, Deserialize)]
struct GetIssuesParams {
    state: Option<String>,
    search: Option<String>,
    labels: Option<Vec<String>>,
    assignee: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    format: Option<String>,
}

/// Parameters for get_merge_requests tool.
#[derive(Debug, Default, Serialize, Deserialize)]
struct GetMergeRequestsParams {
    state: Option<String>,
    author: Option<String>,
    labels: Option<Vec<String>>,
    limit: Option<usize>,
    offset: Option<usize>,
    format: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use devboy_core::{
        Comment, CreateCommentInput, CreateIssueInput, Discussion, FileDiff, Issue, MergeRequest,
        MergeRequestProvider, UpdateIssueInput, User,
    };

    /// Mock provider for testing.
    struct MockProvider {
        issues: Vec<Issue>,
        mrs: Vec<MergeRequest>,
    }

    impl MockProvider {
        fn new() -> Self {
            Self {
                issues: vec![Issue {
                    key: "gh#1".to_string(),
                    title: "Test Issue".to_string(),
                    description: Some("Test description".to_string()),
                    state: "open".to_string(),
                    source: "github".to_string(),
                    priority: None,
                    labels: vec!["bug".to_string()],
                    author: None,
                    assignees: vec![],
                    url: Some("https://github.com/test/repo/issues/1".to_string()),
                    created_at: Some("2024-01-01T00:00:00Z".to_string()),
                    updated_at: Some("2024-01-02T00:00:00Z".to_string()),
                }],
                mrs: vec![MergeRequest {
                    key: "pr#1".to_string(),
                    title: "Test PR".to_string(),
                    description: Some("Test PR description".to_string()),
                    state: "open".to_string(),
                    source: "github".to_string(),
                    source_branch: "feature".to_string(),
                    target_branch: "main".to_string(),
                    author: None,
                    assignees: vec![],
                    reviewers: vec![],
                    labels: vec![],
                    url: Some("https://github.com/test/repo/pull/1".to_string()),
                    created_at: Some("2024-01-01T00:00:00Z".to_string()),
                    updated_at: Some("2024-01-02T00:00:00Z".to_string()),
                    draft: false,
                }],
            }
        }
    }

    #[async_trait]
    impl IssueProvider for MockProvider {
        async fn get_issues(&self, _filter: IssueFilter) -> devboy_core::Result<Vec<Issue>> {
            Ok(self.issues.clone())
        }

        async fn get_issue(&self, _key: &str) -> devboy_core::Result<Issue> {
            Ok(self.issues[0].clone())
        }

        async fn create_issue(&self, _input: CreateIssueInput) -> devboy_core::Result<Issue> {
            Ok(self.issues[0].clone())
        }

        async fn update_issue(
            &self,
            _key: &str,
            _input: UpdateIssueInput,
        ) -> devboy_core::Result<Issue> {
            Ok(self.issues[0].clone())
        }

        async fn get_comments(&self, _issue_key: &str) -> devboy_core::Result<Vec<Comment>> {
            Ok(vec![])
        }

        async fn add_comment(&self, _issue_key: &str, _body: &str) -> devboy_core::Result<Comment> {
            Ok(Comment {
                id: "1".to_string(),
                body: "test".to_string(),
                author: None,
                created_at: None,
                updated_at: None,
                position: None,
            })
        }

        fn provider_name(&self) -> &'static str {
            "mock"
        }
    }

    #[async_trait]
    impl MergeRequestProvider for MockProvider {
        async fn get_merge_requests(
            &self,
            _filter: MrFilter,
        ) -> devboy_core::Result<Vec<MergeRequest>> {
            Ok(self.mrs.clone())
        }

        async fn get_merge_request(&self, _key: &str) -> devboy_core::Result<MergeRequest> {
            Ok(self.mrs[0].clone())
        }

        async fn get_discussions(&self, _mr_key: &str) -> devboy_core::Result<Vec<Discussion>> {
            Ok(vec![])
        }

        async fn get_diffs(&self, _mr_key: &str) -> devboy_core::Result<Vec<FileDiff>> {
            Ok(vec![])
        }

        async fn add_comment(
            &self,
            _mr_key: &str,
            _input: CreateCommentInput,
        ) -> devboy_core::Result<Comment> {
            Ok(Comment {
                id: "1".to_string(),
                body: "test".to_string(),
                author: None,
                created_at: None,
                updated_at: None,
                position: None,
            })
        }

        fn provider_name(&self) -> &'static str {
            "mock"
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn get_current_user(&self) -> devboy_core::Result<User> {
            Ok(User {
                id: "1".to_string(),
                username: "test".to_string(),
                name: Some("Test User".to_string()),
                email: None,
                avatar_url: None,
            })
        }
    }

    #[tokio::test]
    async fn test_get_issues_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let result = handler.execute("get_issues", None).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("gh#1"));
        assert!(content.contains("Test Issue"));
    }

    #[tokio::test]
    async fn test_get_merge_requests_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let result = handler.execute("get_merge_requests", None).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("pr#1"));
        assert!(content.contains("Test PR"));
    }

    #[tokio::test]
    async fn test_unknown_tool() {
        let handler = ToolHandler::new(vec![]);
        let result = handler.execute("unknown_tool", None).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_no_providers() {
        let handler = ToolHandler::new(vec![]);
        let result = handler.execute("get_issues", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("No providers configured"));
    }
}
