//! MCP server implementation.
//!
//! The server handles the MCP protocol lifecycle:
//! 1. Initialize - exchange capabilities
//! 2. Handle tool calls - execute tools via providers
//! 3. Shutdown - graceful cleanup

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use devboy_core::{BuiltinToolsConfig, Provider};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::protocol::{
    InitializeParams, InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse, MCP_VERSION,
    RequestId, ServerCapabilities, ServerInfo, ToolCallParams, ToolCallResult, ToolsCapability,
    ToolsListResult,
};
use crate::proxy::ProxyManager;
use crate::transport::{IncomingMessage, StdioTransport};

/// Result of deferred background initialization (remote config + proxy).
pub struct DeferredInit {
    /// Proxy manager with connected upstream servers and fetched tools.
    pub proxy_manager: ProxyManager,
    /// Builtin tools config from remote config (overrides local if non-empty).
    pub builtin_tools_config: Option<BuiltinToolsConfig>,
}

/// MCP server for devboy-tools.
pub struct McpServer {
    contexts: HashMap<String, Vec<Arc<dyn Provider>>>,
    messenger_contexts: HashMap<String, Vec<Arc<dyn devboy_core::MessengerProvider>>>,
    active_context: RwLock<String>,
    initialized: bool,
    proxy_manager: ProxyManager,
    builtin_tools_config: BuiltinToolsConfig,
    meeting_providers: Vec<Arc<dyn devboy_core::MeetingNotesProvider>>,
    /// Deferred background initialization — resolved on first `tools/list` or `tools/call`.
    /// Returns proxy manager and optional builtin_tools override from remote config.
    deferred_init: Option<oneshot::Receiver<DeferredInit>>,
}

impl McpServer {
    /// Create a new MCP server.
    pub fn new() -> Self {
        let mut contexts = HashMap::new();
        contexts.insert("default".to_string(), Vec::new());
        let mut messenger_contexts = HashMap::new();
        messenger_contexts.insert("default".to_string(), Vec::new());
        Self {
            contexts,
            messenger_contexts,
            active_context: RwLock::new("default".to_string()),
            initialized: false,
            proxy_manager: ProxyManager::new(),
            builtin_tools_config: BuiltinToolsConfig::default(),
            meeting_providers: Vec::new(),
            deferred_init: None,
        }
    }

    /// Set the built-in tools filtering configuration.
    ///
    /// Returns an error if both `disabled` and `enabled` are set (mutually exclusive).
    pub fn set_builtin_tools_config(
        &mut self,
        config: BuiltinToolsConfig,
    ) -> devboy_core::Result<()> {
        config.validate()?;
        self.builtin_tools_config = config;
        Ok(())
    }

    /// Set the proxy manager for upstream MCP server connections.
    pub fn set_proxy_manager(&mut self, proxy_manager: ProxyManager) {
        self.proxy_manager = proxy_manager;
    }

    /// Set deferred initialization that will be resolved on first `tools/list` or `tools/call`.
    ///
    /// This allows the MCP server to start reading stdin immediately while remote
    /// config fetch, proxy connections, and tool loading run in the background.
    pub fn set_deferred_init(&mut self, receiver: oneshot::Receiver<DeferredInit>) {
        self.deferred_init = Some(receiver);
    }

    /// Resolve deferred init if pending — applies proxy manager and remote builtin_tools config.
    async fn resolve_deferred_init(&mut self) {
        if let Some(receiver) = self.deferred_init.take() {
            match receiver.await {
                Ok(init) => {
                    if !init.proxy_manager.is_empty() {
                        self.proxy_manager = init.proxy_manager;
                    }
                    if let Some(bt_config) = init.builtin_tools_config
                        && !bt_config.is_empty()
                    {
                        if let Err(e) = bt_config.validate() {
                            tracing::warn!("Remote builtin_tools config is invalid, ignoring: {e}");
                        } else {
                            self.builtin_tools_config = bt_config;
                        }
                    }
                }
                Err(_) => {
                    tracing::warn!("Deferred initialization was cancelled");
                }
            }
        }
    }

    pub fn add_meeting_provider(&mut self, provider: Arc<dyn devboy_core::MeetingNotesProvider>) {
        self.meeting_providers.push(provider);
    }

    pub fn add_messenger_provider(&mut self, provider: Arc<dyn devboy_core::MessengerProvider>) {
        self.add_messenger_provider_to_context("default", provider);
    }

    pub fn add_messenger_provider_to_context(
        &mut self,
        context: &str,
        provider: Arc<dyn devboy_core::MessengerProvider>,
    ) {
        self.contexts.entry(context.to_string()).or_default();
        self.messenger_contexts
            .entry(context.to_string())
            .or_default()
            .push(provider);
    }

    /// Add a provider to the server.
    pub fn add_provider(&mut self, provider: Arc<dyn Provider>) {
        self.contexts
            .entry("default".to_string())
            .or_default()
            .push(provider);
    }

