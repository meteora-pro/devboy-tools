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
const LIST_TOOLS: &[&str] = &[
    "get_issues",
    "get_issue",
    "get_issue_comments",
    "get_merge_requests",
    "get_merge_request",
    "get_merge_request_discussions",
    "get_merge_request_diffs",
    "list_knowledge_base_pages",
    "search_knowledge_base",
    "get_knowledge_base_page",
];

// =============================================================================
// Safe parameter insertion
// =============================================================================

/// Find a free parameter name: try `preferred`, then `_preferred`, `__preferred`, etc.
///
/// If a tool already has a parameter with the same name (e.g., the tool defines
/// its own `chunk`), the enricher uses a prefixed variant to avoid collisions.
fn safe_param_name(schema: &ToolSchema, preferred: &str) -> String {
    if !schema.properties.contains_key(preferred) {
        return preferred.to_string();
    }
    // Prefix with underscores until we find a free name
    let mut name = format!("_{preferred}");
    while schema.properties.contains_key(&name) {
        name = format!("_{name}");
    }
    name
}

/// Insert a property only if the preferred name (or a safe variant) is available.
/// Returns the actual name used.
fn safe_insert(schema: &mut ToolSchema, preferred: &str, prop: PropertySchema) -> String {
    let name = safe_param_name(schema, preferred);
    schema.add_property(&name, prop);
    name
}

// =============================================================================
// FormatPipelineEnricher
// =============================================================================

/// Format pipeline enricher — adds pipeline-level parameters to list tools.
///
/// Adds these parameters (using safe naming to avoid collisions):
/// - `format` — output format (toon/json)
/// - `budget` — token budget for response size control
/// - `chunk`  — chunk number for navigating large results
///
/// API-level `limit`/`offset` are defined by individual tools where
/// the provider API supports pagination. The enricher does NOT add them.
pub struct FormatPipelineEnricher;

impl ToolEnricher for FormatPipelineEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &[
            ToolCategory::IssueTracker,
            ToolCategory::GitRepository,
            ToolCategory::KnowledgeBase,
        ]
    }

    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema) {
        if !LIST_TOOLS.contains(&tool_name) {
            return;
        }

        // Output format
        safe_insert(
            schema,
            "format",
            PropertySchema::string_enum(
                &["toon", "json", "mckp"],
                "Output format. \
                 `toon` (default) is the legacy token-optimised custom format; \
                 `json` is the pretty-printed baseline; \
                 `mckp` is the format-adaptive encoder from Paper 2 — best for \
                 array/object payloads on `o200k_base` tokenizers, key-lossless.",
            ),
        );

        // Token budget — LLM controls response size
        safe_insert(
            schema,
            "budget",
            PropertySchema::integer(
                "Token budget for this response. Lower = less data + chunk index for navigation. \
                 Higher = more data per call. Default: from server config.",
                Some(100.0),
                Some(100000.0),
            ),
        );

        // Chunk navigation — for fetching specific chunks from large results
        safe_insert(
            schema,
            "chunk",
            PropertySchema::integer(
                "Chunk number to fetch (from chunk index). \
                 When a response exceeds budget, it returns chunk 1 + an index of all chunks. \
                 Use this parameter to fetch a specific chunk by number.",
                Some(1.0),
                None,
            ),
        );
    }

    fn transform_args(&self, _tool_name: &str, args: &mut Value) {
        // Convert `chunk` to `offset`/`limit` for the pipeline.
        // The chunk index includes offset/limit per chunk, but the LLM
        // just sends a chunk number. We resolve it at runtime in the handler.
        // For now, pass through — the handler reads `chunk` from args directly.
        let _ = args;
    }
}

/// Backward-compatible alias.
pub type PipelineFormatEnricher = FormatPipelineEnricher;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_param_name_no_conflict() {
        let schema = ToolSchema::new();
        assert_eq!(safe_param_name(&schema, "budget"), "budget");
        assert_eq!(safe_param_name(&schema, "chunk"), "chunk");
    }

    #[test]
    fn test_safe_param_name_with_conflict() {
        let mut schema = ToolSchema::new();
        schema.add_property("chunk", PropertySchema::string("existing"));
        assert_eq!(safe_param_name(&schema, "chunk"), "_chunk");

        schema.add_property("_chunk", PropertySchema::string("also taken"));
        assert_eq!(safe_param_name(&schema, "chunk"), "__chunk");
    }

    #[test]
    fn test_format_pipeline_enricher_adds_params() {
        let enricher = FormatPipelineEnricher;
        let mut schema = ToolSchema::new();
        enricher.enrich_schema("get_issues", &mut schema);

        // format — `mckp` was added in PR for issue #203 follow-up so the
        // LLM can pick the Paper 2 encoder explicitly when its tokenizer
        // family makes it preferable to TOON.
        let format = schema.properties.get("format").unwrap();
        assert_eq!(
            format.enum_values,
            Some(vec!["toon".into(), "json".into(), "mckp".into()])
        );

        // budget
        let budget = schema.properties.get("budget").unwrap();
        assert_eq!(budget.schema_type, "integer");
        assert_eq!(budget.minimum, Some(100.0));

        // chunk (new)
        let chunk = schema.properties.get("chunk").unwrap();
        assert_eq!(chunk.schema_type, "integer");
        assert_eq!(chunk.minimum, Some(1.0));

        // NO offset/limit — those are API-level, defined by tools themselves
        assert!(!schema.properties.contains_key("offset"));
        assert!(!schema.properties.contains_key("limit"));
    }

    #[test]
    fn test_enricher_skips_non_list_tools() {
        let enricher = FormatPipelineEnricher;
        let mut schema = ToolSchema::new();
        enricher.enrich_schema("create_issue", &mut schema);
        assert!(schema.properties.is_empty());
    }

    #[test]
    fn test_enricher_safe_naming_on_collision() {
        let enricher = FormatPipelineEnricher;
        let mut schema = ToolSchema::new();
        // Tool already defines `chunk` for its own purpose
        schema.add_property("chunk", PropertySchema::string("tool's own chunk param"));

        enricher.enrich_schema("get_merge_request_diffs", &mut schema);

        // Original `chunk` preserved
        let original = schema.properties.get("chunk").unwrap();
        assert_eq!(original.schema_type, "string");

        // Enricher's chunk renamed to `_chunk`
        let enriched = schema.properties.get("_chunk").unwrap();
        assert_eq!(enriched.schema_type, "integer");

        // format and budget added normally (no collision)
        assert!(schema.properties.contains_key("format"));
        assert!(schema.properties.contains_key("budget"));
    }

    #[test]
    fn test_enricher_categories() {
        let enricher = FormatPipelineEnricher;
        let cats = enricher.supported_categories();
        assert!(cats.contains(&ToolCategory::IssueTracker));
        assert!(cats.contains(&ToolCategory::GitRepository));
        assert!(cats.contains(&ToolCategory::KnowledgeBase));
    }

    #[test]
    fn test_format_pipeline_enricher_covers_kb_tools() {
        let enricher = FormatPipelineEnricher;
        let mut schema = ToolSchema::new();

        enricher.enrich_schema("search_knowledge_base", &mut schema);

        assert!(schema.properties.contains_key("format"));
        assert!(schema.properties.contains_key("budget"));
        assert!(schema.properties.contains_key("chunk"));
    }
}
