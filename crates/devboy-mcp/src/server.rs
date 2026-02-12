//! MCP server implementation.
//!
//! The server handles the MCP protocol lifecycle:
//! 1. Initialize - exchange capabilities
//! 2. Handle tool calls - execute tools via providers
//! 3. Shutdown - graceful cleanup

use std::sync::Arc;

use devboy_core::Provider;
use serde_json::Value;

use crate::handlers::ToolHandler;
use crate::protocol::{
    InitializeParams, InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse, RequestId,
    ServerCapabilities, ServerInfo, ToolCallParams, ToolsCapability, ToolsListResult, MCP_VERSION,
};
use crate::transport::{IncomingMessage, StdioTransport};

/// MCP server for devboy-tools.
pub struct McpServer {
    providers: Vec<Arc<dyn Provider>>,
    initialized: bool,
}

impl McpServer {
    /// Create a new MCP server.
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            initialized: false,
        }
    }

    /// Add a provider to the server.
    pub fn add_provider(&mut self, provider: Arc<dyn Provider>) {
        self.providers.push(provider);
    }

    /// Get all registered providers.
    pub fn providers(&self) -> &[Arc<dyn Provider>] {
        &self.providers
    }

    /// Run the MCP server main loop.
    pub async fn run(&mut self) -> devboy_core::Result<()> {
        tracing::info!(
            "Starting MCP server with {} providers",
            self.providers.len()
        );

        let mut transport = StdioTransport::stdio();
        let handler = ToolHandler::new(self.providers.clone());

        loop {
            match transport.read_message() {
                Ok(Some(msg)) => {
                    let response = self.handle_message(msg, &handler).await;
                    if let Some(resp) = response {
                        if let Err(e) = transport.write_response(&resp) {
                            tracing::error!("Failed to write response: {}", e);
                            break;
                        }
                    }
                }
                Ok(None) => {
                    tracing::info!("EOF received, shutting down");
                    break;
                }
                Err(e) => {
                    tracing::error!("Transport error: {}", e);
                    // Try to send error response
                    let error_resp = JsonRpcResponse::error(
                        RequestId::Null,
                        JsonRpcError::parse_error(&e.to_string()),
                    );
                    let _ = transport.write_response(&error_resp);
                }
            }
        }

        tracing::info!("MCP server stopped");
        Ok(())
    }

    /// Handle an incoming message.
    async fn handle_message(
        &mut self,
        msg: IncomingMessage,
        handler: &ToolHandler,
    ) -> Option<JsonRpcResponse> {
        match msg {
            IncomingMessage::Request(req) => Some(self.handle_request(req, handler).await),
            IncomingMessage::Notification(notif) => {
                self.handle_notification(&notif.method);
                None // Notifications don't get responses
            }
        }
    }

    /// Handle a JSON-RPC request.
    async fn handle_request(
        &mut self,
        req: JsonRpcRequest,
        handler: &ToolHandler,
    ) -> JsonRpcResponse {
        tracing::debug!("Handling request: {} (id: {:?})", req.method, req.id);

        match req.method.as_str() {
            "initialize" => self.handle_initialize(req.id, req.params),
            "tools/list" => self.handle_tools_list(req.id, handler),
            "tools/call" => self.handle_tools_call(req.id, req.params, handler).await,
            "ping" => self.handle_ping(req.id),
            method => {
                tracing::warn!("Unknown method: {}", method);
                JsonRpcResponse::error(req.id, JsonRpcError::method_not_found(method))
            }
        }
    }

    /// Handle notifications (no response).
    fn handle_notification(&mut self, method: &str) {
        match method {
            "initialized" => {
                tracing::info!("Client initialized");
            }
            "notifications/cancelled" => {
                tracing::debug!("Request cancelled by client");
            }
            _ => {
                tracing::debug!("Ignoring notification: {}", method);
            }
        }
    }

    /// Handle initialize request.
    fn handle_initialize(&mut self, id: RequestId, params: Option<Value>) -> JsonRpcResponse {
        if self.initialized {
            return JsonRpcResponse::error(
                id,
                JsonRpcError::invalid_request("Server already initialized"),
            );
        }

        // Parse params (optional validation)
        if let Some(params) = params {
            match serde_json::from_value::<InitializeParams>(params) {
                Ok(init_params) => {
                    tracing::info!(
                        "Client: {} v{} (protocol: {})",
                        init_params.client_info.name,
                        init_params.client_info.version,
                        init_params.protocol_version
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to parse initialize params: {}", e);
                }
            }
        }

        self.initialized = true;

        let result = InitializeResult {
            protocol_version: MCP_VERSION.to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: false,
                }),
                resources: None,
                prompts: None,
            },
            server_info: ServerInfo {
                name: "devboy-mcp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        JsonRpcResponse::success(id, serde_json::to_value(result).unwrap())
    }

    /// Handle tools/list request.
    fn handle_tools_list(&self, id: RequestId, handler: &ToolHandler) -> JsonRpcResponse {
        let tools = handler.available_tools();

        let result = ToolsListResult { tools };
        JsonRpcResponse::success(id, serde_json::to_value(result).unwrap())
    }

    /// Handle tools/call request.
    async fn handle_tools_call(
        &self,
        id: RequestId,
        params: Option<Value>,
        handler: &ToolHandler,
    ) -> JsonRpcResponse {
        let params: ToolCallParams = match params {
            Some(p) => match serde_json::from_value(p) {
                Ok(params) => params,
                Err(e) => {
                    return JsonRpcResponse::error(
                        id,
                        JsonRpcError::invalid_params(&e.to_string()),
                    );
                }
            },
            None => {
                return JsonRpcResponse::error(id, JsonRpcError::invalid_params("Missing params"));
            }
        };

        tracing::info!("Calling tool: {}", params.name);

        let result = handler.execute(&params.name, params.arguments).await;
        JsonRpcResponse::success(id, serde_json::to_value(result).unwrap())
    }

    /// Handle ping request.
    fn handle_ping(&self, id: RequestId) -> JsonRpcResponse {
        JsonRpcResponse::success(id, serde_json::json!({}))
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{RequestId, JSONRPC_VERSION};

    #[test]
    fn test_server_creation() {
        let server = McpServer::new();
        assert!(server.providers.is_empty());
        assert!(!server.initialized);
    }

    #[test]
    fn test_initialize_response() {
        let mut server = McpServer::new();
        let handler = ToolHandler::new(vec![]);

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            })),
        };

        let resp = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(server.handle_request(req, &handler));

        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
        assert!(server.initialized);
    }

    #[test]
    fn test_tools_list() {
        let server = McpServer::new();
        let handler = ToolHandler::new(vec![]);

        let resp = server.handle_tools_list(RequestId::Number(1), &handler);

        assert!(resp.result.is_some());
        let result: ToolsListResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(!result.tools.is_empty());
        assert!(result.tools.iter().any(|t| t.name == "get_issues"));
        assert!(result.tools.iter().any(|t| t.name == "get_merge_requests"));
    }

    #[test]
    fn test_ping() {
        let server = McpServer::new();
        let resp = server.handle_ping(RequestId::String("ping-1".to_string()));

        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_double_initialize_error() {
        let mut server = McpServer::new();
        server.initialized = true;

        let resp = server.handle_initialize(RequestId::Number(1), None);

        assert!(resp.error.is_some());
        assert!(resp.result.is_none());
    }

    #[test]
    fn test_unknown_method() {
        let mut server = McpServer::new();
        let handler = ToolHandler::new(vec![]);

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "unknown/method".to_string(),
            params: None,
        };

        let resp = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(server.handle_request(req, &handler));

        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, JsonRpcError::METHOD_NOT_FOUND);
    }

    #[test]
    fn test_add_provider_and_providers() {
        use async_trait::async_trait;
        use devboy_core::{
            Comment, CreateCommentInput, CreateIssueInput, Discussion, FileDiff, Issue,
            IssueFilter, IssueProvider, MergeRequest, MergeRequestProvider, MrFilter,
            UpdateIssueInput, User,
        };

        struct TestProvider;

        #[async_trait]
        impl IssueProvider for TestProvider {
            async fn get_issues(&self, _filter: IssueFilter) -> devboy_core::Result<Vec<Issue>> {
                Ok(vec![])
            }
            async fn get_issue(&self, _key: &str) -> devboy_core::Result<Issue> {
                Err(devboy_core::Error::NotFound("not found".into()))
            }
            async fn create_issue(&self, _input: CreateIssueInput) -> devboy_core::Result<Issue> {
                Err(devboy_core::Error::NotFound("not found".into()))
            }
            async fn update_issue(
                &self,
                _key: &str,
                _input: UpdateIssueInput,
            ) -> devboy_core::Result<Issue> {
                Err(devboy_core::Error::NotFound("not found".into()))
            }
            async fn get_comments(&self, _issue_key: &str) -> devboy_core::Result<Vec<Comment>> {
                Ok(vec![])
            }
            async fn add_comment(
                &self,
                _issue_key: &str,
                _body: &str,
            ) -> devboy_core::Result<Comment> {
                Err(devboy_core::Error::NotFound("not found".into()))
            }
            fn provider_name(&self) -> &'static str {
                "test"
            }
        }

        #[async_trait]
        impl MergeRequestProvider for TestProvider {
            async fn get_merge_requests(
                &self,
                _filter: MrFilter,
            ) -> devboy_core::Result<Vec<MergeRequest>> {
                Ok(vec![])
            }
            async fn get_merge_request(&self, _key: &str) -> devboy_core::Result<MergeRequest> {
                Err(devboy_core::Error::NotFound("not found".into()))
            }
            async fn get_discussions(&self, _mr_key: &str) -> devboy_core::Result<Vec<Discussion>> {
                Ok(vec![])
            }
            async fn get_diffs(&self, _mr_key: &str) -> devboy_core::Result<Vec<FileDiff>> {
                Ok(vec![])
            }
            async fn add_comment(
                &self,
                _mr_key: &str,
                _input: CreateCommentInput,
            ) -> devboy_core::Result<Comment> {
                Err(devboy_core::Error::NotFound("not found".into()))
            }
            fn provider_name(&self) -> &'static str {
                "test"
            }
        }

        #[async_trait]
        impl Provider for TestProvider {
            async fn get_current_user(&self) -> devboy_core::Result<User> {
                Ok(User {
                    id: "1".to_string(),
                    username: "test".to_string(),
                    name: None,
                    email: None,
                    avatar_url: None,
                })
            }
        }

        let mut server = McpServer::new();
        assert!(server.providers().is_empty());

        server.add_provider(Arc::new(TestProvider));
        assert_eq!(server.providers().len(), 1);
    }

    #[test]
    fn test_handle_notification_initialized() {
        let mut server = McpServer::new();
        // Should not panic
        server.handle_notification("initialized");
    }

    #[test]
    fn test_handle_notification_cancelled() {
        let mut server = McpServer::new();
        // Should not panic
        server.handle_notification("notifications/cancelled");
    }

    #[test]
    fn test_handle_notification_unknown() {
        let mut server = McpServer::new();
        // Should not panic
        server.handle_notification("some/unknown/notification");
    }

    #[tokio::test]
    async fn test_handle_message_notification() {
        let mut server = McpServer::new();
        let handler = ToolHandler::new(vec![]);

        let msg = IncomingMessage::Notification(crate::protocol::JsonRpcNotification {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "initialized".to_string(),
            params: None,
        });

        let response = server.handle_message(msg, &handler).await;
        // Notifications should return None
        assert!(response.is_none());
    }

    #[tokio::test]
    async fn test_handle_message_request() {
        let mut server = McpServer::new();
        let handler = ToolHandler::new(vec![]);

        let msg = IncomingMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "ping".to_string(),
            params: None,
        });

        let response = server.handle_message(msg, &handler).await;
        // Requests should return Some
        assert!(response.is_some());
        let resp = response.unwrap();
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn test_handle_tools_call() {
        let mut server = McpServer::new();
        let handler = ToolHandler::new(vec![]);

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "get_issues",
                "arguments": {}
            })),
        };

        let resp = server.handle_request(req, &handler).await;
        // Will return error since no providers, but should not panic
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn test_handle_tools_call_missing_params() {
        let mut server = McpServer::new();
        let handler = ToolHandler::new(vec![]);

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: None,
        };

        let resp = server.handle_request(req, &handler).await;
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn test_handle_tools_call_invalid_params() {
        let mut server = McpServer::new();
        let handler = ToolHandler::new(vec![]);

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!("not an object")),
        };

        let resp = server.handle_request(req, &handler).await;
        assert!(resp.error.is_some());
    }

    #[test]
    fn test_initialize_without_params() {
        let mut server = McpServer::new();

        let resp = server.handle_initialize(RequestId::Number(1), None);

        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
        assert!(server.initialized);
    }

    #[test]
    fn test_initialize_with_invalid_params() {
        let mut server = McpServer::new();

        // Invalid params should still succeed (just log a warning)
        let resp = server.handle_initialize(
            RequestId::Number(1),
            Some(serde_json::json!({"invalid": true})),
        );

        assert!(resp.result.is_some());
        assert!(server.initialized);
    }

    #[test]
    fn test_default_trait() {
        let server = McpServer::default();
        assert!(server.providers().is_empty());
    }
}
