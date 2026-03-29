//! Schema enricher for Fireflies meeting notes tools.

use devboy_core::{ToolCategory, ToolEnricher, ToolSchema};
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
