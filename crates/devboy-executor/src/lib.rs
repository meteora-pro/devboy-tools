//! # devboy-executor
//!
//! Tool execution engine for devboy-tools.
//!
//! Separates tool execution logic from transport (MCP stdio, HTTP, NAPI).
//! Provides:
//! - `Executor` — dispatches tool calls to providers with enrichment pipeline
//! - `AdditionalContext` / `ProviderConfig` — typed runtime context
//! - `ToolOutput` — typed results from tool execution
//! - `ToolEnricher` — plugin trait for dynamic schema modification
//! - `factory` — creates providers from `ProviderConfig`
//!
//! ## Usage
//!
//! ```rust,no_run
//! use devboy_executor::{Executor, AdditionalContext, ProviderConfig, GitLabScope};
//! use devboy_executor::enricher::FormatPipelineEnricher;
//! use std::collections::HashMap;
//!
//! # async fn example() -> devboy_core::Result<()> {
//! let mut executor = Executor::new();
//! executor.add_enricher(Box::new(FormatPipelineEnricher));
//!
//! let ctx = AdditionalContext {
//!     provider: ProviderConfig::GitLab {
//!         base_url: "https://gitlab.com".into(),
//!         access_token: "glpat-xxx".into(),
//!         scope: GitLabScope::Project { id: "12345".into() },
//!         extra: HashMap::new(),
//!     },
//!     proxy: None,
//!     metadata: None,
//!     extra: HashMap::new(),
//! };
//!
//! let args = serde_json::json!({ "state": "opened", "limit": 10 });
//! let output = executor.execute("get_merge_requests", args, &ctx).await?;
//! println!("Got {} items", output.item_count());
//! # Ok(())
//! # }
//! ```

pub mod argv_secrets;
pub mod context;
pub mod enricher;
pub mod executor;
pub mod factory;
pub mod format;
pub mod output;
pub mod tool_docs;
pub mod tools;

// Re-export main types at crate root
pub use context::{
    AdditionalContext, ClickUpScope, ConfluenceAuthConfig, ConfluenceScope, GitHubScope,
    GitLabScope, JiraScope, ProviderConfig, ProviderMetadata, ProxyConfig,
};
pub use devboy_core::{ToolEnricher, ToolSchema, sanitize_field_name};
pub use enricher::PipelineFormatEnricher;
pub use executor::{Executor, SUPPORTED_TOOLS};
pub use factory::{
    create_enricher, create_knowledge_base_enricher, create_knowledge_base_provider,
};
pub use format::{FormatMetadata, FormatResult, execute_and_format, format_output};
pub use output::{ResultMeta, ToolOutput};
pub use tools::{McpOnlyTool, ToolDefinition};
