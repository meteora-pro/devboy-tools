#![warn(missing_docs)]
//! Confluence self-hosted provider implementation for devboy-tools.
//!
//! This crate provides the Confluence Server / Data Center knowledge base
//! provider implementation, including API client logic plus page/space/search
//! operations exposed through the shared `KnowledgeBaseProvider` trait.

mod client;
mod enricher;

pub use client::{ConfluenceAuth, ConfluenceClient};
pub use enricher::ConfluenceSchemaEnricher;

/// Default REST API base path for Confluence Server / Data Center.
pub const DEFAULT_CONFLUENCE_API_PATH: &str = "/rest/api";
