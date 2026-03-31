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

/// Tool category for filtering based on provider availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    /// Tools that require an issue provider (GitLab, GitHub, ClickUp, Jira).
    Issues,
    /// Tools that require a merge request provider (GitLab, GitHub).
    MergeRequests,
    /// Tools that require a meeting notes provider (Fireflies).
    MeetingNotes,
}

use devboy_core::{
    CodePosition, CreateCommentInput, CreateIssueInput, CreateMergeRequestInput, IssueFilter,
    IssueProvider, MergeRequestProvider, MrFilter, Provider, UpdateIssueInput,
};
use devboy_format_pipeline::{OutputFormat, Pipeline, PipelineConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{ToolCallResult, ToolDefinition};

/// Defines the complete tool registry in one place.
///
/// For each provider tool: name, category, description, JSON schema, and handler method.
/// Context management tools only need name (handled by McpServer, not ToolHandler).
///
/// Generates:
/// - `KNOWN_BUILTIN_TOOLS` — all tool names (provider + context)
/// - `ToolHandler::available_tools()` — tool definitions with schemas and categories
/// - `ToolHandler::execute()` — match routing to handler methods
macro_rules! define_tools {
    (
        $(
            $name:literal => $handler:ident {
                category: $category:expr,
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
                            category: Some($category),
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
        category: ToolCategory::Issues,
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
                    "enum": ["toon", "json"],
                    "description": "Output format (default: toon)"
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
                },
                "budget": {
                    "type": "integer",
                    "description": "Token budget for this response (default: from config, typically ~28000). Lower values return less data with chunk index for navigation. Higher values return more data in one call.",
                    "minimum": 100,
                    "maximum": 100000
                }
            }
        }
    },

    "get_issue" => handle_get_issue {
        category: ToolCategory::Issues,
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
                    "enum": ["toon", "json"],
                    "description": "Output format (default: toon)"
                },
                "budget": {
                    "type": "integer",
                    "description": "Token budget for this response (default: from config, typically ~28000). Lower values return less data with chunk index for navigation. Higher values return more data in one call.",
                    "minimum": 100,
                    "maximum": 100000
                }
            }
        }
    },

    "get_issue_comments" => handle_get_issue_comments {
        category: ToolCategory::Issues,
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
                    "enum": ["toon", "json"],
                    "description": "Output format (default: toon)"
                },
                "budget": {
                    "type": "integer",
                    "description": "Token budget for this response (default: from config, typically ~28000). Lower values return less data with chunk index for navigation. Higher values return more data in one call.",
                    "minimum": 100,
                    "maximum": 100000
                }
            }
        }
    },

    "get_issue_relations" => handle_get_issue_relations {
        category: ToolCategory::Issues,
        description: "Get relations for an issue (parent, subtasks, linked issues).",
        schema: {
            "type": "object",
            "required": ["key"],
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Issue key (e.g., 'gh#123', 'gitlab#456', 'CU-abc', 'jira#PROJ-123')"
                }
            }
        }
    },

    "create_issue" => handle_create_issue {
        category: ToolCategory::Issues,
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
                "parent": {
                    "type": "string",
                    "description": "Parent issue key to create a subtask (e.g., 'CU-abc123' or 'DEV-42'). Only supported by ClickUp."
                },
                "markdown": {
                    "type": "boolean",
                    "description": "Whether the description is markdown (default: true). When true, ClickUp renders formatted text."
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
        category: ToolCategory::Issues,
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
                },
                "parentId": {
                    "type": "string",
                    "description": "Parent issue key to move task as subtask (e.g., 'CU-abc123' or 'DEV-42'). Only supported by ClickUp."
                },
                "markdown": {
                    "type": "boolean",
                    "description": "Whether the description is markdown (default: true). When true, ClickUp renders formatted text."
                }
            }
        }
    },

    "add_issue_comment" => handle_add_issue_comment {
        category: ToolCategory::Issues,
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
        category: ToolCategory::MergeRequests,
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
                    "enum": ["toon", "json"],
                    "description": "Output format (default: toon)"
                },
                "budget": {
                    "type": "integer",
                    "description": "Token budget for this response (default: from config, typically ~28000). Lower values return less data with chunk index for navigation. Higher values return more data in one call.",
                    "minimum": 100,
                    "maximum": 100000
                }
            }
        }
    },

    "get_merge_request" => handle_get_merge_request {
        category: ToolCategory::MergeRequests,
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
                    "enum": ["toon", "json"],
                    "description": "Output format (default: toon)"
                },
                "budget": {
                    "type": "integer",
                    "description": "Token budget for this response (default: from config, typically ~28000). Lower values return less data with chunk index for navigation. Higher values return more data in one call.",
                    "minimum": 100,
                    "maximum": 100000
                }
            }
        }
    },

    "get_merge_request_discussions" => handle_get_merge_request_discussions {
        category: ToolCategory::MergeRequests,
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
                    "description": "Maximum number of discussions to return (default: 20)",
                    "minimum": 1,
                    "maximum": 100
                },
                "offset": {
                    "type": "integer",
                    "description": "Number of discussions to skip for pagination (default: 0)",
                    "minimum": 0
                },
                "format": {
                    "type": "string",
                    "enum": ["toon", "json"],
                    "description": "Output format (default: toon)"
                },
                "budget": {
                    "type": "integer",
                    "description": "Token budget for this response (default: from config, typically ~28000). Lower values return less data with chunk index for navigation. Higher values return more data in one call.",
                    "minimum": 100,
                    "maximum": 100000
                }
            }
        }
    },

    "get_merge_request_diffs" => handle_get_merge_request_diffs {
        category: ToolCategory::MergeRequests,
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
                    "enum": ["toon", "json"],
                    "description": "Output format (default: toon)"
                },
                "budget": {
                    "type": "integer",
                    "description": "Token budget for this response (default: from config, typically ~28000). Lower values return less data with chunk index for navigation. Higher values return more data in one call.",
                    "minimum": 100,
                    "maximum": 100000
                }
            }
        }
    },

    "create_merge_request" => handle_create_merge_request {
        category: ToolCategory::MergeRequests,
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
        category: ToolCategory::MergeRequests,
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
    },

    // =====================================================================
    // Pipeline / CI
    // =====================================================================

    "get_pipeline" => handle_get_pipeline {
        category: ToolCategory::MergeRequests,
        description: "Get CI/CD pipeline status for a branch or MR/PR. Returns job statuses grouped by stage/workflow with smart error extraction for failed jobs.",
        schema: {
            "type": "object",
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Branch name (e.g., 'main', 'feat/DEV-123'). If neither branch nor mrKey provided, uses default branch."
                },
                "mrKey": {
                    "type": "string",
                    "description": "MR/PR key (e.g., 'mr#123', 'pr#456'). Takes priority over branch."
                },
                "includeFailedLogs": {
                    "type": "boolean",
                    "description": "Include smart error extraction for failed jobs (default: true)"
                }
            }
        }
    },

    "get_job_logs" => handle_get_job_logs {
        category: ToolCategory::MergeRequests,
        description: "Get detailed CI/CD job logs. Modes: smart (auto error extraction), search (pattern matching), paginated (line range), full (entire log).",
        schema: {
            "type": "object",
            "required": ["jobId"],
            "properties": {
                "jobId": {
                    "type": "string",
                    "description": "Job ID from get_pipeline response"
                },
                "pattern": {
                    "type": "string",
                    "description": "Regex/keyword to search in logs. Returns matches with context."
                },
                "context": {
                    "type": "integer",
                    "description": "Lines of context around each search match (default: 5)"
                },
                "maxMatches": {
                    "type": "integer",
                    "description": "Maximum number of search results (default: 20)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Start line number for paginated browsing"
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of lines to return (default: 200, max: 1000)"
                },
                "full": {
                    "type": "boolean",
                    "description": "Return entire log (can be very large)"
                }
            }
        }
    },

    // =====================================================================
    // Meeting Notes
    // =====================================================================

    "get_meeting_notes" => handle_get_meeting_notes {
        category: ToolCategory::MeetingNotes,
        description: "Get meeting notes and transcripts with optional filters (date range, participants, host).",
        schema: {
            "type": "object",
            "properties": {
                "from_date": {
                    "type": "string",
                    "description": "Filter from date (ISO 8601, e.g., '2025-01-01T00:00:00Z')"
                },
                "to_date": {
                    "type": "string",
                    "description": "Filter to date (ISO 8601)"
                },
                "participants": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Filter by participant email addresses"
                },
                "host_email": {
                    "type": "string",
                    "description": "Filter by host email"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results (default: 50)",
                    "minimum": 1,
                    "maximum": 50
                },
                "offset": {
                    "type": "integer",
                    "description": "Number of results to skip (default: 0)",
                    "minimum": 0
                }
            }
        }
    },

    "get_meeting_transcript" => handle_get_meeting_transcript {
        category: ToolCategory::MeetingNotes,
        description: "Get the full transcript for a meeting. Returns speaker-attributed sentences with timestamps.",
        schema: {
            "type": "object",
            "properties": {
                "meeting_id": {
                    "type": "string",
                    "description": "Meeting ID from get_meeting_notes"
                }
            },
            "required": ["meeting_id"]
        }
    },

    "search_meeting_notes" => handle_search_meeting_notes {
        category: ToolCategory::MeetingNotes,
        description: "Search across meetings by keywords, topics, or action items.",
        schema: {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "from_date": {
                    "type": "string",
                    "description": "Filter from date (ISO 8601)"
                },
                "to_date": {
                    "type": "string",
                    "description": "Filter to date (ISO 8601)"
                },
                "participants": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Filter by participant email addresses"
                },
                "host_email": {
                    "type": "string",
                    "description": "Filter by host email"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results (default: 50)",
                    "minimum": 1,
                    "maximum": 50
                },
                "offset": {
                    "type": "integer",
                    "description": "Number of results to skip (default: 0)",
                    "minimum": 0
                }
            },
            "required": ["query"]
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
    meeting_providers: Vec<Arc<dyn devboy_core::MeetingNotesProvider>>,
    pipeline_config: PipelineConfig,
}

