//! Built-in enrichers for the executor.
//!
//! The `ToolEnricher` trait and `ToolSchema` live in `devboy-core`
//! so that provider crates can implement enrichers without depending
//! on the executor. This module provides built-in enrichers.

use devboy_core::{ToolCategory, ToolEnricher, ToolSchema};
use serde_json::Value;

// Re-export core enricher types for convenience
pub use devboy_core::{ToolSchema as Schema, sanitize_field_name};

/// Pipeline format enricher — adds `format` enum parameter to list tools.
pub struct PipelineFormatEnricher;

const LIST_TOOLS: &[&str] = &[
    "get_issues",
    "get_issue",
    "get_issue_comments",
    "get_merge_requests",
    "get_merge_request",
    "get_merge_request_discussions",
    "get_merge_request_diffs",
];

impl ToolEnricher for PipelineFormatEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &[ToolCategory::IssueTracker, ToolCategory::GitRepository]
    }

    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema) {
        if LIST_TOOLS.contains(&tool_name) {
            schema.add_enum_param("format", &["toon", "json"], "Output format. Default: toon");
        }
    }

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {
        // format is consumed by the pipeline layer, not the provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_pipeline_format_enricher() {
        let enricher = PipelineFormatEnricher;

        assert!(
            enricher
                .supported_categories()
                .contains(&ToolCategory::IssueTracker)
        );
        assert!(
            enricher
                .supported_categories()
                .contains(&ToolCategory::GitRepository)
        );

        let mut schema = ToolSchema::new();
        enricher.enrich_schema("get_issues", &mut schema);

        let format = schema.properties.get("format").unwrap();
        assert_eq!(format.enum_values, Some(vec!["toon".into(), "json".into()]));
    }
}
