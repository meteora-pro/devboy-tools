//! ClickUp schema enricher.
//!
//! Dynamic enricher that uses list metadata to populate enum values
//! and generate custom field parameters.

use devboy_core::{sanitize_field_name, ToolCategory, ToolEnricher, ToolSchema};
use serde_json::{json, Value};

use crate::metadata::{ClickUpFieldType, ClickUpMetadata};

/// Dynamic schema enricher for ClickUp provider.
///
/// Created with metadata from a ClickUp list. Adds:
/// - Status enum from list statuses
/// - Priority enum (static: urgent, high, normal, low)
/// - Custom field `cf_*` parameters from list custom fields
/// - Link types enum
///
/// Removes:
/// - `issueType`, `components`, `projectId` (not applicable)
pub struct ClickUpSchemaEnricher {
    metadata: ClickUpMetadata,
}

impl ClickUpSchemaEnricher {
    pub fn new(metadata: ClickUpMetadata) -> Self {
        Self { metadata }
    }
}

/// Parameters to remove from ClickUp issue tools.
const REMOVE_PARAMS: &[&str] = &["issueType", "components", "projectId"];

/// Parameters to remove from get_issues.
const GET_ISSUES_REMOVE_PARAMS: &[&str] = &["projectKey", "nativeQuery", "stateCategory"];

impl ToolEnricher for ClickUpSchemaEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &[ToolCategory::IssueTracker, ToolCategory::Epics]
    }

    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema) {
        schema.remove_params(REMOVE_PARAMS);

        if tool_name == "get_issues" {
            schema.remove_params(GET_ISSUES_REMOVE_PARAMS);
        }

        // Add status enum from metadata
        if !self.metadata.statuses.is_empty() {
            let status_names: Vec<String> = self
                .metadata
                .statuses
                .iter()
                .map(|s| s.name.clone())
                .collect();

            schema.set_enum("status", &status_names);
            let desc = format!(
                "Filter by exact status name. Available: {}",
                status_names.join(", ")
            );
            schema.set_description("status", &desc);
        }

        // Add priority enum (static for ClickUp)
        schema.add_enum_param(
            "priority",
            &["urgent", "high", "normal", "low"],
            "Priority. Available: urgent, high, normal, low",
        );

        // Add link types for link_issues
        if tool_name == "link_issues" {
            schema.add_enum_param(
                "link_type",
                &["blocks", "blocked_by", "relates_to", "subtask"],
                "Link type between tasks",
            );
        }

        // Replace customFields with individual cf_* params
        if tool_name == "create_issue" || tool_name == "update_issue" {
            schema.remove_params(&["customFields"]);

            for field in &self.metadata.custom_fields {
                let param_name = sanitize_field_name(&field.name);
                let field_schema = custom_field_to_schema(field);
                schema.add_param(&param_name, field_schema);
            }
        }
    }

    fn transform_args(&self, tool_name: &str, args: &mut Value) {
        if tool_name != "create_issue" && tool_name != "update_issue" {
            return;
        }

        // Transform priority name to ClickUp numeric value
        if let Some(obj) = args.as_object_mut() {
            if let Some(priority) = obj.get("priority").and_then(|v| v.as_str()) {
                let numeric = match priority {
                    "urgent" => 1,
                    "high" => 2,
                    "normal" => 3,
                    "low" => 4,
                    _ => 3, // default to normal
                };
                obj.insert("priority".into(), json!(numeric));
            }
        }

        // Transform cf_* params to customFields array
        let Some(obj) = args.as_object_mut() else {
            return;
        };

        let mut custom_fields: Vec<Value> = Vec::new();
        let mut cf_keys_to_remove: Vec<String> = Vec::new();

        for field in &self.metadata.custom_fields {
            let param_name = sanitize_field_name(&field.name);
            if let Some(value) = obj.get(&param_name) {
                let transformed = field.transform_value(value);
                custom_fields.push(json!({
                    "id": field.id,
                    "value": transformed,
                }));
                cf_keys_to_remove.push(param_name);
            }
        }

        // Remove cf_* params and insert customFields
        for key in cf_keys_to_remove {
            obj.remove(&key);
        }
        if !custom_fields.is_empty() {
            obj.insert("customFields".into(), json!(custom_fields));
        }
    }
}

