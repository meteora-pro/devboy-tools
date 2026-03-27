//! Jira schema enricher.
//!
//! Dynamic enricher supporting single-project and multi-project configurations.

use devboy_core::{sanitize_field_name, ToolEnricher, ToolSchema};
use serde_json::{json, Value};

use crate::metadata::{JiraFieldType, JiraMetadata};

/// Dynamic schema enricher for Jira provider.
///
/// Single-project mode: removes `projectId`, adds enums, replaces customFields with cf_*.
/// Multi-project mode: keeps `projectId` as enum, shows known values in descriptions.
pub struct JiraSchemaEnricher {
    metadata: JiraMetadata,
}

impl JiraSchemaEnricher {
    pub fn new(metadata: JiraMetadata) -> Self {
        Self { metadata }
    }
}

const ISSUE_TOOLS: &[&str] = &["create_issue", "update_issue", "get_issues", "link_issues"];

const REMOVE_PARAMS: &[&str] = &["points"];
const GET_ISSUES_REMOVE_PARAMS: &[&str] = &["stateCategory"];

impl ToolEnricher for JiraSchemaEnricher {
    fn supported_tools(&self) -> &[&str] {
        ISSUE_TOOLS
    }

    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema) {
        schema.remove_params(REMOVE_PARAMS);

        if tool_name == "get_issues" {
            schema.remove_params(GET_ISSUES_REMOVE_PARAMS);
        }

        let is_single = self.metadata.is_single_project();

        // Project key handling
        if is_single {
            schema.remove_params(&["projectId"]);
        } else {
            let keys: Vec<String> = self
                .metadata
                .project_keys()
                .iter()
                .map(|k| k.to_string())
                .collect();
            if !keys.is_empty() {
                schema.set_enum("projectId", &keys);
                let desc = format!("REQUIRED. Jira project key. Available: {}", keys.join(", "));
                schema.set_description("projectId", &desc);
                schema.set_required("projectId", true);
            }
        }

        // Issue types enum (non-subtask)
        let issue_types = self.metadata.all_issue_types();
        if !issue_types.is_empty() {
            schema.set_enum("issueType", &issue_types);
            let desc = format!("Issue type. Available: {}", issue_types.join(", "));
            schema.set_description("issueType", &desc);
        }

        // Priorities enum with alias hints
        let priorities = self.metadata.all_priorities();
        if !priorities.is_empty() {
            schema.set_enum("priority", &priorities);
            let desc = format!(
                "Priority. Available: {}. Aliases: urgent\u{2192}Highest, high\u{2192}High, normal\u{2192}Medium, low\u{2192}Low",
                priorities.join(", ")
            );
            schema.set_description("priority", &desc);
        }

        // Components enum
        let components = self.metadata.all_components();
        if !components.is_empty() {
            schema.set_enum("components", &components);
            let desc = format!("Components. Available: {}", components.join(", "));
            schema.set_description("components", &desc);
        }

        // Link types
        if tool_name == "link_issues" {
            let link_types = self.metadata.all_link_types();
            if !link_types.is_empty() {
                let values: Vec<&str> = link_types.iter().map(|s| s.as_str()).collect();
                schema.add_enum_param("link_type", &values, "Issue link type");
            }
        }

        // Custom fields (only for single-project, too complex to merge for multi)
        if (tool_name == "create_issue" || tool_name == "update_issue") && is_single {
            schema.remove_params(&["customFields"]);

            if let Some(project_meta) = self.metadata.projects.values().next() {
                for field in &project_meta.custom_fields {
                    let param_name = sanitize_field_name(&field.name);
                    let field_schema = jira_custom_field_to_schema(field);
                    schema.add_param(&param_name, field_schema);
                }
            }
        }
    }

    fn transform_args(&self, tool_name: &str, args: &mut Value) {
        if tool_name != "create_issue" && tool_name != "update_issue" {
            return;
        }

        // Transform priority aliases
        if let Some(obj) = args.as_object_mut() {
            if let Some(priority) = obj.get("priority").and_then(|v| v.as_str()) {
                let mapped = match priority {
                    "urgent" => "Highest",
                    "high" => "High",
                    "normal" => "Medium",
                    "low" => "Low",
                    other => other,
                };
                obj.insert("priority".into(), json!(mapped));
            }
        }

        // Transform cf_* params to customFields for single-project
        if !self.metadata.is_single_project() {
            return;
        }

        let Some(project_meta) = self.metadata.projects.values().next() else {
            return;
        };

        let Some(obj) = args.as_object_mut() else {
            return;
        };

        let mut custom_fields = serde_json::Map::new();
        let mut cf_keys_to_remove: Vec<String> = Vec::new();

        for field in &project_meta.custom_fields {
            let param_name = sanitize_field_name(&field.name);
            if let Some(value) = obj.get(&param_name) {
                let transformed = field.transform_value(value);
                custom_fields.insert(field.id.clone(), transformed);
                cf_keys_to_remove.push(param_name);
            }
        }

        for key in cf_keys_to_remove {
            obj.remove(&key);
        }
        if !custom_fields.is_empty() {
            obj.insert("customFields".into(), Value::Object(custom_fields));
        }
    }
}

