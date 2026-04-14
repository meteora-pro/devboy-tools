//! Slack provider implementation for devboy-tools.
//!
//! This crate provides Slack bot-token connectivity and messenger provider
//! foundations. Messenger tool methods are scaffolded here and implemented
//! separately from connection/auth setup.

mod client;

pub use client::{SlackAuthInfo, SlackClient};

/// Default Slack Web API base URL.
pub const DEFAULT_SLACK_API_URL: &str = "https://slack.com/api";
