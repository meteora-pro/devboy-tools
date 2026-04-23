//! MCP server implementation.
//!
//! The server handles the MCP protocol lifecycle:
//! 1. Initialize - exchange capabilities
//! 2. Handle tool calls - execute tools via providers
//! 3. Shutdown - graceful cleanup

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

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
use crate::routing::{RoutingEngine, RoutingTarget};
use crate::telemetry::{TelemetryBuffer, TelemetryEvent, TelemetryStatus};
use crate::transport::{IncomingMessage, StdioTransport};

/// Result of deferred background initialization (remote config + proxy).
pub struct DeferredInit {
    /// Proxy manager with connected upstream servers and fetched tools.
    pub proxy_manager: ProxyManager,
    /// Builtin tools config from remote config (overrides local if non-empty).
    pub builtin_tools_config: Option<BuiltinToolsConfig>,
    /// Transparent-routing engine built from the merged config + proxy catalogue.
    /// When `None`, the server keeps its pre-feature dispatch behaviour.
    pub routing_engine: Option<Arc<RoutingEngine>>,
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
    /// Transparent-routing decision engine. When `None`, calls route the legacy way
    /// (explicit proxy prefix → remote, otherwise local).
    routing_engine: Option<Arc<RoutingEngine>>,
    /// Telemetry buffer — every invocation records one event here (best-effort).
    telemetry: Option<TelemetryBuffer>,
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
            routing_engine: None,
            telemetry: None,
            deferred_init: None,
        }
    }

    /// Install a transparent-routing engine. Must be built by the caller after upstream
    /// tools have been fetched and the local tool catalogue has been enumerated.
    pub fn set_routing_engine(&mut self, engine: Arc<RoutingEngine>) {
        self.routing_engine = Some(engine);
    }

    /// Install a telemetry buffer. Every tool invocation emits one event into it.
    pub fn set_telemetry(&mut self, buffer: TelemetryBuffer) {
        self.telemetry = Some(buffer);
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

    /// Resolve deferred init if pending — applies proxy manager, remote builtin_tools
    /// config, and the transparent-routing engine (built off the finalised upstream
    /// catalogue).
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
                    if let Some(engine) = init.routing_engine {
                        self.routing_engine = Some(engine);
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
        // Jira Structure is a Jira-only add-on; don't expose the
        // category under GitHub/GitLab/ClickUp — every call would
        // return ProviderUnsupported and pollute the tool list.
        let has_jira_provider = providers
            .iter()
            .any(|p| IssueProvider::provider_name(p.as_ref()) == "jira");
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
                    devboy_core::ToolCategory::JiraStructure => has_jira_provider,
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

        // Internal context-management tools short-circuit routing entirely.
        if let Some(result) = self.handle_internal_tool(&params).await {
            return JsonRpcResponse::success(id, serde_json::to_value(result).unwrap());
        }

        let started = Instant::now();
        let (result, was_fallback, emitted_reason, emitted_detail, upstream_label, resolved_name) =
            self.dispatch_with_routing(&params).await;

        // Best-effort telemetry — never block response path on this.
        if let Some(buffer) = &self.telemetry {
            let latency_ms = started.elapsed().as_millis() as u64;
            let status = if result.is_error == Some(true) {
                TelemetryStatus::Error
            } else {
                TelemetryStatus::Success
            };
            // Always use the *resolved* (unprefixed) name — upstream-prefixed variants
            // like `cloud__get_issues` would break backend tool-name validation and
            // inflate per-tool dashboards with duplicate labels.
            let mut event = TelemetryEvent::now(&resolved_name, emitted_reason);
            event.routing_detail = emitted_detail;
            event.upstream = upstream_label;
            event.status = status;
            event.latency_ms = latency_ms;
            event.was_fallback = was_fallback;
            buffer.record(event).await;
        }

        JsonRpcResponse::success(id, serde_json::to_value(result).unwrap())
    }

    /// Dispatch context-management tools. Returns `Some` when handled.
    async fn handle_internal_tool(&self, params: &ToolCallParams) -> Option<ToolCallResult> {
        match params.name.as_str() {
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
                Some(ToolCallResult::text(content))
            }
            "get_current_context" => Some(ToolCallResult::text(self.active_context_name())),
            "use_context" => {
                #[derive(Deserialize)]
                struct UseContextParams {
                    name: String,
                }
                Some(match &params.arguments {
                    Some(args) => match serde_json::from_value::<UseContextParams>(args.clone()) {
                        Ok(args) => match self.set_active_context(&args.name) {
                            Ok(()) => ToolCallResult::text(format!(
                                "Active context set to '{}'",
                                args.name
                            )),
                            Err(e) => ToolCallResult::error(e.to_string()),
                        },
                        Err(e) => ToolCallResult::error(format!("Invalid parameters: {}", e)),
                    },
                    None => ToolCallResult::error("Missing required parameter: name".to_string()),
                })
            }
            _ => None,
        }
    }

    /// Resolve executor for `params.name` using the routing engine (when present) and
    /// dispatch. Falls back to the secondary executor on error when the decision allows.
    ///
    /// Returns the final `ToolCallResult` plus telemetry metadata:
    /// `(result, was_fallback, reason_label, reason_detail, upstream_label)`.
    async fn dispatch_with_routing(
        &self,
        params: &ToolCallParams,
    ) -> (
        ToolCallResult,
        bool,
        String,
        Option<String>,
        Option<String>,
        String,
    ) {
        // When no engine is wired, keep legacy behaviour: explicit prefix → remote,
        // otherwise local. For telemetry we still want an unprefixed name, so strip the
        // upstream prefix ourselves.
        let Some(engine) = self.routing_engine.clone() else {
            let result = self.legacy_dispatch(params).await;
            let (reason, resolved) = if self.proxy_manager.has_tool(&params.name) {
                // `foo__bar` → `bar`; falls back to the full name if there is no prefix.
                let stripped = params
                    .name
                    .split_once("__")
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or_else(|| params.name.clone());
                ("legacy_remote", stripped)
            } else {
                ("legacy_local", params.name.clone())
            };
            return (result, false, reason.to_string(), None, None, resolved);
        };

        let decision = engine.decide(&params.name);
        let reason_label = decision.reason.as_label().to_string();
        let reason_detail = decision.reason.detail().map(String::from);
        let resolved_name = decision.resolved_name.clone();

        let primary = decision.primary.clone();
        let result = self
            .execute_target(&primary, &decision.resolved_name, params.arguments.clone())
            .await;

        let upstream_label = match &primary {
            RoutingTarget::Remote { prefix, .. } => Some(prefix.clone()),
            _ => None,
        };

        if result.is_error == Some(true)
            && let Some(fallback) = &decision.fallback
        {
            tracing::warn!(
                tool = params.name.as_str(),
                primary_target = ?primary,
                "primary executor errored; retrying via fallback"
            );
            let fb_result = self
                .execute_target(fallback, &decision.resolved_name, params.arguments.clone())
                .await;
            let fb_upstream = match fallback {
                RoutingTarget::Remote { prefix, .. } => Some(prefix.clone()),
                _ => None,
            };
            return (
                fb_result,
                true,
                reason_label,
                reason_detail,
                fb_upstream,
                resolved_name,
            );
        }

        (
            result,
            false,
            reason_label,
            reason_detail,
            upstream_label,
            resolved_name,
        )
    }

    /// Run a specific [`RoutingTarget`]. Local execution goes through
    /// [`Self::dispatch_builtin_tool`] (full built-in tool set including meeting /
    /// messenger providers and the `Executor`); remote execution forwards to the
    /// matching upstream via [`ProxyManager`].
    async fn execute_target(
        &self,
        target: &RoutingTarget,
        unprefixed_name: &str,
        arguments: Option<Value>,
    ) -> ToolCallResult {
        match target {
            RoutingTarget::Local => self.dispatch_builtin_tool(unprefixed_name, arguments).await,
            RoutingTarget::Remote {
                prefix,
                original_name,
            } => self
                .proxy_manager
                .call_by_prefix(prefix, original_name, arguments)
                .await
                .unwrap_or_else(|| {
                    ToolCallResult::error(format!(
                        "No upstream MCP server connected with prefix '{}'",
                        prefix
                    ))
                }),
            RoutingTarget::Reject => ToolCallResult::error(format!(
                "Tool '{}' is not available (unknown to both local and remote catalogues)",
                unprefixed_name
            )),
        }
    }

    /// Legacy dispatch — used only when no routing engine is installed. Preserves the
    /// pre-transparent-proxy behaviour for integrators that haven't opted in.
    async fn legacy_dispatch(&self, params: &ToolCallParams) -> ToolCallResult {
        if let Some(result) = self
            .proxy_manager
            .try_call(&params.name, params.arguments.clone())
            .await
        {
            return result;
        }
        self.dispatch_builtin_tool(&params.name, params.arguments.clone())
            .await
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
/// invalid for GitHub but valid for GitLab — that case should move on
/// to the next provider. Real upstream errors (missing issue, 5xx,
/// reqwest DNS failure) must not be hidden behind a generic "No
/// provider supports …" fallback.
fn should_try_next_provider(e: &devboy_core::Error) -> bool {
    // `ProviderUnsupported` / `ProviderNotFound` unconditionally mean
    // "this provider can't handle the tool at all".
    if matches!(
        e,
        devboy_core::Error::ProviderUnsupported { .. } | devboy_core::Error::ProviderNotFound(_)
    ) {
        return true;
    }

    // A handful of providers surface "this key does not belong to me"
    // as `InvalidData("Invalid {issue,mr,pr} key: <k>")` (see
    // `parse_issue_key` / `parse_pr_key` / `parse_mr_key` across
    // `plugins/api/*/src/client.rs`). Those are structurally
    // equivalent to `ProviderUnsupported` — the caller asked a
    // provider a key it cannot parse — and skipping them preserves
    // the multi-provider chain while still letting real upstream
    // `InvalidData` (bad payload from the server, failed
    // deserialisation, etc.) bubble up.
    if let devboy_core::Error::InvalidData(msg) = e {
        let lower = msg.to_ascii_lowercase();
        let is_key_prefix_mismatch = (lower.contains("invalid") && lower.contains("key"))
            || lower.contains("unsupported key prefix");
        if is_key_prefix_mismatch {
            return true;
        }
    }

    false
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

    #[test]
    fn should_try_next_provider_retries_unsupported_and_not_found() {
        assert!(should_try_next_provider(
            &devboy_core::Error::ProviderUnsupported {
                provider: "github".into(),
                operation: "get_structures".into(),
            }
        ));
        assert!(should_try_next_provider(
            &devboy_core::Error::ProviderNotFound("jira".into())
        ));
    }

    #[test]
    fn should_try_next_provider_retries_key_prefix_mismatch_invalid_data() {
        // Regression for #187 Copilot comment — GitHub's
        // `parse_issue_key("gitlab#1")` returns `InvalidData("Invalid
        // issue key: gitlab#1")`. In a multi-provider chain that is
        // structurally equivalent to "this provider can't handle that
        // key"; we must keep iterating so GitLab gets a turn.
        assert!(should_try_next_provider(&devboy_core::Error::InvalidData(
            "Invalid issue key: gitlab#1".into()
        )));
        assert!(should_try_next_provider(&devboy_core::Error::InvalidData(
            "Invalid PR key: gh#7".into()
        )));
        assert!(should_try_next_provider(&devboy_core::Error::InvalidData(
            "Invalid mr key: pr#4".into()
        )));
    }

    #[test]
    fn should_try_next_provider_bubbles_up_real_errors() {
        // The original bug: these error classes used to skip to the
        // next provider and then produce "No provider supports …",
        // hiding the real cause. They must not be retryable.
        assert!(!should_try_next_provider(&devboy_core::Error::NotFound(
            "No workflow runs found for branch 'main'".into()
        )));
        assert!(!should_try_next_provider(&devboy_core::Error::Http(
            "500 Internal Server Error".into()
        )));
        assert!(!should_try_next_provider(&devboy_core::Error::Unauthorized(
            "Bad credentials".into()
        )));
        // Generic InvalidData that doesn't look like a key mismatch is
        // also a real error (e.g., the executor's own "invalid …
        // params" from parse_tool_params).
        assert!(!should_try_next_provider(&devboy_core::Error::InvalidData(
            "invalid get_issues params: expected string, found integer".into()
        )));
    }

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

    // =========================================================================
    // Routing engine integration (Wire-up #1)
    // =========================================================================

    use crate::protocol::ToolDefinition;
    use crate::routing::RoutingEngine;
    use crate::signature_match::{MatchReport, ToolMatch};
    use devboy_core::config::{ProxyRoutingConfig, RoutingStrategy};

    fn match_report_with(items: Vec<ToolMatch>) -> MatchReport {
        let mut r = MatchReport::default();
        for m in items {
            r.matches.insert(m.tool_name.clone(), m);
        }
        r
    }

    #[tokio::test]
    async fn test_routing_engine_reject_decision_surfaces_as_error_result() {
        let mut server = McpServer::new();
        // Empty match report → every unknown tool is rejected.
        let engine = RoutingEngine::new(ProxyRoutingConfig::default(), MatchReport::default());
        server.set_routing_engine(Arc::new(engine));

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "mystery_tool",
                "arguments": {}
            })),
        };
        let resp = server.handle_request(req).await;
        let result: ToolCallResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert_eq!(result.is_error, Some(true));
        match &result.content[0] {
            ToolResultContent::Text { text } => {
                assert!(text.contains("unknown to both local and remote"));
            }
        }
    }

    #[tokio::test]
    async fn test_routing_engine_local_dispatch_uses_toolhandler() {
        let mut server = McpServer::new();
        server.add_provider(Arc::new(TestProvider));

        let report = match_report_with(vec![ToolMatch {
            tool_name: "get_issues".to_string(),
            local_present: true,
            remote_present: false,
            schema_compatible: None,
            upstream_prefix: None,
            schema_mismatch: None,
        }]);
        let engine = RoutingEngine::new(ProxyRoutingConfig::default(), report);
        server.set_routing_engine(Arc::new(engine));

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
        // TestProvider returns empty list — success path.
        assert!(resp.error.is_none());
        let result: ToolCallResult = serde_json::from_value(resp.result.unwrap()).unwrap();
        assert!(result.is_error.is_none());
    }

    #[tokio::test]
    async fn test_telemetry_buffer_receives_event_per_call() {
        let mut server = McpServer::new();
        server.add_provider(Arc::new(TestProvider));

        let report = match_report_with(vec![ToolMatch {
            tool_name: "get_issues".to_string(),
            local_present: true,
            remote_present: false,
            schema_compatible: None,
            upstream_prefix: None,
            schema_mismatch: None,
        }]);
        let engine = RoutingEngine::new(ProxyRoutingConfig::default(), report);
        server.set_routing_engine(Arc::new(engine));

        let buffer = TelemetryBuffer::new(16);
        server.set_telemetry(buffer.clone());

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "get_issues",
                "arguments": {}
            })),
        };
        let _resp = server.handle_request(req).await;

        let events = buffer.drain(100).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool, "get_issues");
        assert_eq!(events[0].routing_decision, "local_only");
        assert_eq!(events[0].status, TelemetryStatus::Success);
    }

    #[tokio::test]
    async fn test_telemetry_event_captures_reason_detail_for_override_rule() {
        let mut server = McpServer::new();
        server.add_provider(Arc::new(TestProvider));

        let report = match_report_with(vec![ToolMatch {
            tool_name: "get_issues".to_string(),
            local_present: true,
            remote_present: true,
            schema_compatible: Some(true),
            upstream_prefix: Some("cloud".to_string()),
            schema_mismatch: None,
        }]);
        // Override all get_* to local so we don't hit the unconnected proxy.
        let config = ProxyRoutingConfig {
            strategy: RoutingStrategy::Remote,
            fallback_on_error: true,
            tool_overrides: vec![devboy_core::config::ProxyToolRule {
                pattern: "get_*".to_string(),
                strategy: RoutingStrategy::Local,
            }],
        };
        let engine = RoutingEngine::new(config, report);
        server.set_routing_engine(Arc::new(engine));

        let buffer = TelemetryBuffer::new(16);
        server.set_telemetry(buffer.clone());

        let req = JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: RequestId::Number(1),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "get_issues",
                "arguments": {}
            })),
        };
        let _resp = server.handle_request(req).await;

        let events = buffer.drain(100).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].routing_decision, "override_rule");
        assert_eq!(events[0].routing_detail.as_deref(), Some("get_*"));
        assert!(events[0].upstream.is_none());
    }

    #[tokio::test]
    async fn test_no_routing_engine_keeps_legacy_behaviour() {
        // When set_routing_engine has not been called, server must keep the pre-feature
        // dispatch semantics intact so existing deployments see zero behaviour change.
        let mut server = McpServer::new();
        server.add_provider(Arc::new(TestProvider));
        let buffer = TelemetryBuffer::new(16);
        server.set_telemetry(buffer.clone());

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
        assert!(resp.error.is_none());
        let events = buffer.drain(100).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].routing_decision, "legacy_local");
    }

    // Keep rustc from flagging the imports as unused if any cfg path changes.
    #[allow(dead_code)]
    fn _unused_cfg_helper_tooldef() -> ToolDefinition {
        ToolDefinition {
            name: "x".into(),
            description: "x".into(),
            input_schema: serde_json::json!({}),
            category: None,
        }
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
                routing_engine: None,
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
            routing_engine: None,
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
