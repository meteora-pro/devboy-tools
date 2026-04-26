//! Confluence self-hosted provider implementation for devboy-tools.
//!
//! This crate currently provides a reusable HTTP client foundation for
//! Confluence Server / Data Center integrations. Knowledge-base provider
//! methods are added separately on top of this client.

mod client;

pub use client::{ConfluenceAuth, ConfluenceClient};

/// Default REST API base path for Confluence Server / Data Center.
pub const DEFAULT_CONFLUENCE_API_PATH: &str = "/rest/api";
