//! Tool handlers for MCP server.
//!
//! This module implements the actual tool execution logic,
//! calling providers and transforming output through the pipeline.
//!
//! Tools are organized by category:
//! - **Issues**: get_issues, get_issue, get_issue_comments, create_issue, update_issue, add_issue_comment
//! - **Merge Requests**: get_merge_requests, get_merge_request, get_merge_request_discussions,
//!   get_merge_request_diffs, create_merge_request, create_merge_request_comment

use std::sync::Arc;

use devboy_core::{
    CodePosition, CreateCommentInput, CreateIssueInput, CreateMergeRequestInput, IssueFilter,
    IssueProvider, MergeRequestProvider, MrFilter, Provider, UpdateIssueInput,
};
use devboy_pipeline::{OutputFormat, Pipeline, PipelineConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{ToolCallResult, ToolDefinition};

/// Defines the complete tool registry in one place.
///
/// For each provider tool: name, description, JSON schema, and handler method.
/// Context management tools only need name (handled by McpServer, not ToolHandler).
///
/// Generates:
/// - `KNOWN_BUILTIN_TOOLS` — all tool names (provider + context)
/// - `ToolHandler::available_tools()` — tool definitions with schemas
/// - `ToolHandler::execute()` — match routing to handler methods
macro_rules! define_tools {
    (
        $(
            $name:literal => $handler:ident {
                description: $desc:literal,
                schema: $schema:tt
            }
        ),+ $(,)?
        ;
        context: $( $ctx_name:literal ),+ $(,)?
    ) => {
        /// All known built-in tool names (provider tools + context management tools).
        pub const KNOWN_BUILTIN_TOOLS: &[&str] = &[
            $( $name, )+
            $( $ctx_name, )+
        ];

        impl ToolHandler {
            /// Get available tool definitions.
            pub fn available_tools(&self) -> Vec<ToolDefinition> {
                vec![
                    $(
                        ToolDefinition {
                            name: $name.to_string(),
                            description: $desc.to_string(),
                            input_schema: serde_json::json!($schema),
                        },
                    )+
                ]
            }

            /// Execute a tool by name with arguments.
            pub async fn execute(&self, name: &str, arguments: Option<Value>) -> ToolCallResult {
                match name {
                    $( $name => self.$handler(arguments).await, )+
                    _ => ToolCallResult::error(format!("Unknown tool: {}", name)),
                }
            }
        }
    };
}

define_tools! {
    // =====================================================================
    // Issues
    // =====================================================================

    "get_issues" => handle_get_issues {
        description: "Get issues from configured providers (GitLab, GitHub, ClickUp). Returns a list of issues with filters.",
        schema: {
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
                },
                "provider": {
                    "type": "string",
                    "enum": ["github", "gitlab", "clickup", "jira"],
                    "description": "Filter by provider. If not specified, returns issues from all configured providers."
                },
                "sort_by": {
                    "type": "string",
                    "enum": ["created_at", "updated_at"],
                    "description": "Sort by field (default: updated_at)"
                },
                "sort_order": {
                    "type": "string",
                    "enum": ["asc", "desc"],
                    "description": "Sort order (default: desc)"
                }
            }
        }
    },

    "get_issue" => handle_get_issue {
        description: "Get a single issue by key (e.g., 'gh#123', 'gitlab#456', 'CU-abc', 'DEV-42', 'jira#PROJ-123'). Returns full issue details.",
        schema: {
            "type": "object",
            "required": ["key"],
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Issue key (e.g., 'gh#123' for GitHub, 'gitlab#456' for GitLab, 'CU-abc' or custom ID like 'DEV-42' for ClickUp, 'jira#PROJ-123' for Jira)"
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "compact", "json"],
                    "description": "Output format (default: markdown)"
                }
            }
        }
    },

    "get_issue_comments" => handle_get_issue_comments {
        description: "Get comments for an issue. Returns all comments with author and timestamp.",
        schema: {
            "type": "object",
            "required": ["key"],
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Issue key (e.g., 'gh#123')"
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "compact", "json"],
                    "description": "Output format (default: markdown)"
                }
            }
        }
    },

    "create_issue" => handle_create_issue {
        description: "Create a new issue in the configured provider.",
        schema: {
            "type": "object",
            "required": ["title"],
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Issue title"
                },
                "description": {
                    "type": "string",
                    "description": "Issue description/body"
                },
                "labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Labels to add"
                },
                "assignees": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Assignee usernames"
                },
                "provider": {
                    "type": "string",
                    "enum": ["github", "gitlab", "clickup", "jira"],
                    "description": "Target provider to create the issue in. If not specified, uses the first configured provider."
                }
            }
        }
    },

    "update_issue" => handle_update_issue {
        description: "Update an existing issue. Only provided fields will be changed.",
        schema: {
            "type": "object",
            "required": ["key"],
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Issue key (e.g., 'gh#123')"
                },
                "title": {
                    "type": "string",
                    "description": "New title"
                },
                "description": {
                    "type": "string",
                    "description": "New description"
                },
                "state": {
                    "type": "string",
                    "enum": ["open", "closed"],
                    "description": "New state"
                },
                "labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "New labels (replaces existing)"
                },
                "assignees": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "New assignees (replaces existing)"
                }
            }
        }
    },

    "add_issue_comment" => handle_add_issue_comment {
        description: "Add a comment to an issue.",
        schema: {
            "type": "object",
            "required": ["key", "body"],
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Issue key (e.g., 'gh#123')"
                },
                "body": {
                    "type": "string",
                    "description": "Comment text"
                }
            }
        }
    },

    // =====================================================================
    // Merge Requests
    // =====================================================================

    "get_merge_requests" => handle_get_merge_requests {
        description: "Get merge requests / pull requests from configured providers.",
        schema: {
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
                "source_branch": {
                    "type": "string",
                    "description": "Filter by source branch"
                },
                "target_branch": {
                    "type": "string",
                    "description": "Filter by target branch"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results (default: 20)",
                    "minimum": 1,
                    "maximum": 100
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "compact", "json"],
                    "description": "Output format (default: markdown)"
                }
            }
        }
    },

    "get_merge_request" => handle_get_merge_request {
        description: "Get a single merge request / pull request by key (e.g., 'pr#123', 'mr#456').",
        schema: {
            "type": "object",
            "required": ["key"],
            "properties": {
                "key": {
                    "type": "string",
                    "description": "MR/PR key (e.g., 'pr#123' for GitHub, 'mr#456' for GitLab)"
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "compact", "json"],
                    "description": "Output format (default: markdown)"
                }
            }
        }
    },

    "get_merge_request_discussions" => handle_get_merge_request_discussions {
        description: "Get discussions/review comments for a merge request. Includes code review threads with positions.",
        schema: {
            "type": "object",
            "required": ["key"],
            "properties": {
                "key": {
                    "type": "string",
                    "description": "MR/PR key (e.g., 'pr#123')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of discussions to return (default: 20)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Number of discussions to skip for pagination (default: 0)"
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "compact", "json"],
                    "description": "Output format (default: markdown)"
                }
            }
        }
    },

    "get_merge_request_diffs" => handle_get_merge_request_diffs {
        description: "Get file diffs for a merge request. Shows changed files with additions/deletions.",
        schema: {
            "type": "object",
            "required": ["key"],
            "properties": {
                "key": {
                    "type": "string",
                    "description": "MR/PR key (e.g., 'pr#123')"
                },
                "format": {
                    "type": "string",
                    "enum": ["markdown", "compact", "json"],
                    "description": "Output format (default: markdown)"
                }
            }
        }
    },

    "create_merge_request" => handle_create_merge_request {
        description: "Create a new merge request (GitLab) or pull request (GitHub).",
        schema: {
            "type": "object",
            "required": ["title", "source_branch", "target_branch"],
            "properties": {
                "title": {
                    "type": "string",
                    "description": "MR/PR title"
                },
                "description": {
                    "type": "string",
                    "description": "MR/PR description/body"
                },
                "source_branch": {
                    "type": "string",
                    "description": "Source branch (head branch with changes)"
                },
                "target_branch": {
                    "type": "string",
                    "description": "Target branch (base branch to merge into)"
                },
                "draft": {
                    "type": "boolean",
                    "description": "Create as draft/WIP (default: false)"
                },
                "labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Labels to add"
                },
                "reviewers": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Reviewer usernames"
                },
                "provider": {
                    "type": "string",
                    "enum": ["github", "gitlab"],
                    "description": "Target provider. If not specified, uses the first configured provider."
                }
            }
        }
    },

    "create_merge_request_comment" => handle_create_merge_request_comment {
        description: "Add a comment to a merge request. Can be a general comment or an inline code review comment.",
        schema: {
            "type": "object",
            "required": ["key", "body"],
            "properties": {
                "key": {
                    "type": "string",
                    "description": "MR/PR key (e.g., 'pr#123')"
                },
                "body": {
                    "type": "string",
                    "description": "Comment text"
                },
                "file_path": {
                    "type": "string",
                    "description": "File path for inline comment (optional)"
                },
                "line": {
                    "type": "integer",
                    "description": "Line number for inline comment (required if file_path is set)"
                },
                "line_type": {
                    "type": "string",
                    "enum": ["old", "new"],
                    "description": "Line type: 'old' for deleted line, 'new' for added line (default: new)"
                },
                "commit_sha": {
                    "type": "string",
                    "description": "Commit SHA for inline comment (required for GitHub)"
                },
                "discussion_id": {
                    "type": "string",
                    "description": "Reply to existing discussion (optional)"
                }
            }
        }
    };

    // Context management (handled by McpServer, not ToolHandler)
    context: "list_contexts", "use_context", "get_current_context"
}

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

    // =========================================================================
    // ISSUES HANDLERS
    // =========================================================================

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
            sort_by: params.sort_by,
            sort_order: params.sort_order,
        };

        let mut all_issues = Vec::new();
        let mut errors = Vec::new();

        let providers: Vec<_> = if let Some(ref name) = params.provider {
            match self.find_provider_by_name(name) {
                Some(p) => vec![p],
                None => {
                    let available: Vec<_> = self
                        .providers
                        .iter()
                        .map(|p| get_provider_name(p.as_ref()))
                        .collect();
                    return ToolCallResult::error(format!(
                        "Provider '{}' not configured. Available: {}",
                        name,
                        available.join(", ")
                    ));
                }
            }
        } else {
            self.providers.iter().collect()
        };

        for provider in &providers {
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

        let pipeline = self.create_pipeline(&params.format);
        match pipeline.transform_issues(all_issues) {
            Ok(output) => ToolCallResult::text(output.to_string_with_hints()),
            Err(e) => ToolCallResult::error(format!("Pipeline error: {}", e)),
        }
    }

    async fn handle_get_issue(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: GetIssueParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => return ToolCallResult::error("Missing required parameter: key".to_string()),
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        // Try to get from appropriate provider based on key prefix
        for provider in &self.providers {
            match provider.get_issue(&params.key).await {
                Ok(issue) => {
                    let pipeline = self.create_pipeline(&params.format);
                    return match pipeline.transform_issues(vec![issue]) {
                        Ok(output) => ToolCallResult::text(output.to_string_with_hints()),
                        Err(e) => ToolCallResult::error(format!("Pipeline error: {}", e)),
                    };
                }
                Err(e) => {
                    tracing::debug!(
                        "Provider {} failed for key {}: {}",
                        get_provider_name(provider.as_ref()),
                        params.key,
                        e
                    );
                }
            }
        }

        ToolCallResult::error(format!("Issue not found: {}", params.key))
    }

    async fn handle_get_issue_comments(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: GetIssueCommentsParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => return ToolCallResult::error("Missing required parameter: key".to_string()),
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        for provider in &self.providers {
            match provider.get_comments(&params.key).await {
                Ok(comments) => {
                    let pipeline = self.create_pipeline(&params.format);
                    return match pipeline.transform_comments(comments) {
                        Ok(output) => ToolCallResult::text(output.to_string_with_hints()),
                        Err(e) => ToolCallResult::error(format!("Pipeline error: {}", e)),
                    };
                }
                Err(e) => {
                    tracing::debug!(
                        "Provider {} failed for key {}: {}",
                        get_provider_name(provider.as_ref()),
                        params.key,
                        e
                    );
                }
            }
        }

        ToolCallResult::error(format!("Issue not found: {}", params.key))
    }

    async fn handle_create_issue(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: CreateIssueParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => return ToolCallResult::error("Missing required parameter: title".to_string()),
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        let input = CreateIssueInput {
            title: params.title,
            description: params.description,
            labels: params.labels.unwrap_or_default(),
            assignees: params.assignees.unwrap_or_default(),
            priority: None,
        };

        let provider = if let Some(ref name) = params.provider {
            match self.find_provider_by_name(name) {
                Some(p) => p,
                None => {
                    let available: Vec<_> = self
                        .providers
                        .iter()
                        .map(|p| get_provider_name(p.as_ref()))
                        .collect();
                    return ToolCallResult::error(format!(
                        "Provider '{}' not configured. Available: {}",
                        name,
                        available.join(", ")
                    ));
                }
            }
        } else {
            &self.providers[0]
        };
        match provider.create_issue(input).await {
            Ok(issue) => {
                let msg = format!(
                    "Created issue {} - {}\nURL: {}",
                    issue.key,
                    issue.title,
                    issue.url.unwrap_or_default()
                );
                ToolCallResult::text(msg)
            }
            Err(e) => ToolCallResult::error(format!("Failed to create issue: {}", e)),
        }
    }

    async fn handle_update_issue(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: UpdateIssueParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => return ToolCallResult::error("Missing required parameter: key".to_string()),
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        let input = UpdateIssueInput {
            title: params.title,
            description: params.description,
            state: params.state,
            labels: params.labels,
            assignees: params.assignees,
            priority: None,
        };

        for provider in &self.providers {
            match provider.update_issue(&params.key, input.clone()).await {
                Ok(issue) => {
                    let msg = format!("Updated issue {} - {}", issue.key, issue.title);
                    return ToolCallResult::text(msg);
                }
                Err(e) => {
                    tracing::debug!(
                        "Provider {} failed for key {}: {}",
                        get_provider_name(provider.as_ref()),
                        params.key,
                        e
                    );
                }
            }
        }

        ToolCallResult::error(format!("Failed to update issue: {}", params.key))
    }

    async fn handle_add_issue_comment(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: AddIssueCommentParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => {
                return ToolCallResult::error("Missing required parameters: key, body".to_string())
            }
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        for provider in &self.providers {
            match IssueProvider::add_comment(provider.as_ref(), &params.key, &params.body).await {
                Ok(comment) => {
                    let msg = format!("Added comment {} to issue {}", comment.id, params.key);
                    return ToolCallResult::text(msg);
                }
                Err(e) => {
                    tracing::debug!(
                        "Provider {} failed for key {}: {}",
                        get_provider_name(provider.as_ref()),
                        params.key,
                        e
                    );
                }
            }
        }

        ToolCallResult::error(format!("Failed to add comment to issue: {}", params.key))
    }

    // =========================================================================
    // MERGE REQUESTS HANDLERS
    // =========================================================================

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
            source_branch: params.source_branch,
            target_branch: params.target_branch,
            limit: Some(params.limit.unwrap_or(20) as u32),
        };

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

        let pipeline = self.create_pipeline(&params.format);
        match pipeline.transform_merge_requests(all_mrs) {
            Ok(output) => ToolCallResult::text(output.to_string_with_hints()),
            Err(e) => ToolCallResult::error(format!("Pipeline error: {}", e)),
        }
    }

    async fn handle_get_merge_request(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: GetMergeRequestParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => return ToolCallResult::error("Missing required parameter: key".to_string()),
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        for provider in &self.providers {
            match provider.get_merge_request(&params.key).await {
                Ok(mr) => {
                    let pipeline = self.create_pipeline(&params.format);
                    return match pipeline.transform_merge_requests(vec![mr]) {
                        Ok(output) => ToolCallResult::text(output.to_string_with_hints()),
                        Err(e) => ToolCallResult::error(format!("Pipeline error: {}", e)),
                    };
                }
                Err(e) => {
                    tracing::debug!(
                        "Provider {} failed for key {}: {}",
                        get_provider_name(provider.as_ref()),
                        params.key,
                        e
                    );
                }
            }
        }

        ToolCallResult::error(format!("Merge request not found: {}", params.key))
    }

    async fn handle_get_merge_request_discussions(
        &self,
        arguments: Option<Value>,
    ) -> ToolCallResult {
        let params: GetMergeRequestDiscussionsParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => return ToolCallResult::error("Missing required parameter: key".to_string()),
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        for provider in &self.providers {
            match provider.get_discussions(&params.key).await {
                Ok(discussions) => {
                    let offset = params.offset.unwrap_or(0);
                    let limit = params.limit.unwrap_or(self.pipeline_config.max_items);
                    let total = discussions.len();
                    let paged_discussions: Vec<_> =
                        discussions.into_iter().skip(offset).take(limit).collect();
                    let included = paged_discussions.len();

                    let pipeline =
                        self.create_pipeline_with_max_items(&params.format, included.max(limit));
                    return match pipeline.transform_discussions(paged_discussions) {
                        Ok(mut output) => {
                            if self.pipeline_config.include_hints && offset + included < total {
                                let remaining = total - offset - included;
                                let next_offset = offset + included;
                                let start = if included == 0 { 0 } else { offset + 1 };
                                let end = offset + included;
                                let pagination_hint = format!(
                                    "📊 Showing {}-{} of {} discussions. {} more available. Use `offset={}` and `limit={}` for next page.",
                                    start, end, total, remaining, next_offset, limit
                                );

                                output.truncated = true;
                                output.total_count = Some(total);
                                output.included_count = included;
                                output.agent_hint = Some(match output.agent_hint.take() {
                                    Some(existing) => format!("{}\n{}", existing, pagination_hint),
                                    None => pagination_hint,
                                });
                            }

                            ToolCallResult::text(output.to_string_with_hints())
                        }
                        Err(e) => ToolCallResult::error(format!("Pipeline error: {}", e)),
                    };
                }
                Err(e) => {
                    tracing::debug!(
                        "Provider {} failed for key {}: {}",
                        get_provider_name(provider.as_ref()),
                        params.key,
                        e
                    );
                }
            }
        }

        ToolCallResult::error(format!("Merge request not found: {}", params.key))
    }

    async fn handle_get_merge_request_diffs(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: GetMergeRequestDiffsParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => return ToolCallResult::error("Missing required parameter: key".to_string()),
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        for provider in &self.providers {
            match provider.get_diffs(&params.key).await {
                Ok(diffs) => {
                    let pipeline = self.create_pipeline(&params.format);
                    return match pipeline.transform_diffs(diffs) {
                        Ok(output) => ToolCallResult::text(output.to_string_with_hints()),
                        Err(e) => ToolCallResult::error(format!("Pipeline error: {}", e)),
                    };
                }
                Err(e) => {
                    tracing::debug!(
                        "Provider {} failed for key {}: {}",
                        get_provider_name(provider.as_ref()),
                        params.key,
                        e
                    );
                }
            }
        }

        ToolCallResult::error(format!("Merge request not found: {}", params.key))
    }

    async fn handle_create_merge_request(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: CreateMergeRequestParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => {
                return ToolCallResult::error(
                    "Missing required parameters: title, source_branch, target_branch".to_string(),
                )
            }
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        let input = CreateMergeRequestInput {
            title: params.title,
            description: params.description,
            source_branch: params.source_branch,
            target_branch: params.target_branch,
            draft: params.draft.unwrap_or(false),
            labels: params.labels.unwrap_or_default(),
            reviewers: params.reviewers.unwrap_or_default(),
        };

        let providers: Vec<_> = if let Some(ref name) = params.provider {
            match self.find_provider_by_name(name) {
                Some(p) => vec![p],
                None => {
                    let available: Vec<_> = self
                        .providers
                        .iter()
                        .map(|p| get_provider_name(p.as_ref()))
                        .collect();
                    return ToolCallResult::error(format!(
                        "Provider '{}' not configured. Available: {}",
                        name,
                        available.join(", ")
                    ));
                }
            }
        } else {
            self.providers.iter().collect()
        };

        // Try providers in order until one succeeds (skip those that don't support MRs)
        let mut last_error = String::new();
        for provider in &providers {
            match provider.create_merge_request(input.clone()).await {
                Ok(mr) => {
                    let msg = format!(
                        "Created {} - {}\n{} -> {}\nURL: {}",
                        mr.key,
                        mr.title,
                        mr.source_branch,
                        mr.target_branch,
                        mr.url.unwrap_or_default()
                    );
                    return ToolCallResult::text(msg);
                }
                Err(e) => {
                    last_error = format!("{}: {}", get_provider_name(provider.as_ref()), e);
                }
            }
        }

        ToolCallResult::error(format!("Failed to create merge request: {}", last_error))
    }

    async fn handle_create_merge_request_comment(
        &self,
        arguments: Option<Value>,
    ) -> ToolCallResult {
        let params: CreateMergeRequestCommentParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => {
                return ToolCallResult::error("Missing required parameters: key, body".to_string())
            }
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        // Build position if file_path is provided
        let position = params.file_path.map(|file_path| CodePosition {
            file_path,
            line: params.line.unwrap_or(1),
            line_type: params.line_type.unwrap_or_else(|| "new".to_string()),
            commit_sha: params.commit_sha,
        });

        let input = CreateCommentInput {
            body: params.body,
            position,
            discussion_id: params.discussion_id,
        };

        for provider in &self.providers {
            match MergeRequestProvider::add_comment(provider.as_ref(), &params.key, input.clone())
                .await
            {
                Ok(comment) => {
                    let msg = format!("Added comment {} to {}", comment.id, params.key);
                    return ToolCallResult::text(msg);
                }
                Err(e) => {
                    tracing::debug!(
                        "Provider {} failed for key {}: {}",
                        get_provider_name(provider.as_ref()),
                        params.key,
                        e
                    );
                }
            }
        }

        ToolCallResult::error(format!(
            "Failed to add comment to merge request: {}",
            params.key
        ))
    }

    // =========================================================================
    // HELPER METHODS
    // =========================================================================

    fn find_provider_by_name(&self, name: &str) -> Option<&Arc<dyn Provider>> {
        self.providers
            .iter()
            .find(|p| get_provider_name(p.as_ref()) == name)
    }

    fn create_pipeline(&self, format: &Option<String>) -> Pipeline {
        self.create_pipeline_with_max_items(format, self.pipeline_config.max_items)
    }

    fn create_pipeline_with_max_items(
        &self,
        format: &Option<String>,
        max_items: usize,
    ) -> Pipeline {
        let output_format = match format.as_deref() {
            Some("json") => OutputFormat::Json,
            Some("compact") => OutputFormat::Compact,
            _ => OutputFormat::Markdown,
        };

        Pipeline::with_config(PipelineConfig {
            max_items,
            format: output_format,
            ..self.pipeline_config.clone()
        })
    }
}

