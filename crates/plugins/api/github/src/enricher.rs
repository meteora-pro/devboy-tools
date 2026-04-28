//! GitHub schema enricher.
//!
//! Removes parameters not supported by GitHub and adjusts GitHub-specific behavior.

use devboy_core::{
    CostModel, FollowUpLink, SideEffectClass, ToolCategory, ToolEnricher, ToolSchema,
    ToolValueModel, ValueClass,
};
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
        if tool_name == "create_merge_request_comment"
            && let Some(obj) = args.as_object_mut()
            && let Some(line_type) = obj.get("line_type").and_then(|v| v.as_str())
        {
            let side = match line_type {
                "old" => "LEFT",
                _ => "RIGHT",
            };
            obj.insert("side".into(), Value::String(side.into()));
        }
    }

    /// Paper 3 — value-model annotations for GitHub read-only tools.
    /// Mirrors the GitLab structure (PRs/issues/comments) — they share
    /// the same `list → detail → comments` shape.
    fn value_model(&self, tool_name: &str) -> Option<ToolValueModel> {
        let model = match tool_name {
            "get_merge_requests" => ToolValueModel {
                value_class: ValueClass::Supporting,
                cost_model: CostModel {
                    typical_kb: 4.5,
                    max_kb: Some(45.0),
                    latency_ms_p50: Some(380),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![
                    FollowUpLink {
                        tool: "get_merge_request_discussions".into(),
                        probability: 0.60,
                        projection: Some("number".into()),
                        projection_arg: Some("pull_request_number".into()),
                    },
                    FollowUpLink {
                        tool: "get_merge_request_diffs".into(),
                        probability: 0.40,
                        projection: Some("number".into()),
                        projection_arg: Some("pull_request_number".into()),
                    },
                ],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_merge_request" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 1.6,
                    latency_ms_p50: Some(220),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![FollowUpLink {
                    tool: "get_merge_request_discussions".into(),
                    probability: 0.55,
                    projection: Some("number".into()),
                    projection_arg: Some("pull_request_number".into()),
                }],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_merge_request_discussions" | "get_merge_request_diffs" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 6.0,
                    max_kb: Some(60.0),
                    latency_ms_p50: Some(360),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_issues" => ToolValueModel {
                value_class: ValueClass::Supporting,
                cost_model: CostModel {
                    typical_kb: 3.5,
                    latency_ms_p50: Some(380),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![FollowUpLink {
                    tool: "get_issue_comments".into(),
                    probability: 0.45,
                    projection: Some("number".into()),
                    projection_arg: Some("issue_number".into()),
                }],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_issue" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 1.0,
                    latency_ms_p50: Some(180),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![FollowUpLink {
                    tool: "get_issue_comments".into(),
                    probability: 0.50,
                    projection: Some("number".into()),
                    projection_arg: Some("issue_number".into()),
                }],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_issue_comments" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 2.5,
                    max_kb: Some(20.0),
                    latency_ms_p50: Some(280),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "create_issue"
            | "update_issue"
            | "create_merge_request"
            | "create_merge_request_comment"
            | "add_issue_comment" => ToolValueModel {
                value_class: ValueClass::Supporting,
                cost_model: CostModel {
                    typical_kb: 0.8,
                    latency_ms_p50: Some(350),
                    ..CostModel::default()
                },
                side_effect_class: SideEffectClass::MutatesExternal,
                ..ToolValueModel::default()
            },
            _ => return None,
        };
        Some(model)
    }

    /// GitHub SaaS = `api.github.com`. Self-hosted Enterprise users
    /// can override per-tool via TOML; we don't read args because the
    /// host is a session-level fixed value.
    fn rate_limit_host(&self, _tool_name: &str, _args: &Value) -> Option<String> {
        Some("api.github.com".into())
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

    // ─── Paper 3 — value_model annotations ───────────────────────────

    #[test]
    fn paper3_get_merge_requests_chains_to_discussions_with_pr_number() {
        let m = GitHubSchemaEnricher
            .value_model("get_merge_requests")
            .unwrap();
        assert_eq!(m.side_effect_class, SideEffectClass::ReadOnly);
        let link = m
            .follow_up
            .iter()
            .find(|l| l.tool == "get_merge_request_discussions")
            .unwrap();
        assert_eq!(link.projection_arg.as_deref(), Some("pull_request_number"));
    }

    #[test]
    fn paper3_get_issues_chains_to_comments_with_issue_number() {
        let m = GitHubSchemaEnricher.value_model("get_issues").unwrap();
        let link = m
            .follow_up
            .iter()
            .find(|l| l.tool == "get_issue_comments")
            .unwrap();
        assert_eq!(link.projection_arg.as_deref(), Some("issue_number"));
    }

    #[test]
    fn paper3_mutating_endpoints_are_never_speculatable() {
        for tool in [
            "create_issue",
            "update_issue",
            "create_merge_request",
            "create_merge_request_comment",
            "add_issue_comment",
        ] {
            let m = GitHubSchemaEnricher.value_model(tool).unwrap();
            assert_eq!(m.side_effect_class, SideEffectClass::MutatesExternal);
            assert!(!m.is_speculatable());
        }
    }

    #[test]
    fn paper3_rate_limit_host_is_api_github_com() {
        let host = GitHubSchemaEnricher.rate_limit_host("get_issues", &json!({}));
        assert_eq!(host.as_deref(), Some("api.github.com"));
    }
}