    /// Add a provider under a named context.
    pub fn add_provider_to_context(&mut self, context: &str, provider: Arc<dyn Provider>) {
        self.contexts
            .entry(context.to_string())
            .or_default()
            .push(provider);
    }

    /// Ensure a named context exists, even if it has no providers.
    pub fn ensure_context(&mut self, context: &str) {
        self.contexts.entry(context.to_string()).or_default();
        self.messenger_contexts
            .entry(context.to_string())
            .or_default();
    }

    /// Set active context.
    pub fn set_active_context(&self, context: &str) -> devboy_core::Result<()> {
        if !self.contexts.contains_key(context) {
            return Err(devboy_core::Error::Config(format!(
                "Context '{}' not found",
                context
            )));
        }

        let mut active = self
            .active_context
            .write()
            .map_err(|_| devboy_core::Error::Config("Active context lock poisoned".to_string()))?;
        *active = context.to_string();
        Ok(())
    }

    /// Get active context name.
    pub fn active_context_name(&self) -> String {
        self.active_context
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| "default".to_string())
    }

    /// List all context names.
    pub fn context_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.contexts.keys().cloned().collect();
        names.sort();
        names
    }

    /// Get providers in active context.
    pub fn active_providers(&self) -> Vec<Arc<dyn Provider>> {
        let active = self.active_context_name();
        self.contexts.get(&active).cloned().unwrap_or_default()
    }

    /// Get messenger providers in active context.
    pub fn active_messenger_providers(&self) -> Vec<Arc<dyn devboy_core::MessengerProvider>> {
        let active = self.active_context_name();
        self.messenger_contexts
            .get(&active)
            .cloned()
            .unwrap_or_default()
    }

    /// Get providers in the default context.
    pub fn providers(&self) -> &[Arc<dyn Provider>] {
        self.contexts
            .get("default")
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Run the MCP server main loop.
    pub async fn run(&mut self) -> devboy_core::Result<()> {
        tracing::info!(
            "Starting MCP server with {} contexts (active: {})",
            self.contexts.len(),
            self.active_context_name()
        );

        let mut transport = StdioTransport::stdio();

        loop {
            match transport.read_message() {
                Ok(Some(msg)) => {
                    let response = self.handle_message(msg).await;
                    if let Some(resp) = response
                        && let Err(e) = transport.write_response(&resp)
                    {
                        tracing::error!("Failed to write response: {}", e);
                        break;
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
    async fn handle_message(&mut self, msg: IncomingMessage) -> Option<JsonRpcResponse> {
        match msg {
            IncomingMessage::Request(req) => Some(self.handle_request(req).await),
            IncomingMessage::Notification(notif) => {
                self.handle_notification(&notif.method);
                None // Notifications don't get responses
            }
        }
    }

    /// Handle a JSON-RPC request.
    pub async fn handle_request(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        tracing::debug!("Handling request: {} (id: {:?})", req.method, req.id);

        match req.method.as_str() {
            "initialize" => self.handle_initialize(req.id, req.params),
            "tools/list" => {
                self.resolve_deferred_init().await;
                self.handle_tools_list(req.id)
            }
            "tools/call" => {
                self.resolve_deferred_init().await;
                self.handle_tools_call(req.id, req.params).await
            }
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
    ///
    /// Returns the list of available tools filtered by configured providers.
    /// This method is public to allow integration testing.
    pub fn handle_tools_list(&self, id: RequestId) -> JsonRpcResponse {
        let providers = self.active_providers();

        // Build tool list from executor's base definitions (source of truth)
        let base_tools = devboy_executor::tools::base_tool_definitions();
        let mut tools: Vec<crate::protocol::ToolDefinition> = base_tools
            .into_iter()
            .map(|t| {
                let mut schema = serde_json::to_value(&t.input_schema).unwrap_or_default();
                // Ensure "type": "object" is present — required by MCP spec.
                if let Some(obj) = schema.as_object_mut() {
                    obj.entry("type").or_insert_with(|| "object".into());
                }
                crate::protocol::ToolDefinition {
                    name: t.name,
                    description: t.description,
                    input_schema: schema,
                    category: Some(t.category),
                }
            })
            .collect();

        // Pre-compute category availability to avoid repeated provider lookups.
        use devboy_core::IssueProvider;
        let has_issue_providers = !providers.is_empty();
        let has_mr_providers = providers.iter().any(|p| {
            matches!(
                IssueProvider::provider_name(p.as_ref()),
                "github" | "gitlab"
            )
        });
        let has_meeting_providers = !self.meeting_providers.is_empty();
        let has_messenger_providers = !self.active_messenger_providers().is_empty();

        // Pre-compute per-tool asset capability flags.
        // If no provider supports upload/delete, hide those tools entirely.
        let any_upload = providers
            .iter()
            .any(|p| p.asset_capabilities().issue.upload);
        let any_delete = providers
            .iter()
            .any(|p| p.asset_capabilities().issue.delete);

        // Filter tools based on available providers (dynamic filtering).
        // This prevents exposing tools that would always fail due to missing providers.
        tools.retain(|t| {
            // Per-tool capability checks (asset tools).
            match t.name.as_str() {
                "upload_asset" => return any_upload,
                "delete_asset" => return any_delete,
                _ => {}
            }
            t.category
                .map(|cat| match cat {
                    devboy_core::ToolCategory::IssueTracker => has_issue_providers,
                    devboy_core::ToolCategory::Epics => has_issue_providers,
                    devboy_core::ToolCategory::GitRepository => has_mr_providers,
                    devboy_core::ToolCategory::MeetingNotes => has_meeting_providers,
                    devboy_core::ToolCategory::Messenger => has_messenger_providers,
                    devboy_core::ToolCategory::Releases => has_mr_providers,
                    devboy_core::ToolCategory::JiraStructure => has_issue_providers,
                })
                .unwrap_or(true) // Tools without category are always available
        });

        // Context management tools are always available
        tools.push(crate::protocol::ToolDefinition {
            name: "list_contexts".to_string(),
            description: "List configured contexts and indicate the active context.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
            category: None,
        });
        tools.push(crate::protocol::ToolDefinition {
            name: "use_context".to_string(),
            description: "Switch active context at runtime.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string", "description": "Context name to activate" }
                }
            }),
            category: None,
        });
        tools.push(crate::protocol::ToolDefinition {
            name: "get_current_context".to_string(),
            description: "Get current active context name.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
            category: None,
        });

        // Filter built-in tools based on config (static filtering)
        if !self.builtin_tools_config.is_empty() {
            tools.retain(|t| self.builtin_tools_config.is_tool_allowed(&t.name));
        }

        // Append proxied tools from upstream MCP servers (not affected by builtin_tools filter)
        tools.extend(self.proxy_manager.all_tools());

        let result = ToolsListResult { tools };
        JsonRpcResponse::success(id, serde_json::to_value(result).unwrap())
    }

    /// Handle tools/call request.
    async fn handle_tools_call(&mut self, id: RequestId, params: Option<Value>) -> JsonRpcResponse {
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

        // Block disabled built-in tools (proxy tools are not affected)
        if !self.builtin_tools_config.is_empty()
            && !self.builtin_tools_config.is_tool_allowed(&params.name)
            && !self.proxy_manager.has_tool(&params.name)
        {
            return JsonRpcResponse::error(
                id,
                JsonRpcError::method_not_found(&format!(
                    "Tool '{}' is disabled by builtin_tools configuration",
                    params.name
                )),
            );
        }

        let result = match params.name.as_str() {
            "list_contexts" => {
                let active = self.active_context_name();
                let names = self.context_names();
                let content = names
                    .into_iter()
                    .map(|name| {
                        if name == active {
                            format!("* {} (active)", name)
                        } else {
                            format!("* {}", name)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                crate::protocol::ToolCallResult::text(content)
            }
            "get_current_context" => {
                crate::protocol::ToolCallResult::text(self.active_context_name())
            }
            "use_context" => {
                #[derive(Deserialize)]
                struct UseContextParams {
                    name: String,
                }

                match params.arguments {
                    Some(args) => match serde_json::from_value::<UseContextParams>(args) {
                        Ok(args) => match self.set_active_context(&args.name) {
                            Ok(()) => crate::protocol::ToolCallResult::text(format!(
                                "Active context set to '{}'",
                                args.name
                            )),
                            Err(e) => crate::protocol::ToolCallResult::error(e.to_string()),
                        },
                        Err(e) => crate::protocol::ToolCallResult::error(format!(
                            "Invalid parameters: {}",
                            e
                        )),
                    },
                    None => crate::protocol::ToolCallResult::error(
                        "Missing required parameter: name".to_string(),
                    ),
                }
            }
            _ => {
                // Try proxied upstream tools first
                if let Some(proxy_result) = self
                    .proxy_manager
                    .try_call(&params.name, params.arguments.clone())
                    .await
                {
                    proxy_result
                } else {
                    self.dispatch_builtin_tool(&params.name, params.arguments)
                        .await
                }
            }
        };
        JsonRpcResponse::success(id, serde_json::to_value(result).unwrap())
    }

    /// Dispatch a built-in tool call through the Executor.
    ///
    /// Routes tool calls to the appropriate provider type based on tool category:
    /// - MeetingNotes -> meeting providers
    /// - Messenger -> messenger providers
    /// - Everything else -> standard providers (issues, MRs, pipelines, assets, epics)
    async fn dispatch_builtin_tool(&self, name: &str, arguments: Option<Value>) -> ToolCallResult {
        let executor = self.create_executor();
        let args = arguments.unwrap_or(Value::Null);
        let category = devboy_executor::Executor::tool_category(name);

        match category {
            Some(devboy_core::ToolCategory::MeetingNotes) => {
                for provider in &self.meeting_providers {
                    match executor
                        .execute_direct_meeting(name, args.clone(), provider.as_ref())
                        .await
                    {
                        Ok(output) => return output_to_result(output),
                        Err(e) => {
                            tracing::debug!("Meeting provider failed: {}", e);
                            continue;
                        }
                    }
                }
                ToolCallResult::error(format!("No meeting provider supports '{}'", name))
            }
            Some(devboy_core::ToolCategory::Messenger) => {
                for provider in &self.active_messenger_providers() {
                    match executor
                        .execute_direct_messenger(name, args.clone(), provider.as_ref())
                        .await
                    {
                        Ok(output) => return output_to_result(output),
                        Err(e) => {
                            tracing::debug!("Messenger provider failed: {}", e);
                            continue;
                        }
                    }
                }
                ToolCallResult::error(format!("No messenger provider supports '{}'", name))
            }
            _ => {
                // Issues, MRs, Pipelines, Assets, Epics, etc.
                let providers = self.active_providers();
                if providers.is_empty() {
                    return ToolCallResult::error("No providers configured".to_string());
                }
                for provider in &providers {
                    match executor
                        .execute_direct(name, args.clone(), provider.as_ref())
                        .await
                    {
                        Ok(output) => return output_to_result(output),
                        Err(e) if should_try_next_provider(&e) => continue,
                        Err(e) => return ToolCallResult::error(format!("{e}")),
                    }
                }
                ToolCallResult::error(format!("No provider supports '{}'", name))
            }
        }
    }

    /// Create an Executor instance with best-effort asset cache.
    fn create_executor(&self) -> devboy_executor::Executor {
        let mut executor = devboy_executor::Executor::new();
        // Best-effort asset cache
        if let Ok(mgr) =
            devboy_assets::AssetManager::from_config(devboy_assets::AssetConfig::default())
        {
            executor = executor.with_asset_manager(mgr);
        }
        executor
    }

    /// Handle ping request.
    fn handle_ping(&self, id: RequestId) -> JsonRpcResponse {
        JsonRpcResponse::success(id, serde_json::json!({}))
    }
}

/// Convert an executor ToolOutput to an MCP ToolCallResult.
fn output_to_result(output: devboy_executor::ToolOutput) -> ToolCallResult {
    match devboy_executor::format_output(output, None, None, None) {
        Ok(formatted) => ToolCallResult::text(formatted.content),
        Err(e) => ToolCallResult::error(format!("Format error: {e}")),
    }
}

/// Check whether an error from one provider should cause the handler to
/// try the next. In multi-provider setups, a key like `gitlab#1` is
/// invalid for GitHub but valid for GitLab.
fn should_try_next_provider(e: &devboy_core::Error) -> bool {
    matches!(
        e,
        devboy_core::Error::ProviderUnsupported { .. }
            | devboy_core::Error::ProviderNotFound(_)
            | devboy_core::Error::NotFound(_)
            | devboy_core::Error::InvalidData(_)
            | devboy_core::Error::Http(_)
    )
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{JSONRPC_VERSION, RequestId, ToolCallResult, ToolResultContent};

    use async_trait::async_trait;
    use devboy_core::types::ChatType;
    use devboy_core::{
        Comment, CreateCommentInput, CreateIssueInput, Discussion, FileDiff, GetChatsParams,
        GetMessagesParams, Issue, IssueFilter, IssueProvider, MergeRequest, MergeRequestProvider,
        MessageAuthor, MessengerChat, MessengerMessage, MessengerProvider, MrFilter,
        SearchMessagesParams, SendMessageParams, UpdateIssueInput, User,
    };

    /// Test provider that simulates a GitHub-like provider (supports both issues and MRs).
    struct TestProvider;

    #[async_trait]
    impl IssueProvider for TestProvider {
        async fn get_issues(
            &self,
            _filter: IssueFilter,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Issue>> {
            Ok(vec![].into())
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
        async fn get_comments(
            &self,
            _issue_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Comment>> {
            Ok(vec![].into())
        }
        async fn add_comment(&self, _issue_key: &str, _body: &str) -> devboy_core::Result<Comment> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        fn provider_name(&self) -> &'static str {
            "github" // Changed from "test" to "github" for MR tools to work
        }
    }

    #[async_trait]
    impl MergeRequestProvider for TestProvider {
        async fn get_merge_requests(
            &self,
            _filter: MrFilter,
        ) -> devboy_core::Result<devboy_core::ProviderResult<MergeRequest>> {
            Ok(vec![].into())
        }
        async fn get_merge_request(&self, _key: &str) -> devboy_core::Result<MergeRequest> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        async fn get_discussions(
            &self,
            _mr_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Discussion>> {
            Ok(vec![].into())
        }
        async fn get_diffs(
            &self,
            _mr_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<FileDiff>> {
            Ok(vec![].into())
        }
        async fn add_comment(
            &self,
            _mr_key: &str,
            _input: CreateCommentInput,
        ) -> devboy_core::Result<Comment> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        fn provider_name(&self) -> &'static str {
            "github" // Changed from "test" to "github" for MR tools to work
        }
    }

    #[async_trait]
    impl devboy_core::PipelineProvider for TestProvider {
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

    struct TestMessengerProvider;

    #[async_trait]
    impl MessengerProvider for TestMessengerProvider {
        fn provider_name(&self) -> &'static str {
            "slack"
        }

        async fn get_chats(
            &self,
            _params: GetChatsParams,
        ) -> devboy_core::Result<devboy_core::ProviderResult<MessengerChat>> {
            Ok(vec![MessengerChat {
                id: "C123".to_string(),
                key: "slack:C123".to_string(),
                name: "general".to_string(),
                chat_type: ChatType::Channel,
                source: "slack".to_string(),
                member_count: Some(3),
                description: None,
                is_active: true,
            }]
            .into())
        }

        async fn get_messages(
            &self,
            _params: GetMessagesParams,
        ) -> devboy_core::Result<devboy_core::ProviderResult<MessengerMessage>> {
            Ok(vec![].into())
        }

        async fn search_messages(
            &self,
            _params: SearchMessagesParams,
        ) -> devboy_core::Result<devboy_core::ProviderResult<MessengerMessage>> {
            Ok(vec![].into())
        }

        async fn send_message(
            &self,
            _params: SendMessageParams,
        ) -> devboy_core::Result<MessengerMessage> {
            Ok(MessengerMessage {
                id: "1710000000.000100".to_string(),
                chat_id: "C123".to_string(),
                text: "test".to_string(),
                author: MessageAuthor {
                    id: "U123".to_string(),
                    name: "DevBoy".to_string(),
                    username: Some("devboy".to_string()),
                    avatar_url: None,
                },
                source: "slack".to_string(),
                timestamp: "1710000000.000100".to_string(),
                thread_id: None,
                reply_to_id: None,
                attachments: vec![],
                is_edited: false,
            })
        }
    }

    #[test]
    fn test_server_creation() {
        let server = McpServer::new();
        assert!(server.providers().is_empty());
        assert!(!server.initialized);
    }

    #[test]
    fn test_initialize_response() {
        let mut server = McpServer::new();

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "test-client",
                    "version": "1.0.0"
                }
            })),
        };

        let resp = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(server.handle_request(req));

        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
        assert!(server.initialized);
    }

    #[test]
    fn test_tools_list_without_providers() {
        // Without providers, only context management tools should be available
        let server = McpServer::new();

        let resp = server.handle_tools_list(RequestId::Number(1));

        assert!(resp.result.is_some());
        let result: ToolsListResult = serde_json::from_value(resp.result.unwrap()).unwrap();

        // Context tools are always available
        assert!(result.tools.iter().any(|t| t.name == "list_contexts"));
        assert!(result.tools.iter().any(|t| t.name == "use_context"));
        assert!(result.tools.iter().any(|t| t.name == "get_current_context"));

        // Issue and MR tools should NOT be available without providers
        assert!(!result.tools.iter().any(|t| t.name == "get_issues"));
        assert!(!result.tools.iter().any(|t| t.name == "get_merge_requests"));
    }

    #[test]
    fn test_tools_list_with_provider() {
        let mut server = McpServer::new();
        server.add_provider(Arc::new(TestProvider));

        let resp = server.handle_tools_list(RequestId::Number(1));

        assert!(resp.result.is_some());
        let result: ToolsListResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(!result.tools.is_empty());

        // With a provider, all tools should be available
        assert!(result.tools.iter().any(|t| t.name == "get_issues"));
        assert!(result.tools.iter().any(|t| t.name == "get_merge_requests"));
        assert!(result.tools.iter().any(|t| t.name == "list_contexts"));
        assert!(result.tools.iter().any(|t| t.name == "use_context"));
        assert!(result.tools.iter().any(|t| t.name == "get_current_context"));
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

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "unknown/method".to_string(),
            params: None,
        };

        let resp = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(server.handle_request(req));

        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, JsonRpcError::METHOD_NOT_FOUND);
    }

    #[test]
    fn test_add_provider_and_providers() {
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

        let msg = IncomingMessage::Notification(crate::protocol::JsonRpcNotification {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "initialized".to_string(),
            params: None,
        });

        let response = server.handle_message(msg).await;
        // Notifications should return None
        assert!(response.is_none());
    }

    #[tokio::test]
    async fn test_handle_message_request() {
        let mut server = McpServer::new();

        let msg = IncomingMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "ping".to_string(),
            params: None,
        });

        let response = server.handle_message(msg).await;
        // Requests should return Some
        assert!(response.is_some());
        let resp = response.unwrap();
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn test_handle_tools_call() {
        let mut server = McpServer::new();

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "get_issues",
                "arguments": {}
            })),
        };

        let resp = server.handle_request(req).await;
        // Will return error since no providers, but should not panic
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn test_handle_tools_call_missing_params() {
        let mut server = McpServer::new();

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: None,
        };

        let resp = server.handle_request(req).await;
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn test_handle_tools_call_invalid_params() {
        let mut server = McpServer::new();

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!("not an object")),
        };

        let resp = server.handle_request(req).await;
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

    #[test]
    fn test_context_switch_missing_context() {
        let server = McpServer::new();
        let err = server.set_active_context("missing").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_context_names_and_active_context_switch() {
        let server = McpServer::new();
        assert_eq!(server.active_context_name(), "default".to_string());
        assert_eq!(server.context_names(), vec!["default".to_string()]);

        let mut server = server;
        server.ensure_context("workspace");

        assert_eq!(
            server.context_names(),
            vec!["default".to_string(), "workspace".to_string()]
        );

        server.set_active_context("workspace").unwrap();
        assert_eq!(server.active_context_name(), "workspace".to_string());
    }

    #[tokio::test]
    async fn test_tools_call_get_current_context() {
        let mut server = McpServer::new();
        server.contexts.insert("workspace".to_string(), vec![]);
        server.set_active_context("workspace").unwrap();

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "get_current_context",
                "arguments": {}
            })),
        };

        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        let result: ToolCallResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        let text = match &result.content[0] {
            ToolResultContent::Text { text } => text,
        };
        assert_eq!(text, "workspace");
        assert_eq!(result.is_error, None);
    }

    #[tokio::test]
    async fn test_tools_call_list_contexts_marks_active() {
        let mut server = McpServer::new();
        server.contexts.insert("workspace".to_string(), vec![]);
        server.set_active_context("workspace").unwrap();

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(2),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "list_contexts",
                "arguments": {}
            })),
        };

        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        let result: ToolCallResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        let text = match &result.content[0] {
            ToolResultContent::Text { text } => text,
        };
        assert!(text.contains("* default"));
        assert!(text.contains("* workspace (active)"));
    }

    #[tokio::test]
    async fn test_tools_call_use_context_success_and_error_paths() {
        let mut server = McpServer::new();
        server.contexts.insert("workspace".to_string(), vec![]);

        let missing_name_req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(3),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "use_context",
                "arguments": {}
            })),
        };
        let missing_name_resp = server.handle_request(missing_name_req).await;
        let missing_name_result: ToolCallResult =
            serde_json::from_value(missing_name_resp.result.unwrap()).unwrap();
        assert_eq!(missing_name_result.is_error, Some(true));

        let missing_context_req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(4),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "use_context",
                "arguments": { "name": "missing" }
            })),
        };
        let missing_context_resp = server.handle_request(missing_context_req).await;
        let missing_context_result: ToolCallResult =
            serde_json::from_value(missing_context_resp.result.unwrap()).unwrap();
        assert_eq!(missing_context_result.is_error, Some(true));

        let success_req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(5),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "use_context",
                "arguments": { "name": "workspace" }
            })),
        };
        let success_resp = server.handle_request(success_req).await;
        let success_result: ToolCallResult =
            serde_json::from_value(success_resp.result.unwrap()).unwrap();
        assert_eq!(success_result.is_error, None);
        assert_eq!(server.active_context_name(), "workspace".to_string());
    }

    #[test]
    fn test_set_proxy_manager() {
        let mut server = McpServer::new();
        let proxy_manager = ProxyManager::new();
        server.set_proxy_manager(proxy_manager);
        // No panic, proxy_manager is set
    }

    #[test]
    fn test_tools_list_includes_proxy_tools() {
        let mut server = McpServer::new();
        // Add a provider so tools are available
        server.add_provider(Arc::new(TestProvider));

        // Create a ProxyManager and manually simulate fetched tools
        // by checking that the server returns proxy tools in tools/list.
        // Since ProxyManager.all_tools() returns empty when no clients are added,
        // we verify the baseline behavior.
        let proxy_manager = ProxyManager::new();
        server.set_proxy_manager(proxy_manager);

        let resp = server.handle_tools_list(RequestId::Number(1));
        let result: ToolsListResult = serde_json::from_value(resp.result.unwrap()).unwrap();

        // Should have base tools (get_issues, get_merge_requests, etc.) + context tools
        assert!(result.tools.iter().any(|t| t.name == "get_issues"));
        assert!(result.tools.iter().any(|t| t.name == "list_contexts"));
        assert!(result.tools.iter().any(|t| t.name == "use_context"));
        assert!(result.tools.iter().any(|t| t.name == "get_current_context"));
        // No proxy tools (empty manager)
        assert!(!result.tools.iter().any(|t| t.name.contains("__")));
    }

    #[test]
    fn test_default_server_has_empty_proxy_manager() {
        let server = McpServer::default();
        // proxy_manager is empty by default — all_tools returns nothing
        let resp = server.handle_tools_list(RequestId::Number(1));
        let result: ToolsListResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(!result.tools.iter().any(|t| t.name.contains("__")));
    }

    #[test]
    fn test_builtin_tools_disabled_filters_tools_list() {
        let mut server = McpServer::new();
        // Add a provider so tools are available
        server.add_provider(Arc::new(TestProvider));
        server
            .set_builtin_tools_config(BuiltinToolsConfig {
                disabled: vec!["get_issues".to_string(), "create_issue".to_string()],
                enabled: vec![],
            })
            .unwrap();

        let resp = server.handle_tools_list(RequestId::Number(1));
        let result: ToolsListResult = serde_json::from_value(resp.result.unwrap()).unwrap();

        assert!(!result.tools.iter().any(|t| t.name == "get_issues"));
        assert!(!result.tools.iter().any(|t| t.name == "create_issue"));
        // Non-disabled tools should still be present
        assert!(result.tools.iter().any(|t| t.name == "get_merge_requests"));
        assert!(result.tools.iter().any(|t| t.name == "list_contexts"));
    }

    #[test]
    fn test_builtin_tools_enabled_whitelist_filters_tools_list() {
        let mut server = McpServer::new();
        server
            .set_builtin_tools_config(BuiltinToolsConfig {
                disabled: vec![],
                enabled: vec![
                    "list_contexts".to_string(),
                    "use_context".to_string(),
                    "get_current_context".to_string(),
                ],
            })
            .unwrap();

        let resp = server.handle_tools_list(RequestId::Number(1));
        let result: ToolsListResult = serde_json::from_value(resp.result.unwrap()).unwrap();

        assert_eq!(result.tools.len(), 3);
        assert!(result.tools.iter().any(|t| t.name == "list_contexts"));
        assert!(result.tools.iter().any(|t| t.name == "use_context"));
        assert!(result.tools.iter().any(|t| t.name == "get_current_context"));
        assert!(!result.tools.iter().any(|t| t.name == "get_issues"));
    }

    #[tokio::test]
    async fn test_disabled_tool_call_returns_error() {
        let mut server = McpServer::new();
        server
            .set_builtin_tools_config(BuiltinToolsConfig {
                disabled: vec!["get_issues".to_string()],
                enabled: vec![],
            })
            .unwrap();

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "get_issues",
                "arguments": {}
            })),
        };

        let resp = server.handle_request(req).await;
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, JsonRpcError::METHOD_NOT_FOUND);
        assert!(err.message.contains("disabled"));
    }

    #[tokio::test]
    async fn test_disabled_tool_allows_non_disabled() {
        let mut server = McpServer::new();
        server
            .set_builtin_tools_config(BuiltinToolsConfig {
                disabled: vec!["get_issues".to_string()],
                enabled: vec![],
            })
            .unwrap();

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "get_current_context",
                "arguments": {}
            })),
        };

        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    /// Test provider that simulates a ClickUp-like provider (issues only, no MRs).
    struct IssueOnlyTestProvider;

    #[async_trait]
    impl IssueProvider for IssueOnlyTestProvider {
        async fn get_issues(
            &self,
            _filter: IssueFilter,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Issue>> {
            Ok(vec![].into())
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
        async fn get_comments(
            &self,
            _issue_key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Comment>> {
            Ok(vec![].into())
        }
        async fn add_comment(&self, _issue_key: &str, _body: &str) -> devboy_core::Result<Comment> {
            Err(devboy_core::Error::NotFound("not found".into()))
        }
        fn provider_name(&self) -> &'static str {
            "clickup" // Issue-only provider (not github/gitlab)
        }
    }

    #[async_trait]
    impl MergeRequestProvider for IssueOnlyTestProvider {
        fn provider_name(&self) -> &'static str {
            "clickup"
        }
        // Default implementations return ProviderUnsupported
    }

    #[async_trait]
    impl devboy_core::PipelineProvider for IssueOnlyTestProvider {
        fn provider_name(&self) -> &'static str {
            "test"
        }
    }

    #[async_trait]
    impl Provider for IssueOnlyTestProvider {
        async fn get_current_user(&self) -> devboy_core::Result<User> {
            Ok(User {
                id: "1".to_string(),
                username: "clickup-user".to_string(),
                name: None,
                email: None,
                avatar_url: None,
            })
        }
    }

    #[test]
    fn test_issue_only_provider_has_issue_tools_but_no_mr_tools() {
        let mut server = McpServer::new();
        server.add_provider(Arc::new(IssueOnlyTestProvider));

        let resp = server.handle_tools_list(RequestId::Number(1));
        let result: ToolsListResult = serde_json::from_value(resp.result.unwrap()).unwrap();

        // Issue tools should be available
        assert!(result.tools.iter().any(|t| t.name == "get_issues"));
        assert!(result.tools.iter().any(|t| t.name == "get_issue"));
        assert!(result.tools.iter().any(|t| t.name == "create_issue"));

        // MR tools should NOT be available (ClickUp doesn't support MRs)
        assert!(!result.tools.iter().any(|t| t.name == "get_merge_requests"));
        assert!(
            !result
                .tools
                .iter()
                .any(|t| t.name == "get_merge_request_discussions")
        );

        // Context tools should always be available
        assert!(result.tools.iter().any(|t| t.name == "list_contexts"));
    }

    #[test]
    fn test_add_provider_to_context() {
        let mut server = McpServer::new();
        server.ensure_context("custom");
        server.add_provider_to_context("custom", Arc::new(TestProvider));

        // Default context should still be empty
        assert!(server.providers().is_empty());

        // Switch to custom context and verify provider is there
        server.set_active_context("custom").unwrap();
        assert_eq!(server.active_providers().len(), 1);
    }

    #[test]
    fn test_messenger_providers_are_scoped_to_active_context() {
        let mut server = McpServer::new();
        server.ensure_context("slack-context");
        server.ensure_context("plain-context");
        server.add_messenger_provider_to_context("slack-context", Arc::new(TestMessengerProvider));

        server.set_active_context("plain-context").unwrap();
        let plain_result: ToolsListResult = serde_json::from_value(
            server
                .handle_tools_list(RequestId::Number(1))
                .result
                .unwrap(),
        )
        .unwrap();
        assert!(
            !plain_result
                .tools
                .iter()
                .any(|tool| tool.name == "get_messenger_chats")
        );

        server.set_active_context("slack-context").unwrap();
        let slack_result: ToolsListResult = serde_json::from_value(
            server
                .handle_tools_list(RequestId::Number(2))
                .result
                .unwrap(),
        )
        .unwrap();
        assert!(
            slack_result
                .tools
                .iter()
                .any(|tool| tool.name == "get_messenger_chats")
        );
    }

    #[test]
    fn test_add_messenger_provider_creates_context_for_activation() {
        let mut server = McpServer::new();
        server.add_messenger_provider_to_context("messenger-only", Arc::new(TestMessengerProvider));

        assert!(
            server
                .context_names()
                .contains(&"messenger-only".to_string())
        );
        assert!(server.set_active_context("messenger-only").is_ok());
    }

    #[tokio::test]
    async fn test_deferred_init_resolves_proxy_on_tools_list() {
        let mut server = McpServer::new();
        server.initialized = true;

        // Set up deferred init with a proxy that has mock tools
        let (tx, rx) = oneshot::channel();
        server.set_deferred_init(rx);

        // Send the deferred init in background (simulates proxy loading)
        tokio::spawn(async move {
            // Small delay to simulate network
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let proxy_manager = ProxyManager::new();
            let _ = tx.send(DeferredInit {
                proxy_manager,
                builtin_tools_config: None,
            });
        });

        // tools/list should wait for deferred init to resolve
        let resp = server
            .handle_request(JsonRpcRequest {
                jsonrpc: JSONRPC_VERSION.to_string(),
                id: RequestId::Number(1),
                method: "tools/list".to_string(),
                params: None,
            })
            .await;

        assert!(resp.result.is_some());
        // Deferred init should be consumed (None after resolve)
        assert!(server.deferred_init.is_none());
    }

    #[tokio::test]
    async fn test_deferred_init_applies_builtin_tools_config() {
        let mut server = McpServer::new();
        server.initialized = true;
        server.add_provider(Arc::new(TestProvider));

        let (tx, rx) = oneshot::channel();
        server.set_deferred_init(rx);

        // Send deferred init that disables get_issues
        let _ = tx.send(DeferredInit {
            proxy_manager: ProxyManager::new(),
            builtin_tools_config: Some(BuiltinToolsConfig {
                disabled: vec!["get_issues".to_string()],
                enabled: vec![],
            }),
        });

        let resp = server
            .handle_request(JsonRpcRequest {
                jsonrpc: JSONRPC_VERSION.to_string(),
                id: RequestId::Number(1),
                method: "tools/list".to_string(),
                params: None,
            })
            .await;

        let result: ToolsListResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        // get_issues should be filtered out by remote builtin_tools config
        assert!(!result.tools.iter().any(|t| t.name == "get_issues"));
        // Other tools should still be present
        assert!(result.tools.iter().any(|t| t.name == "get_issue"));
    }
}
