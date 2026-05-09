//! MCP proxy — connects to upstream MCP servers and proxies tool calls.
//!
//! Supports two transports:
//! - **SSE**: Legacy MCP transport with SSE stream for responses.
//! - **Streamable HTTP**: POST-based transport with `mcp-session-id` header.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest_eventsource::{Event, EventSource};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, oneshot};

use crate::protocol::{
    JSONRPC_VERSION, JsonRpcRequest, JsonRpcResponse, RequestId, ToolCallResult, ToolDefinition,
};

/// Transport mode for upstream MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyTransport {
    /// SSE-based transport (legacy MCP): GET for SSE stream, POST for requests.
    Sse,
    /// Streamable HTTP transport: POST with JSON response and mcp-session-id header.
    StreamableHttp,
}

impl ProxyTransport {
    pub fn parse(s: &str) -> Self {
        match s {
            "streamable-http" | "streamable_http" | "http" => Self::StreamableHttp,
            _ => Self::Sse,
        }
    }
}

/// Pending SSE response receivers indexed by request ID.
type PendingResponses = Arc<Mutex<Vec<(i64, oneshot::Sender<JsonRpcResponse>)>>>;

/// Single upstream MCP server connection.
pub struct McpProxyClient {
    name: String,
    tool_prefix: String,
    post_url: String,
    http_client: reqwest::Client,
    upstream_tools: Vec<ToolDefinition>,
    next_id: AtomicI64,
    transport: ProxyTransport,
    /// Session ID for Streamable HTTP transport.
    session_id: RwLock<Option<String>>,
    /// Channel to receive SSE responses routed by request id (SSE transport only).
    pending: PendingResponses,
}

impl McpProxyClient {
    /// Connect to an upstream MCP server, perform initialize handshake, fetch tools.
    pub async fn connect(
        name: &str,
        url: &str,
        tool_prefix: Option<&str>,
        token: Option<&SecretString>,
        auth_type: &str,
        transport: ProxyTransport,
    ) -> devboy_core::Result<Self> {
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            match auth_type {
                "bearer" => {
                    let val = HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
                        .map_err(|e| {
                        devboy_core::Error::Config(format!("Invalid token: {}", e))
                    })?;
                    headers.insert(AUTHORIZATION, val);
                }
                "api_key" => {
                    let val = HeaderValue::from_str(token.expose_secret())
                        .map_err(|e| devboy_core::Error::Config(format!("Invalid token: {}", e)))?;
                    headers.insert("X-API-Key", val);
                }
                _ => {}
            }
        }

