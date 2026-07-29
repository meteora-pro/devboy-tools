//! Confluence provider implementation for devboy-tools.
//!
//! This crate provides the Confluence knowledge base provider
//! implementation, including API client logic plus page/space/search
//! operations exposed through the shared `KnowledgeBaseProvider` trait.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]
mod client;
mod enricher;
pub mod liveness;

pub use client::{ConfluenceAuth, ConfluenceClient, ConfluenceFlavor, ConfluenceOAuthTokens};
pub use enricher::ConfluenceSchemaEnricher;

/// Default REST API base path for Confluence Server / Data Center.
pub const DEFAULT_CONFLUENCE_API_PATH: &str = "/rest/api";
