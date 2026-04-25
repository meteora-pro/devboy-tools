//! Base tool definitions for all provider tools.
//!
//! These are the "generic" schemas before enrichment.
//! Provider enrichers modify them based on capabilities and metadata.

use devboy_core::{PropertySchema, ToolCategory, ToolSchema};

/// A tool definition with name, description, category, and input schema.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub category: ToolCategory,
    pub input_schema: ToolSchema,
}

/// Get all base tool definitions (before enrichment).
pub fn base_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        // Issue tools
        ToolDefinition {
            name: "get_issues".into(),
            description: "Get issues from configured provider. Returns a list with filters.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("state", PropertySchema::string_enum(&["open", "closed", "all"], "Filter by issue state (default: open)"));
                s.add_property("search", PropertySchema::string("Search query for title and description"));
                s.add_property("labels", PropertySchema::array(PropertySchema::string("label"), "Filter by label names"));
                s.add_property("assignee", PropertySchema::string("Filter by assignee username"));
                s.add_property("limit", PropertySchema::integer("Maximum number of results (default: 20)", Some(1.0), Some(100.0)));
                s.add_property("offset", PropertySchema::integer("Number of results to skip (default: 0)", Some(0.0), None));
                s.add_property("sort_by", PropertySchema::string_enum(&["created_at", "updated_at"], "Sort by field (default: updated_at)"));
                s.add_property("sort_order", PropertySchema::string_enum(&["asc", "desc"], "Sort order (default: desc)"));
                s.add_property("projectKey", PropertySchema::string("Project key to filter issues (e.g., \"PROJ\"). Overrides default project. Removed by providers that don't support it."));
                s.add_property("nativeQuery", PropertySchema::string("Native query passed directly to provider (e.g., Jira JQL). Replaces auto-generated filters. If the query omits a project clause, the default project is auto-injected."));
                s
            },
        },
        ToolDefinition {
            name: "get_issue".into(),
            description: "Get a single issue by key with optional comments and relations.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("Issue key (e.g., 'gh#123', 'gitlab#456', 'CU-abc', 'DEV-42', 'jira#PROJ-123')"));
                s.add_property("includeComments", PropertySchema::boolean("Include issue comments (default: true)"));
                s.add_property("includeRelations", PropertySchema::boolean("Include issue relations — parent, subtasks, dependencies (default: true)"));
                s.set_required("key", true);
                s
            },
        },
        ToolDefinition {
            name: "get_issue_comments".into(),
            description: "Get comments for an issue.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("Issue key"));
                s.set_required("key", true);
                s
            },
        },
        ToolDefinition {
            name: "get_issue_relations".into(),
            description: "Get relations for an issue (parent, subtasks, linked issues).".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("Issue key"));
                s.set_required("key", true);
                s
            },
        },
        ToolDefinition {
            name: "create_issue".into(),
            description: "Create a new issue in the configured provider.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("title", PropertySchema::string("Issue title"));
                s.add_property("description", PropertySchema::string("Issue description/body"));
                s.add_property("labels", PropertySchema::array(PropertySchema::string("label"), "Labels to add"));
                s.add_property("assignees", PropertySchema::array(PropertySchema::string("assignee"), "Assignee usernames"));
                s.add_property("parentId", PropertySchema::string("Parent issue key to create a subtask (e.g., 'CU-abc123' or 'DEV-42'). Only supported by ClickUp."));
                s.add_property("markdown", PropertySchema::boolean("Whether the description is markdown (default: true). When true, ClickUp renders formatted text."));
                s.add_property("projectId", PropertySchema::string("Jira project key (not numeric ID) for issue creation (e.g., \"PROJ\"). Optional — overrides the default project."));
                s.add_property("issueType", PropertySchema::string("Issue type (e.g., \"Task\", \"Bug\", \"Story\"). Default: \"Task\". Removed by providers that don't support it."));
                // Issue #197 — component names. The Jira enricher
                // populates an enum at metadata-assembly time so schema
                // consumers see the valid values; here we just declare
                // the property exists (Codex review on PR #205).
                s.add_property("components", PropertySchema::array(
                    PropertySchema::string("component name"),
                    "Jira component names to associate with the issue. Ignored by providers that don't have Components (GitHub/GitLab/ClickUp).",
                ));
                s.set_required("title", true);
                s
            },
        },
        ToolDefinition {
            name: "update_issue".into(),
            description: "Update an existing issue. Only provided fields will be changed.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("Issue key"));
                s.add_property("title", PropertySchema::string("New title"));
                s.add_property("description", PropertySchema::string("New description"));
                s.add_property("state", PropertySchema::string_enum(&["open", "closed"], "New state"));
                s.add_property("labels", PropertySchema::array(PropertySchema::string("label"), "New labels (replaces existing)"));
                s.add_property("assignees", PropertySchema::array(PropertySchema::string("assignee"), "New assignees"));
                s.add_property("parentId", PropertySchema::string("Parent issue key to move task as subtask (e.g., 'CU-abc123' or 'DEV-42'). Only supported by ClickUp."));
                s.add_property("markdown", PropertySchema::boolean("Whether the description is markdown (default: true). When true, ClickUp renders formatted text."));
                // Issue #197 — `None` leaves components untouched, empty
                // array clears them, array with names replaces them.
                s.add_property("components", PropertySchema::array(
                    PropertySchema::string("component name"),
                    "Replace components with these Jira component names. Omit the field to leave existing components untouched; pass an empty array to clear.",
                ));
                s.set_required("key", true);
                s
            },
        },
        ToolDefinition {
            name: "add_issue_comment".into(),
            description: "Add a comment to an issue with optional file attachments (ClickUp only).".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("Issue key"));
                s.add_property("body", PropertySchema::string("Comment text"));
                s.add_property("attachments", PropertySchema::array(
                    PropertySchema::string("Attachment object with fileData (base64) and filename"),
                    "File attachments (ClickUp only, max 10MB per file). Each: {fileData: base64, filename: string}",
                ));
                s.set_required("key", true);
                s.set_required("body", true);
                s
            },
        },

        // MR/PR tools
        ToolDefinition {
            name: "get_merge_requests".into(),
            description: "Get merge requests / pull requests from configured provider.".into(),
            category: ToolCategory::GitRepository,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("state", PropertySchema::string_enum(&["open", "closed", "merged", "all"], "Filter by state (default: open)"));
                s.add_property("author", PropertySchema::string("Filter by author username"));
                s.add_property("labels", PropertySchema::array(PropertySchema::string("label"), "Filter by label names"));
                s.add_property("source_branch", PropertySchema::string("Filter by source branch"));
                s.add_property("target_branch", PropertySchema::string("Filter by target branch"));
                s.add_property("limit", PropertySchema::integer("Maximum results (default: 20)", Some(1.0), Some(100.0)));
                s
            },
        },
        ToolDefinition {
            name: "get_merge_request".into(),
            description: "Get a single merge request by key (e.g., 'pr#123', 'mr#456').".into(),
            category: ToolCategory::GitRepository,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("MR/PR key"));
                s.set_required("key", true);
                s
            },
        },
        ToolDefinition {
            name: "get_merge_request_discussions".into(),
            description: "Get discussions/review comments for a merge request with code positions.".into(),
            category: ToolCategory::GitRepository,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("MR/PR key"));
                s.add_property("limit", PropertySchema::integer("Max discussions (default: 20)", Some(1.0), Some(100.0)));
                s.add_property("offset", PropertySchema::integer("Skip N discussions (default: 0)", Some(0.0), None));
                s.set_required("key", true);
                s
            },
        },
        ToolDefinition {
            name: "get_merge_request_diffs".into(),
            description: "Get file diffs for a merge request.".into(),
            category: ToolCategory::GitRepository,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("MR/PR key"));
                s.set_required("key", true);
                s
            },
        },
        ToolDefinition {
            name: "create_merge_request".into(),
            description: "Create a new merge request (GitLab) or pull request (GitHub).".into(),
            category: ToolCategory::GitRepository,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("title", PropertySchema::string("MR/PR title"));
                s.add_property("description", PropertySchema::string("MR/PR description"));
                s.add_property("source_branch", PropertySchema::string("Source branch"));
                s.add_property("target_branch", PropertySchema::string("Target branch"));
                s.add_property("draft", PropertySchema::boolean("Create as draft (default: false)"));
                s.add_property("labels", PropertySchema::array(PropertySchema::string("label"), "Labels"));
                s.add_property("reviewers", PropertySchema::array(PropertySchema::string("reviewer"), "Reviewers"));
                s.set_required("title", true);
                s.set_required("source_branch", true);
                s.set_required("target_branch", true);
                s
            },
        },
        ToolDefinition {
            name: "create_merge_request_comment".into(),
            description: "Add a comment to a merge request. Can be general or inline code review.".into(),
            category: ToolCategory::GitRepository,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("MR/PR key"));
                s.add_property("body", PropertySchema::string("Comment text"));
                s.add_property("file_path", PropertySchema::string("File path for inline comment"));
                s.add_property("line", PropertySchema::integer("Line number for inline comment", None, None));
                s.add_property("line_type", PropertySchema::string_enum(&["old", "new"], "Line type (default: new)"));
                s.add_property("commit_sha", PropertySchema::string("Commit SHA for inline comment"));
                s.add_property("discussion_id", PropertySchema::string("Reply to existing discussion"));
                s.set_required("key", true);
                s.set_required("body", true);
                s
            },
        },

        // Pipeline tools
        ToolDefinition {
            name: "get_pipeline".into(),
            description: "Get CI/CD pipeline status for branch or MR/PR with job details.".into(),
            category: ToolCategory::GitRepository,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("branch", PropertySchema::string("Branch name (default: main)"));
                s.add_property("mrKey", PropertySchema::string("MR/PR key (priority over branch)"));
                s.add_property("includeFailedLogs", PropertySchema::boolean("Include error extraction for failed jobs (default: true)"));
                s
            },
        },
        ToolDefinition {
            name: "get_job_logs".into(),
            description: "Get CI/CD job logs. Modes: smart (auto errors), search (pattern), paginated, full.".into(),
            category: ToolCategory::GitRepository,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("jobId", PropertySchema::string("Job ID from get_pipeline"));
                s.add_property("pattern", PropertySchema::string("Regex/keyword search pattern"));
                s.add_property("context", PropertySchema::integer("Context lines around match (default: 5)", None, None));
                s.add_property("maxMatches", PropertySchema::integer("Max search results (default: 20)", None, None));
                s.add_property("offset", PropertySchema::integer("Start line for paginated mode", None, None));
                s.add_property("limit", PropertySchema::integer("Lines to return (default: 200, max: 1000)", Some(1.0), Some(1000.0)));
                s.add_property("full", PropertySchema::boolean("Return entire log"));
                s.set_required("jobId", true);
                s
            },
        },

        // Status / user / link / epic tools
        ToolDefinition {
            name: "get_available_statuses".into(),
            description: "Get available statuses for the issue tracker.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: ToolSchema::new(),
        },
        ToolDefinition {
            name: "get_users".into(),
            description: "Get users from the issue tracker (Jira). Search by name, project, or ID.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("userId", PropertySchema::string("Get specific user by ID"));
                s.add_property("projectKey", PropertySchema::string("Get assignable users for project"));
                s.add_property("search", PropertySchema::string("Search by name or email"));
                s.add_property("maxResults", PropertySchema::integer("Max results (default: 50)", Some(1.0), Some(1000.0)));
                s
            },
        },
        ToolDefinition {
            name: "link_issues".into(),
            description: "Link two issues together (blocks, relates_to, etc.).".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("sourceIssueKey", PropertySchema::string("Source issue key"));
                s.add_property("targetIssueKey", PropertySchema::string("Target issue key"));
                s.add_property("linkType", PropertySchema::string("Link type (e.g., blocks, relates_to)"));
                s.set_required("sourceIssueKey", true);
                s.set_required("targetIssueKey", true);
                s.set_required("linkType", true);
                s
            },
        },
        ToolDefinition {
            name: "unlink_issues".into(),
            description: "Remove a link between two issues.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("sourceIssueKey", PropertySchema::string("Source issue key"));
                s.add_property("targetIssueKey", PropertySchema::string("Target issue key"));
                s.add_property("linkType", PropertySchema::string("Link type to remove (e.g., blocks, relates_to, subtask)"));
                s.set_required("sourceIssueKey", true);
                s.set_required("targetIssueKey", true);
                s.set_required("linkType", true);
                s
            },
        },
        ToolDefinition {
            name: "get_epics".into(),
            description: "Get epics (high-level tasks) from the issue tracker.".into(),
            category: ToolCategory::Epics,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("search", PropertySchema::string("Search in epic title"));
                s.add_property("limit", PropertySchema::integer("Max results (default: 50)", Some(1.0), Some(100.0)));
                s.add_property("offset", PropertySchema::integer("Skip N results (default: 0)", Some(0.0), None));
                s
            },
        },
        ToolDefinition {
            name: "create_epic".into(),
            description: "Create a new epic.".into(),
            category: ToolCategory::Epics,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("title", PropertySchema::string("Epic title"));
                s.add_property("description", PropertySchema::string("Epic description"));
                s.set_required("title", true);
                s
            },
        },
        ToolDefinition {
            name: "update_epic".into(),
            description: "Update an existing epic.".into(),
            category: ToolCategory::Epics,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("epicKey", PropertySchema::string("Epic key (e.g., 'CU-abc', 'DEV-123')"));
                s.add_property("title", PropertySchema::string("New title"));
                s.add_property("description", PropertySchema::string("New description"));
                s.add_property("state", PropertySchema::string("New epic state"));
                s.add_property("goalId", PropertySchema::string("Goal ID (G1-G9) to associate with the epic"));
                s.add_property("priority", PropertySchema::string("New priority (urgent/high/normal/low)"));
                s.add_property("labels", PropertySchema::array(PropertySchema::string("label"), "Labels to set"));
                s.add_property("assignees", PropertySchema::array(PropertySchema::string("assignee"), "Assignees to set"));
                s.set_required("epicKey", true);
                s
            },
        },
        // Meeting notes tools
        ToolDefinition {
            name: "get_meeting_notes".into(),
            description: "Get meeting notes and transcripts with optional filters (date range, participants, host).".into(),
            category: ToolCategory::MeetingNotes,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("from_date", PropertySchema::string("Filter from date (ISO 8601, e.g., '2025-01-01T00:00:00Z')"));
                s.add_property("to_date", PropertySchema::string("Filter to date (ISO 8601)"));
                s.add_property("participants", PropertySchema::array(PropertySchema::string("email"), "Filter by participant email addresses"));
                s.add_property("host_email", PropertySchema::string("Filter by host email"));
                s.add_property("limit", PropertySchema::integer("Maximum number of results (default: 50)", Some(1.0), Some(50.0)));
                s.add_property("offset", PropertySchema::integer("Number of results to skip (default: 0)", Some(0.0), None));
                s
            },
        },
        ToolDefinition {
            name: "get_meeting_transcript".into(),
            description: "Get the full transcript for a meeting. Returns speaker-attributed sentences with timestamps.".into(),
            category: ToolCategory::MeetingNotes,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("meeting_id", PropertySchema::string("Meeting ID from get_meeting_notes"));
                s.set_required("meeting_id", true);
                s
            },
        },
        ToolDefinition {
            name: "search_meeting_notes".into(),
            description: "Search across meetings by keywords, topics, or action items, with optional filters (date range, participants, host).".into(),
            category: ToolCategory::MeetingNotes,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("query", PropertySchema::string("Search query"));
                s.add_property("from_date", PropertySchema::string("Filter from date (ISO 8601)"));
                s.add_property("to_date", PropertySchema::string("Filter to date (ISO 8601)"));
                s.add_property("participants", PropertySchema::array(PropertySchema::string("email"), "Filter by participant email addresses"));
                s.add_property("host_email", PropertySchema::string("Filter by host email"));
                s.add_property("limit", PropertySchema::integer("Maximum number of results (default: 50)", Some(1.0), Some(50.0)));
                s.add_property("offset", PropertySchema::integer("Number of results to skip (default: 0)", Some(0.0), None));
                s.set_required("query", true);
                s
            },
        },
        ToolDefinition {
            name: "update_merge_request".into(),
            description: "Update a merge request / pull request (title, description, state, labels, draft).".into(),
            category: ToolCategory::GitRepository,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("MR key (e.g. 'mr#1', 'pr#42')"));
                s.add_property("title", PropertySchema::string("New title"));
                s.add_property("description", PropertySchema::string("New description / body (supports markdown)"));
                s.add_property("state", PropertySchema::string_enum(&["close", "reopen"], "Change MR state"));
                s.add_property("labels", PropertySchema::array(PropertySchema::string("label"), "New labels (replaces existing)"));
                s.set_required("key", true);
                s
            },
        },
        // =====================================================================
        // Asset tools
        // =====================================================================
        ToolDefinition {
            name: "get_assets".into(),
            description: "List file attachments for an issue or merge request.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("context_type", PropertySchema::string_enum(&["issue", "mr"], "Context type: 'issue' or 'mr' (merge request / pull request)"));
                s.add_property("key", PropertySchema::string("Issue key (e.g. 'DEV-123', 'gitlab#42') or MR key (e.g. 'mr#42', 'pr#42')"));
                s.set_required("context_type", true);
                s.set_required("key", true);
                s
            },
        },
        ToolDefinition {
            name: "upload_asset".into(),
            description: "Upload a file attachment to an issue. Returns the download URL.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("context_type", PropertySchema::string_enum(&["issue"], "Context type (currently only 'issue' is supported for uploads)"));
                s.add_property("key", PropertySchema::string("Issue key (e.g. 'DEV-123')"));
                s.add_property("filename", PropertySchema::string("Original filename (e.g. 'screenshot.png')"));
                s.add_property("fileData", PropertySchema::string("Base64-encoded file content"));
                s.set_required("context_type", true);
                s.set_required("key", true);
                s.set_required("filename", true);
                s.set_required("fileData", true);
                s
            },
        },
        ToolDefinition {
            name: "download_asset".into(),
            description: "Download a file attachment to local cache. Returns local file path when cache is available, base64-encoded content as fallback.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("context_type", PropertySchema::string_enum(&["issue", "mr"], "Context type: 'issue' or 'mr'"));
                s.add_property("key", PropertySchema::string("Issue key or MR key"));
                s.add_property("asset_id", PropertySchema::string("Asset identifier from get_assets response"));
                s.set_required("context_type", true);
                s.set_required("key", true);
                s.set_required("asset_id", true);
                s
            },
        },
        // =====================================================================
        // Messenger tools
        // =====================================================================
        ToolDefinition {
            name: "get_messenger_chats".into(),
            description: "List available messenger chats, channels, groups, or direct messages.".into(),
            category: ToolCategory::Messenger,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("search", PropertySchema::string("Optional chat name search"));
                s.add_property("chat_type", PropertySchema::string_enum(&["direct", "group", "channel"], "Optional chat type filter"));
                s.add_property("limit", PropertySchema::integer("Maximum number of chats to return", Some(1.0), Some(1000.0)));
                s.add_property("cursor", PropertySchema::string("Provider pagination cursor"));
                s.add_property("include_inactive", PropertySchema::boolean("Include archived or inactive chats"));
                s
            },
        },
        ToolDefinition {
            name: "get_chat_messages".into(),
            description: "Get message history for a chat or fetch replies for a specific thread.".into(),
            category: ToolCategory::Messenger,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("chat_id", PropertySchema::string("Messenger chat ID"));
                s.add_property("limit", PropertySchema::integer("Maximum number of messages to return", Some(1.0), Some(1000.0)));
                s.add_property("cursor", PropertySchema::string("Provider pagination cursor"));
                s.add_property("thread_id", PropertySchema::string("Thread identifier to fetch replies for"));
                s.add_property("since", PropertySchema::string("Only include messages after this provider timestamp"));
                s.add_property("until", PropertySchema::string("Only include messages before this provider timestamp"));
                s.set_required("chat_id", true);
                s
            },
        },
        ToolDefinition {
            name: "search_chat_messages".into(),
            description: "Search messages across accessible chats or within a specific chat.".into(),
            category: ToolCategory::Messenger,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("query", PropertySchema::string("Message search query"));
                s.add_property("chat_id", PropertySchema::string("Optional chat ID to scope the search"));
                s.add_property("limit", PropertySchema::integer("Maximum number of matches to return", Some(1.0), Some(1000.0)));
                s.add_property("cursor", PropertySchema::string("Provider pagination cursor"));
                s.add_property("since", PropertySchema::string("Only include messages after this provider timestamp"));
                s.add_property("until", PropertySchema::string("Only include messages before this provider timestamp"));
                s.set_required("query", true);
                s
            },
        },
        ToolDefinition {
            name: "send_message".into(),
            description: "Send a message to a chat or as a threaded reply.".into(),
            category: ToolCategory::Messenger,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("chat_id", PropertySchema::string("Messenger chat ID"));
                s.add_property("text", PropertySchema::string("Message body"));
                s.add_property("thread_id", PropertySchema::string("Thread identifier to post as a threaded reply"));
                s.add_property("reply_to_id", PropertySchema::string("Direct parent message ID when supported"));
                s.set_required("chat_id", true);
                s.set_required("text", true);
                s
            },
        },
        ToolDefinition {
            name: "delete_asset".into(),
            description: "Delete a file attachment from an issue. Not all providers support this — check asset_capabilities first.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("Issue key (e.g. 'PROJ-123')"));
                s.add_property("asset_id", PropertySchema::string("Asset identifier to delete"));
                s.set_required("key", true);
                s.set_required("asset_id", true);
                s
            },
        },
        // Jira Structure plugin tools
        ToolDefinition {
            name: "get_structures".into(),
            description: "List all available Jira Structures. Returns structure ID, name, and description. Requires Jira with Structure plugin.".into(),
            category: ToolCategory::JiraStructure,
            input_schema: ToolSchema::new(),
        },
        ToolDefinition {
            name: "get_structure_forest".into(),
            description: "Get the hierarchy tree of a Jira Structure. Returns nested tree with rowId, itemId (Jira issue key), itemType, and children. Supports pagination for large structures.".into(),
            category: ToolCategory::JiraStructure,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("structureId", PropertySchema::integer("Structure ID. Use get_structures to find it.", None, None));
                s.add_property("offset", PropertySchema::integer("Offset for pagination (default: 0)", Some(0.0), None));
                s.add_property("limit", PropertySchema::integer("Max rows to return (default: 200)", Some(1.0), Some(10000.0)));
                s.set_required("structureId", true);
                s
            },
        },
        ToolDefinition {
            name: "add_structure_rows".into(),
            description: "Add items (Jira issues or folders) to a Structure. Specify position with under (parent row) and/or after (sibling row). Use forestVersion for optimistic concurrency.".into(),
            category: ToolCategory::JiraStructure,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("structureId", PropertySchema::integer("Structure ID", None, None));
                s.add_property("items", PropertySchema::array(
                    PropertySchema::string("Item: Jira issue key (e.g. 'PROJ-123') or JSON {\"itemId\":\"PROJ-123\",\"itemType\":\"issue\"}"),
                    "Items to add",
                ));
                s.add_property("under", PropertySchema::integer("Parent row ID — items become children of this row", None, None));
                s.add_property("after", PropertySchema::integer("Sibling row ID — items placed after this row", None, None));
                s.add_property("forestVersion", PropertySchema::integer("Forest version for optimistic locking (from get_structure_forest)", None, None));
                s.set_required("structureId", true);
                s.set_required("items", true);
                s
            },
        },
        ToolDefinition {
            name: "move_structure_rows".into(),
            description: "Move rows within a Jira Structure hierarchy. Specify new position with under (new parent) and/or after (sibling).".into(),
            category: ToolCategory::JiraStructure,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("structureId", PropertySchema::integer("Structure ID", None, None));
                s.add_property("rowIds", PropertySchema::array(
                    PropertySchema::integer("Row ID", None, None),
                    "Row IDs to move (from get_structure_forest)",
                ));
                s.add_property("under", PropertySchema::integer("New parent row ID", None, None));
                s.add_property("after", PropertySchema::integer("Sibling row ID to place after", None, None));
                s.add_property("forestVersion", PropertySchema::integer("Forest version for optimistic locking", None, None));
                s.set_required("structureId", true);
                s.set_required("rowIds", true);
                s
            },
        },
        ToolDefinition {
            name: "remove_structure_row".into(),
            description: "Remove a row from a Jira Structure. Only removes from the structure hierarchy — the underlying Jira issue is NOT deleted.".into(),
            category: ToolCategory::JiraStructure,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("structureId", PropertySchema::integer("Structure ID", None, None));
                s.add_property("rowId", PropertySchema::integer("Row ID to remove (from get_structure_forest)", None, None));
                s.set_required("structureId", true);
                s.set_required("rowId", true);
                s
            },
        },
        ToolDefinition {
            name: "get_structure_values".into(),
            description: "Read column values (including Expr formulas like SUM, PROGRESS, COUNT) for specific rows in a Jira Structure. Values are computed server-side.".into(),
            category: ToolCategory::JiraStructure,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("structureId", PropertySchema::integer("Structure ID", None, None));
                s.add_property("rows", PropertySchema::array(
                    PropertySchema::integer("Row ID", None, None),
                    "Row IDs to read values for",
                ));
                s.add_property("columns", PropertySchema::array(
                    PropertySchema::string("Column spec: field name (e.g. 'summary'), or JSON {\"field\":\"status\"} or {\"formula\":\"SUM(\\\"Story Points\\\")\"}"),
                    "Columns to read",
                ));
                s.set_required("structureId", true);
                s.set_required("rows", true);
                s.set_required("columns", true);
                s
            },
        },
        ToolDefinition {
            name: "get_structure_views".into(),
            description: "Get views for a Jira Structure. Without viewId: lists all views. With viewId: returns full view configuration (columns, grouping, sorting, filter).".into(),
            category: ToolCategory::JiraStructure,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("structureId", PropertySchema::integer("Structure ID", None, None));
                s.add_property("viewId", PropertySchema::integer("View ID for full config (optional — omit to list all views)", None, None));
                s.set_required("structureId", true);
                s
            },
        },
        ToolDefinition {
            name: "save_structure_view".into(),
            description: "Create or update a Jira Structure view. Views define column layout (fields and formulas), grouping, sorting, and filters. Omit id to create new.".into(),
            category: ToolCategory::JiraStructure,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("id", PropertySchema::integer("View ID to update (omit to create new)", None, None));
                s.add_property("structureId", PropertySchema::integer("Structure ID this view belongs to", None, None));
                s.add_property("name", PropertySchema::string("View name"));
                s.add_property("columns", PropertySchema::array(
                    PropertySchema::string("Column spec: JSON {\"field\":\"summary\"} or {\"formula\":\"SUM(\\\"Story Points\\\")\",\"width\":100}"),
                    "Column definitions",
                ));
                s.add_property("groupBy", PropertySchema::string("Field name to group by"));
                s.add_property("sortBy", PropertySchema::string("Field name to sort by"));
                s.add_property("filter", PropertySchema::string("JQL filter expression"));
                s.set_required("structureId", true);
                s.set_required("name", true);
                s
            },
        },
        ToolDefinition {
            name: "create_structure".into(),
            description: "Create a new Jira Structure. After creation, use add_structure_rows to populate and save_structure_view to configure columns.".into(),
            category: ToolCategory::JiraStructure,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("name", PropertySchema::string("Structure name"));
                s.add_property("description", PropertySchema::string("Structure description"));
                s.set_required("name", true);
                s
            },
        },
    ]
}

