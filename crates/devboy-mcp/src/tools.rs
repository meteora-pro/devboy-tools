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
    fn test_available_tools_not_empty() {
        let tools = available_tools();
        assert!(!tools.is_empty(), "should have at least one tool");
    }

    #[test]
    fn test_available_tools_contain_expected_names() {
        let tools = available_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"get_issues"), "missing get_issues tool");
        assert!(
            names.contains(&"get_merge_requests"),
            "missing get_merge_requests tool"
        );
    }

    #[test]
    fn test_available_tools_have_descriptions() {
        let tools = available_tools();
        for tool in &tools {
            assert!(
                !tool.description.is_empty(),
                "tool '{}' has empty description",
                tool.name
            );
        }
    }

    #[test]
    fn test_available_tools_have_valid_parameters() {
        let tools = available_tools();
        for tool in &tools {
            assert_eq!(
                tool.parameters["type"], "object",
                "tool '{}' parameters should be object type",
                tool.name
            );
            assert!(
                tool.parameters["properties"].is_object(),
                "tool '{}' should have properties object",
                tool.name
            );
        }
    }

    #[test]
    fn test_tool_serialization_roundtrip() {
        let tools = available_tools();
        let json = serde_json::to_string(&tools).unwrap();
        let parsed: Vec<Tool> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), tools.len());
        let parsed_names: Vec<&str> = parsed.iter().map(|t| t.name.as_str()).collect();
        for tool in &tools {
            assert!(parsed_names.contains(&tool.name.as_str()));
        }
    }
}
