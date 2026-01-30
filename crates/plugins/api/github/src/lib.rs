//! GitHub provider implementation for devboy-tools.
//!
//! This crate provides integration with GitHub API for issues,
//! pull requests, and other GitHub-specific functionality.

mod client;
mod types;

pub use client::GitHubClient;
pub use types::*;

/// Default GitHub API URL.
pub const DEFAULT_GITHUB_URL: &str = "https://api.github.com";
