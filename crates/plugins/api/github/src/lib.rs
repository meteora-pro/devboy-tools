#![warn(missing_docs)]
//! GitHub provider implementation for devboy-tools.
//!
//! This crate provides integration with GitHub API for issues,
//! pull requests, and other GitHub-specific functionality.

#[allow(missing_docs)]
mod client;
pub mod enricher;
#[allow(missing_docs)]
mod types;

pub use client::GitHubClient;
pub use enricher::GitHubSchemaEnricher;
pub use types::*;

/// Default GitHub API URL.
pub const DEFAULT_GITHUB_URL: &str = "https://api.github.com";
