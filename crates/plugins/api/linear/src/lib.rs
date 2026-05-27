//! Linear provider implementation for devboy-tools.
//!
//! This crate provides a GraphQL-backed provider shell for Linear.
//! The initial slice wires configuration, provider construction,
//! authenticated user lookup, and liveness probing. Issue
//! operations are added in follow-up changes.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]

mod client;
pub mod enricher;
pub mod liveness;
mod types;

pub use client::LinearClient;
pub use enricher::LinearSchemaEnricher;

pub const DEFAULT_LINEAR_URL: &str = "https://api.linear.app/graphql";
