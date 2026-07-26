//! Linear provider implementation for devboy-tools.
//!
//! This crate provides a GraphQL-backed Linear provider with
//! configuration, provider construction, authenticated user lookup,
//! liveness probing, issue operations, comments, and status discovery.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]

mod client;
pub mod enricher;
pub mod liveness;
pub mod metadata;
mod types;

pub use client::LinearClient;
pub use enricher::LinearSchemaEnricher;
pub use metadata::LinearMetadata;

pub const DEFAULT_LINEAR_URL: &str = "https://api.linear.app/graphql";
