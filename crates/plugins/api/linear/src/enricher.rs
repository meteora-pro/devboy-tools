//! Linear schema enricher.
//!
//! Static enrichment for the Linear issue-tracker provider.
//!
//! Linear supports the common issue-tool surface but not Jira-only
//! fields such as issue types or project keys on read paths. It also
//! exposes a fixed priority scale and semantic status categories that
//! are worth surfacing directly in the tool schema even without cached
//! per-team workflow metadata.

use devboy_core::{ToolCategory, ToolEnricher, ToolSchema};
use serde_json::Value;

pub struct LinearSchemaEnricher;

const CREATE_UPDATE_REMOVE_PARAMS: &[&str] = &["issueType"];
const GET_ISSUES_REMOVE_PARAMS: &[&str] = &["projectKey", "nativeQuery"];

const PRIORITY_VALUES: &[&str] = &["urgent", "high", "normal", "low"];
const STATE_CATEGORY_VALUES: &[&str] = &["backlog", "todo", "in_progress", "done", "cancelled"];
const LABELS_OPERATOR_VALUES: &[&str] = &["and", "or"];

impl ToolEnricher for LinearSchemaEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &[ToolCategory::IssueTracker]
    }

    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema) {
        if tool_name == "create_issue" || tool_name == "update_issue" {
            schema.remove_params(CREATE_UPDATE_REMOVE_PARAMS);
            schema.add_enum_param(
                "priority",
                PRIORITY_VALUES,
                "Linear priority. Available: urgent, high, normal, low",
            );

            if tool_name == "create_issue" {
                schema.set_description(
                    "projectId",
                    "Optional Linear project ID (UUID). Overrides the default team-only placement when supplied.",
                );
                schema.set_description(
                    "parentId",
                    "Parent Linear issue key or native UUID. Creates a sub-issue when supplied.",
                );
            } else {
                schema.set_description(
                    "status",
                    "Exact Linear workflow state name for this team (for example, \"In Review\"). Takes precedence over `state`. Use `get_available_statuses` to discover valid names.",
                );
                schema.set_description(
                    "state",
                    "Generic state shortcut. `open` maps to a non-completed workflow state; `closed` maps to a completed workflow state. For an exact Linear workflow state name, use `status` instead.",
                );
                schema.set_description(
                    "parentId",
                    "Parent Linear issue key or native UUID. Set to `none` or empty string to detach from the current parent.",
                );
            }
        }

        if tool_name == "get_issues" {
            schema.remove_params(GET_ISSUES_REMOVE_PARAMS);
            schema.add_enum_param(
                "stateCategory",
                STATE_CATEGORY_VALUES,
                "Filter by semantic Linear workflow category: backlog, todo, in_progress, done, cancelled.",
            );
            schema.add_enum_param(
                "labelsOperator",
                LABELS_OPERATOR_VALUES,
                "Label matching logic: `and` requires all labels, `or` requires any label (default: `or`).",
            );
            schema.set_description(
                "state",
                "Filter by issue state. Built-in values: `open`, `closed`, `all`. You can also pass an exact Linear workflow state name.",
            );
            schema.set_description(
                "assignee",
                "Filter by assignee name, display name, or email.",
            );
        }
    }

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn linear_enricher_adds_issue_filters_and_priority_enums() {
        let enricher = LinearSchemaEnricher;

        let mut get_issues = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "state": { "type": "string" },
                "projectKey": { "type": "string" },
                "nativeQuery": { "type": "string" }
            }
        }));
        enricher.enrich_schema("get_issues", &mut get_issues);

        assert!(!get_issues.properties.contains_key("projectKey"));
        assert!(!get_issues.properties.contains_key("nativeQuery"));
        assert!(get_issues.properties.contains_key("stateCategory"));
        assert!(get_issues.properties.contains_key("labelsOperator"));

        let mut create_issue = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "priority": { "type": "string" },
                "issueType": { "type": "string" },
                "projectId": { "type": "string" },
                "parentId": { "type": "string" }
            }
        }));
        enricher.enrich_schema("create_issue", &mut create_issue);

        assert!(!create_issue.properties.contains_key("issueType"));
        let priority = create_issue.properties.get("priority").unwrap();
        assert_eq!(
            priority.enum_values.as_ref().unwrap(),
            &vec![
                "urgent".to_string(),
                "high".to_string(),
                "normal".to_string(),
                "low".to_string()
            ]
        );
        assert!(
            create_issue.properties["projectId"]
                .description
                .as_deref()
                .unwrap()
                .contains("UUID")
        );
    }

    #[test]
    fn linear_enricher_updates_status_descriptions() {
        let enricher = LinearSchemaEnricher;
        let mut update_issue = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "state": { "type": "string" },
                "status": { "type": "string" },
                "priority": { "type": "string" },
                "parentId": { "type": "string" },
                "issueType": { "type": "string" }
            }
        }));

        enricher.enrich_schema("update_issue", &mut update_issue);

        assert!(!update_issue.properties.contains_key("issueType"));
        assert!(
            update_issue.properties["status"]
                .description
                .as_deref()
                .unwrap()
                .contains("get_available_statuses")
        );
        assert!(
            update_issue.properties["state"]
                .description
                .as_deref()
                .unwrap()
                .contains("Generic state shortcut")
        );
    }
}
