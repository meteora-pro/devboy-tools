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
                s
            },
        },
        ToolDefinition {
            name: "get_issue".into(),
            description: "Get a single issue by key (e.g., 'gh#123', 'gitlab#456', 'CU-abc', 'jira#PROJ-123').".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("Issue key"));
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
            name: "create_issue".into(),
            description: "Create a new issue in the configured provider.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("title", PropertySchema::string("Issue title"));
                s.add_property("description", PropertySchema::string("Issue description/body"));
                s.add_property("labels", PropertySchema::array(PropertySchema::string("label"), "Labels to add"));
                s.add_property("assignees", PropertySchema::array(PropertySchema::string("assignee"), "Assignee usernames"));
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
                s.set_required("key", true);
                s
            },
        },
        ToolDefinition {
            name: "add_issue_comment".into(),
            description: "Add a comment to an issue.".into(),
            category: ToolCategory::IssueTracker,
            input_schema: {
                let mut s = ToolSchema::new();
                s.add_property("key", PropertySchema::string("Issue key"));
                s.add_property("body", PropertySchema::string("Comment text"));
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
                s.add_property("key", PropertySchema::string("Epic key"));
                s.add_property("title", PropertySchema::string("New title"));
                s.add_property("description", PropertySchema::string("New description"));
                s.set_required("key", true);
                s
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_definitions_count() {
        let tools = base_tool_definitions();
        assert_eq!(tools.len(), 20);
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
            "create_issue",
            "update_issue",
            "add_issue_comment",
            "get_available_statuses",
            "get_users",
            "link_issues",
        ];
        let git_repository_tools = [
            "get_merge_requests",
            "get_merge_request",
            "get_merge_request_discussions",
            "get_merge_request_diffs",
            "create_merge_request",
            "create_merge_request_comment",
            "get_pipeline",
            "get_job_logs",
        ];
        let epics_tools = ["get_epics", "create_epic", "update_epic"];

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

        // get_issues should have state, search, labels, assignee, limit, offset, sort_by, sort_order
        assert!(json.contains("state"));
        assert!(json.contains("search"));
        assert!(json.contains("labels"));
        assert!(json.contains("assignee"));
        assert!(json.contains("limit"));
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