/// Always-available MCP tools that don't belong to a provider category.
///
/// These are surfaced by `devboy-mcp` on every `tools/list` regardless of
/// which providers are configured. They live here (not in the MCP crate)
/// so the published reference doc can render them from a single source.
#[derive(Debug, Clone)]
pub struct McpOnlyTool {
    pub name: String,
    pub description: String,
    pub input_schema: ToolSchema,
}

/// MCP-only tools (context management). Always advertised by the server.
pub fn mcp_only_tools() -> Vec<McpOnlyTool> {
    vec![
        McpOnlyTool {
            name: "list_contexts".into(),
            description: "List configured contexts and indicate the active context.".into(),
            input_schema: ToolSchema::new(),
        },
        McpOnlyTool {
            name: "use_context".into(),
            description: "Switch active context at runtime.".into(),
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("name", PropertySchema::string("Context name to activate"));
                s.set_required("name", true);
                s
            },
        },
        McpOnlyTool {
            name: "get_current_context".into(),
            description: "Get current active context name.".into(),
            input_schema: ToolSchema::new(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_definitions_count() {
        let tools = base_tool_definitions();
        assert_eq!(tools.len(), 43);
    }

    #[test]
    fn test_all_tools_have_names() {
        for tool in base_tool_definitions() {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
        }
    }

    #[test]
    fn test_tool_categories() {
        let tools = base_tool_definitions();

        let issue_tracker_tools = [
            "get_issues",
            "get_issue",
            "get_issue_comments",
            "get_issue_relations",
            "create_issue",
            "update_issue",
            "add_issue_comment",
            "get_available_statuses",
            "get_users",
            "link_issues",
            "unlink_issues",
            "get_assets",
            "upload_asset",
            "download_asset",
            "delete_asset",
        ];
        let git_repository_tools = [
            "get_merge_requests",
            "get_merge_request",
            "get_merge_request_discussions",
            "get_merge_request_diffs",
            "create_merge_request",
            "create_merge_request_comment",
            "update_merge_request",
            "get_pipeline",
            "get_job_logs",
        ];
        let epics_tools = ["get_epics", "create_epic", "update_epic"];
        let meeting_notes_tools = [
            "get_meeting_notes",
            "get_meeting_transcript",
            "search_meeting_notes",
        ];
        let messenger_tools = [
            "get_messenger_chats",
            "get_chat_messages",
            "search_chat_messages",
            "send_message",
        ];
        let jira_structure_tools = [
            "get_structures",
            "get_structure_forest",
            "add_structure_rows",
            "move_structure_rows",
            "remove_structure_row",
            "get_structure_values",
            "get_structure_views",
            "save_structure_view",
            "create_structure",
        ];

        for tool in &tools {
            if issue_tracker_tools.contains(&tool.name.as_str()) {
                assert_eq!(
                    tool.category,
                    ToolCategory::IssueTracker,
                    "tool {} should be IssueTracker",
                    tool.name
                );
            } else if git_repository_tools.contains(&tool.name.as_str()) {
                assert_eq!(
                    tool.category,
                    ToolCategory::GitRepository,
                    "tool {} should be GitRepository",
                    tool.name
                );
            } else if epics_tools.contains(&tool.name.as_str()) {
                assert_eq!(
                    tool.category,
                    ToolCategory::Epics,
                    "tool {} should be Epics",
                    tool.name
                );
            } else if meeting_notes_tools.contains(&tool.name.as_str()) {
                assert_eq!(
                    tool.category,
                    ToolCategory::MeetingNotes,
                    "tool {} should be MeetingNotes",
                    tool.name
                );
            } else if messenger_tools.contains(&tool.name.as_str()) {
                assert_eq!(
                    tool.category,
                    ToolCategory::Messenger,
                    "tool {} should be Messenger",
                    tool.name
                );
            } else if jira_structure_tools.contains(&tool.name.as_str()) {
                assert_eq!(
                    tool.category,
                    ToolCategory::JiraStructure,
                    "tool {} should be JiraStructure",
                    tool.name
                );
            } else {
                panic!("tool {} has no expected category mapping", tool.name);
            }
        }
    }

    #[test]
    fn test_required_params() {
        let tools = base_tool_definitions();
        let get_issue = tools.iter().find(|t| t.name == "get_issue").unwrap();
        assert!(get_issue.input_schema.required.contains(&"key".to_string()));

        let create_mr = tools
            .iter()
            .find(|t| t.name == "create_merge_request")
            .unwrap();
        assert!(
            create_mr
                .input_schema
                .required
                .contains(&"title".to_string())
        );
        assert!(
            create_mr
                .input_schema
                .required
                .contains(&"source_branch".to_string())
        );
    }

    // --- ToolDefinition serialization ---

    #[test]
    fn test_tool_definition_serializes_to_json() {
        let tools = base_tool_definitions();
        let tool = &tools[0]; // get_issues
        let json = serde_json::to_string(tool).unwrap();

        assert!(json.contains("\"name\":\"get_issues\""));
        assert!(json.contains("\"description\""));
        assert!(json.contains("\"category\""));
        assert!(json.contains("\"input_schema\""));

        // Should be valid JSON
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["name"], "get_issues");
    }

    #[test]
    fn test_all_tool_definitions_serialize() {
        let tools = base_tool_definitions();
        for tool in &tools {
            let json = serde_json::to_string(tool);
            assert!(
                json.is_ok(),
                "tool '{}' failed to serialize: {:?}",
                tool.name,
                json.err()
            );
        }
    }

    #[test]
    fn test_tool_definition_json_contains_properties() {
        let tools = base_tool_definitions();
        let get_issues = tools.iter().find(|t| t.name == "get_issues").unwrap();
        let json = serde_json::to_string_pretty(get_issues).unwrap();

        // get_issues should have state, search, labels, assignee, limit, offset, sort_by, sort_order, projectKey, nativeQuery
        assert!(json.contains("state"));
        assert!(json.contains("search"));
        assert!(json.contains("labels"));
        assert!(json.contains("assignee"));
        assert!(json.contains("limit"));
        assert!(json.contains("projectKey"));
        assert!(json.contains("nativeQuery"));
    }

    #[test]
    fn test_tool_definition_required_fields_in_json() {
        let tools = base_tool_definitions();
        let add_comment = tools
            .iter()
            .find(|t| t.name == "add_issue_comment")
            .unwrap();
        let json_val: serde_json::Value = serde_json::to_value(add_comment).unwrap();

        let required = json_val["input_schema"]["required"]
            .as_array()
            .expect("required should be an array");
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"key"));
        assert!(required_strs.contains(&"body"));
    }

    #[test]
    fn test_tool_names_are_unique() {
        let tools = base_tool_definitions();
        let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        let original_len = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), original_len, "tool names should all be unique");
    }

    #[test]
    fn test_link_issues_required_params() {
        let tools = base_tool_definitions();
        let link = tools.iter().find(|t| t.name == "link_issues").unwrap();
        assert!(
            link.input_schema
                .required
                .contains(&"sourceIssueKey".to_string())
        );
        assert!(
            link.input_schema
                .required
                .contains(&"targetIssueKey".to_string())
        );
        assert!(link.input_schema.required.contains(&"linkType".to_string()));
    }

    #[test]
    fn test_get_available_statuses_has_empty_schema() {
        let tools = base_tool_definitions();
        let statuses = tools
            .iter()
            .find(|t| t.name == "get_available_statuses")
            .unwrap();
        assert!(statuses.input_schema.required.is_empty());
        assert!(statuses.input_schema.properties.is_empty());
    }

    #[test]
    fn test_epic_tools_exist() {
        let tools = base_tool_definitions();
        let epic_names: Vec<&str> = tools
            .iter()
            .filter(|t| t.category == ToolCategory::Epics)
            .map(|t| t.name.as_str())
            .collect();
        assert!(epic_names.contains(&"get_epics"));
        assert!(epic_names.contains(&"create_epic"));
        assert!(epic_names.contains(&"update_epic"));
    }
}
