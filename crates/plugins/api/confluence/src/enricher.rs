//! Confluence schema enricher.
//!
//! Confluence currently exposes the KnowledgeBase tool category without
//! provider-specific schema mutations. This enricher exists so the executor
//! can register a KnowledgeBase-capable provider consistently, and so later
//! Paper 3 value-model work has a stable home in this crate.

use devboy_core::{
    ToolCategory, ToolCostModel, ToolEffect, ToolEnricher, ToolFollowUp, ToolSchema,
    ToolValueClass, ToolValueModel,
};
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

    fn value_model(&self, tool_name: &str) -> Option<ToolValueModel> {
        match tool_name {
            "search_knowledge_base" => Some(ToolValueModel {
                effect: ToolEffect::ReadOnly,
                value_class: ToolValueClass::Supporting,
                cost_model: ToolCostModel {
                    typical_kb: 4.0,
                    max_kb: None,
                },
                freshness_ttl_s: 60,
                follow_up: Some(ToolFollowUp {
                    tool_name: "get_knowledge_base_page".into(),
                    projection: Some("id".into()),
                    projection_arg: Some("pageId".into()),
                }),
            }),
            "list_knowledge_base_pages" => Some(ToolValueModel {
                effect: ToolEffect::ReadOnly,
                value_class: ToolValueClass::Supporting,
                cost_model: ToolCostModel {
                    typical_kb: 3.0,
                    max_kb: None,
                },
                freshness_ttl_s: 60,
                follow_up: Some(ToolFollowUp {
                    tool_name: "get_knowledge_base_page".into(),
                    projection: Some("id".into()),
                    projection_arg: Some("pageId".into()),
                }),
            }),
            "get_knowledge_base_page" => Some(ToolValueModel {
                effect: ToolEffect::ReadOnly,
                value_class: ToolValueClass::Critical,
                cost_model: ToolCostModel {
                    typical_kb: 8.0,
                    max_kb: Some(80.0),
                },
                freshness_ttl_s: 300,
                follow_up: None,
            }),
            "get_knowledge_base_spaces" => Some(ToolValueModel {
                effect: ToolEffect::ReadOnly,
                value_class: ToolValueClass::Supporting,
                cost_model: ToolCostModel {
                    typical_kb: 1.5,
                    max_kb: None,
                },
                freshness_ttl_s: 1800,
                follow_up: None,
            }),
            "create_knowledge_base_page" | "update_knowledge_base_page" => Some(ToolValueModel {
                effect: ToolEffect::MutatesExternal,
                value_class: ToolValueClass::Critical,
                cost_model: ToolCostModel {
                    typical_kb: 8.0,
                    max_kb: Some(80.0),
                },
                freshness_ttl_s: 0,
                follow_up: None,
            }),
            _ => None,
        }
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

    #[test]
    fn paper3_search_chains_to_get_page_with_page_id_projection() {
        let enricher = ConfluenceSchemaEnricher::new();
        let model = enricher.value_model("search_knowledge_base").unwrap();

        let follow_up = model.follow_up.unwrap();
        assert_eq!(follow_up.tool_name, "get_knowledge_base_page");
        assert_eq!(follow_up.projection.as_deref(), Some("id"));
        assert_eq!(follow_up.projection_arg.as_deref(), Some("pageId"));
    }

    #[test]
    fn paper3_list_chains_to_get_page() {
        let enricher = ConfluenceSchemaEnricher::new();
        let model = enricher.value_model("list_knowledge_base_pages").unwrap();

        let follow_up = model.follow_up.unwrap();
        assert_eq!(follow_up.tool_name, "get_knowledge_base_page");
        assert_eq!(follow_up.projection.as_deref(), Some("id"));
        assert_eq!(follow_up.projection_arg.as_deref(), Some("pageId"));
    }

    #[test]
    fn paper3_get_page_is_read_only_with_long_ttl() {
        let enricher = ConfluenceSchemaEnricher::new();
        let model = enricher.value_model("get_knowledge_base_page").unwrap();

        assert_eq!(model.effect, ToolEffect::ReadOnly);
        assert_eq!(model.value_class, ToolValueClass::Critical);
        assert!(model.freshness_ttl_s >= 300);
        assert!(model.is_speculatable());
        assert_eq!(model.cost_model.max_kb, Some(80.0));
    }

    #[test]
    fn paper3_mutating_endpoints_are_never_speculatable() {
        let enricher = ConfluenceSchemaEnricher::new();

        for tool_name in ["create_knowledge_base_page", "update_knowledge_base_page"] {
            let model = enricher.value_model(tool_name).unwrap();
            assert_eq!(model.effect, ToolEffect::MutatesExternal);
            assert!(!model.is_speculatable());
        }
    }
}
