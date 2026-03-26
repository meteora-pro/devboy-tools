//! Built-in enrichers for the executor.
//!
//! The `ToolEnricher` trait and `ToolSchema` live in `devboy-core`
//! so that provider crates can implement enrichers without depending
//! on the executor. This module provides built-in enrichers.

use devboy_core::{ToolEnricher, ToolSchema};
use serde_json::Value;

// Re-export core enricher types for convenience
pub use devboy_core::{sanitize_field_name, ToolSchema as Schema};

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
    fn supported_tools(&self) -> &[&str] {
        LIST_TOOLS
    }

    fn enrich_schema(&self, _tool_name: &str, schema: &mut ToolSchema) {
        schema.add_enum_param(
            "format",
            &["markdown", "compact", "json"],
            "Output format. Default: markdown",
        );
    }

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {
        // format is consumed by the pipeline layer, not the provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_pipeline_format_enricher() {
        let enricher = PipelineFormatEnricher;

        assert!(enricher.supported_tools().contains(&"get_issues"));
        assert!(enricher.supported_tools().contains(&"get_merge_requests"));

        let mut schema = ToolSchema {
            properties: serde_json::Map::new(),
            required: vec![],
        };
        enricher.enrich_schema("get_issues", &mut schema);

        let format = schema.properties.get("format").unwrap();
        assert_eq!(format["enum"], json!(["markdown", "compact", "json"]));
    }
}
