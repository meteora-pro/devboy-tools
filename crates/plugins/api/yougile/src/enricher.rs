//! YouGile schema enricher scaffold.

use devboy_core::{ToolCategory, ToolEnricher, ToolSchema};
use serde_json::Value;

use crate::metadata::YouGileMetadata;

/// No-op schema enricher placeholder for YouGile.
///
/// The real implementation will use board/column metadata to refine issue tool
/// schemas once the provider behavior is in place.
pub struct YouGileSchemaEnricher {
    metadata: YouGileMetadata,
}

impl YouGileSchemaEnricher {
    pub fn new(metadata: YouGileMetadata) -> Self {
        Self { metadata }
    }
}

impl ToolEnricher for YouGileSchemaEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &[ToolCategory::IssueTracker]
    }

    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema) {
        const REMOVE_PARAMS: &[&str] = &["issueType", "components", "projectId", "points"];
        const GET_ISSUES_REMOVE_PARAMS: &[&str] = &["projectKey", "nativeQuery"];

        schema.remove_params(REMOVE_PARAMS);

        if tool_name == "get_issues" {
            schema.remove_params(GET_ISSUES_REMOVE_PARAMS);
            schema.add_enum_param(
                "stateCategory",
                &["backlog", "todo", "in_progress", "done", "cancelled"],
                "Filter by semantic status category. Maps YouGile board columns to shared status buckets using column-title heuristics.",
            );
        }

        let statuses: Vec<String> = self
            .metadata
            .columns
            .iter()
            .map(|column| column.title.clone())
            .collect();
        if !statuses.is_empty() {
            schema.set_enum("status", &statuses);
            schema.set_description(
                "status",
                &format!(
                    "Filter by exact board column title. Available: {}",
                    statuses.join(", ")
                ),
            );
        }
    }

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{YouGileBoard, YouGileColumn};

    #[test]
    fn enrich_schema_sets_status_enum_and_state_category() {
        let enricher = YouGileSchemaEnricher::new(YouGileMetadata {
            boards: vec![YouGileBoard {
                id: "board-1".to_string(),
                title: "Platform".to_string(),
            }],
            columns: vec![
                YouGileColumn {
                    id: "col-1".to_string(),
                    title: "Backlog".to_string(),
                    board_id: Some("board-1".to_string()),
                },
                YouGileColumn {
                    id: "col-2".to_string(),
                    title: "In Progress".to_string(),
                    board_id: Some("board-1".to_string()),
                },
            ],
        });

        let mut schema = ToolSchema::from_json(&serde_json::json!({
            "type": "object",
            "properties": {
                "status": { "type": "string" },
                "projectKey": { "type": "string" }
            }
        }));

        enricher.enrich_schema("get_issues", &mut schema);

        assert_eq!(
            schema.properties["status"].enum_values.as_ref().unwrap(),
            &vec!["Backlog".to_string(), "In Progress".to_string()]
        );
        assert!(schema.properties.contains_key("stateCategory"));
        assert!(!schema.properties.contains_key("projectKey"));
    }
}
