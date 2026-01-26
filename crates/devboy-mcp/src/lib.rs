//! MCP (Model Context Protocol) server for devboy-tools.
//!
//! This crate implements the MCP server that exposes devboy functionality
//! to AI assistants like Claude.

pub mod server;
pub mod tools;

pub use server::McpServer;
