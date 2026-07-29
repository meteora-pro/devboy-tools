//! Linear provider metadata for dynamic schema enrichment.

use serde::{Deserialize, Serialize};

/// Metadata for a Linear team, used to enrich issue schemas with
/// team-specific workflow states.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LinearMetadata {
    #[serde(default)]
    pub statuses: Vec<LinearStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearStatus {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub category: Option<String>,
}
