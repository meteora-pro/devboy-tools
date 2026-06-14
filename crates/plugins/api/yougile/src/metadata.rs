//! YouGile provider metadata types for dynamic schema enrichment.

use serde::{Deserialize, Serialize};

/// Workspace metadata used to enrich the YouGile tool schema at runtime.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct YouGileMetadata {
    /// Available boards in the current workspace/company.
    #[serde(default)]
    pub boards: Vec<YouGileBoard>,
    /// Available columns across the scoped boards.
    #[serde(default)]
    pub columns: Vec<YouGileColumn>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YouGileBoard {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YouGileColumn {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub board_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metadata_deserializes_board_and_column_lists() {
        let raw = json!({
            "boards": [
                { "id": "board-1", "title": "Platform" }
            ],
            "columns": [
                { "id": "col-1", "title": "To Do", "board_id": "board-1" }
            ]
        });

        let metadata: YouGileMetadata = serde_json::from_value(raw).unwrap();
        assert_eq!(metadata.boards.len(), 1);
        assert_eq!(metadata.columns.len(), 1);
        assert_eq!(metadata.columns[0].board_id.as_deref(), Some("board-1"));
    }
}
