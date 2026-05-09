// TODO: fix legacy doc links (#250 follow-up):
//   - intent_keywords→intent_boost (private item)
//   - LayeredPipeline / decide (unresolved)
#![allow(rustdoc::private_intra_doc_links)]
#![allow(rustdoc::broken_intra_doc_links)]
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

#![warn(missing_docs)]

pub mod handlers;
pub mod layered;
#[allow(missing_docs)]
pub mod prefetch_adapter;
#[allow(missing_docs)]
pub mod protocol;
#[allow(missing_docs)]
pub mod proxy;
#[allow(missing_docs)]
pub mod routing;
#[allow(missing_docs)]
pub mod server;
#[allow(missing_docs)]
pub mod signature_match;
#[allow(missing_docs)]
pub mod speculation;
#[allow(missing_docs)]
pub mod telemetry;
#[allow(missing_docs)]
pub mod tools;
#[allow(missing_docs)]
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