// =============================================================================
// PARAMETER TYPES
// =============================================================================

#[derive(Debug, Default, Serialize, Deserialize)]
struct GetIssuesParams {
    state: Option<String>,
    search: Option<String>,
    labels: Option<Vec<String>>,
    assignee: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    format: Option<String>,
    provider: Option<String>,
    sort_by: Option<String>,
    sort_order: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetIssueParams {
    key: String,
    format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetIssueCommentsParams {
    key: String,
    format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateIssueParams {
    title: String,
    description: Option<String>,
    labels: Option<Vec<String>>,
    assignees: Option<Vec<String>>,
    provider: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct UpdateIssueParams {
    key: String,
    title: Option<String>,
    description: Option<String>,
    state: Option<String>,
    labels: Option<Vec<String>>,
    assignees: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AddIssueCommentParams {
    key: String,
    body: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GetMergeRequestsParams {
    state: Option<String>,
    author: Option<String>,
    labels: Option<Vec<String>>,
    source_branch: Option<String>,
    target_branch: Option<String>,
    limit: Option<usize>,
    format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetMergeRequestParams {
    key: String,
    format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetMergeRequestDiscussionsParams {
    key: String,
    limit: Option<usize>,
    offset: Option<usize>,
    format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetMergeRequestDiffsParams {
    key: String,
    format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateMergeRequestParams {
    title: String,
    description: Option<String>,
    source_branch: String,
    target_branch: String,
    draft: Option<bool>,
    labels: Option<Vec<String>>,
    reviewers: Option<Vec<String>>,
    provider: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateMergeRequestCommentParams {
    key: String,
    body: String,
    file_path: Option<String>,
    line: Option<u32>,
    line_type: Option<String>,
    commit_sha: Option<String>,
    discussion_id: Option<String>,
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use devboy_core::{
        Comment, CreateMergeRequestInput, Discussion, FileDiff, Issue, MergeRequest, User,
    };

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

    struct ManyDiscussionsProvider {
        base: MockProvider,
        discussions: Vec<Discussion>,
    }

    impl ManyDiscussionsProvider {
        fn new(count: usize) -> Self {
            let discussions = (1..=count)
                .map(|i| Discussion {
                    id: i.to_string(),
                    resolved: false,
                    resolved_by: None,
                    comments: vec![Comment {
                        id: i.to_string(),
                        body: format!("Review comment {}", i),
                        author: None,
                        created_at: None,
                        updated_at: None,
                        position: None,
                    }],
                    position: None,
                })
                .collect();

            Self {
                base: MockProvider::new(),
                discussions,
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
            Ok(vec![Comment {
                id: "1".to_string(),
                body: "Test comment".to_string(),
                author: None,
                created_at: None,
                updated_at: None,
                position: None,
            }])
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
            Ok(vec![Discussion {
                id: "1".to_string(),
                resolved: false,
                resolved_by: None,
                comments: vec![Comment {
                    id: "1".to_string(),
                    body: "Review comment".to_string(),
                    author: None,
                    created_at: None,
                    updated_at: None,
                    position: None,
                }],
                position: None,
            }])
        }

        async fn get_diffs(&self, _mr_key: &str) -> devboy_core::Result<Vec<FileDiff>> {
            Ok(vec![FileDiff {
                file_path: "src/main.rs".to_string(),
                old_path: None,
                new_file: false,
                deleted_file: false,
                renamed_file: false,
                diff: "+added line\n-removed line".to_string(),
                additions: Some(1),
                deletions: Some(1),
            }])
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

        async fn create_merge_request(
            &self,
            _input: CreateMergeRequestInput,
        ) -> devboy_core::Result<MergeRequest> {
            Ok(self.mrs[0].clone())
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

    #[async_trait]
    impl IssueProvider for ManyDiscussionsProvider {
        async fn get_issues(&self, filter: IssueFilter) -> devboy_core::Result<Vec<Issue>> {
            self.base.get_issues(filter).await
        }

        async fn get_issue(&self, key: &str) -> devboy_core::Result<Issue> {
            self.base.get_issue(key).await
        }

        async fn create_issue(&self, input: CreateIssueInput) -> devboy_core::Result<Issue> {
            self.base.create_issue(input).await
        }

        async fn update_issue(
            &self,
            key: &str,
            input: UpdateIssueInput,
        ) -> devboy_core::Result<Issue> {
            self.base.update_issue(key, input).await
        }

        async fn get_comments(&self, issue_key: &str) -> devboy_core::Result<Vec<Comment>> {
            self.base.get_comments(issue_key).await
        }

        async fn add_comment(&self, issue_key: &str, body: &str) -> devboy_core::Result<Comment> {
            IssueProvider::add_comment(&self.base, issue_key, body).await
        }

        fn provider_name(&self) -> &'static str {
            IssueProvider::provider_name(&self.base)
        }
    }

    #[async_trait]
    impl MergeRequestProvider for ManyDiscussionsProvider {
        async fn get_merge_requests(
            &self,
            filter: MrFilter,
        ) -> devboy_core::Result<Vec<MergeRequest>> {
            self.base.get_merge_requests(filter).await
        }

        async fn get_merge_request(&self, key: &str) -> devboy_core::Result<MergeRequest> {
            self.base.get_merge_request(key).await
        }

        async fn get_discussions(&self, _mr_key: &str) -> devboy_core::Result<Vec<Discussion>> {
            Ok(self.discussions.clone())
        }

        async fn get_diffs(&self, mr_key: &str) -> devboy_core::Result<Vec<FileDiff>> {
            self.base.get_diffs(mr_key).await
        }

        async fn add_comment(
            &self,
            mr_key: &str,
            input: CreateCommentInput,
        ) -> devboy_core::Result<Comment> {
            MergeRequestProvider::add_comment(&self.base, mr_key, input).await
        }

        async fn create_merge_request(
            &self,
            input: CreateMergeRequestInput,
        ) -> devboy_core::Result<MergeRequest> {
            self.base.create_merge_request(input).await
        }

        fn provider_name(&self) -> &'static str {
            MergeRequestProvider::provider_name(&self.base)
        }
    }

    #[async_trait]
    impl Provider for ManyDiscussionsProvider {
        async fn get_current_user(&self) -> devboy_core::Result<User> {
            self.base.get_current_user().await
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
    async fn test_get_issue_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "gh#1"});
        let result = handler.execute("get_issue", Some(args)).await;

        assert!(result.is_error.is_none());
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
    async fn test_get_merge_request_discussions_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "pr#1"});
        let result = handler
            .execute("get_merge_request_discussions", Some(args))
            .await;

        assert!(result.is_error.is_none());
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_handler_supports_pagination() {
        let provider = Arc::new(ManyDiscussionsProvider::new(23)) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "key": "pr#1",
            "offset": 20,
            "limit": 5,
            "format": "json"
        });
        let result = handler
            .execute("get_merge_request_discussions", Some(args))
            .await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Review comment 21"));
        assert!(content.contains("Review comment 23"));
        assert!(!content.contains("Review comment 1"));
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_handler_includes_next_page_hint() {
        let provider = Arc::new(ManyDiscussionsProvider::new(26)) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "key": "pr#1",
            "offset": 20,
            "limit": 5,
            "format": "json"
        });
        let result = handler
            .execute("get_merge_request_discussions", Some(args))
            .await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("offset=25"));
        assert!(content.contains("limit=5"));
        assert!(content.contains("Showing 21-25 of 26 discussions"));
    }

    #[tokio::test]
    async fn test_get_merge_request_diffs_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "pr#1"});
        let result = handler.execute("get_merge_request_diffs", Some(args)).await;

        assert!(result.is_error.is_none());
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

    #[tokio::test]
    async fn test_tools_count() {
        let handler = ToolHandler::new(vec![]);
        let tools = handler.available_tools();

        // 6 issue tools + 6 MR tools = 12 total
        assert_eq!(tools.len(), 12);
    }

    #[tokio::test]
    async fn test_create_issue_with_provider() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "title": "New issue",
            "provider": "mock"
        });
        let result = handler.execute("create_issue", Some(args)).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Created issue"));
    }

    #[tokio::test]
    async fn test_create_issue_with_unknown_provider() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "title": "New issue",
            "provider": "jira"
        });
        let result = handler.execute("create_issue", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Provider 'jira' not configured"));
        assert!(content.contains("mock"));
    }

