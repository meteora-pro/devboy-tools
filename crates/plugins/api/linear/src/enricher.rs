//! Linear schema enricher.
//!
//! The initial provider slice advertises IssueTracker support but
//! does not yet inject team-specific workflow metadata.

use devboy_core::{ToolCategory, ToolEnricher, ToolSchema};
use serde_json::Value;

pub struct LinearSchemaEnricher;

impl ToolEnricher for LinearSchemaEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &[ToolCategory::IssueTracker]
    }

    fn enrich_schema(&self, _tool_name: &str, _schema: &mut ToolSchema) {}

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {}
}
