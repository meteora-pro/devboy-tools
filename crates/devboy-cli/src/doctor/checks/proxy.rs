use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};
use async_trait::async_trait;
use devboy_core::ProxyMcpServerConfig;
use devboy_mcp::{McpProxyClient, ProxyTransport};
use serde_json::{Value, json};
use std::time::Instant;

pub struct ProxyServersCheck;

#[derive(Debug)]
struct ProxyProbeResult {
    status: CheckStatus,
    detail: Value,
    fix_command: Option<String>,
}

#[async_trait]
impl DiagnosticCheck for ProxyServersCheck {
    fn id(&self) -> &'static str {
        "proxy.servers"
    }

    fn name(&self) -> &'static str {
        "Proxy MCP server connectivity"
    }

    fn category(&self) -> &'static str {
        "Proxy"
    }

    async fn run(&self, ctx: &DiagnosticContext) -> CheckResult {
        let Some(config) = &ctx.config else {
            return skipped(self, "Skipped because config could not be loaded");
        };

        if config.proxy_mcp_servers.is_empty() {
            return skipped(self, "Skipped because no proxy MCP servers are configured");
        }

        let mut passed = 0usize;
        let mut warnings = 0usize;
        let mut errors = 0usize;
        let mut details = Vec::with_capacity(config.proxy_mcp_servers.len());
        let mut fix_command = None;

        for proxy in &config.proxy_mcp_servers {
            let probe = probe_proxy_server(ctx, proxy).await;
            match probe.status {
                CheckStatus::Pass => passed += 1,
                CheckStatus::Warning => warnings += 1,
                CheckStatus::Error => errors += 1,
                CheckStatus::Skipped => {}
            }

            if fix_command.is_none() {
                fix_command = probe.fix_command.clone();
            }
            details.push(probe.detail);
        }

        let status = if errors > 0 {
            CheckStatus::Error
        } else if warnings > 0 {
            CheckStatus::Warning
        } else {
            CheckStatus::Pass
        };

        let message = if errors > 0 {
            format!(
                "Proxy servers checked: {} reachable, {} warning(s), {} failed",
                passed, warnings, errors
            )
        } else if warnings > 0 {
            format!(
                "Proxy servers checked: {} reachable, {} warning(s)",
                passed, warnings
            )
        } else {
            format!("Proxy servers reachable: {}", passed)
        };

        CheckResult {
            id: self.id().to_string(),
            category: self.category().to_string(),
            name: self.name().to_string(),
            status,
            message,
            details: ctx.verbose.then(|| json!({ "servers": details })),
            fix_command,
            fix_url: None,
        }
    }
}

fn skipped(check: &dyn DiagnosticCheck, message: &str) -> CheckResult {
    CheckResult {
        id: check.id().to_string(),
        category: check.category().to_string(),
        name: check.name().to_string(),
        status: CheckStatus::Skipped,
        message: message.to_string(),
        details: None,
        fix_command: None,
        fix_url: None,
    }
}

fn normalized_auth_type(auth_type: &str) -> Option<&'static str> {
    match auth_type {
        "bearer" => Some("bearer"),
        "api_key" => Some("api_key"),
        "none" => Some("none"),
        "oauth2" => Some("oauth2"),
        _ => None,
    }
}

/// Report the OAuth login state for an `auth_type = "oauth2"` proxy (issue #306).
/// This is the stdio-side awareness signal: a human — or a coding agent reading
/// `devboy doctor` — sees whether the proxy is authorized and, if not, the exact
/// `devboy login <name>` to run. (No live probe: a valid session may still be
/// mid-flight, and tokens auto-refresh at request time.)
fn probe_oauth_state(
    ctx: &DiagnosticContext,
    proxy: &ProxyMcpServerConfig,
    auth_type: &str,
) -> ProxyProbeResult {
    let key = format!("proxy.{}.oauth", proxy.name);
    let logged_in = matches!(ctx.credential_store.get(&key), Ok(Some(_)));
    let has_client = proxy
        .oauth
        .as_ref()
        .and_then(|o| o.client_id.as_deref())
        .is_some();
    if logged_in && has_client {
        ProxyProbeResult {
            status: CheckStatus::Pass,
            detail: json!({
                "name": proxy.name,
                "url": proxy.url,
                "auth_type": auth_type,
                "transport": proxy.transport,
                "status": "logged_in",
                "message": "OAuth: logged in; tokens auto-refresh",
            }),
            fix_command: None,
        }
    } else {
        ProxyProbeResult {
            status: CheckStatus::Error,
            detail: json!({
                "name": proxy.name,
                "url": proxy.url,
                "auth_type": auth_type,
                "transport": proxy.transport,
                "status": "needs_login",
                "message": format!("OAuth proxy '{}' is not logged in", proxy.name),
            }),
            fix_command: Some(format!("devboy login {}", proxy.name)),
        }
    }
}