        let http_client = reqwest::Client::builder()
            .default_headers(headers.clone())
            .timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|e| devboy_core::Error::Http(format!("Failed to build HTTP client: {}", e)))?;

        let prefix = tool_prefix.unwrap_or(name).to_string();

        match transport {
            ProxyTransport::Sse => {
                Self::connect_sse(name, url, &prefix, headers, http_client).await
            }
            ProxyTransport::StreamableHttp => {
                Self::connect_streamable_http(name, url, &prefix, http_client).await
            }
        }
    }

    /// Connect via SSE transport.
    async fn connect_sse(
        name: &str,
        url: &str,
        prefix: &str,
        headers: HeaderMap,
        http_client: reqwest::Client,
    ) -> devboy_core::Result<Self> {
        let sse_url = url.to_string();
        let mut es = EventSource::new(
            reqwest::Client::builder()
                .default_headers(headers)
                .build()
                .unwrap()
                .get(&sse_url),
        )
        .map_err(|e| {
            devboy_core::Error::Http(format!("Failed to connect SSE to {}: {}", sse_url, e))
        })?;

        let post_url = Self::wait_for_endpoint(&mut es, url).await?;

        let pending: PendingResponses = Arc::new(Mutex::new(Vec::new()));

        // Spawn SSE listener
        let pending_clone = pending.clone();
        tokio::spawn(async move {
            while let Some(event) = es.next().await {
                match event {
                    Ok(Event::Message(msg)) => {
                        if msg.event == "message"
                            && let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&msg.data)
                        {
                            let id_num = match &resp.id {
                                RequestId::Number(n) => *n,
                                _ => continue,
                            };
                            let mut pending = pending_clone.lock().await;
                            if let Some(idx) = pending.iter().position(|(id, _)| *id == id_num) {
                                let (_, sender) = pending.remove(idx);
                                let _ = sender.send(resp);
                            }
                        }
                    }
                    Ok(Event::Open) => {
                        tracing::debug!("SSE stream open");
                    }
                    Err(e) => {
                        tracing::warn!("SSE error: {}", e);
                        break;
                    }
                }
            }
        });

        let client = Self {
            name: name.to_string(),
            tool_prefix: prefix.to_string(),
            post_url,
            http_client,
            upstream_tools: Vec::new(),
            next_id: AtomicI64::new(1),
            transport: ProxyTransport::Sse,
            session_id: RwLock::new(None),
            pending,
        };

        client.initialize().await?;

        Ok(client)
    }

    /// Connect via Streamable HTTP transport.
    async fn connect_streamable_http(
        name: &str,
        url: &str,
        prefix: &str,
        http_client: reqwest::Client,
    ) -> devboy_core::Result<Self> {
        let client = Self {
            name: name.to_string(),
            tool_prefix: prefix.to_string(),
            post_url: url.to_string(),
            http_client,
            upstream_tools: Vec::new(),
            next_id: AtomicI64::new(1),
            transport: ProxyTransport::StreamableHttp,
            session_id: RwLock::new(None),
            pending: Arc::new(Mutex::new(Vec::new())),
        };

        client.initialize().await?;

        Ok(client)
    }

    /// Wait for the SSE `endpoint` event that tells us where to POST requests.
    async fn wait_for_endpoint(
        es: &mut EventSource,
        base_url: &str,
    ) -> devboy_core::Result<String> {
        let timeout = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = es.next().await {
                match event {
                    Ok(Event::Message(msg)) if msg.event == "endpoint" => {
                        let endpoint = msg.data.trim().to_string();
                        // If relative URL, resolve against base
                        if endpoint.starts_with('/')
                            && let Ok(base) = reqwest::Url::parse(base_url)
                            && let Ok(resolved) = base.join(&endpoint)
                        {
                            return Ok(resolved.to_string());
                        }
                        return Ok(endpoint);
                    }
                    Ok(Event::Open) => continue,
                    Ok(_) => continue,
                    Err(e) => {
                        return Err(devboy_core::Error::Http(format!("SSE error: {}", e)));
                    }
                }
            }
            Err(devboy_core::Error::Http(
                "SSE stream ended before endpoint event".to_string(),
            ))
        });

        timeout.await.map_err(|_| {
            devboy_core::Error::Http("Timeout waiting for SSE endpoint event".to_string())
        })?
    }

    fn next_request_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a JSON-RPC request and wait for response.
    async fn request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> devboy_core::Result<JsonRpcResponse> {
        match self.transport {
            ProxyTransport::Sse => self.request_sse(method, params).await,
            ProxyTransport::StreamableHttp => self.request_http(method, params).await,
        }
    }

    /// Send request via SSE transport (POST request, response via SSE stream).
    async fn request_sse(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> devboy_core::Result<JsonRpcResponse> {
        let id = self.next_request_id();
        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(id),
            method: method.to_string(),
            params,
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.push((id, tx));
        }

        self.http_client
            .post(&self.post_url)
            .json(&req)
            .send()
            .await
            .map_err(|e| devboy_core::Error::Http(format!("POST failed: {}", e)))?;

        let resp = tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .map_err(|_| devboy_core::Error::Http("Timeout waiting for response".to_string()))?
            .map_err(|_| devboy_core::Error::Http("Response channel closed".to_string()))?;

        Ok(resp)
    }

    /// Send request via Streamable HTTP transport (POST, JSON response, session header).
    async fn request_http(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> devboy_core::Result<JsonRpcResponse> {
        let id = self.next_request_id();
        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(id),
            method: method.to_string(),
            params,
        };

        let mut request = self
            .http_client
            .post(&self.post_url)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream");

        // Add session ID for all requests after initialize
        if method != "initialize" {
            let session = self.session_id.read().await;
            if let Some(sid) = session.as_ref() {
                request = request.header("mcp-session-id", sid);
            }
        }

        let response = request.json(&req).send().await.map_err(|e| {
            tracing::error!(
                "POST to {} failed: {} (is_timeout={}, is_connect={}, is_request={})",
                self.post_url,
                e,
                e.is_timeout(),
                e.is_connect(),
                e.is_request(),
            );
            devboy_core::Error::Http(format!("POST failed: {}", e))
        })?;

        // Extract session ID from response headers (set during initialize)
        if method == "initialize"
            && let Some(sid) = response.headers().get("mcp-session-id")
            && let Ok(sid_str) = sid.to_str()
        {
            let mut session = self.session_id.write().await;
            *session = Some(sid_str.to_string());
            tracing::debug!("Proxy '{}': got session ID", self.name);
        }

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(devboy_core::Error::Http(format!(
                "HTTP {}: {}",
                status, body
            )));
        }

        // Check Content-Type: server may respond with JSON or SSE stream
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let resp = if content_type.contains("text/event-stream") {
            // Parse SSE stream to extract JSON-RPC response
            tracing::debug!("Response is SSE stream, parsing events...");
            self.parse_sse_response(response, id).await?
        } else {
            // Direct JSON response
            tracing::debug!("Response is JSON (content-type: {})", content_type);
            response
                .json::<JsonRpcResponse>()
                .await
                .map_err(|e| devboy_core::Error::Http(format!("Failed to parse response: {}", e)))?
        };

        // Verify response ID matches request ID
        let expected_id = RequestId::Number(id);
        if resp.id != expected_id {
            return Err(devboy_core::Error::Http(format!(
                "Mismatched JSON-RPC id: expected {:?}, got {:?}",
                expected_id, resp.id
            )));
        }

        Ok(resp)
    }

    /// Parse an SSE event stream response to extract the JSON-RPC response.
    ///
    /// Streamable HTTP spec allows servers to respond with SSE for long-running
    /// operations like tool calls. Reads the stream line-by-line instead of
    /// buffering the entire body (which would hang on open SSE connections).
    async fn parse_sse_response(
        &self,
        response: reqwest::Response,
        expected_id: i64,
    ) -> devboy_core::Result<JsonRpcResponse> {
        use futures::TryStreamExt;
        use tokio::io::AsyncBufReadExt;

        let stream = response.bytes_stream().map_err(std::io::Error::other);
        let reader = tokio_util::io::StreamReader::new(stream);
        let mut lines = tokio::io::BufReader::new(reader).lines();

        let mut current_data = String::new();

        tracing::debug!("Starting SSE line reader...");

        tokio::time::timeout(Duration::from_secs(60), async {
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                let debug_len = line
                    .char_indices()
                    .nth(100)
                    .map(|(i, _)| i)
                    .unwrap_or(line.len());
                tracing::debug!("SSE line: {}", &line[..debug_len]);

                if line.is_empty() {
                    // End of SSE event — try to parse collected data
                    if !current_data.is_empty()
                        && let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&current_data)
                    {
                        let id_matches = match &resp.id {
                            RequestId::Number(n) => *n == expected_id,
                            _ => false,
                        };
                        if id_matches {
                            return Ok(resp);
                        }
                        current_data.clear();
                    } else if !current_data.is_empty() {
                        current_data.clear();
                    }
                    continue;
                }

                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if !data.is_empty() {
                        current_data.push_str(data);
                    }
                }
                // Skip event:, id:, retry: lines
            }

            // Try last accumulated data
            if !current_data.is_empty()
                && let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&current_data)
            {
                return Ok(resp);
            }

            Err(devboy_core::Error::Http(
                "No matching JSON-RPC response found in SSE stream".to_string(),
            ))
        })
        .await
        .map_err(|_| devboy_core::Error::Http("Timeout reading SSE response".to_string()))?
    }

    /// Send initialize handshake.
    async fn initialize(&self) -> devboy_core::Result<()> {
        let params = serde_json::json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {
                "name": "devboy-mcp-proxy",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let resp = self.request("initialize", Some(params)).await?;
        if let Some(err) = resp.error {
            return Err(devboy_core::Error::Http(format!(
                "Initialize failed: {}",
                err.message
            )));
        }

        tracing::info!("Proxy '{}' initialized", self.name);
        Ok(())
    }

    /// Fetch tools/list from upstream. Call this before using `prefixed_tools()`.
    pub async fn fetch_tools(&mut self) -> devboy_core::Result<()> {
        let resp = self.request("tools/list", None).await?;

        if let Some(result) = resp.result {
            #[derive(serde::Deserialize)]
            struct ToolsList {
                tools: Vec<ToolDefinition>,
            }

            if let Ok(list) = serde_json::from_value::<ToolsList>(result) {
                self.upstream_tools = list.tools;
                tracing::info!(
                    "Proxy '{}': fetched {} tools",
                    self.name,
                    self.upstream_tools.len()
                );
            }
        }

        Ok(())
    }

    /// Return upstream tools with prefixed names.
    pub fn prefixed_tools(&self) -> Vec<ToolDefinition> {
        self.upstream_tools
            .iter()
            .map(|t| ToolDefinition {
                name: format!("{}__{}", self.tool_prefix, t.name),
                description: format!("[{}] {}", self.name, t.description),
                input_schema: t.input_schema.clone(),
                category: None, // Proxy tools don't have a category (always available)
            })
            .collect()
    }

    /// Return the raw upstream tool catalogue (without prefixing).
    /// Used by the signature matcher to compare upstream definitions against the local
    /// tool registry. Empty until `fetch_tools()` has been called.
    pub fn raw_upstream_tools(&self) -> &[ToolDefinition] {
        &self.upstream_tools
    }

    /// Execute a tool call, stripping the prefix before forwarding.
    pub async fn call_tool(
        &self,
        original_name: &str,
        arguments: Option<Value>,
    ) -> devboy_core::Result<ToolCallResult> {
        let params = serde_json::json!({
            "name": original_name,
            "arguments": arguments.unwrap_or(Value::Object(Default::default()))
        });

        let resp = self.request("tools/call", Some(params)).await?;

        if let Some(err) = resp.error {
            return Ok(ToolCallResult::error(err.message));
        }

        match resp.result {
            Some(result) => serde_json::from_value(result).map_err(|e| {
                devboy_core::Error::InvalidData(format!("Invalid tool result: {}", e))
            }),
            None => Ok(ToolCallResult::error(
                "Empty response from upstream".to_string(),
            )),
        }
    }

    /// Get the tool prefix for this client.
    pub fn prefix(&self) -> &str {
        &self.tool_prefix
    }
}

