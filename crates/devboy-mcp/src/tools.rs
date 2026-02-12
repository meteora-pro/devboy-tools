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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_available_tools_count() {
        let tools = available_tools();
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn test_available_tools_names() {
        let tools = available_tools();
        assert_eq!(tools[0].name, "get_issues");
        assert_eq!(tools[1].name, "get_merge_requests");
    }

    #[test]
    fn test_available_tools_descriptions() {
        let tools = available_tools();
        assert!(!tools[0].description.is_empty());
        assert!(!tools[1].description.is_empty());
    }

    #[test]
    fn test_available_tools_parameters() {
        let tools = available_tools();
        for tool in &tools {
            assert_eq!(tool.parameters["type"], "object");
            assert!(tool.parameters["properties"].is_object());
        }
    }

    #[test]
    fn test_tool_serialization() {
        let tools = available_tools();
        let json = serde_json::to_string(&tools).unwrap();
        let parsed: Vec<Tool> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "get_issues");
    }
}
