//! MCP protocol types based on JSON-RPC 2.0.
//!
//! The Model Context Protocol uses JSON-RPC 2.0 for communication.
//! This module defines the message types for request/response handling.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC version constant.
pub const JSONRPC_VERSION: &str = "2.0";

/// MCP protocol version.
pub const MCP_VERSION: &str = "2024-11-05";

/// JSON-RPC request message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC response message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC notification (no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// Request ID - can be string, number, or null.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Number(i64),
    Null,
}

/// JSON-RPC error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// Standard JSON-RPC error codes
impl JsonRpcError {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;

    pub fn parse_error(msg: &str) -> Self {
        Self {
            code: Self::PARSE_ERROR,
            message: format!("Parse error: {}", msg),
            data: None,
        }
    }

    pub fn invalid_request(msg: &str) -> Self {
        Self {
            code: Self::INVALID_REQUEST,
            message: format!("Invalid request: {}", msg),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: Self::METHOD_NOT_FOUND,
            message: format!("Method not found: {}", method),
            data: None,
        }
    }

    pub fn invalid_params(msg: &str) -> Self {
        Self {
            code: Self::INVALID_PARAMS,
            message: format!("Invalid params: {}", msg),
            data: None,
        }
    }

    pub fn internal_error(msg: &str) -> Self {
        Self {
            code: Self::INTERNAL_ERROR,
            message: format!("Internal error: {}", msg),
            data: None,
        }
    }
}

impl JsonRpcResponse {
    /// Create a successful response.
    pub fn success(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response.
    pub fn error(id: RequestId, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

// ============================================================================
// MCP-specific types
// ============================================================================

/// MCP initialization request params.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    pub client_info: ClientInfo,
}

/// Client capabilities.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientCapabilities {
    #[serde(default)]
    pub roots: Option<RootsCapability>,
    #[serde(default)]
    pub sampling: Option<SamplingCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootsCapability {
    #[serde(default)]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplingCapability {}

/// Client info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// MCP initialization response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: ServerInfo,
}

/// Server capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(default)]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcesCapability {
    #[serde(default)]
    pub subscribe: bool,
    #[serde(default)]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptsCapability {
    #[serde(default)]
    pub list_changed: bool,
}

/// Server info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// Tool definition for tools/list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Tools list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolDefinition>,
}

/// Tool call request params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Option<Value>,
}

/// Tool call result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    pub content: Vec<ToolResultContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// Content in tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultContent {
    #[serde(rename = "text")]
    Text { text: String },
}

impl ToolCallResult {
    /// Create a successful text result.
    pub fn text(content: String) -> Self {
        Self {
            content: vec![ToolResultContent::Text { text: content }],
            is_error: None,
        }
    }

    /// Create an error result.
    pub fn error(message: String) -> Self {
        Self {
            content: vec![ToolResultContent::Text { text: message }],
            is_error: Some(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({"test": true})),
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn test_response_success() {
        let resp = JsonRpcResponse::success(
            RequestId::String("abc".to_string()),
            serde_json::json!({"result": "ok"}),
        );

        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[test]
    fn test_response_error() {
        let resp =
            JsonRpcResponse::error(RequestId::Number(1), JsonRpcError::method_not_found("test"));

        assert!(resp.result.is_none());
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, JsonRpcError::METHOD_NOT_FOUND);
    }

    #[test]
    fn test_tool_call_result() {
        let result = ToolCallResult::text("Hello".to_string());
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"Hello\""));
    }

    #[test]
    fn test_tool_call_result_error() {
        let result = ToolCallResult::error("Something failed".to_string());
        assert_eq!(result.is_error, Some(true));
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("Something failed"));
    }

    #[test]
    fn test_parse_error() {
        let err = JsonRpcError::parse_error("bad json");
        assert_eq!(err.code, JsonRpcError::PARSE_ERROR);
        assert!(err.message.contains("bad json"));
        assert!(err.data.is_none());
    }

    #[test]
    fn test_invalid_request_error() {
        let err = JsonRpcError::invalid_request("not initialized");
        assert_eq!(err.code, JsonRpcError::INVALID_REQUEST);
        assert!(err.message.contains("not initialized"));
    }

    #[test]
    fn test_invalid_params_error() {
        let err = JsonRpcError::invalid_params("missing field");
        assert_eq!(err.code, JsonRpcError::INVALID_PARAMS);
        assert!(err.message.contains("missing field"));
    }

    #[test]
    fn test_internal_error() {
        let err = JsonRpcError::internal_error("unexpected");
        assert_eq!(err.code, JsonRpcError::INTERNAL_ERROR);
        assert!(err.message.contains("unexpected"));
    }

    #[test]
    fn test_request_id_variants() {
        let num = RequestId::Number(42);
        let str_id = RequestId::String("abc".to_string());
        let null = RequestId::Null;

        assert_eq!(num, RequestId::Number(42));
        assert_eq!(str_id, RequestId::String("abc".to_string()));
        assert_eq!(null, RequestId::Null);

        // Serialization
        let json = serde_json::to_string(&num).unwrap();
        assert_eq!(json, "42");

        let json = serde_json::to_string(&str_id).unwrap();
        assert_eq!(json, "\"abc\"");

        let json = serde_json::to_string(&null).unwrap();
        assert_eq!(json, "null");
    }

    #[test]
    fn test_notification_serialization() {
        let notif = JsonRpcNotification {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "initialized".to_string(),
            params: None,
        };

        let json = serde_json::to_string(&notif).unwrap();
        assert!(json.contains("\"method\":\"initialized\""));
        // params should be skipped when None
        assert!(!json.contains("params"));
    }
}
