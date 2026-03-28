//! GitHub schema enricher.
//!
//! Removes parameters not supported by GitHub and adjusts GitHub-specific behavior.

use devboy_core::{ToolCategory, ToolEnricher, ToolSchema};
use serde_json::Value;

/// Static schema enricher for GitHub provider.
///
/// GitHub doesn't support:
/// - `priority` (no built-in priority on issues)
/// - `parentId` (sub-issues are relatively new and limited)
/// - `customFields` (no custom fields)
/// - `issueType` (no issue types)
/// - `components` (no components)
/// - `projectId` (not applicable)
/// - `points` (no story points)
/// - `link_issues` tool (not supported via API — use #123 mentions instead)
pub struct GitHubSchemaEnricher;

const ISSUE_TOOLS: &[&str] = &["create_issue", "update_issue", "get_issues"];

/// Parameters to remove from issue tools.
const ISSUE_REMOVE_PARAMS: &[&str] = &[
    "priority",
    "parentId",
    "customFields",
    "issueType",
    "components",
    "projectId",
    "points",
];

/// Parameters to remove from get_issues specifically.
const GET_ISSUES_REMOVE_PARAMS: &[&str] = &["projectKey", "nativeQuery", "stateCategory"];

impl ToolEnricher for GitHubSchemaEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &[ToolCategory::IssueTracker, ToolCategory::GitRepository]
    }

    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema) {
        // Remove unsupported params from issue tools
        if ISSUE_TOOLS.contains(&tool_name) {
            schema.remove_params(ISSUE_REMOVE_PARAMS);
        }

        // Additional removals for get_issues
        if tool_name == "get_issues" {
            schema.remove_params(GET_ISSUES_REMOVE_PARAMS);
        }

        // link_issues is not supported by GitHub API — will be filtered by unsupported_tools()
        if tool_name == "link_issues" {
            schema.remove_params(&["source_key", "target_key", "link_type"]);
        }
    }

    fn transform_args(&self, tool_name: &str, args: &mut Value) {
        // Map line_type to GitHub side parameter for code comments
        if tool_name == "create_merge_request_comment" {
            if let Some(obj) = args.as_object_mut() {
                if let Some(line_type) = obj.get("line_type").and_then(|v| v.as_str()) {
                    let side = match line_type {
                        "old" => "LEFT",
                        _ => "RIGHT",
                    };
                    obj.insert("side".into(), Value::String(side.into()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_github_enricher_removes_unsupported_params() {
        let enricher = GitHubSchemaEnricher;
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "priority": { "type": "string" },
                "parentId": { "type": "string" },
                "customFields": { "type": "object" },
            },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        assert!(schema.properties.contains_key("title"));
        assert!(!schema.properties.contains_key("priority"));
        assert!(!schema.properties.contains_key("parentId"));
        assert!(!schema.properties.contains_key("customFields"));
    }

    #[test]
    fn test_github_enricher_transforms_line_type_to_side() {
        let enricher = GitHubSchemaEnricher;
        let mut args = json!({
            "key": "pr#1",
            "body": "test",
            "file_path": "src/main.rs",
            "line": 10,
            "line_type": "old",
        });

        enricher.transform_args("create_merge_request_comment", &mut args);

        assert_eq!(args["side"], "LEFT");
    }

    #[test]
    fn test_github_enricher_transforms_new_line_to_right() {
        let enricher = GitHubSchemaEnricher;
        let mut args = json!({
            "key": "pr#1",
            "body": "test",
            "line_type": "new",
        });

        enricher.transform_args("create_merge_request_comment", &mut args);

        assert_eq!(args["side"], "RIGHT");
    }

    #[test]
    fn test_github_enricher_no_transform_for_other_tools() {
        let enricher = GitHubSchemaEnricher;
        let mut args = json!({"line_type": "old"});
        enricher.transform_args("get_issues", &mut args);
        // No side added for non-comment tools
        assert!(args.get("side").is_none());
    }

    #[test]
    fn test_github_enricher_no_transform_without_line_type() {
        let enricher = GitHubSchemaEnricher;
        let mut args = json!({"key": "pr#1", "body": "test"});
        enricher.transform_args("create_merge_request_comment", &mut args);
        // No line_type → no side
        assert!(args.get("side").is_none());
    }

    #[test]
    fn test_github_enricher_get_issues_removals() {
        let enricher = GitHubSchemaEnricher;
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "state": { "type": "string" },
                "projectKey": { "type": "string" },
                "nativeQuery": { "type": "string" },
                "stateCategory": { "type": "string" },
            }
        }));
        enricher.enrich_schema("get_issues", &mut schema);
        assert!(schema.properties.contains_key("state"));
        assert!(!schema.properties.contains_key("projectKey"));
        assert!(!schema.properties.contains_key("nativeQuery"));
        assert!(!schema.properties.contains_key("stateCategory"));
    }

    #[test]
    fn test_github_enricher_link_issues_unsupported() {
        let enricher = GitHubSchemaEnricher;
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "target_key": { "type": "string" },
                "link_type": { "type": "string" }
            }
        }));
        enricher.enrich_schema("link_issues", &mut schema);
        assert!(!schema.properties.contains_key("target_key"));
        assert!(!schema.properties.contains_key("link_type"));
    }
}