/// Convert a ClickUp custom field definition to a JSON Schema property.
fn custom_field_to_schema(field: &crate::metadata::ClickUpCustomField) -> Value {
    let type_desc = match field.field_type {
        ClickUpFieldType::Dropdown => {
            let options: Vec<&str> = field.options.iter().map(|o| o.name.as_str()).collect();
            return json!({
                "type": "string",
                "enum": options,
                "description": format!("Custom field: {} (dropdown). Select one option.", field.name),
                "x-enriched": true,
            });
        }
        ClickUpFieldType::Labels => {
            let options: Vec<&str> = field.options.iter().map(|o| o.name.as_str()).collect();
            return json!({
                "type": "array",
                "items": { "type": "string", "enum": options },
                "description": format!("Custom field: {} (labels). Select one or more.", field.name),
                "x-enriched": true,
            });
        }
        ClickUpFieldType::Number | ClickUpFieldType::Currency => {
            return json!({
                "type": "number",
                "description": format!("Custom field: {} ({:?}).", field.name, field.field_type),
                "x-enriched": true,
            });
        }
        ClickUpFieldType::Checkbox => {
            return json!({
                "type": "boolean",
                "description": format!("Custom field: {} (checkbox).", field.name),
                "x-enriched": true,
            });
        }
        ClickUpFieldType::Date => "date (ISO 8601)",
        ClickUpFieldType::Text => "text",
        ClickUpFieldType::Email => "email",
        ClickUpFieldType::Url => "url",
        ClickUpFieldType::Phone => "phone",
    };

    json!({
        "type": "string",
        "description": format!("Custom field: {} ({}).", field.name, type_desc),
        "x-enriched": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::*;

    fn sample_metadata() -> ClickUpMetadata {
        ClickUpMetadata {
            statuses: vec![
                ClickUpStatus {
                    name: "To Do".into(),
                    r#type: Some("open".into()),
                },
                ClickUpStatus {
                    name: "In Progress".into(),
                    r#type: Some("custom".into()),
                },
                ClickUpStatus {
                    name: "Done".into(),
                    r#type: Some("closed".into()),
                },
            ],
            custom_fields: vec![
                ClickUpCustomField {
                    id: "uuid-1".into(),
                    name: "Story Points".into(),
                    field_type: ClickUpFieldType::Number,
                    required: false,
                    options: vec![],
                },
                ClickUpCustomField {
                    id: "uuid-2".into(),
                    name: "Risk Level".into(),
                    field_type: ClickUpFieldType::Dropdown,
                    required: false,
                    options: vec![
                        ClickUpFieldOption {
                            id: "opt-1".into(),
                            name: "Low".into(),
                            orderindex: Some(0),
                        },
                        ClickUpFieldOption {
                            id: "opt-2".into(),
                            name: "Medium".into(),
                            orderindex: Some(1),
                        },
                        ClickUpFieldOption {
                            id: "opt-3".into(),
                            name: "High".into(),
                            orderindex: Some(2),
                        },
                    ],
                },
            ],
        }
    }

    #[test]
    fn test_clickup_enricher_adds_status_enum() {
        let enricher = ClickUpSchemaEnricher::new(sample_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "status": { "type": "string" },
            },
        }));

        enricher.enrich_schema("get_issues", &mut schema);

        let status = schema.properties.get("status").unwrap();
        assert_eq!(
            status.enum_values,
            Some(vec!["To Do".into(), "In Progress".into(), "Done".into()])
        );
    }

    #[test]
    fn test_clickup_enricher_adds_priority_enum() {
        let enricher = ClickUpSchemaEnricher::new(sample_metadata());
        let mut schema = ToolSchema::new();

        enricher.enrich_schema("create_issue", &mut schema);

        let priority = schema.properties.get("priority").unwrap();
        assert_eq!(
            priority.enum_values,
            Some(vec![
                "urgent".into(),
                "high".into(),
                "normal".into(),
                "low".into()
            ])
        );
    }

    #[test]
    fn test_clickup_enricher_adds_custom_field_params() {
        let enricher = ClickUpSchemaEnricher::new(sample_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "customFields": { "type": "object" },
            },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        // customFields removed
        assert!(!schema.properties.contains_key("customFields"));

        // cf_* params added
        assert!(schema.properties.contains_key("cf_story_points"));
        assert!(schema.properties.contains_key("cf_risk_level"));

        let risk = schema.properties.get("cf_risk_level").unwrap();
        assert_eq!(
            risk.enum_values,
            Some(vec!["Low".into(), "Medium".into(), "High".into()])
        );

        let points = schema.properties.get("cf_story_points").unwrap();
        assert_eq!(points.schema_type, "number");
    }

    #[test]
    fn test_clickup_enricher_removes_unsupported_params() {
        let enricher = ClickUpSchemaEnricher::new(sample_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "issueType": { "type": "string" },
                "components": { "type": "array" },
                "projectId": { "type": "string" },
            },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        assert!(!schema.properties.contains_key("issueType"));
        assert!(!schema.properties.contains_key("components"));
        assert!(!schema.properties.contains_key("projectId"));
    }

    #[test]
    fn test_clickup_enricher_transform_args_priority() {
        let enricher = ClickUpSchemaEnricher::new(sample_metadata());
        let mut args = json!({
            "title": "Test",
            "priority": "high",
        });

        enricher.transform_args("create_issue", &mut args);

        assert_eq!(args["priority"], 2);
    }

    #[test]
    fn test_clickup_enricher_transform_args_custom_fields() {
        let enricher = ClickUpSchemaEnricher::new(sample_metadata());
        let mut args = json!({
            "title": "Test",
            "cf_story_points": 5,
            "cf_risk_level": "Medium",
        });

        enricher.transform_args("create_issue", &mut args);

        // cf_* params removed
        assert!(args.get("cf_story_points").is_none());
        assert!(args.get("cf_risk_level").is_none());

        // customFields created
        let custom_fields = args["customFields"].as_array().unwrap();
        assert_eq!(custom_fields.len(), 2);

        // Story Points: pass-through
        let sp = custom_fields.iter().find(|f| f["id"] == "uuid-1").unwrap();
        assert_eq!(sp["value"], 5);

        // Risk Level: name → orderindex
        let rl = custom_fields.iter().find(|f| f["id"] == "uuid-2").unwrap();
        assert_eq!(rl["value"], 1); // Medium = orderindex 1
    }

    #[test]
    fn test_clickup_enricher_transform_args_skips_non_create() {
        let enricher = ClickUpSchemaEnricher::new(sample_metadata());
        let mut args = json!({"cf_story_points": 5});
        enricher.transform_args("get_issues", &mut args);
        // Should not transform — only create/update
        assert!(args.get("cf_story_points").is_some());
    }

    #[test]
    fn test_clickup_enricher_transform_args_no_custom_fields() {
        let enricher = ClickUpSchemaEnricher::new(sample_metadata());
        let mut args = json!({"title": "Test"});
        enricher.transform_args("create_issue", &mut args);
        // No cf_* → no customFields key
        assert!(args.get("customFields").is_none());
    }

    #[test]
    fn test_clickup_enricher_link_types() {
        let enricher = ClickUpSchemaEnricher::new(sample_metadata());
        let mut schema = ToolSchema::new();
        enricher.enrich_schema("link_issues", &mut schema);
        let lt = schema.properties.get("link_type").unwrap();
        assert_eq!(
            lt.enum_values,
            Some(vec![
                "blocks".into(),
                "blocked_by".into(),
                "relates_to".into(),
                "subtask".into()
            ])
        );
    }

    #[test]
    fn test_clickup_enricher_empty_metadata() {
        let enricher = ClickUpSchemaEnricher::new(ClickUpMetadata {
            statuses: vec![],
            custom_fields: vec![],
        });
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "customFields": { "type": "object" }
            }
        }));
        enricher.enrich_schema("create_issue", &mut schema);
        // customFields removed, no cf_* added
        assert!(!schema.properties.contains_key("customFields"));
        assert!(schema.properties.contains_key("priority")); // always added
    }

    #[test]
    fn test_clickup_enricher_priority_default() {
        let enricher = ClickUpSchemaEnricher::new(sample_metadata());
        let mut args = json!({"title": "Test", "priority": "unknown_value"});
        enricher.transform_args("create_issue", &mut args);
        assert_eq!(args["priority"], 3); // default to normal
    }
}