/// Convert a Jira custom field definition to a JSON Schema property.
fn jira_custom_field_to_schema(field: &crate::metadata::JiraCustomField) -> Value {
    match field.field_type {
        JiraFieldType::Option => {
            let options: Vec<&str> = field.options.iter().map(|o| o.name.as_str()).collect();
            json!({
                "type": "string",
                "enum": options,
                "description": format!("Custom field: {} (select). Choose one option.", field.name),
                "x-enriched": true,
            })
        }
        JiraFieldType::Array => {
            let options: Vec<&str> = field.options.iter().map(|o| o.name.as_str()).collect();
            json!({
                "type": "array",
                "items": { "type": "string", "enum": options },
                "description": format!("Custom field: {} (multi-select). Choose one or more.", field.name),
                "x-enriched": true,
            })
        }
        JiraFieldType::Number => json!({
            "type": "number",
            "description": format!("Custom field: {} (number).", field.name),
            "x-enriched": true,
        }),
        JiraFieldType::Date => json!({
            "type": "string",
            "description": format!("Custom field: {} (date, YYYY-MM-DD).", field.name),
            "x-enriched": true,
        }),
        JiraFieldType::DateTime => json!({
            "type": "string",
            "description": format!("Custom field: {} (datetime, ISO 8601).", field.name),
            "x-enriched": true,
        }),
        JiraFieldType::String | JiraFieldType::Any => json!({
            "type": "string",
            "description": format!("Custom field: {} (text).", field.name),
            "x-enriched": true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::*;
    use std::collections::HashMap;

    fn single_project_metadata() -> JiraMetadata {
        let mut projects = HashMap::new();
        projects.insert(
            "PROJ".into(),
            JiraProjectMetadata {
                issue_types: vec![
                    JiraIssueType {
                        id: "1".into(),
                        name: "Task".into(),
                        subtask: false,
                    },
                    JiraIssueType {
                        id: "2".into(),
                        name: "Bug".into(),
                        subtask: false,
                    },
                    JiraIssueType {
                        id: "3".into(),
                        name: "Sub-task".into(),
                        subtask: true,
                    },
                ],
                priorities: vec![
                    JiraPriority {
                        id: "1".into(),
                        name: "Highest".into(),
                    },
                    JiraPriority {
                        id: "2".into(),
                        name: "High".into(),
                    },
                    JiraPriority {
                        id: "3".into(),
                        name: "Medium".into(),
                    },
                    JiraPriority {
                        id: "4".into(),
                        name: "Low".into(),
                    },
                ],
                components: vec![
                    JiraComponent {
                        id: "10".into(),
                        name: "API".into(),
                    },
                    JiraComponent {
                        id: "11".into(),
                        name: "Frontend".into(),
                    },
                ],
                link_types: vec![JiraLinkType {
                    id: "1".into(),
                    name: "Blocks".into(),
                    outward: Some("blocks".into()),
                    inward: Some("is blocked by".into()),
                }],
                custom_fields: vec![JiraCustomField {
                    id: "customfield_10001".into(),
                    name: "Story Points".into(),
                    field_type: JiraFieldType::Number,
                    required: false,
                    options: vec![],
                }],
            },
        );
        JiraMetadata {
            flavor: JiraFlavor::Cloud,
            projects,
        }
    }

    #[test]
    fn test_jira_enricher_single_project_removes_project_id() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "projectId": { "type": "string" },
                "issueType": { "type": "string" },
                "priority": { "type": "string" },
            },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        assert!(!schema.properties.contains_key("projectId"));
        assert_eq!(
            schema.properties["issueType"].enum_values,
            Some(vec!["Bug".into(), "Task".into()]) // sorted, no Sub-task
        );
        assert_eq!(
            schema.properties["priority"].enum_values,
            Some(vec![
                "High".into(),
                "Highest".into(),
                "Low".into(),
                "Medium".into()
            ]) // sorted
        );
    }

    #[test]
    fn test_jira_enricher_adds_custom_fields() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "customFields": { "type": "object" },
            },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        assert!(!schema.properties.contains_key("customFields"));
        assert!(schema.properties.contains_key("cf_story_points"));
        assert_eq!(schema.properties["cf_story_points"].schema_type, "number");
    }

    #[test]
    fn test_jira_enricher_transform_priority_alias() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut args = json!({ "title": "Test", "priority": "urgent" });

        enricher.transform_args("create_issue", &mut args);

        assert_eq!(args["priority"], "Highest");
    }

    #[test]
    fn test_jira_enricher_transform_custom_fields() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut args = json!({
            "title": "Test",
            "cf_story_points": 8,
        });

        enricher.transform_args("create_issue", &mut args);

        assert!(args.get("cf_story_points").is_none());
        assert_eq!(args["customFields"]["customfield_10001"], 8);
    }

    #[test]
    fn test_jira_enricher_multi_project_keeps_project_id() {
        let mut meta = single_project_metadata();
        meta.projects.insert(
            "INFRA".into(),
            JiraProjectMetadata {
                issue_types: vec![],
                priorities: vec![],
                components: vec![],
                link_types: vec![],
                custom_fields: vec![],
            },
        );
        let enricher = JiraSchemaEnricher::new(meta);
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "projectId": { "type": "string" },
                "customFields": { "type": "object" },
            },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        // projectId kept as enum
        assert!(schema.properties.contains_key("projectId"));
        let project_enum = schema.properties["projectId"].enum_values.as_ref().unwrap();
        assert!(project_enum.contains(&"PROJ".to_string()));
        assert!(project_enum.contains(&"INFRA".to_string()));

        // customFields NOT replaced (multi-project)
        assert!(schema.properties.contains_key("customFields"));
        assert!(!schema.properties.contains_key("cf_story_points"));
    }

    #[test]
    fn test_jira_enricher_transform_args_skips_non_create() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut args = json!({"priority": "urgent"});
        enricher.transform_args("get_issues", &mut args);
        // Should not transform priority for get_issues
        assert_eq!(args["priority"], "urgent");
    }

    #[test]
    fn test_jira_enricher_transform_args_normal_priority() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut args = json!({"title": "T", "priority": "normal"});
        enricher.transform_args("create_issue", &mut args);
        assert_eq!(args["priority"], "Medium");
    }

    #[test]
    fn test_jira_enricher_transform_args_non_alias_priority() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut args = json!({"title": "T", "priority": "Highest"});
        enricher.transform_args("create_issue", &mut args);
        assert_eq!(args["priority"], "Highest"); // pass-through
    }

    #[test]
    fn test_jira_enricher_multi_project_no_cf_transform() {
        let mut meta = single_project_metadata();
        meta.projects.insert(
            "INFRA".into(),
            JiraProjectMetadata {
                issue_types: vec![],
                priorities: vec![],
                components: vec![],
                link_types: vec![],
                custom_fields: vec![],
            },
        );
        let enricher = JiraSchemaEnricher::new(meta);
        let mut args = json!({"title": "T", "cf_story_points": 5});
        enricher.transform_args("create_issue", &mut args);
        // Multi-project: cf_* NOT transformed
        assert!(args.get("cf_story_points").is_some());
        assert!(args.get("customFields").is_none());
    }

    #[test]
    fn test_jira_enricher_components_enum() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "components": { "type": "array" }
            }
        }));
        enricher.enrich_schema("create_issue", &mut schema);
        let comp = schema.properties.get("components").unwrap();
        assert_eq!(
            comp.enum_values,
            Some(vec!["API".into(), "Frontend".into()])
        );
    }

    #[test]
    fn test_jira_enricher_link_types() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::new();
        enricher.enrich_schema("link_issues", &mut schema);
        let lt = schema.properties.get("link_type").unwrap();
        assert_eq!(lt.enum_values, Some(vec!["Blocks".into()]));
    }

    #[test]
    fn test_jira_enricher_get_issues_removes_state_category() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "state": { "type": "string" },
                "stateCategory": { "type": "string" }
            }
        }));
        enricher.enrich_schema("get_issues", &mut schema);
        assert!(!schema.properties.contains_key("stateCategory"));
        assert!(schema.properties.contains_key("state"));
    }
}
