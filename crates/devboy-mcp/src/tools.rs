//! MCP tool definitions.

use serde::{Deserialize, Serialize};

/// MCP tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Available MCP tools.
pub fn available_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "get_issues".to_string(),
            description: "Get issues from configured git providers".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "state": {
                        "type": "string",
                        "enum": ["open", "closed", "all"],
                        "description": "Filter by issue state"
                    }
                }
            }),
        },
        Tool {
            name: "get_merge_requests".to_string(),
            description: "Get merge requests / pull requests from configured git providers"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "state": {
                        "type": "string",
                        "enum": ["open", "closed", "merged", "all"],
                        "description": "Filter by MR/PR state"
                    }
                }
            }),
        },
    ]
}
