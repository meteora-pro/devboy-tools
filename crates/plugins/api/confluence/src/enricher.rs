//! Confluence schema enricher.
//!
//! Confluence currently exposes the KnowledgeBase tool category without
//! provider-specific schema mutations. This enricher exists so the executor
//! can register a KnowledgeBase-capable provider consistently, and so
//! Paper 3 value models for KB tools have a stable home in this crate.

use devboy_core::{
    CostModel, FollowUpLink, SideEffectClass, ToolCategory, ToolEnricher, ToolSchema,
    ToolValueModel, ValueClass,
};
use serde_json::Value;

/// Static schema enricher for Confluence knowledge base tools.
///
/// Today this enricher only advertises category support and leaves schemas
/// and arguments unchanged. Confluence-specific schema hints can be added
/// here later without changing the provider shape.
pub struct ConfluenceSchemaEnricher;

impl ConfluenceSchemaEnricher {
    /// Build a default Confluence schema enricher.
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

    /// Paper 3 value models for the six KB tools.
    ///
    /// Read endpoints are `ReadOnly` with realistic typical/max sizes
    /// drawn from observed Confluence pages (a single page can be tens
    /// of KB once attachments / large tables / embedded images are
    /// rendered as storage XML). `search_knowledge_base` and
    /// `list_knowledge_base_pages` declare a `get_knowledge_base_page`
    /// follow-up with `pageId` projection so the planner can prefetch
    /// the most likely next call. Mutating endpoints are
    /// `MutatesExternal` and never speculatable.
    fn value_model(&self, tool_name: &str) -> Option<ToolValueModel> {
        let model = match tool_name {
            "search_knowledge_base" => ToolValueModel {
                value_class: ValueClass::Supporting,
                cost_model: CostModel {
                    typical_kb: 4.0,
                    max_kb: Some(40.0),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![FollowUpLink {
                    tool: "get_knowledge_base_page".into(),
                    probability: 0.55,
                    projection: Some("id".into()),
                    projection_arg: Some("pageId".into()),
                }],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "list_knowledge_base_pages" => ToolValueModel {
                value_class: ValueClass::Supporting,
                cost_model: CostModel {
                    typical_kb: 3.0,
                    max_kb: Some(30.0),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![FollowUpLink {
                    tool: "get_knowledge_base_page".into(),
                    probability: 0.50,
                    projection: Some("id".into()),
                    projection_arg: Some("pageId".into()),
                }],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_knowledge_base_page" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 8.0,
                    max_kb: Some(80.0),
                    freshness_ttl_s: Some(300),
                    ..CostModel::default()
                },
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_knowledge_base_spaces" => ToolValueModel {
                value_class: ValueClass::Supporting,
                cost_model: CostModel {
                    typical_kb: 1.5,
                    max_kb: Some(15.0),
                    // Space tree is fairly stable — long TTL is safe.
                    freshness_ttl_s: Some(1800),
                    ..CostModel::default()
                },
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "create_knowledge_base_page" | "update_knowledge_base_page" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 8.0,
                    max_kb: Some(80.0),
                    ..CostModel::default()
                },
                side_effect_class: SideEffectClass::MutatesExternal,
                // A successful update invalidates any cached snapshot of
                // this page. Search / list responses include only
                // metadata so they are not invalidated here.
                invalidates: vec!["get_knowledge_base_page".into()],
                ..ToolValueModel::default()
            },
            _ => return None,
        };
        Some(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn confluence_enricher_supports_knowledge_base_category() {
        let enricher = ConfluenceSchemaEnricher::new();
        assert_eq!(
            enricher.supported_categories(),
            &[ToolCategory::KnowledgeBase]
        );
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
        assert_eq!(model.follow_up.len(), 1);
        let follow_up = &model.follow_up[0];
        assert_eq!(follow_up.tool, "get_knowledge_base_page");
        assert_eq!(follow_up.projection.as_deref(), Some("id"));
        assert_eq!(follow_up.projection_arg.as_deref(), Some("pageId"));
    }

    #[test]
    fn paper3_list_chains_to_get_page() {
        let enricher = ConfluenceSchemaEnricher::new();
        let model = enricher.value_model("list_knowledge_base_pages").unwrap();
        assert_eq!(model.follow_up.len(), 1);
        let follow_up = &model.follow_up[0];
        assert_eq!(follow_up.tool, "get_knowledge_base_page");
    }

    #[test]
    fn paper3_get_page_is_read_only_with_long_ttl() {
        let enricher = ConfluenceSchemaEnricher::new();
        let model = enricher.value_model("get_knowledge_base_page").unwrap();
        assert_eq!(model.side_effect_class, SideEffectClass::ReadOnly);
        assert_eq!(model.value_class, ValueClass::Critical);
        assert!(model.cost_model.freshness_ttl_s.unwrap_or(0) >= 300);
        assert!(model.side_effect_class.is_speculatable());
        assert_eq!(model.cost_model.max_kb, Some(80.0));
    }

    #[test]
    fn paper3_mutating_endpoints_are_never_speculatable() {
        let enricher = ConfluenceSchemaEnricher::new();
        for tool_name in ["create_knowledge_base_page", "update_knowledge_base_page"] {
            let model = enricher.value_model(tool_name).unwrap();
            assert_eq!(model.side_effect_class, SideEffectClass::MutatesExternal);
            assert!(!model.side_effect_class.is_speculatable());
            assert!(
                model
                    .invalidates
                    .iter()
                    .any(|t| t == "get_knowledge_base_page")
            );
        }
    }
}
