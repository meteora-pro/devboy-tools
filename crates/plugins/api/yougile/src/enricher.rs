//! YouGile schema enricher scaffold.

use devboy_core::{ToolCategory, ToolEnricher, ToolSchema};
use serde_json::Value;

use crate::metadata::YouGileMetadata;

/// No-op schema enricher placeholder for YouGile.
///
/// The real implementation will use board/column metadata to refine issue tool
/// schemas once the provider behavior is in place.
pub struct YouGileSchemaEnricher {
    #[allow(dead_code)]
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

    fn enrich_schema(&self, _tool_name: &str, _schema: &mut ToolSchema) {}

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {}
}
