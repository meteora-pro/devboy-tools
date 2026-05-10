//! Jira provider implementation for devboy-tools.
//!
//! This crate provides integration with Jira API for issues.
//! Supports both Jira Cloud (API v3) and Jira Self-Hosted/Data Center (API v2).
//! Jira does not have merge requests, so MR operations return
//! `ProviderUnsupported` errors.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]
mod client;
pub mod enricher;
pub mod metadata;
mod types;

pub use client::{JiraClient, JiraFlavor};
pub use enricher::JiraSchemaEnricher;
pub use metadata::JiraMetadata;
// Curated re-export. Only types meant for downstream consumers are
// surfaced — wire DTOs (JiraIssue, JiraIssueFields, CreateIssuePayload,
// JiraSearchResponse, the Structure plugin payloads, etc.) stay
// internal so the public API doesn't lock us into the raw Jira REST
// shape across SemVer bumps. If you need a wire DTO out of this
// crate, file an issue.
pub use types::{JiraField, JiraFieldSchema};
