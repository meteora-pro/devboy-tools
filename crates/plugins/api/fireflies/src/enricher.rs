//! Schema enricher for Fireflies meeting notes tools.

use devboy_core::{
    CostModel, FollowUpLink, SideEffectClass, ToolCategory, ToolEnricher, ToolSchema,
    ToolValueModel, ValueClass,
};
use serde_json::Value;

/// Enricher for Fireflies.ai — declares support for MeetingNotes category.
pub struct FirefliesSchemaEnricher;

impl ToolEnricher for FirefliesSchemaEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &[ToolCategory::MeetingNotes]
    }

    fn enrich_schema(&self, _tool_name: &str, _schema: &mut ToolSchema) {
        // Fireflies has no dynamic enrichment (no custom fields or metadata-driven enums).
        // All parameters are statically defined in the base tool definitions.
    }

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {
        // No argument transformation needed.
    }

    /// Paper 3 — Fireflies meetings → transcript chain. Transcripts
    /// are large (typical 8 kB, max ~50 kB) but content is highly
    /// reusable across the session, so prefetch hit rate is high
    /// when the user asks "what was discussed".
    fn value_model(&self, tool_name: &str) -> Option<ToolValueModel> {
        let model = match tool_name {
            "list_meetings" | "get_meeting_notes" => ToolValueModel {
                value_class: ValueClass::Supporting,
                cost_model: CostModel {
                    typical_kb: 3.0,
                    latency_ms_p50: Some(450),
                    freshness_ttl_s: Some(300),
                    ..CostModel::default()
                },
                follow_up: vec![FollowUpLink {
                    tool: "get_meeting_transcript".into(),
                    probability: 0.55,
                    projection: Some("id".into()),
                    projection_arg: Some("meeting_id".into()),
                }],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_meeting_transcript" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 8.0,
                    max_kb: Some(50.0),
                    latency_ms_p50: Some(900),
                    freshness_ttl_s: Some(600),
                    ..CostModel::default()
                },
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            _ => return None,
        };
        Some(model)
    }

    /// Fireflies API host.
    fn rate_limit_host(&self, _tool_name: &str, _args: &Value) -> Option<String> {
        Some("api.fireflies.ai".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_categories() {
        let enricher = FirefliesSchemaEnricher;
        assert_eq!(
            enricher.supported_categories(),
            &[ToolCategory::MeetingNotes]
        );
    }

    #[test]
    fn test_enrich_schema_is_noop() {
        let enricher = FirefliesSchemaEnricher;
        let mut schema = ToolSchema::new();
        schema.add_property("test", devboy_core::PropertySchema::string("test field"));
        let original_len = schema.properties.len();
        enricher.enrich_schema("get_meeting_notes", &mut schema);
        assert_eq!(schema.properties.len(), original_len);
    }
}
