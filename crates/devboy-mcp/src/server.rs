//! MCP server implementation.

use devboy_core::Provider;
use std::sync::Arc;

/// MCP server for devboy-tools.
pub struct McpServer {
    providers: Vec<Arc<dyn Provider>>,
}

impl McpServer {
    /// Create a new MCP server.
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
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

    /// Run the MCP server.
    pub async fn run(&self) -> devboy_core::Result<()> {
        // TODO: Implement MCP protocol handling
        tracing::info!("MCP server started with {} providers", self.providers.len());
        Ok(())
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}
