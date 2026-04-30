//! Confluence schema enricher.
//!
//! Confluence currently exposes the KnowledgeBase tool category without
//! provider-specific schema mutations. This enricher exists so the executor
//! can register a KnowledgeBase-capable provider consistently, and so later
//! Paper 3 value-model work has a stable home in this crate.

use devboy_core::{ToolCategory, ToolEnricher, ToolSchema};
use serde_json::Value;

/// Static schema enricher for Confluence knowledge base tools.
///
/// Today this enricher only advertises category support and leaves schemas
/// and arguments unchanged. Confluence-specific value models and richer
/// schema hints can be added here later without changing the provider shape.
pub struct ConfluenceSchemaEnricher;

impl ConfluenceSchemaEnricher {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConfluenceSchemaEnricher {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolEnricher for ConfluenceSchemaEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &[ToolCategory::KnowledgeBase]
    }

    fn enrich_schema(&self, _tool_name: &str, _schema: &mut ToolSchema) {
        // No-op for now. The provider already exposes the correct base
        // KnowledgeBase schemas; this enricher mainly acts as the category
        // claim for Confluence until richer provider-specific hints land.
    }

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {
        // No-op for now.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn confluence_enricher_supports_knowledge_base_category() {
        let enricher = ConfluenceSchemaEnricher::new();
        assert_eq!(enricher.supported_categories(), &[ToolCategory::KnowledgeBase]);
    }

    #[test]
    fn confluence_enricher_leaves_schema_unchanged() {
        let enricher = ConfluenceSchemaEnricher::new();
        let original = json!({
            "type": "object",
            "properties": {
                "spaceKey": { "type": "string" },
                "parentId": { "type": "string" }
            },
            "required": ["spaceKey"]
        });
        let mut schema = ToolSchema::from_json(&original);

        enricher.enrich_schema("list_knowledge_base_pages", &mut schema);

        assert_eq!(schema.to_json(), original);
    }

    #[test]
    fn confluence_enricher_leaves_args_unchanged() {
        let enricher = ConfluenceSchemaEnricher::new();
        let mut args = json!({
            "query": "architecture",
            "spaceKey": "ENG",
            "rawQuery": false
        });
        let expected = args.clone();

        enricher.transform_args("search_knowledge_base", &mut args);

        assert_eq!(args, expected);
    }
}
