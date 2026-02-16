//! ClickUp provider implementation for devboy-tools.
//!
//! This crate provides integration with ClickUp API for issues (tasks).
//! ClickUp does not have merge requests, so MR operations return
//! `ProviderUnsupported` errors.

mod client;
mod types;

pub use client::ClickUpClient;
pub use types::*;

/// Default ClickUp API URL.
pub const DEFAULT_CLICKUP_URL: &str = "https://api.clickup.com/api/v2";
