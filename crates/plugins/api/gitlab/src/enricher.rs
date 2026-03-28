//! GitLab schema enricher.
//!
//! Removes parameters not supported by GitLab and adds GitLab-specific enums.

use devboy_core::{ToolCategory, ToolEnricher, ToolSchema};
use serde_json::Value;

/// Static schema enricher for GitLab provider.
///
/// GitLab doesn't support:
/// - `priority` (no built-in priority on issues)
/// - `parentId` (no subtask hierarchy via API)
/// - `customFields` (no custom fields)
/// - `issueType` (no issue types)
/// - `components` (no components)
/// - `projectId` (single project scope, not needed)
/// - `points` (no story points)
pub struct GitLabSchemaEnricher;

const ISSUE_TOOLS: &[&str] = &["create_issue", "update_issue", "get_issues", "link_issues"];

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

impl ToolEnricher for GitLabSchemaEnricher {
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

        // Add link types enum for link_issues
        if tool_name == "link_issues" {
            schema.add_enum_param(
                "link_type",
                &["relates_to", "blocks", "is_blocked_by"],
                "Link type between issues",
            );
        }
    }

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {
        // GitLab doesn't need arg transformation — no custom fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_gitlab_enricher_removes_unsupported_params() {
        let enricher = GitLabSchemaEnricher;
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "priority": { "type": "string" },
                "parentId": { "type": "string" },
                "customFields": { "type": "object" },
                "issueType": { "type": "string" },
                "components": { "type": "array" },
                "projectId": { "type": "string" },
                "points": { "type": "number" },
            },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        assert!(schema.properties.contains_key("title"));
        assert!(!schema.properties.contains_key("priority"));
        assert!(!schema.properties.contains_key("parentId"));
        assert!(!schema.properties.contains_key("customFields"));
        assert!(!schema.properties.contains_key("issueType"));
        assert!(!schema.properties.contains_key("components"));
        assert!(!schema.properties.contains_key("projectId"));
        assert!(!schema.properties.contains_key("points"));
    }

    #[test]
    fn test_gitlab_enricher_adds_link_types() {
        let enricher = GitLabSchemaEnricher;
        let mut schema = ToolSchema::new();

        enricher.enrich_schema("link_issues", &mut schema);

        let link_type = schema.properties.get("link_type").unwrap();
        assert_eq!(
            link_type.enum_values,
            Some(vec![
                "relates_to".into(),
                "blocks".into(),
                "is_blocked_by".into()
            ])
        );
    }

    #[test]
    fn test_gitlab_enricher_get_issues_extra_removals() {
        let enricher = GitLabSchemaEnricher;
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "state": { "type": "string" },
                "projectKey": { "type": "string" },
                "nativeQuery": { "type": "string" },
                "stateCategory": { "type": "string" },
            },
        }));

        enricher.enrich_schema("get_issues", &mut schema);

        assert!(schema.properties.contains_key("state"));
        assert!(!schema.properties.contains_key("projectKey"));
        assert!(!schema.properties.contains_key("nativeQuery"));
        assert!(!schema.properties.contains_key("stateCategory"));
    }
}
