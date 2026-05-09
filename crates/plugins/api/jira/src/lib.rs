//! Jira provider implementation for devboy-tools.
//!
//! This crate provides integration with Jira API for issues.
//! Supports both Jira Cloud (API v3) and Jira Self-Hosted/Data Center (API v2).
//! Jira does not have merge requests, so MR operations return
//! `ProviderUnsupported` errors.

mod client;
pub mod enricher;
pub mod liveness;
pub mod metadata;
mod types;

pub use client::{JiraClient, JiraFlavor};
pub use enricher::JiraSchemaEnricher;
pub use metadata::JiraMetadata;
pub use types::*;
