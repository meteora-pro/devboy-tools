//! Built-in tool registry for MCP server.
//!
//! Tool execution is delegated to `devboy_executor::Executor`.
//! This module provides the list of known built-in tool names
//! for validation and configuration purposes.

/// All known built-in tool names (provider tools + context management tools).
///
/// This list must stay in sync with `devboy_executor::tools::base_tool_definitions()`
/// plus the context management tools handled by McpServer.
pub const KNOWN_BUILTIN_TOOLS: &[&str] = &[
    // Issue tracker tools
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
    // Git repository tools (MR/PR, pipeline)
    "get_merge_requests",
    "get_merge_request",
    "get_merge_request_discussions",
    "get_merge_request_diffs",
    "create_merge_request",
    "create_merge_request_comment",
    "update_merge_request",
    "get_pipeline",
    "get_job_logs",
    // Epic tools
    "get_epics",
    "create_epic",
    "update_epic",
    // Meeting notes tools
    "get_meeting_notes",
    "get_meeting_transcript",
    "search_meeting_notes",
    // Messenger tools
    "get_messenger_chats",
    "get_chat_messages",
    "search_chat_messages",
    "send_message",
    // Jira Structure plugin tools
    "get_structures",
    "get_structure_forest",
    "add_structure_rows",
    "move_structure_rows",
    "remove_structure_row",
    "get_structure_values",
    "get_structure_views",
    "save_structure_view",
    "create_structure",
    // Context management (handled by McpServer directly)
    "list_contexts",
    "use_context",
    "get_current_context",
    // Layered-pipeline lifecycle (Paper 2 §Implementation Status)
    "compact_pipeline_cache",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_builtin_tools_includes_context_tools() {
        assert!(KNOWN_BUILTIN_TOOLS.contains(&"list_contexts"));
        assert!(KNOWN_BUILTIN_TOOLS.contains(&"use_context"));
        assert!(KNOWN_BUILTIN_TOOLS.contains(&"get_current_context"));
    }

    #[test]
    fn test_known_builtin_tools_includes_provider_tools() {
        assert!(KNOWN_BUILTIN_TOOLS.contains(&"get_issues"));
        assert!(KNOWN_BUILTIN_TOOLS.contains(&"get_merge_requests"));
        assert!(KNOWN_BUILTIN_TOOLS.contains(&"get_meeting_notes"));
        assert!(KNOWN_BUILTIN_TOOLS.contains(&"get_messenger_chats"));
        assert!(KNOWN_BUILTIN_TOOLS.contains(&"get_epics"));
    }

    /// Verify that KNOWN_BUILTIN_TOOLS stays in sync with executor's base definitions.
    #[test]
    fn test_known_builtin_tools_matches_executor() {
        let base_tools = devboy_executor::tools::base_tool_definitions();
        let context_tools = &[
            "list_contexts",
            "use_context",
            "get_current_context",
            "compact_pipeline_cache",
        ];

        // Every base tool should be in KNOWN_BUILTIN_TOOLS
        for tool in &base_tools {
            assert!(
                KNOWN_BUILTIN_TOOLS.contains(&tool.name.as_str()),
                "executor tool '{}' missing from KNOWN_BUILTIN_TOOLS",
                tool.name
            );
        }

        // Every non-context tool in KNOWN_BUILTIN_TOOLS should be in base_tools
        for name in KNOWN_BUILTIN_TOOLS {
            if context_tools.contains(name) {
                continue;
            }
            assert!(
                base_tools.iter().any(|t| t.name == *name),
                "KNOWN_BUILTIN_TOOLS contains '{}' which is not in executor base definitions",
                name
            );
        }
    }

    #[test]
    fn test_known_builtin_tools_no_duplicates() {
        let mut sorted: Vec<&str> = KNOWN_BUILTIN_TOOLS.to_vec();
        let original_len = sorted.len();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            original_len,
            "should have no duplicate tool names"
        );
    }
}