/// Manages multiple upstream MCP proxy connections.
pub struct ProxyManager {
    clients: Vec<McpProxyClient>,
}

impl Default for ProxyManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProxyManager {
    pub fn new() -> Self {
        Self {
            clients: Vec::new(),
        }
    }

    pub fn add_client(&mut self, client: McpProxyClient) {
        self.clients.push(client);
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// Fetch tool lists from all upstream servers.
    pub async fn fetch_all_tools(&mut self) -> devboy_core::Result<()> {
        for client in &mut self.clients {
            client.fetch_tools().await?;
        }
        Ok(())
    }

    /// Get all proxied tools (with prefixes) from all upstreams.
    pub fn all_tools(&self) -> Vec<ToolDefinition> {
        self.clients
            .iter()
            .flat_map(|c| c.prefixed_tools())
            .collect()
    }

    /// Check whether a tool name belongs to a proxied upstream.
    pub fn has_tool(&self, tool_name: &str) -> bool {
        self.clients
            .iter()
            .any(|c| tool_name.starts_with(&format!("{}__", c.prefix())))
    }

    /// Try to route a tool call to the matching upstream.
    /// Returns None if no upstream matches the tool name prefix.
    pub async fn try_call(
        &self,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> Option<ToolCallResult> {
        for client in &self.clients {
            let prefix = format!("{}__", client.prefix());
            if let Some(original_name) = tool_name.strip_prefix(&prefix) {
                let result = client.call_tool(original_name, arguments).await;
                return Some(match result {
                    Ok(r) => r,
                    Err(e) => ToolCallResult::error(format!("Proxy error: {}", e)),
                });
            }
        }
        None
    }

    /// Call a specific upstream by prefix using the unprefixed tool name.
    /// Used by the routing engine when it has already decided the remote executor is the
    /// right target for a matched tool (and therefore doesn't need to rely on the
    /// prefixed alias).
    pub async fn call_by_prefix(
        &self,
        prefix: &str,
        unprefixed_tool_name: &str,
        arguments: Option<Value>,
    ) -> Option<ToolCallResult> {
        for client in &self.clients {
            if client.prefix() == prefix {
                let result = client.call_tool(unprefixed_tool_name, arguments).await;
                return Some(match result {
                    Ok(r) => r,
                    Err(e) => ToolCallResult::error(format!("Proxy error: {}", e)),
                });
            }
        }
        None
    }

    /// Return every upstream's raw (unprefixed) tool catalogue tagged by prefix.
    /// Consumers use this to feed the signature matcher.
    pub fn raw_upstream_catalogue(&self) -> Vec<(String, &[ToolDefinition])> {
        self.clients
            .iter()
            .map(|c| (c.prefix().to_string(), c.raw_upstream_tools()))
            .collect()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
#[allow(clippy::err_expect)]
mod tests {
    use super::*;
    use crate::protocol::ToolResultContent;
    use httpmock::prelude::*;

    fn token_secret(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    // =========================================================================
    // ProxyTransport
    // =========================================================================

    #[test]
    fn test_proxy_transport_parse() {
        assert_eq!(
            ProxyTransport::parse("streamable-http"),
            ProxyTransport::StreamableHttp
        );
        assert_eq!(
            ProxyTransport::parse("streamable_http"),
            ProxyTransport::StreamableHttp
        );
        assert_eq!(
            ProxyTransport::parse("http"),
            ProxyTransport::StreamableHttp
        );
        assert_eq!(ProxyTransport::parse("sse"), ProxyTransport::Sse);
        assert_eq!(ProxyTransport::parse(""), ProxyTransport::Sse);
        assert_eq!(ProxyTransport::parse("unknown"), ProxyTransport::Sse);
    }

    #[test]
    fn test_proxy_transport_debug_clone_eq() {
        let t = ProxyTransport::Sse;
        let t2 = t;
        assert_eq!(t, t2);
        assert_eq!(format!("{:?}", t), "Sse");
        assert_eq!(
            format!("{:?}", ProxyTransport::StreamableHttp),
            "StreamableHttp"
        );
    }

    // =========================================================================
    // Helper: mock upstream that implements Streamable HTTP MCP protocol
    // =========================================================================

    /// Create a MockServer that responds to initialize and tools/list.
    fn setup_mock_upstream(server: &MockServer, tools: Vec<serde_json::Value>) {
        // Initialize endpoint — returns session ID in header
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"initialize""#);
            then.status(200)
                .header("mcp-session-id", "test-session-123")
                .json_body(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "mock-server", "version": "1.0.0" }
                    }
                }));
        });

