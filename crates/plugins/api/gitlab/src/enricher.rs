//! GitLab schema enricher.
//!
//! Removes parameters not supported by GitLab and adds GitLab-specific enums.

use devboy_core::{
    CostModel, FollowUpLink, SideEffectClass, ToolCategory, ToolEnricher, ToolSchema,
    ToolValueModel, ValueClass,
};
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

    /// Paper 3 — value-model annotations for GitLab read-only tools.
    ///
    /// Speculative pre-fetch wins for the canonical `list → detail`
    /// chains: after `get_merge_requests` the agent almost always
    /// reads discussions / diffs of the top hit; after `get_issues`
    /// it reads comments. We annotate the read-only endpoints (Pure
    /// for inside-of-TTL, ReadOnly otherwise) and leave mutating
    /// endpoints (`create_issue`, `update_issue`, …) as the default
    /// `Indeterminate` so they are never speculated.
    fn value_model(&self, tool_name: &str) -> Option<ToolValueModel> {
        let model = match tool_name {
            "get_merge_requests" => ToolValueModel {
                value_class: ValueClass::Supporting,
                cost_model: CostModel {
                    typical_kb: 4.0,
                    max_kb: Some(40.0),
                    latency_ms_p50: Some(450),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![
                    FollowUpLink {
                        tool: "get_merge_request_discussions".into(),
                        probability: 0.62,
                        projection: Some("iid".into()),
                        projection_arg: Some("key".into()),
                    },
                    FollowUpLink {
                        tool: "get_merge_request_diffs".into(),
                        probability: 0.41,
                        projection: Some("iid".into()),
                        projection_arg: Some("key".into()),
                    },
                ],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_merge_request" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 1.5,
                    latency_ms_p50: Some(220),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![FollowUpLink {
                    tool: "get_merge_request_discussions".into(),
                    probability: 0.55,
                    projection: Some("iid".into()),
                    projection_arg: Some("key".into()),
                }],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_merge_request_discussions" | "get_merge_request_diffs" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 6.0,
                    max_kb: Some(60.0),
                    latency_ms_p50: Some(380),
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
                    max_kb: Some(35.0),
                    latency_ms_p50: Some(420),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![FollowUpLink {
                    tool: "get_issue_comments".into(),
                    probability: 0.48,
                    projection: Some("iid".into()),
                    projection_arg: Some("key".into()),
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
                    projection: Some("iid".into()),
                    projection_arg: Some("key".into()),
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
            // Mutating endpoints — explicit MutatesExternal so they
            // are never speculated even by accident.
            "create_issue"
            | "update_issue"
            | "create_merge_request"
            | "create_merge_request_comment"
            | "add_issue_comment"
            | "link_issues"
            | "run_pipeline_job" => ToolValueModel {
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

    /// Paper 3 — `gitlab.com` for SaaS, picked up from the runtime
    /// args if the tool carries an explicit instance URL. We don't
    /// look at `args` here because GitLab tools live behind one
    /// configured client per session; the host falls back to the
    /// tool's static `rate_limit_host` annotation.
    fn rate_limit_host(&self, _tool_name: &str, _args: &Value) -> Option<String> {
        // Static host is not embedded — GitLab self-hosted instances
        // vary per deployment. Operators that need rate-limit grouping
        // for self-hosted GitLab set `[tools.<name>].rate_limit_host`
        // in pipeline_config.toml. SaaS users get the default `None`
        // here and the dispatcher leaves it uncapped.
        None
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

    // ─── Paper 3 — value_model annotations ───────────────────────────

    #[test]
    fn paper3_get_merge_requests_is_read_only_with_discussion_followup() {
        let m = GitLabSchemaEnricher
            .value_model("get_merge_requests")
            .expect("get_merge_requests must be annotated");
        assert_eq!(m.side_effect_class, SideEffectClass::ReadOnly);
        assert!(m.is_speculatable());
        let link = m
            .follow_up
            .iter()
            .find(|l| l.tool == "get_merge_request_discussions")
            .expect("discussions follow-up missing");
        assert_eq!(link.projection.as_deref(), Some("iid"));
        assert_eq!(link.projection_arg.as_deref(), Some("key"));
        assert!(link.probability >= 0.5);
    }

    #[test]
    fn paper3_get_issues_chains_to_comments_with_iid_to_issue_id() {
        let m = GitLabSchemaEnricher.value_model("get_issues").unwrap();
        assert_eq!(m.side_effect_class, SideEffectClass::ReadOnly);
        let link = m
            .follow_up
            .iter()
            .find(|l| l.tool == "get_issue_comments")
            .expect("comments follow-up missing");
        assert_eq!(link.projection_arg.as_deref(), Some("key"));
    }

    #[test]
    fn paper3_mutating_endpoints_are_never_speculatable() {
        for tool in [
            "create_issue",
            "update_issue",
            "create_merge_request",
            "create_merge_request_comment",
            "add_issue_comment",
            "link_issues",
            "run_pipeline_job",
        ] {
            let m = GitLabSchemaEnricher
                .value_model(tool)
                .unwrap_or_else(|| panic!("{tool} must be annotated"));
            assert_eq!(
                m.side_effect_class,
                SideEffectClass::MutatesExternal,
                "{tool} must be MutatesExternal — never speculate writes"
            );
            assert!(!m.is_speculatable());
        }
    }
}
