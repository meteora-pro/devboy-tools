//! MCP (Model Context Protocol) server for devboy-tools.
//!
//! This crate implements the MCP server that exposes devboy functionality
//! to AI assistants like Claude.
//!
//! # Architecture
//!
//! - **Protocol**: JSON-RPC 2.0 over stdin/stdout
//! - **Transport**: Newline-delimited JSON messages
//! - **Tools**: get_issues, get_merge_requests
//! - **Pipeline**: Output transformation (Markdown, truncation)
//!
//! # Example
//!
//! ```ignore
//! use devboy_mcp::McpServer;
//! use devboy_github::GitHubClient;
//!
//! let mut server = McpServer::new();
//! server.add_provider(Arc::new(github_client));
//! server.run().await?;
//! ```

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]

pub mod agent_safety;
pub mod handlers;
pub mod layered;
pub mod oauth_auth;
pub mod prefetch_adapter;
pub mod protocol;
pub mod proxy;
pub mod proxy_secrets;
pub mod routing;
pub mod secrets_provision;
pub mod secrets_tool;
pub mod server;
pub mod signature_match;
pub mod speculation;
pub mod telemetry;
pub mod tools;
pub mod transport;

pub use handlers::KNOWN_BUILTIN_TOOLS;
pub use protocol::{JSONRPC_VERSION, JsonRpcRequest, RequestId};
pub use proxy::{McpProxyClient, ProxyManager, ProxyTransport};
pub use routing::{
    IncompatibleTool, ProxyStatus, RoutingDecision, RoutingEngine, RoutingReason, RoutingTarget,
};
pub use server::{DeferredInit, McpServer};
pub use signature_match::{MatchReport, ToolCatalogue, ToolMatch, build_report};
pub use telemetry::{
    TelemetryAuth, TelemetryBatch, TelemetryBuffer, TelemetryEvent, TelemetryPipeline,
    TelemetryStatus, TelemetryUploader,
};

// Re-export executor types for consumers that use devboy-mcp as entry point
pub use devboy_executor::{
    AdditionalContext, Executor, GitHubScope, GitLabScope, PipelineFormatEnricher, ProviderConfig,
    SUPPORTED_TOOLS, ToolEnricher, ToolOutput, ToolSchema,
};
