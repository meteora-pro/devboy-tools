//! GitHub provider implementation for devboy-tools.
//!
//! This crate provides integration with GitHub API for issues,
//! pull requests, and other GitHub-specific functionality.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]
mod client;
pub mod enricher;
mod types;

pub use client::GitHubClient;
pub use enricher::GitHubSchemaEnricher;
pub use types::*;

/// Default GitHub API URL.
pub const DEFAULT_GITHUB_URL: &str = "https://api.github.com";
