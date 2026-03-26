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

pub mod handlers;
pub mod protocol;
pub mod proxy;
pub mod server;
pub mod tools;
pub mod transport;

pub use handlers::{ToolHandler, KNOWN_BUILTIN_TOOLS};
pub use protocol::{JsonRpcRequest, RequestId, JSONRPC_VERSION};
pub use proxy::{McpProxyClient, ProxyManager, ProxyTransport};
pub use server::McpServer;

// Re-export executor types for consumers that use devboy-mcp as entry point
pub use devboy_executor::{
    AdditionalContext, Executor, GitHubScope, GitLabScope, PipelineFormatEnricher, ProviderConfig,
    ToolEnricher, ToolOutput, ToolSchema, SUPPORTED_TOOLS,
};
