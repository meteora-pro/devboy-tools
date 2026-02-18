//! Jira provider implementation for devboy-tools.
//!
//! This crate provides integration with Jira API for issues.
//! Supports both Jira Cloud (API v3) and Jira Self-Hosted/Data Center (API v2).
//! Jira does not have merge requests, so MR operations return
//! `ProviderUnsupported` errors.

mod client;
mod types;

pub use client::JiraClient;
pub use types::*;