impl ToolHandler {
    /// Create a new tool handler with providers.
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        Self {
            providers,
            meeting_providers: Vec::new(),
            pipeline_config: PipelineConfig::default(),
        }
    }

    /// Add meeting notes providers (e.g., Fireflies).
    pub fn with_meeting_providers(
        mut self,
        providers: Vec<Arc<dyn devboy_core::MeetingNotesProvider>>,
    ) -> Self {
        self.meeting_providers = providers;
        self
    }

    /// Create with custom pipeline configuration.
    pub fn with_pipeline_config(mut self, config: PipelineConfig) -> Self {
        self.pipeline_config = config;
        self
    }

    /// Check if meeting notes providers are configured.
    pub fn has_meeting_providers(&self) -> bool {
        !self.meeting_providers.is_empty()
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
                Ok(result) => {
                    tracing::debug!(
                        "Got {} issues from {}",
                        result.items.len(),
                        get_provider_name(provider.as_ref())
                    );
                    all_issues.extend(result.items);
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

        let pipeline = self.create_pipeline(&params.format, params.budget);
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
                    let pipeline = self.create_pipeline(&params.format, params.budget);
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
                Ok(result) => {
                    let pipeline = self.create_pipeline(&params.format, params.budget);
                    return match pipeline.transform_comments(result.items) {
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

    async fn handle_get_issue_relations(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: GetIssueRelationsParams = match arguments {
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
            match provider.get_issue_relations(&params.key).await {
                Ok(relations) => {
                    let json = match serde_json::to_string_pretty(&relations) {
                        Ok(j) => j,
                        Err(e) => {
                            return ToolCallResult::error(format!(
                                "Failed to serialize relations: {}",
                                e
                            ));
                        }
                    };
                    return ToolCallResult::text(json);
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
            parent: params.parent,
            markdown: params.markdown.unwrap_or(true),
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
            parent_id: params.parent_id,
            markdown: params.markdown.unwrap_or(true),
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
                return ToolCallResult::error("Missing required parameters: key, body".to_string());
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
            ..Default::default()
        };

        let mut all_mrs = Vec::new();
        let mut errors = Vec::new();

        for provider in &self.providers {
            match provider.get_merge_requests(filter.clone()).await {
                Ok(result) => {
                    tracing::debug!(
                        "Got {} MRs from {}",
                        result.items.len(),
                        get_provider_name(provider.as_ref())
                    );
                    all_mrs.extend(result.items);
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

        let pipeline = self.create_pipeline(&params.format, params.budget);
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
                    let pipeline = self.create_pipeline(&params.format, params.budget);
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

        if let Some(limit) = params.limit
            && (limit == 0 || limit > 100)
        {
            return ToolCallResult::error(
                "Invalid parameters: limit must be between 1 and 100".to_string(),
            );
        }

        for provider in &self.providers {
            match provider.get_discussions(&params.key).await {
                Ok(result) => {
                    let discussions = result.items;
                    let offset = params.offset.unwrap_or(0);
                    let limit = params.limit.unwrap_or(20);
                    let total = discussions.len();
                    let paged_discussions: Vec<_> =
                        discussions.into_iter().skip(offset).take(limit).collect();
                    let included = paged_discussions.len();

                    let pipeline = self.create_pipeline(&params.format, params.budget);
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
                Ok(result) => {
                    let pipeline = self.create_pipeline(&params.format, params.budget);
                    return match pipeline.transform_diffs(result.items) {
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
                );
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
                return ToolCallResult::error("Missing required parameters: key, body".to_string());
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
    // PIPELINE HANDLERS
    // =========================================================================

    async fn handle_get_pipeline(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: GetPipelineParams = arguments
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        let input = devboy_core::GetPipelineInput {
            branch: params.branch,
            mr_key: params.mr_key,
            include_failed_logs: params.include_failed_logs.unwrap_or(true),
        };

        for provider in &self.providers {
            match devboy_core::PipelineProvider::get_pipeline(provider.as_ref(), input.clone())
                .await
            {
                Ok(info) => {
                    let output = devboy_executor::ToolOutput::Pipeline(Box::new(info));
                    return match devboy_executor::format_output(
                        output,
                        params.format.as_deref(),
                        Some("get_pipeline"),
                        None,
                    ) {
                        Ok(result) => ToolCallResult::text(result.content),
                        Err(e) => ToolCallResult::error(format!("Format error: {}", e)),
                    };
                }
                Err(e) => {
                    tracing::debug!(
                        "Provider {} failed: {}",
                        get_provider_name(provider.as_ref()),
                        e
                    );
                }
            }
        }

        ToolCallResult::error("No pipeline found".to_string())
    }

    async fn handle_get_job_logs(&self, arguments: Option<Value>) -> ToolCallResult {
        let params: GetJobLogsParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid parameters: {}", e)),
            },
            None => return ToolCallResult::error("Missing required parameter: jobId".to_string()),
        };

        if self.providers.is_empty() {
            return ToolCallResult::error("No providers configured".to_string());
        }

        let mode = if let Some(ref pattern) = params.pattern {
            devboy_core::JobLogMode::Search {
                pattern: pattern.clone(),
                context: params.context.unwrap_or(5),
                max_matches: params.max_matches.unwrap_or(20),
            }
        } else if params.full.unwrap_or(false) {
            devboy_core::JobLogMode::Full {
                max_lines: params.limit.unwrap_or(10000),
            }
        } else if params.offset.is_some() || params.limit.is_some() {
            devboy_core::JobLogMode::Paginated {
                offset: params.offset.unwrap_or(0),
                limit: params.limit.unwrap_or(200),
            }
        } else {
            devboy_core::JobLogMode::Smart
        };

        let options = devboy_core::JobLogOptions { mode };

        for provider in &self.providers {
            match devboy_core::PipelineProvider::get_job_logs(
                provider.as_ref(),
                &params.job_id,
                options.clone(),
            )
            .await
            {
                Ok(log) => {
                    let output = devboy_executor::ToolOutput::JobLog(Box::new(log));
                    return match devboy_executor::format_output(
                        output,
                        None,
                        Some("get_job_logs"),
                        None,
                    ) {
                        Ok(result) => ToolCallResult::text(result.content),
                        Err(e) => ToolCallResult::error(format!("Format error: {}", e)),
                    };
                }
                Err(e) => {
                    tracing::debug!(
                        "Provider {} failed: {}",
                        get_provider_name(provider.as_ref()),
                        e
                    );
                }
            }
        }

        ToolCallResult::error(format!("Job logs not found: {}", params.job_id))
    }

    // =========================================================================
    // MEETING NOTES HANDLERS
    // =========================================================================

    async fn handle_get_meeting_notes(&self, arguments: Option<Value>) -> ToolCallResult {
        if self.meeting_providers.is_empty() {
            return ToolCallResult::error("No meeting notes providers configured".to_string());
        }

        let params: GetMeetingNotesParams = arguments
            .map(|v| serde_json::from_value(v).unwrap_or_default())
            .unwrap_or_default();

        let filter = devboy_core::MeetingFilter {
            keyword: None,
            from_date: params.from_date,
            to_date: params.to_date,
            participants: params.participants,
            host_email: params.host_email,
            limit: params.limit,
            skip: params.offset,
        };

        let mut last_error: Option<String> = None;
        for provider in &self.meeting_providers {
            match provider.get_meetings(filter.clone()).await {
                Ok(result) => {
                    let meta = devboy_executor::ResultMeta {
                        pagination: result.pagination,
                        sort_info: result.sort_info,
                    };
                    let output =
                        devboy_executor::ToolOutput::MeetingNotes(result.items, Some(meta));
                    return match devboy_executor::format_output(
                        output,
                        None,
                        Some("get_meeting_notes"),
                        None,
                    ) {
                        Ok(result) => ToolCallResult::text(result.content),
                        Err(e) => ToolCallResult::error(format!("Format error: {e}")),
                    };
                }
                Err(e) => {
                    tracing::warn!(
                        "Meeting provider {} failed: {}",
                        provider.provider_name(),
                        e
                    );
                    last_error = Some(format!("{}: {}", provider.provider_name(), e));
                }
            }
        }

        ToolCallResult::error(
            last_error.unwrap_or_else(|| "No meeting notes providers configured".to_string()),
        )
    }

    async fn handle_get_meeting_transcript(&self, arguments: Option<Value>) -> ToolCallResult {
        if self.meeting_providers.is_empty() {
            return ToolCallResult::error("No meeting notes providers configured".to_string());
        }

        let params: GetMeetingTranscriptParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid params: {e}")),
            },
            None => return ToolCallResult::error("meeting_id is required".to_string()),
        };

        let mut last_error: Option<String> = None;
        for provider in &self.meeting_providers {
            match provider.get_transcript(&params.meeting_id).await {
                Ok(transcript) => {
                    let output =
                        devboy_executor::ToolOutput::MeetingTranscript(Box::new(transcript));
                    return match devboy_executor::format_output(
                        output,
                        None,
                        Some("get_meeting_transcript"),
                        None,
                    ) {
                        Ok(result) => ToolCallResult::text(result.content),
                        Err(e) => ToolCallResult::error(format!("Format error: {e}")),
                    };
                }
                Err(e) => {
                    tracing::debug!(
                        "Meeting provider {} failed: {}",
                        provider.provider_name(),
                        e
                    );
                    last_error = Some(format!("{}: {}", provider.provider_name(), e));
                }
            }
        }

        ToolCallResult::error(
            last_error.unwrap_or_else(|| "No meeting notes providers configured".to_string()),
        )
    }

    async fn handle_search_meeting_notes(&self, arguments: Option<Value>) -> ToolCallResult {
        if self.meeting_providers.is_empty() {
            return ToolCallResult::error("No meeting notes providers configured".to_string());
        }

        let params: SearchMeetingNotesParams = match arguments {
            Some(v) => match serde_json::from_value(v) {
                Ok(p) => p,
                Err(e) => return ToolCallResult::error(format!("Invalid params: {e}")),
            },
            None => return ToolCallResult::error("query is required".to_string()),
        };

        let filter = devboy_core::MeetingFilter {
            from_date: params.from_date,
            to_date: params.to_date,
            participants: params.participants,
            host_email: params.host_email,
            limit: params.limit,
            skip: params.offset,
            ..Default::default()
        };

        let mut last_error: Option<String> = None;
        for provider in &self.meeting_providers {
            match provider
                .search_meetings(&params.query, filter.clone())
                .await
            {
                Ok(result) => {
                    let meta = devboy_executor::ResultMeta {
                        pagination: result.pagination,
                        sort_info: result.sort_info,
                    };
                    let output =
                        devboy_executor::ToolOutput::MeetingNotes(result.items, Some(meta));
                    return match devboy_executor::format_output(
                        output,
                        None,
                        Some("search_meeting_notes"),
                        None,
                    ) {
                        Ok(result) => ToolCallResult::text(result.content),
                        Err(e) => ToolCallResult::error(format!("Format error: {e}")),
                    };
                }
                Err(e) => {
                    tracing::debug!(
                        "Meeting provider {} failed: {}",
                        provider.provider_name(),
                        e
                    );
                    last_error = Some(format!("{}: {}", provider.provider_name(), e));
                }
            }
        }

        ToolCallResult::error(
            last_error.unwrap_or_else(|| "No meeting notes providers configured".to_string()),
        )
    }

    // =========================================================================
    // HELPER METHODS
    // =========================================================================

    fn find_provider_by_name(&self, name: &str) -> Option<&Arc<dyn Provider>> {
        self.providers
            .iter()
            .find(|p| get_provider_name(p.as_ref()) == name)
    }

    fn create_pipeline(&self, format: &Option<String>, budget: Option<usize>) -> Pipeline {
        let output_format = match format.as_deref() {
            Some("json") => OutputFormat::Json,
            _ => OutputFormat::Toon,
        };

        let mut config = PipelineConfig {
            format: output_format,
            ..self.pipeline_config.clone()
        };

        // LLM-controlled budget overrides default
        if let Some(b) = budget {
            // Convert token budget to max_chars (tokens * 3.5)
            config.max_chars = (b as f64 * 3.5).floor() as usize;
        }

        Pipeline::with_config(config)
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
    budget: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetIssueParams {
    key: String,
    format: Option<String>,
    budget: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetIssueCommentsParams {
    key: String,
    format: Option<String>,
    budget: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetIssueRelationsParams {
    key: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateIssueParams {
    title: String,
    description: Option<String>,
    labels: Option<Vec<String>>,
    assignees: Option<Vec<String>>,
    parent: Option<String>,
    markdown: Option<bool>,
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
    #[serde(rename = "parentId")]
    parent_id: Option<String>,
    markdown: Option<bool>,
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
    budget: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetMergeRequestParams {
    key: String,
    format: Option<String>,
    budget: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetMergeRequestDiscussionsParams {
    key: String,
    limit: Option<usize>,
    offset: Option<usize>,
    format: Option<String>,
    budget: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetMergeRequestDiffsParams {
    key: String,
    format: Option<String>,
    budget: Option<usize>,
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

#[derive(Debug, Default, Serialize, Deserialize)]
struct GetPipelineParams {
    branch: Option<String>,
    #[serde(rename = "mrKey")]
    mr_key: Option<String>,
    #[serde(rename = "includeFailedLogs")]
    include_failed_logs: Option<bool>,
    format: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GetJobLogsParams {
    #[serde(rename = "jobId")]
    job_id: String,
    pattern: Option<String>,
    context: Option<usize>,
    #[serde(rename = "maxMatches")]
    max_matches: Option<usize>,
    offset: Option<usize>,
    limit: Option<usize>,
    full: Option<bool>,
}

// Meeting notes params
#[derive(serde::Deserialize, Default)]
struct GetMeetingNotesParams {
    from_date: Option<String>,
    to_date: Option<String>,
    participants: Option<Vec<String>>,
    host_email: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

#[derive(serde::Deserialize)]
struct GetMeetingTranscriptParams {
    meeting_id: String,
}

#[derive(serde::Deserialize)]
struct SearchMeetingNotesParams {
    query: String,
    from_date: Option<String>,
    to_date: Option<String>,
    participants: Option<Vec<String>>,
    host_email: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use devboy_core::{
        Comment, CreateMergeRequestInput, Discussion, FileDiff, Issue, IssueLink, IssueRelations,
        MergeRequest, User,
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
                    parent: None,
                    subtasks: vec![],
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
        async fn get_issues(
            &self,
            _filter: IssueFilter,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Issue>> {
            Ok(self.issues.clone().into())
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

        async fn get_comments(
            &self,
            _issue_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Comment>> {
            Ok(vec![Comment {
                id: "1".to_string(),
                body: "Test comment".to_string(),
                author: None,
                created_at: None,
                updated_at: None,
                position: None,
            }]
            .into())
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

        async fn get_issue_relations(
            &self,
            _issue_key: &str,
        ) -> devboy_core::Result<IssueRelations> {
            Ok(IssueRelations {
                parent: Some(self.issues[0].clone()),
                subtasks: vec![self.issues[0].clone()],
                blocks: vec![IssueLink {
                    issue: self.issues[0].clone(),
                    link_type: "Blocks".to_string(),
                }],
                blocked_by: vec![],
                related_to: vec![],
                duplicates: vec![],
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
        ) -> devboy_core::Result<devboy_core::ProviderResult<MergeRequest>> {
            Ok(self.mrs.clone().into())
        }

        async fn get_merge_request(&self, _key: &str) -> devboy_core::Result<MergeRequest> {
            Ok(self.mrs[0].clone())
        }

        async fn get_discussions(
            &self,
            _mr_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Discussion>> {
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
            }]
            .into())
        }

        async fn get_diffs(
            &self,
            _mr_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<FileDiff>> {
            Ok(vec![FileDiff {
                file_path: "src/main.rs".to_string(),
                old_path: None,
                new_file: false,
                deleted_file: false,
                renamed_file: false,
                diff: "+added line\n-removed line".to_string(),
                additions: Some(1),
                deletions: Some(1),
            }]
            .into())
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
    impl devboy_core::PipelineProvider for MockProvider {
        fn provider_name(&self) -> &'static str {
            "test"
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
        async fn get_issues(
            &self,
            filter: IssueFilter,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Issue>> {
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

        async fn get_comments(
            &self,
            issue_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Comment>> {
            self.base.get_comments(issue_key).await
        }

        async fn add_comment(&self, issue_key: &str, body: &str) -> devboy_core::Result<Comment> {
            IssueProvider::add_comment(&self.base, issue_key, body).await
        }

        async fn get_issue_relations(
            &self,
            issue_key: &str,
        ) -> devboy_core::Result<IssueRelations> {
            self.base.get_issue_relations(issue_key).await
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
        ) -> devboy_core::Result<devboy_core::ProviderResult<MergeRequest>> {
            self.base.get_merge_requests(filter).await
        }

        async fn get_merge_request(&self, key: &str) -> devboy_core::Result<MergeRequest> {
            self.base.get_merge_request(key).await
        }

        async fn get_discussions(
            &self,
            _mr_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Discussion>> {
            Ok(self.discussions.clone().into())
        }

        async fn get_diffs(
            &self,
            mr_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<FileDiff>> {
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
    impl devboy_core::PipelineProvider for ManyDiscussionsProvider {
        fn provider_name(&self) -> &'static str {
            "test"
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
    async fn test_many_discussions_provider_forwards_merge_request_methods() {
        let provider = ManyDiscussionsProvider::new(2);

        let merge_requests = provider
            .get_merge_requests(MrFilter::default())
            .await
            .expect("merge requests should be forwarded")
            .items;
        assert_eq!(merge_requests.len(), 1);
        assert_eq!(merge_requests[0].key, "pr#1");

        let merge_request = provider
            .get_merge_request("pr#1")
            .await
            .expect("single merge request should be forwarded");
        assert_eq!(merge_request.key, "pr#1");

        let discussions = provider
            .get_discussions("pr#1")
            .await
            .expect("custom discussions should be returned")
            .items;
        assert_eq!(discussions.len(), 2);
        assert_eq!(discussions[0].comments[0].body, "Review comment 1");

        let diffs = provider
            .get_diffs("pr#1")
            .await
            .expect("diffs should be forwarded")
            .items;
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].file_path, "src/main.rs");
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
    async fn test_get_merge_request_discussions_handler_uses_default_pagination() {
        let provider = Arc::new(ManyDiscussionsProvider::new(26)) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]).with_pipeline_config(PipelineConfig {
            max_chars: 20_000,
            ..Default::default()
        });

        let args = serde_json::json!({
            "key": "pr#1",
            "format": "json"
        });
        let result = handler
            .execute("get_merge_request_discussions", Some(args))
            .await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Review comment 1"));
        assert!(content.contains("Review comment 20"));
        assert!(!content.contains("Review comment 21"));
        assert!(content.contains("offset=20"));
        assert!(content.contains("limit=20"));
        assert!(content.contains("Showing 1-20 of 26 discussions"));
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_handler_uses_limit_parameter() {
        let provider = Arc::new(ManyDiscussionsProvider::new(10)) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "key": "pr#1",
            "limit": 3,
            "format": "json"
        });
        let result = handler
            .execute("get_merge_request_discussions", Some(args))
            .await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Review comment 1"));
        assert!(content.contains("Review comment 3"));
        assert!(!content.contains("Review comment 4"));
        assert!(content.contains("offset=3"));
        assert!(content.contains("limit=3"));
        assert!(content.contains("Showing 1-3 of 10 discussions"));
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_handler_omits_next_page_hint_on_last_page() {
        let provider = Arc::new(ManyDiscussionsProvider::new(25)) as Arc<dyn Provider>;
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
        assert!(content.contains("Review comment 25"));
        assert!(!content.contains("offset=25"));
        assert!(!content.contains("Showing 21-25 of 25 discussions"));
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_handler_supports_offset_past_end() {
        let provider = Arc::new(ManyDiscussionsProvider::new(5)) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "key": "pr#1",
            "offset": 10,
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
        assert!(content.contains("[]"));
        assert!(!content.contains("offset=10"));
        assert!(!content.contains("Review comment 1"));
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_handler_skips_hints_when_disabled() {
        let provider = Arc::new(ManyDiscussionsProvider::new(26)) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]).with_pipeline_config(PipelineConfig {
            include_hints: false,
            ..Default::default()
        });

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
        assert!(content.contains("Review comment 25"));
        assert!(!content.contains("offset=25"));
        assert!(!content.contains("Showing 21-25 of 26 discussions"));
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_handler_rejects_zero_limit() {
        let provider = Arc::new(ManyDiscussionsProvider::new(26)) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "key": "pr#1",
            "limit": 0
        });
        let result = handler
            .execute("get_merge_request_discussions", Some(args))
            .await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("limit must be between 1 and 100"));
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_handler_rejects_limit_above_maximum() {
        let provider = Arc::new(ManyDiscussionsProvider::new(26)) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({
            "key": "pr#1",
            "limit": 101
        });
        let result = handler
            .execute("get_merge_request_discussions", Some(args))
            .await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("limit must be between 1 and 100"));
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

        // 7 issue tools + 6 MR tools + 2 pipeline tools + 3 meeting tools = 18 total
        assert_eq!(tools.len(), 18);
    }

    #[tokio::test]
    async fn test_get_merge_request_discussions_tool_schema_includes_pagination_bounds() {
        let handler = ToolHandler::new(vec![]);
        let tool = handler
            .available_tools()
            .into_iter()
            .find(|tool| tool.name == "get_merge_request_discussions")
            .expect("tool should exist");

        let limit = &tool.input_schema["properties"]["limit"];
        assert_eq!(limit["type"], serde_json::json!("integer"));
        assert_eq!(limit["minimum"], serde_json::json!(1));
        assert_eq!(limit["maximum"], serde_json::json!(100));
        assert_eq!(
            limit["description"],
            serde_json::json!("Maximum number of discussions to return (default: 20)")
        );

        let offset = &tool.input_schema["properties"]["offset"];
        assert_eq!(offset["type"], serde_json::json!("integer"));
        assert_eq!(offset["minimum"], serde_json::json!(0));
        assert_eq!(
            offset["description"],
            serde_json::json!("Number of discussions to skip for pagination (default: 0)")
        );
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
    async fn test_get_issues_with_format_toon() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"format": "toon"});
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

        let pipeline = handler.create_pipeline(&Some("json".to_string()), None);
        assert!(pipeline.transform_issues(vec![]).is_ok());

        let pipeline = handler.create_pipeline(&Some("toon".to_string()), None);
        assert!(pipeline.transform_issues(vec![]).is_ok());

        let pipeline = handler.create_pipeline(&None, None);
        assert!(pipeline.transform_issues(vec![]).is_ok());
    }

    #[tokio::test]
    async fn test_with_pipeline_config() {
        let _handler = ToolHandler::new(vec![]).with_pipeline_config(PipelineConfig {
            format: OutputFormat::Toon,
            ..Default::default()
        });

        // The default format from config should be used as base
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]).with_pipeline_config(PipelineConfig {
            format: OutputFormat::Toon,
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
        async fn get_issues(
            &self,
            _filter: IssueFilter,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Issue>> {
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
        async fn get_comments(
            &self,
            _key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Comment>> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        async fn add_comment(&self, _key: &str, _body: &str) -> devboy_core::Result<Comment> {
            Err(devboy_core::Error::Api {
                status: 500,
                message: "comment failed".into(),
            })
        }
        async fn get_issue_relations(&self, _key: &str) -> devboy_core::Result<IssueRelations> {
            Err(devboy_core::Error::NotFound("not found".into()))
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
        ) -> devboy_core::Result<devboy_core::ProviderResult<MergeRequest>> {
            Err(devboy_core::Error::Api {
                status: 500,
                message: "api error".into(),
            })
        }
        async fn get_merge_request(&self, _key: &str) -> devboy_core::Result<MergeRequest> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        async fn get_discussions(
            &self,
            _mr_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Discussion>> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        async fn get_diffs(
            &self,
            _mr_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<FileDiff>> {
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
    impl devboy_core::PipelineProvider for FailingProvider {
        fn provider_name(&self) -> &'static str {
            "test"
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

    // =========================================================================
    // Pipeline handler tests
    // =========================================================================

    #[tokio::test]
    async fn test_get_pipeline_no_providers() {
        let handler = ToolHandler::new(vec![]);
        let result = handler.execute("get_pipeline", None).await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_pipeline_provider_unsupported() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);
        let args = serde_json::json!({"branch": "main"});
        let result = handler.execute("get_pipeline", Some(args)).await;
        // MockProvider returns ProviderUnsupported for pipeline
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_job_logs_no_providers() {
        let handler = ToolHandler::new(vec![]);
        let args = serde_json::json!({"jobId": "123"});
        let result = handler.execute("get_job_logs", Some(args)).await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_job_logs_missing_params() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);
        let result = handler.execute("get_job_logs", None).await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_job_logs_provider_unsupported() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);
        let args = serde_json::json!({"jobId": "123"});
        let result = handler.execute("get_job_logs", Some(args)).await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_job_logs_with_pattern() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);
        let args = serde_json::json!({"jobId": "123", "pattern": "ERROR"});
        let result = handler.execute("get_job_logs", Some(args)).await;
        assert_eq!(result.is_error, Some(true)); // unsupported
    }

    #[tokio::test]
    async fn test_get_job_logs_paginated() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);
        let args = serde_json::json!({"jobId": "123", "offset": 10, "limit": 50});
        let result = handler.execute("get_job_logs", Some(args)).await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_job_logs_full() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);
        let args = serde_json::json!({"jobId": "123", "full": true});
        let result = handler.execute("get_job_logs", Some(args)).await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_pipeline_with_mr_key() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);
        let args = serde_json::json!({"mrKey": "pr#1"});
        let result = handler.execute("get_pipeline", Some(args)).await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn test_get_pipeline_default_params() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);
        let result = handler
            .execute("get_pipeline", Some(serde_json::json!({})))
            .await;
        assert_eq!(result.is_error, Some(true));
    }

    // =========================================================================
    // get_issue_relations handler tests
    // =========================================================================

    #[tokio::test]
    async fn test_get_issue_relations_handler() {
        let provider = Arc::new(MockProvider::new()) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "gh#1"});
        let result = handler.execute("get_issue_relations", Some(args)).await;

        assert!(result.is_error.is_none());
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        // Should contain serialized JSON with parent, subtasks, blocks
        assert!(content.contains("gh#1"));
        assert!(content.contains("Blocks"));
    }

    #[tokio::test]
    async fn test_get_issue_relations_missing_params() {
        let handler = ToolHandler::new(vec![Arc::new(MockProvider::new()) as Arc<dyn Provider>]);

        let result = handler.execute("get_issue_relations", None).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Missing required parameter: key"));
    }

    #[tokio::test]
    async fn test_get_issue_relations_no_providers() {
        let handler = ToolHandler::new(vec![]);

        let args = serde_json::json!({"key": "gh#1"});
        let result = handler.execute("get_issue_relations", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("No providers configured"));
    }

    #[tokio::test]
    async fn test_get_issue_relations_provider_fails() {
        let provider = Arc::new(FailingProvider) as Arc<dyn Provider>;
        let handler = ToolHandler::new(vec![provider]);

        let args = serde_json::json!({"key": "gh#1"});
        let result = handler.execute("get_issue_relations", Some(args)).await;

        assert_eq!(result.is_error, Some(true));
        let content = match &result.content[0] {
            crate::protocol::ToolResultContent::Text { text } => text,
        };
        assert!(content.contains("Issue not found"));
    }
}
