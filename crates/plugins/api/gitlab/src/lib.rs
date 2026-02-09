//! GitLab provider implementation for devboy-tools.
//!
//! This crate provides integration with GitLab API for issues,
//! merge requests, and other GitLab-specific functionality.

mod client;
pub mod types;

pub use client::GitLabClient;
pub use types::*;

/// Default GitLab API URL.
pub const DEFAULT_GITLAB_URL: &str = "https://gitlab.com";