fn normalized_transport(raw: &str) -> Option<ProxyTransport> {
    match raw {
        "streamable-http" | "streamable_http" | "http" => Some(ProxyTransport::StreamableHttp),
        "sse" => Some(ProxyTransport::Sse),
        _ => None,
    }
}

fn classify_proxy_error(error: &str) -> String {
    if error.contains("HTTP 401") || error.contains("HTTP 403") || error.contains("Invalid token") {
        format!("authentication failed: {error}")
    } else {
        format!("connectivity failed: {error}")
    }
}

async fn probe_proxy_server(
    ctx: &DiagnosticContext,
    proxy: &ProxyMcpServerConfig,
) -> ProxyProbeResult {
    let auth_type = match normalized_auth_type(proxy.auth_type.as_str()) {
        Some(auth_type) => auth_type,
        None => {
            return ProxyProbeResult {
                status: CheckStatus::Error,
                detail: json!({
                    "name": proxy.name,
                    "url": proxy.url,
                    "auth_type": proxy.auth_type,
                    "transport": proxy.transport,
                    "status": "error",
                    "message": format!("Invalid auth_type '{}'", proxy.auth_type),
                }),
                fix_command: None,
            };
        }
    };

    let transport = match normalized_transport(proxy.transport.as_str()) {
        Some(transport) => transport,
        None => {
            return ProxyProbeResult {
                status: CheckStatus::Error,
                detail: json!({
                    "name": proxy.name,
                    "url": proxy.url,
                    "auth_type": auth_type,
                    "transport": proxy.transport,
                    "status": "error",
                    "message": format!("Invalid transport '{}'", proxy.transport),
                }),
                fix_command: None,
            };
        }
    };

    // OAuth2 proxies carry no token_key — report login state instead of probing.
    if auth_type == "oauth2" {
        return probe_oauth_state(ctx, proxy, auth_type);
    }

    let token = match auth_type {
        "none" => None,
        _ => {
            let Some(token_key) = proxy.token_key.as_deref() else {
                return ProxyProbeResult {
                    status: CheckStatus::Error,
                    detail: json!({
                        "name": proxy.name,
                        "url": proxy.url,
                        "auth_type": auth_type,
                        "transport": proxy.transport,
                        "status": "error",
                        "message": "Missing token_key for authenticated proxy",
                    }),
                    fix_command: None,
                };
            };

            match ctx.credential_store.get(token_key) {
                Ok(Some(token)) => Some((token_key.to_string(), token)),
                Ok(None) => {
                    return ProxyProbeResult {
                        status: CheckStatus::Error,
                        detail: json!({
                            "name": proxy.name,
                            "url": proxy.url,
                            "auth_type": auth_type,
                            "transport": proxy.transport,
                            "token_key": token_key,
                            "status": "error",
                            "message": "Token not found in credential store",
                        }),
                        fix_command: Some(format!("devboy config set-secret {token_key} <TOKEN>")),
                    };
                }
                Err(error) => {
                    return ProxyProbeResult {
                        status: CheckStatus::Error,
                        detail: json!({
                            "name": proxy.name,
                            "url": proxy.url,
                            "auth_type": auth_type,
                            "transport": proxy.transport,
                            "token_key": token_key,
                            "status": "error",
                            "message": error.to_string(),
                        }),
                        fix_command: None,
                    };
                }
            }
        }
    };

    let started = Instant::now();
    let token_value = token.as_ref().map(|(_, value)| value);
    let mut client = match McpProxyClient::connect(
        &proxy.name,
        &proxy.url,
        proxy.tool_prefix.as_deref(),
        token_value,
        auth_type,
        transport,
        None,
    )
    .await
    {
        Ok(client) => client,
        Err(error) => {
            let latency_ms = started.elapsed().as_millis();
            let message = classify_proxy_error(&error.to_string());
            return ProxyProbeResult {
                status: CheckStatus::Error,
                detail: json!({
                    "name": proxy.name,
                    "url": proxy.url,
                    "auth_type": auth_type,
                    "transport": proxy.transport,
                    "token_key": token.as_ref().map(|(key, _)| key),
                    "latency_ms": latency_ms,
                    "status": "error",
                    "message": message,
                }),
                fix_command: token
                    .as_ref()
                    .map(|(key, _)| format!("devboy config set-secret {key} <TOKEN>")),
            };
        }
    };

    let tools_result = client.fetch_tools().await;
    let latency_ms = started.elapsed().as_millis();

    match tools_result {
        Ok(()) => {
            let tools = client.prefixed_tools();
            let status = if tools.is_empty() {
                CheckStatus::Warning
            } else {
                CheckStatus::Pass
            };
            let message = if tools.is_empty() {
                "connected but returned no tools".to_string()
            } else {
                format!("connected and exposed {} tool(s)", tools.len())
            };

            ProxyProbeResult {
                status,
                detail: json!({
                    "name": proxy.name,
                    "url": proxy.url,
                    "auth_type": auth_type,
                    "transport": proxy.transport,
                    "tool_prefix": proxy.tool_prefix.as_deref().unwrap_or(proxy.name.as_str()),
                    "token_key": token.as_ref().map(|(key, _)| key),
                    "latency_ms": latency_ms,
                    "tools_count": tools.len(),
                    "tools": tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>(),
                    "status": match status {
                        CheckStatus::Pass => "pass",
                        CheckStatus::Warning => "warning",
                        CheckStatus::Error => "error",
                        CheckStatus::Skipped => "skipped",
                    },
                    "message": message,
                }),
                fix_command: None,
            }
        }
        Err(error) => {
            let message = classify_proxy_error(&error.to_string());
            ProxyProbeResult {
                status: CheckStatus::Error,
                detail: json!({
                    "name": proxy.name,
                    "url": proxy.url,
                    "auth_type": auth_type,
                    "transport": proxy.transport,
                    "token_key": token.as_ref().map(|(key, _)| key),
                    "latency_ms": latency_ms,
                    "status": "error",
                    "message": message,
                }),
                fix_command: token
                    .as_ref()
                    .map(|(key, _)| format!("devboy config set-secret {key} <TOKEN>")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::DiagnosticContext;
    use devboy_core::{Config, Error, ProxyMcpServerConfig};
    use devboy_storage::{CredentialStore, MemoryStore};
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use secrecy::SecretString;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[derive(Debug)]
    struct FailingStore;

    impl CredentialStore for FailingStore {
        fn store(&self, _key: &str, _value: &SecretString) -> devboy_core::Result<()> {
            Err(Error::Storage("store failed".to_string()))
        }

        fn get(&self, _key: &str) -> devboy_core::Result<Option<SecretString>> {
            Err(Error::Storage("proxy store unavailable".to_string()))
        }

        fn delete(&self, _key: &str) -> devboy_core::Result<()> {
            Err(Error::Storage("delete failed".to_string()))
        }
    }

    fn context_with_proxy(config: Config, store: MemoryStore, verbose: bool) -> DiagnosticContext {
        DiagnosticContext {
            config: Some(config),
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: Arc::new(store),
            verbose,
        }
    }

    fn setup_streamable_http_proxy(server: &MockServer) {
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"initialize""#);
            then.status(200)
                .header("mcp-session-id", "sess-1")
                .json_body(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "serverInfo": { "name": "mock", "version": "1.0" }
                    }
                }));
        });

        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"tools/list""#);
            then.status(200).json_body(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "tools": [{
                        "name": "get_issues",
                        "description": "Get issues",
                        "inputSchema": { "type": "object" }
                    }]
                }
            }));
        });
    }

    fn setup_empty_streamable_http_proxy(server: &MockServer) {
        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"initialize""#);
            then.status(200)
                .header("mcp-session-id", "sess-1")
                .json_body(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "serverInfo": { "name": "mock", "version": "1.0" }
                    }
                }));
        });

        server.mock(|when, then| {
            when.method(POST)
                .path("/mcp")
                .body_includes(r#""method":"tools/list""#);
            then.status(200).json_body(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": { "tools": [] }
            }));
        });
    }

    #[tokio::test]
    async fn proxy_servers_check_passes_for_reachable_proxy() {
        let server = MockServer::start();
        setup_streamable_http_proxy(&server);

        let ctx = context_with_proxy(
            Config {
                proxy_mcp_servers: vec![ProxyMcpServerConfig {
                    name: "cloud".to_string(),
                    url: format!("{}/mcp", server.base_url()),
                    auth_type: "none".to_string(),
                    token_key: None,
                    tool_prefix: Some("cloud".to_string()),
                    transport: "streamable-http".to_string(),
                    routing: None,
                    oauth: None,
                }],
                ..Default::default()
            },
            MemoryStore::new(),
            true,
        );

        let result = ProxyServersCheck.run(&ctx).await;
        assert_eq!(result.status, CheckStatus::Pass);
        let details = result.details.unwrap();
        assert_eq!(details["servers"][0]["tools_count"], 1);
    }

    #[tokio::test]
    async fn proxy_servers_check_skips_without_config_or_servers() {
        let no_config_ctx = DiagnosticContext {
            config: None,
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: Arc::new(MemoryStore::new()),
            verbose: true,
        };
        assert_eq!(
            ProxyServersCheck.run(&no_config_ctx).await.status,
            CheckStatus::Skipped
        );

        let empty_ctx = context_with_proxy(Config::default(), MemoryStore::new(), true);
        let result = ProxyServersCheck.run(&empty_ctx).await;
        assert_eq!(result.status, CheckStatus::Skipped);
        assert!(result.message.contains("no proxy MCP servers"));
    }

    #[tokio::test]
    async fn proxy_servers_check_errors_when_token_missing() {
        let ctx = context_with_proxy(
            Config {
                proxy_mcp_servers: vec![ProxyMcpServerConfig {
                    name: "cloud".to_string(),
                    url: "https://example.com/mcp".to_string(),
                    auth_type: "bearer".to_string(),
                    token_key: Some("proxy.cloud.token".to_string()),
                    tool_prefix: None,
                    transport: "streamable-http".to_string(),
                    routing: None,
                    oauth: None,
                }],
                ..Default::default()
            },
            MemoryStore::new(),
            true,
        );

        let result = ProxyServersCheck.run(&ctx).await;
        assert_eq!(result.status, CheckStatus::Error);
        assert_eq!(
            result.fix_command.as_deref(),
            Some("devboy config set-secret proxy.cloud.token <TOKEN>")
        );
    }

    #[tokio::test]
    async fn proxy_servers_check_errors_for_invalid_transport() {
        let ctx = context_with_proxy(
            Config {
                proxy_mcp_servers: vec![ProxyMcpServerConfig {
                    name: "cloud".to_string(),
                    url: "https://example.com/mcp".to_string(),
                    auth_type: "none".to_string(),
                    token_key: None,
                    tool_prefix: None,
                    transport: "grpc".to_string(),
                    routing: None,
                    oauth: None,
                }],
                ..Default::default()
            },
            MemoryStore::new(),
            true,
        );

        let result = ProxyServersCheck.run(&ctx).await;
        assert_eq!(result.status, CheckStatus::Error);
        let details = result.details.unwrap();
        assert_eq!(details["servers"][0]["message"], "Invalid transport 'grpc'");
    }

    #[tokio::test]
    async fn proxy_servers_check_warns_when_proxy_has_no_tools() {
        let server = MockServer::start();
        setup_empty_streamable_http_proxy(&server);

        let ctx = context_with_proxy(
            Config {
                proxy_mcp_servers: vec![ProxyMcpServerConfig {
                    name: "cloud".to_string(),
                    url: format!("{}/mcp", server.base_url()),
                    auth_type: "none".to_string(),
                    token_key: None,
                    tool_prefix: None,
                    transport: "streamable-http".to_string(),
                    routing: None,
                    oauth: None,
                }],
                ..Default::default()
            },
            MemoryStore::new(),
            true,
        );

        let result = ProxyServersCheck.run(&ctx).await;
        assert_eq!(result.status, CheckStatus::Warning);
        assert!(result.message.contains("1 warning"));
    }

    #[tokio::test]
    async fn probe_proxy_server_covers_auth_and_store_errors() {
        let ctx = DiagnosticContext {
            config: Some(Config::default()),
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: Arc::new(FailingStore),
            verbose: true,
        };

        let invalid_auth = probe_proxy_server(
            &ctx,
            &ProxyMcpServerConfig {
                name: "cloud".to_string(),
                url: "https://example.com/mcp".to_string(),
                auth_type: "basic".to_string(),
                token_key: None,
                tool_prefix: None,
                transport: "streamable-http".to_string(),
                routing: None,
                oauth: None,
            },
        )
        .await;
        assert_eq!(invalid_auth.status, CheckStatus::Error);
        assert_eq!(invalid_auth.detail["message"], "Invalid auth_type 'basic'");

        let missing_key = probe_proxy_server(
            &ctx,
            &ProxyMcpServerConfig {
                name: "cloud".to_string(),
                url: "https://example.com/mcp".to_string(),
                auth_type: "bearer".to_string(),
                token_key: None,
                tool_prefix: None,
                transport: "streamable-http".to_string(),
                routing: None,
                oauth: None,
            },
        )
        .await;
        assert_eq!(missing_key.status, CheckStatus::Error);
        assert_eq!(
            missing_key.detail["message"],
            "Missing token_key for authenticated proxy"
        );

        let store_error = probe_proxy_server(
            &ctx,
            &ProxyMcpServerConfig {
                name: "cloud".to_string(),
                url: "https://example.com/mcp".to_string(),
                auth_type: "bearer".to_string(),
                token_key: Some("proxy.cloud.token".to_string()),
                tool_prefix: None,
                transport: "streamable-http".to_string(),
                routing: None,
                oauth: None,
            },
        )
        .await;
        assert_eq!(store_error.status, CheckStatus::Error);
        assert_eq!(
            store_error.detail["message"],
            "Storage error: proxy store unavailable"
        );
    }

    #[tokio::test]
    async fn probe_proxy_server_reports_connection_failures() {
        let ctx = DiagnosticContext {
            config: Some(Config::default()),
            config_path: Some(PathBuf::from("config.toml")),
            config_exists: true,
            config_source: "test",
            config_path_error: None,
            config_load_error: None,
            credential_store: Arc::new(MemoryStore::with_credentials([(
                "proxy.cloud.token".to_string(),
                "secret".to_string(),
            )])),
            verbose: true,
        };

        let result = probe_proxy_server(
            &ctx,
            &ProxyMcpServerConfig {
                name: "cloud".to_string(),
                url: "http://127.0.0.1:9/mcp".to_string(),
                auth_type: "bearer".to_string(),
                token_key: Some("proxy.cloud.token".to_string()),
                tool_prefix: None,
                transport: "streamable-http".to_string(),
                routing: None,
                oauth: None,
            },
        )
        .await;

        assert_eq!(result.status, CheckStatus::Error);
        assert!(
            result.detail["message"]
                .as_str()
                .unwrap()
                .contains("connectivity failed")
        );
    }

    #[test]
    fn proxy_helpers_cover_normalization_and_classification() {
        assert_eq!(normalized_auth_type("bearer"), Some("bearer"));
        assert_eq!(normalized_auth_type("none"), Some("none"));
        assert_eq!(normalized_auth_type("weird"), None);

        assert!(matches!(
            normalized_transport("http"),
            Some(ProxyTransport::StreamableHttp)
        ));
        assert!(matches!(
            normalized_transport("sse"),
            Some(ProxyTransport::Sse)
        ));
        assert!(normalized_transport("grpc").is_none());

        assert_eq!(
            classify_proxy_error("HTTP 401 unauthorized"),
            "authentication failed: HTTP 401 unauthorized"
        );
        assert_eq!(
            classify_proxy_error("socket closed"),
            "connectivity failed: socket closed"
        );

        let skipped_result = skipped(&ProxyServersCheck, "skip");
        assert_eq!(skipped_result.status, CheckStatus::Skipped);
        assert_eq!(skipped_result.message, "skip");
    }
}
