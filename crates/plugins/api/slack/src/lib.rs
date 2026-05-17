//! Slack provider implementation for devboy-tools.
//!
//! This crate provides Slack bot-token connectivity and messenger provider
//! foundations. Messenger tool methods are scaffolded here and implemented
//! separately from connection/auth setup.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]
mod client;
pub mod liveness;

pub use client::{SlackAuthInfo, SlackClient};

/// Default Slack Web API base URL.
pub const DEFAULT_SLACK_API_URL: &str = "https://slack.com/api";