        // tools/list endpoint
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"tools/list""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": { "tools": tools }
            }));
        });
    }

    fn sample_tools() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({
                "name": "get_issues",
                "description": "Get issues from tracker",
                "inputSchema": { "type": "object", "properties": {} }
            }),
            serde_json::json!({
                "name": "get_merge_requests",
                "description": "Get merge requests",
                "inputSchema": { "type": "object", "properties": {} }
            }),
        ]
    }

    // =========================================================================
    // McpProxyClient — Streamable HTTP connect
    // =========================================================================

    #[tokio::test]
    async fn test_connect_streamable_http() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        let url = format!("{}/mcp", server.base_url());
        let token = token_secret("my-token");
        let client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            Some(&token),
            "bearer",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        assert_eq!(client.prefix(), "test-server");
        assert!(client.upstream_tools.is_empty()); // Not fetched yet
    }

    #[tokio::test]
    async fn test_connect_with_custom_prefix() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "test-server",
            &url,
            Some("custom"),
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        assert_eq!(client.prefix(), "custom");
    }

    #[tokio::test]
    async fn test_connect_initialize_failure() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"initialize""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": { "code": -32600, "message": "Bad request" }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let result = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await;

        let err = result.err().expect("should be error");
        assert!(err.to_string().contains("Initialize failed"));
    }

    #[tokio::test]
    async fn test_connect_http_error() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(POST).path("/mcp");
            then.status(500).body("Internal Server Error");
        });

        let url = format!("{}/mcp", server.base_url());
        let result = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await;

        let err = result.err().expect("should be error");
        assert!(err.to_string().contains("500"));
    }

    // =========================================================================
    // McpProxyClient — fetch_tools
    // =========================================================================

    #[tokio::test]
    async fn test_fetch_tools() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        let url = format!("{}/mcp", server.base_url());
        let mut client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        assert!(client.upstream_tools.is_empty());

        client.fetch_tools().await.unwrap();

        assert_eq!(client.upstream_tools.len(), 2);
        assert_eq!(client.upstream_tools[0].name, "get_issues");
        assert_eq!(client.upstream_tools[1].name, "get_merge_requests");
    }

    // =========================================================================
    // McpProxyClient — prefixed_tools
    // =========================================================================

    #[tokio::test]
    async fn test_prefixed_tools() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        let url = format!("{}/mcp", server.base_url());
        let mut client = McpProxyClient::connect(
            "my-server",
            &url,
            Some("cloud"),
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        client.fetch_tools().await.unwrap();

        let prefixed = client.prefixed_tools();
        assert_eq!(prefixed.len(), 2);
        assert_eq!(prefixed[0].name, "cloud__get_issues");
        assert_eq!(prefixed[1].name, "cloud__get_merge_requests");
        assert!(prefixed[0].description.starts_with("[my-server]"));
    }

    #[tokio::test]
    async fn test_prefixed_tools_empty_when_not_fetched() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let prefixed = client.prefixed_tools();
        assert!(prefixed.is_empty());
    }

    // =========================================================================
    // McpProxyClient — call_tool
    // =========================================================================

    #[tokio::test]
    async fn test_call_tool_success() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        // tools/call endpoint
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"tools/call""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "content": [{ "type": "text", "text": "issue data here" }]
                }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let result = client
            .call_tool("get_issues", Some(serde_json::json!({"state": "open"})))
            .await
            .unwrap();

        assert!(result.is_error.is_none());
        assert_eq!(result.content.len(), 1);
        match &result.content[0] {
            ToolResultContent::Text { text } => assert_eq!(text, "issue data here"),
        }
    }

    #[tokio::test]
    async fn test_call_tool_with_upstream_error() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"tools/call""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "error": { "code": -32000, "message": "Tool execution failed" }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let result = client.call_tool("get_issues", None).await.unwrap();

        assert_eq!(result.is_error, Some(true));
        match &result.content[0] {
            ToolResultContent::Text { text } => assert!(text.contains("Tool execution failed")),
        }
    }

    #[tokio::test]
    async fn test_call_tool_empty_response() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"tools/call""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let result = client.call_tool("get_issues", None).await.unwrap();

        assert_eq!(result.is_error, Some(true));
        match &result.content[0] {
            ToolResultContent::Text { text } => assert!(text.contains("Empty response")),
        }
    }

    // =========================================================================
    // McpProxyClient — session ID management
    // =========================================================================

    #[tokio::test]
    async fn test_session_id_sent_on_subsequent_requests() {
        let server = MockServer::start();

        // Initialize — returns session ID
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"initialize""#);
            then.status(200)
                .header("mcp-session-id", "sess-abc")
                .json_body(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": { "name": "mock", "version": "1.0" }
                    }
                }));
        });

        // tools/call — expect session ID header
        let tool_call_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .header("mcp-session-id", "sess-abc")
                .body_includes(r#""method":"tools/call""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "content": [{ "type": "text", "text": "ok" }]
                }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        client.call_tool("test_tool", None).await.unwrap();

        // Verify the session header was actually sent
        tool_call_mock.assert();
    }

    // =========================================================================
    // McpProxyClient — auth types
    // =========================================================================

    #[tokio::test]
    async fn test_bearer_auth_header() {
        let server = MockServer::start();

        let init_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .header("Authorization", "Bearer secret-token")
                .body_includes(r#""method":"initialize""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "serverInfo": { "name": "mock", "version": "1.0" }
                }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let token = token_secret("secret-token");
        McpProxyClient::connect(
            "test-server",
            &url,
            None,
            Some(&token),
            "bearer",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        init_mock.assert();
    }

    #[tokio::test]
    async fn test_api_key_auth_header() {
        let server = MockServer::start();

        let init_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .header("X-API-Key", "my-api-key")
                .body_includes(r#""method":"initialize""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "serverInfo": { "name": "mock", "version": "1.0" }
                }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let token = token_secret("my-api-key");
        McpProxyClient::connect(
            "test-server",
            &url,
            None,
            Some(&token),
            "api_key",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        init_mock.assert();
    }

    // =========================================================================
    // ProxyManager
    // =========================================================================

    #[test]
    fn test_proxy_manager_new_is_empty() {
        let mgr = ProxyManager::new();
        assert!(mgr.is_empty());
        assert!(mgr.all_tools().is_empty());
    }

    #[tokio::test]
    async fn test_proxy_manager_all_tools() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        let url = format!("{}/mcp", server.base_url());
        let mut client = McpProxyClient::connect(
            "upstream",
            &url,
            Some("up"),
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        client.fetch_tools().await.unwrap();

        let mut mgr = ProxyManager::new();
        mgr.add_client(client);

        assert!(!mgr.is_empty());

        let tools = mgr.all_tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "up__get_issues");
        assert_eq!(tools[1].name, "up__get_merge_requests");
    }

    #[tokio::test]
    async fn test_proxy_manager_try_call_routes_correctly() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"tools/call""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "content": [{ "type": "text", "text": "routed ok" }]
                }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "upstream",
            &url,
            Some("up"),
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let mut mgr = ProxyManager::new();
        mgr.add_client(client);

        let result = mgr
            .try_call("up__get_issues", Some(serde_json::json!({})))
            .await;

        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.is_error.is_none());
        match &result.content[0] {
            ToolResultContent::Text { text } => assert_eq!(text, "routed ok"),
        }
    }

    #[tokio::test]
    async fn test_proxy_manager_try_call_no_match() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "upstream",
            &url,
            Some("up"),
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let mut mgr = ProxyManager::new();
        mgr.add_client(client);

        let result = mgr
            .try_call("unknown__get_issues", Some(serde_json::json!({})))
            .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_proxy_manager_try_call_without_prefix_no_match() {
        let mgr = ProxyManager::new();
        let result = mgr.try_call("get_issues", None).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_proxy_manager_fetch_all_tools() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "upstream",
            &url,
            Some("up"),
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let mut mgr = ProxyManager::new();
        mgr.add_client(client);

        assert!(mgr.all_tools().is_empty());

        mgr.fetch_all_tools().await.unwrap();

        assert_eq!(mgr.all_tools().len(), 2);
    }

    // =========================================================================
    // McpProxyClient — invalid token (non-ASCII)
    // =========================================================================

    #[tokio::test]
    async fn test_connect_invalid_bearer_token() {
        let token = token_secret("token-with-\x01-control-chars");
        let result = McpProxyClient::connect(
            "test-server",
            "http://localhost:1/mcp",
            None,
            Some(&token),
            "bearer",
            ProxyTransport::StreamableHttp,
        )
        .await;

        let err = result.err().expect("should be error");
        assert!(err.to_string().contains("Invalid token"));
    }

    #[tokio::test]
    async fn test_connect_invalid_api_key_token() {
        let token = token_secret("key-with-\x01-control");
        let result = McpProxyClient::connect(
            "test-server",
            "http://localhost:1/mcp",
            None,
            Some(&token),
            "api_key",
            ProxyTransport::StreamableHttp,
        )
        .await;

        let err = result.err().expect("should be error");
        assert!(err.to_string().contains("Invalid token"));
    }

    // =========================================================================
    // McpProxyClient — SSE transport via mock
    // =========================================================================

    /// Helper: set up SSE mock endpoint that returns endpoint event + initialize response.
    fn setup_sse_mock(server: &MockServer) {
        // SSE stream: returns endpoint event, then initialize response
        server.mock(|when, then| {
            when.method(GET).path("/sse");
            then.status(200)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .body(
                    "event: endpoint\ndata: /messages\n\n\
                     event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"serverInfo\":{\"name\":\"mock-sse\",\"version\":\"1.0\"}}}\n\n"
                );
        });

        // POST endpoint for requests (initialize is sent here)
        server.mock(|when, then| {
            when.method(POST).path("/messages");
            then.status(200);
        });
    }

    #[tokio::test]
    async fn test_connect_sse_transport() {
        let server = MockServer::start();
        setup_sse_mock(&server);

        let url = format!("{}/sse", server.base_url());
        let result = McpProxyClient::connect(
            "sse-server",
            &url,
            Some("sse"),
            None,
            "none",
            ProxyTransport::Sse,
        )
        .await;

        assert!(result.is_ok(), "SSE connect failed: {:?}", result.err());
        let client = result.unwrap();
        assert_eq!(client.prefix(), "sse");
        assert_eq!(client.transport, ProxyTransport::Sse);
    }

    #[tokio::test]
    async fn test_connect_sse_with_bearer_auth() {
        let server = MockServer::start();

        // SSE stream with auth check
        server.mock(|when, then| {
            when.method(GET)
                .path("/sse")
                .header("Authorization", "Bearer sse-token");
            then.status(200)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .body(
                    "event: endpoint\ndata: /messages\n\n\
                     event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"serverInfo\":{\"name\":\"mock\",\"version\":\"1.0\"}}}\n\n"
                );
        });

        server.mock(|when, then| {
            when.method(POST).path("/messages");
            then.status(200);
        });

        let url = format!("{}/sse", server.base_url());
        let token = token_secret("sse-token");
        let result = McpProxyClient::connect(
            "sse-server",
            &url,
            None,
            Some(&token),
            "bearer",
            ProxyTransport::Sse,
        )
        .await;

        assert!(
            result.is_ok(),
            "SSE connect with auth failed: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn test_sse_request_dispatch_path() {
        // Verify that an SSE-transport client dispatches via request_sse.
        // We test that the request method correctly routes to SSE path
        // by checking the client transport type after connect.
        let server = MockServer::start();
        setup_sse_mock(&server);

        let url = format!("{}/sse", server.base_url());
        let client = McpProxyClient::connect(
            "sse-server",
            &url,
            Some("sse"),
            None,
            "none",
            ProxyTransport::Sse,
        )
        .await
        .unwrap();

        // Verify the client is configured for SSE transport
        assert_eq!(client.transport, ProxyTransport::Sse);
        // The post_url should be resolved from the endpoint event
        assert!(client.post_url.contains("/messages"));
    }

    // =========================================================================
    // McpProxyClient — fetch_tools with error response
    // =========================================================================

    #[tokio::test]
    async fn test_fetch_tools_with_error_response() {
        let server = MockServer::start();

        // Initialize endpoint
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"initialize""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "serverInfo": { "name": "mock", "version": "1.0" }
                }
            }));
        });

        // tools/list returns error
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"tools/list""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "error": { "code": -32601, "message": "Method not found" }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let mut client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        // fetch_tools should succeed but not populate tools (error response has no result)
        client.fetch_tools().await.unwrap();
        assert!(client.upstream_tools.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_tools_with_empty_result() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"initialize""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "serverInfo": { "name": "mock", "version": "1.0" }
                }
            }));
        });

        // tools/list returns result without tools field
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"tools/list""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": { "something_else": true }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let mut client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        // fetch_tools should succeed, but tools remain empty (deserialization fails silently)
        client.fetch_tools().await.unwrap();
        assert!(client.upstream_tools.is_empty());
    }

    // =========================================================================
    // McpProxyClient — call_tool with None arguments
    // =========================================================================

    #[tokio::test]
    async fn test_call_tool_with_none_arguments_uses_empty_object() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        // Verify that arguments defaults to {}
        let tool_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""arguments":{}"#)
                .body_includes(r#""method":"tools/call""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "content": [{ "type": "text", "text": "no args ok" }]
                }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let result = client.call_tool("get_issues", None).await.unwrap();
        assert!(result.is_error.is_none());
        tool_mock.assert();
    }

    // =========================================================================
    // ProxyManager — try_call transport error
    // =========================================================================

    #[tokio::test]
    async fn test_proxy_manager_try_call_transport_error() {
        let server = MockServer::start();
        setup_mock_upstream(&server, sample_tools());

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "upstream",
            &url,
            Some("up"),
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let mut mgr = ProxyManager::new();
        mgr.add_client(client);

        // Drop the mock server so the next call fails with a transport error
        drop(server);

        let result = mgr
            .try_call("up__get_issues", Some(serde_json::json!({})))
            .await;

        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.is_error, Some(true));
        match &result.content[0] {
            ToolResultContent::Text { text } => assert!(text.contains("Proxy error")),
        }
    }

    // =========================================================================
    // ProxyManager — default trait
    // =========================================================================

    #[test]
    fn test_proxy_manager_default() {
        let mgr = ProxyManager::default();
        assert!(mgr.is_empty());
        assert!(mgr.all_tools().is_empty());
    }

    // =========================================================================
    // ProxyManager — multiple clients routing
    // =========================================================================

    #[tokio::test]
    async fn test_proxy_manager_multiple_clients() {
        let server1 = MockServer::start();
        let server2 = MockServer::start();

        setup_mock_upstream(
            &server1,
            vec![serde_json::json!({
                "name": "tool_a",
                "description": "Tool A",
                "inputSchema": { "type": "object" }
            })],
        );
        setup_mock_upstream(
            &server2,
            vec![serde_json::json!({
                "name": "tool_b",
                "description": "Tool B",
                "inputSchema": { "type": "object" }
            })],
        );

        let url1 = format!("{}/mcp", server1.base_url());
        let url2 = format!("{}/mcp", server2.base_url());

        let client1 = McpProxyClient::connect(
            "server1",
            &url1,
            Some("s1"),
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let client2 = McpProxyClient::connect(
            "server2",
            &url2,
            Some("s2"),
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let mut mgr = ProxyManager::new();
        mgr.add_client(client1);
        mgr.add_client(client2);

        mgr.fetch_all_tools().await.unwrap();

        let tools = mgr.all_tools();
        assert_eq!(tools.len(), 2);
        assert!(tools.iter().any(|t| t.name == "s1__tool_a"));
        assert!(tools.iter().any(|t| t.name == "s2__tool_b"));
    }

    // =========================================================================
    // Response ID validation
    // =========================================================================

    #[tokio::test]
    async fn test_mismatched_response_id_returns_error() {
        let server = MockServer::start();

        // Initialize returns correct id
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"initialize""#);
            then.status(200)
                .header("mcp-session-id", "sess-1")
                .json_body(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": { "name": "mock", "version": "1.0" }
                    }
                }));
        });

        // tools/call returns mismatched id
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"tools/call""#);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 999,
                "result": {
                    "content": [{ "type": "text", "text": "wrong id" }]
                }
            }));
        });

        let url = format!("{}/mcp", server.base_url());
        let client = McpProxyClient::connect(
            "test-server",
            &url,
            None,
            None,
            "none",
            ProxyTransport::StreamableHttp,
        )
        .await
        .unwrap();

        let result = client.call_tool("some_tool", None).await;
        let err = result.expect_err("should be error");
        assert!(err.to_string().contains("Mismatched JSON-RPC id"));
    }
}
