//! Fireflies.ai meeting notes provider for devboy-tools.
//!
//! Integrates with the Fireflies.ai GraphQL API to provide meeting
//! transcripts, summaries, and search capabilities via MCP tools.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]
mod client;
mod enricher;
pub mod liveness;
mod types;

pub use client::FirefliesClient;
pub use enricher::FirefliesSchemaEnricher;
