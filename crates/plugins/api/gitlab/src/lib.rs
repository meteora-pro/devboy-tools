#![warn(missing_docs)]
//! GitLab provider implementation for devboy-tools.
//!
//! This crate provides integration with GitLab API for issues,
//! merge requests, and other GitLab-specific functionality.

#[allow(missing_docs)]
mod client;
pub mod enricher;
#[allow(missing_docs)]
pub mod types;

pub use client::GitLabClient;
pub use enricher::GitLabSchemaEnricher;
pub use types::*;

/// Default GitLab API URL.
pub const DEFAULT_GITLAB_URL: &str = "https://gitlab.com";
