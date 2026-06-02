//! ClickUp provider implementation for devboy-tools.
//!
//! This crate provides integration with ClickUp API for issues (tasks).
//! ClickUp does not have merge requests, so MR operations return
//! `ProviderUnsupported` errors.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]
mod client;
mod comment_format;
pub mod enricher;
pub mod liveness;
pub mod metadata;
mod types;

pub use client::ClickUpClient;
pub use enricher::ClickUpSchemaEnricher;
pub use metadata::ClickUpMetadata;
pub use types::*;

pub const DEFAULT_CLICKUP_URL: &str = "https://api.clickup.com/api/v2";
