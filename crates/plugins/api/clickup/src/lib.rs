#![warn(missing_docs)]
//! ClickUp provider implementation for devboy-tools.
//!
//! This crate provides integration with ClickUp API for issues (tasks).
//! ClickUp does not have merge requests, so MR operations return
//! `ProviderUnsupported` errors.

#[allow(missing_docs)]
mod client;
pub mod enricher;
#[allow(missing_docs)]
pub mod metadata;
#[allow(missing_docs)]
mod types;

pub use client::ClickUpClient;
pub use enricher::ClickUpSchemaEnricher;
pub use metadata::ClickUpMetadata;
pub use types::*;

/// Default ClickUp API URL.
pub const DEFAULT_CLICKUP_URL: &str = "https://api.clickup.com/api/v2";
