//! Built-in enrichers for the executor.
//!
//! The `ToolEnricher` trait and `ToolSchema` live in `devboy-core`
//! so that provider crates can implement enrichers without depending
//! on the executor. This module provides built-in enrichers.

use devboy_core::{PropertySchema, ToolCategory, ToolEnricher, ToolSchema};
use serde_json::Value;

// Re-export core enricher types for convenience
pub use devboy_core::{ToolSchema as Schema, sanitize_field_name};

/// Tools that return lists and go through the format pipeline.
/// These get `format`, `budget`, `offset`, `limit` parameters automatically.
const LIST_TOOLS: &[&str] = &[
    "get_issues",
    "get_issue",
    "get_issue_comments",
    "get_merge_requests",
    "get_merge_request",
    "get_merge_request_discussions",
    "get_merge_request_diffs",
];

/// Format pipeline enricher — adds pagination and budget parameters to list tools.
///
/// Automatically adds these parameters to all list-returning tools:
/// - `format` — output format (toon/json)
/// - `budget` — token budget for response size control
/// - `offset` — pagination offset
/// - `limit` — pagination limit
///
/// This avoids duplicating these params in every tool schema definition.
pub struct FormatPipelineEnricher;

impl ToolEnricher for FormatPipelineEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &[ToolCategory::IssueTracker, ToolCategory::GitRepository]
    }

    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema) {
        if !LIST_TOOLS.contains(&tool_name) {
            return;
        }

        // Output format
        if !schema.properties.contains_key("format") {
            schema.add_enum_param("format", &["toon", "json"], "Output format (default: toon)");
        }

        // Token budget — LLM controls response size
        if !schema.properties.contains_key("budget") {
            schema.add_property(
                "budget",
                PropertySchema::integer(
                    "Token budget for response. Lower = less data + chunk index for navigation. \
                     Higher = more data per call. Default: from server config (~28000 tokens).",
                    Some(100.0),
                    Some(100000.0),
                ),
            );
        }

        // Pagination — offset and limit
        if !schema.properties.contains_key("offset") {
            schema.add_property(
                "offset",
                PropertySchema::integer(
                    "Skip N items for pagination. Use with chunk index offsets.",
                    Some(0.0),
                    None,
                ),
            );
        }

        if !schema.properties.contains_key("limit") {
            schema.add_property(
                "limit",
                PropertySchema::integer(
                    "Maximum items to return (default: 20, max: 100)",
                    Some(1.0),
                    Some(100.0),
                ),
            );
        }
    }

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {
        // These params are consumed by the pipeline layer, not the provider
    }
}

/// Backward-compatible alias.
pub type PipelineFormatEnricher = FormatPipelineEnricher;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_pipeline_enricher_adds_all_params() {
        let enricher = FormatPipelineEnricher;
        let mut schema = ToolSchema::new();
        enricher.enrich_schema("get_issues", &mut schema);

        // format
        let format = schema.properties.get("format").unwrap();
        assert_eq!(format.enum_values, Some(vec!["toon".into(), "json".into()]));

        // budget
        let budget = schema.properties.get("budget").unwrap();
        assert_eq!(budget.schema_type, "integer");
        assert_eq!(budget.minimum, Some(100.0));
        assert_eq!(budget.maximum, Some(100000.0));

        // offset
        let offset = schema.properties.get("offset").unwrap();
        assert_eq!(offset.schema_type, "integer");
        assert_eq!(offset.minimum, Some(0.0));

        // limit
        let limit = schema.properties.get("limit").unwrap();
        assert_eq!(limit.schema_type, "integer");
        assert_eq!(limit.minimum, Some(1.0));
        assert_eq!(limit.maximum, Some(100.0));
    }

    #[test]
    fn test_enricher_skips_non_list_tools() {
        let enricher = FormatPipelineEnricher;
        let mut schema = ToolSchema::new();
        enricher.enrich_schema("create_issue", &mut schema);

        assert!(schema.properties.is_empty());
    }

    #[test]
    fn test_enricher_does_not_overwrite_existing() {
        let enricher = FormatPipelineEnricher;
        let mut schema = ToolSchema::new();
        // Pre-existing limit with different bounds
        schema.add_property(
            "limit",
            PropertySchema::integer("Custom limit", Some(1.0), Some(50.0)),
        );

        enricher.enrich_schema("get_merge_request_discussions", &mut schema);

        // limit should NOT be overwritten
        let limit = schema.properties.get("limit").unwrap();
        assert_eq!(limit.maximum, Some(50.0));

        // but format/budget/offset should be added
        assert!(schema.properties.contains_key("format"));
        assert!(schema.properties.contains_key("budget"));
        assert!(schema.properties.contains_key("offset"));
    }

    #[test]
    fn test_enricher_categories() {
        let enricher = FormatPipelineEnricher;
        let cats = enricher.supported_categories();
        assert!(cats.contains(&ToolCategory::IssueTracker));
        assert!(cats.contains(&ToolCategory::GitRepository));
    }
}
