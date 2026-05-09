//! GitLab provider implementation for devboy-tools.
//!
//! This crate provides integration with GitLab API for issues,
//! merge requests, and other GitLab-specific functionality.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]
mod client;
pub mod enricher;
pub mod liveness;
pub mod types;

pub use client::GitLabClient;
pub use enricher::GitLabSchemaEnricher;
pub use types::*;

pub const DEFAULT_GITLAB_URL: &str = "https://gitlab.com";