    #[tokio::test]
    async fn test_get_issue_comments_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "gh#1"});
        let result = handler.execute("get_issue_comments", Some(args)).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Test comment"));
    }

    #[tokio::test]
    async fn test_get_issue_comments_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("get_issue_comments", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Missing required parameter: key"));
    }

    #[tokio::test]
    async fn test_get_issue_comments_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({"key": "gh#1"});
        let result = handler.execute("get_issue_comments", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("No providers configured"));
    }

    #[tokio::test]
    async fn test_update_issue_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "key": "gh#1",
            "title": "Updated title",
            "state": "closed"
        });
        let result = handler.execute("update_issue", Some(args)).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Updated issue"));
    }

    #[tokio::test]
    async fn test_update_issue_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("update_issue", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Missing required parameter: key"));
    }

    #[tokio::test]
    async fn test_update_issue_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({"key": "gh#1"});
        let result = handler.execute("update_issue", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_add_issue_comment_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "key": "gh#1",
            "body": "My comment"
        });
        let result = handler.execute("add_issue_comment", Some(args)).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Added comment"));
    }

    #[tokio::test]
    async fn test_add_issue_comment_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("add_issue_comment", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Missing required parameters: key, body"));
    }

    #[tokio::test]
    async fn test_add_issue_comment_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({"key": "gh#1", "body": "comment"});
        let result = handler.execute("add_issue_comment", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_merge_request_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "pr#1"});
        let result = handler.execute("get_merge_request", Some(args)).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("pr#1"));
        assert!(content.contains("Test PR"));
    }

    #[tokio::test]
    async fn test_get_merge_request_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("get_merge_request", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Missing required parameter: key"));
    }

    #[tokio::test]
    async fn test_get_merge_request_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({"key": "pr#1"});
        let result = handler.execute("get_merge_request", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_create_merge_request_comment_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "key": "pr#1",
            "body": "Looks good"
        });
        let result = handler
            .execute("create_merge_request_comment", Some(args))
            .await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Added comment"));
    }

    #[tokio::test]
    async fn test_create_merge_request_comment_inline() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "key": "pr#1",
            "body": "Fix this",
            "file_path": "src/main.rs",
            "line": 42,
            "line_type": "old",
            "commit_sha": "abc123"
        });
        let result = handler
            .execute("create_merge_request_comment", Some(args))
            .await;

        assert!(result.is_error.is_none());
    }

    #[tokio::test]
    async fn test_create_merge_request_comment_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("create_merge_request_comment", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Missing required parameters: key, body"));
    }

    #[tokio::test]
    async fn test_create_merge_request_comment_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({"key": "pr#1", "body": "comment"});
        let result = handler
            .execute("create_merge_request_comment", Some(args))
            .await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_create_merge_request_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "title": "New MR",
            "source_branch": "feature",
            "target_branch": "main"
        });
        let result = handler.execute("create_merge_request", Some(args)).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Created"));
    }

    #[tokio::test]
    async fn test_create_merge_request_with_provider() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "title": "New MR",
            "source_branch": "feature",
            "target_branch": "main",
            "provider": "mock"
        });
        let result = handler.execute("create_merge_request", Some(args)).await;

        assert!(result.is_error.is_none());
    }

    #[tokio::test]
    async fn test_create_merge_request_unknown_provider() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "title": "New MR",
            "source_branch": "feature",
            "target_branch": "main",
            "provider": "jira"
        });
        let result = handler.execute("create_merge_request", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Provider 'jira' not configured"));
    }

    #[tokio::test]
    async fn test_create_merge_request_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("create_merge_request", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Missing required parameters"));
    }

    #[tokio::test]
    async fn test_create_merge_request_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({
            "title": "New MR",
            "source_branch": "feature",
            "target_branch": "main"
        });
        let result = handler.execute("create_merge_request", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_issues_with_format_json() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"format": "json"});
        let result = handler.execute("get_issues", Some(args)).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        // JSON format should contain valid JSON
        assert!(content.contains("gh#1"));
    }

    #[tokio::test]
    async fn test_get_issues_with_format_compact() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"format": "compact"});
        let result = handler.execute("get_issues", Some(args)).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("gh#1"));
    }

    #[tokio::test]
    async fn test_create_pipeline_formats() {
        let handler = ToolHandler::new(vec![]);

        let pipeline = handler.create_pipeline(&Some("json".to_string()));
        assert!(pipeline.transform_issues(vec![]).is_ok());

        let pipeline = handler.create_pipeline(&Some("compact".to_string()));
        assert!(pipeline.transform_issues(vec![]).is_ok());

        let pipeline = handler.create_pipeline(&None);
        assert!(pipeline.transform_issues(vec![]).is_ok());
    }

    #[tokio::test]
    async fn test_with_pipeline_config() {
        let _handler = ToolHandler::new(vec![]).with_pipeline_config(PipelineConfig {
            format: OutputFormat::Compact,
            ..Default::default()
        });

        // The default format from config should be used as base
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]).with_pipeline_config(PipelineConfig {
            format: OutputFormat::Compact,
            ..Default::default()
        });

        let result = handler.execute("get_issues", None).await;
        assert!(result.is_error.is_none());
    }

    #[tokio::test]
    async fn test_create_issue_without_provider_param() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "title": "New issue"
        });
        let result = handler.execute("create_issue", Some(args)).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Created issue"));
    }

    #[tokio::test]
    async fn test_create_issue_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("create_issue", None).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_create_issue_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({"title": "New issue"});
        let result = handler.execute("create_issue", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_issue_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("get_issue", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Missing required parameter: key"));
    }

    #[tokio::test]
    async fn test_get_issue_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({"key": "gh#1"});
        let result = handler.execute("get_issue", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_merge_requests_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let result = handler.execute("get_merge_requests", None).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("get_merge_request_discussions", None).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({"key": "pr#1"});
        let result = handler
            .execute("get_merge_request_discussions", Some(args))
            .await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_merge_request_diffs_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("get_merge_request_diffs", None).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_merge_request_diffs_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({"key": "pr#1"});
        let result = handler.execute("get_merge_request_diffs", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_issue_invalid_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        // Invalid JSON structure for GetIssueParams (missing required 'key' field)
        let args = serde_json::json!({"invalid": true});
        let result = handler.execute("get_issue", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Invalid parameters"));
    }

    // =========================================================================
    // Tests with FailingProvider to cover error paths in handler loops
    // =========================================================================

    struct FailingProvider;

    #[async_trait]
    impl IssueProvider for FailingProvider {
        async fn get_issues(&self, _filter: IssueFilter) -> devboy_core::Result<Vec<Issue>> {
            Err(devboy_core::Error::Api {
                status: 500,
                message: "api error".into(),
            })
        }
        async fn get_issue(&self, _key: &str) -> devboy_core::Result<Issue> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        async fn create_issue(&self, _input: CreateIssueInput) -> devboy_core::Result<Issue> {
            Err(devboy_core::Error::Api {
                status: 500,
                message: "create failed".into(),
            })
        }
        async fn update_issue(
            &self,
            _key: &str,
            _input: UpdateIssueInput,
        ) -> devboy_core::Result<Issue> {
            Err(devboy_core::Error::Api {
                status: 500,
                message: "update failed".into(),
            })
        }
        async fn get_comments(&self, _key: &str) -> devboy_core::Result<Vec<Comment>> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        async fn add_comment(&self, _key: &str, _body: &str) -> devboy_core::Result<Comment> {
            Err(devboy_core::Error::Api {
                status: 500,
                message: "comment failed".into(),
            })
        }
        fn provider_name(&self) -> &'static str {
            "failing"
        }
    }

    #[async_trait]
    impl MergeRequestProvider for FailingProvider {
        async fn get_merge_requests(
            &self,
            _filter: MrFilter,
        ) -> devboy_core::Result<Vec<MergeRequest>> {
            Err(devboy_core::Error::Api {
                status: 500,
                message: "api error".into(),
            })
        }
        async fn get_merge_request(&self, _key: &str) -> devboy_core::Result<MergeRequest> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        async fn get_discussions(&self, _mr_key: &str) -> devboy_core::Result<Vec<Discussion>> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        async fn get_diffs(&self, _mr_key: &str) -> devboy_core::Result<Vec<FileDiff>> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        async fn add_comment(
            &self,
            _mr_key: &str,
            _input: CreateCommentInput,
        ) -> devboy_core::Result<Comment> {
            Err(devboy_core::Error::Api {
                status: 500,
                message: "comment failed".into(),
            })
        }
        async fn create_merge_request(
            &self,
            _input: CreateMergeRequestInput,
        ) -> devboy_core::Result<MergeRequest> {
            Err(devboy_core::Error::Api {
                status: 500,
                message: "create mr failed".into(),
            })
        }
        fn provider_name(&self) -> &'static str {
            "failing"
        }
    }

    #[async_trait]
    impl Provider for FailingProvider {
        async fn get_current_user(&self) -> devboy_core::Result<User> {
            Err(devboy_core::Error::Api {
                status: 401,
                message: "auth error".into(),
            })
        }
    }

    #[tokio::test]
    async fn test_get_issues_all_providers_fail() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let result = handler.execute("get_issues", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Failed to get issues"));
    }

    #[tokio::test]
    async fn test_get_issue_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "gh#1"});
        let result = handler.execute("get_issue", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Issue not found"));
    }

    #[tokio::test]
    async fn test_get_issue_comments_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "gh#1"});
        let result = handler.execute("get_issue_comments", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Issue not found"));
    }

    #[tokio::test]
    async fn test_create_issue_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"title": "New issue"});
        let result = handler.execute("create_issue", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Failed to create issue"));
    }

    #[tokio::test]
    async fn test_update_issue_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "gh#1", "title": "Updated"});
        let result = handler.execute("update_issue", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Failed to update issue"));
    }

    #[tokio::test]
    async fn test_add_issue_comment_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "gh#1", "body": "comment"});
        let result = handler.execute("add_issue_comment", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Failed to add comment to issue"));
    }

    #[tokio::test]
    async fn test_get_merge_requests_all_providers_fail() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let result = handler.execute("get_merge_requests", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Failed to get merge requests"));
    }

    #[tokio::test]
    async fn test_get_merge_request_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "pr#1"});
        let result = handler.execute("get_merge_request", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Merge request not found"));
    }

    #[tokio::test]
    async fn test_get_discussions_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "pr#1"});
        let result = handler
            .execute("get_merge_request_discussions", Some(args))
            .await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Merge request not found"));
    }

    #[tokio::test]
    async fn test_get_diffs_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "pr#1"});
        let result = handler.execute("get_merge_request_diffs", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Merge request not found"));
    }

    #[tokio::test]
    async fn test_create_mr_comment_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "pr#1", "body": "comment"});
        let result = handler
            .execute("create_merge_request_comment", Some(args))
            .await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Failed to add comment to merge request"));
    }

    #[tokio::test]
    async fn test_create_merge_request_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "title": "New MR",
            "source_branch": "feature",
            "target_branch": "main"
        });
        let result = handler.execute("create_merge_request", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Failed to create merge request"));
    }

    #[tokio::test]
    async fn test_create_issue_with_failing_named_provider() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "title": "New issue",
            "provider": "failing"
        });
        let result = handler.execute("create_issue", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Failed to create issue"));
    }
}
