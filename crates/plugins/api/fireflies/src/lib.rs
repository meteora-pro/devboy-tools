#![warn(missing_docs)]
//! Fireflies.ai meeting notes provider for devboy-tools.
//!
//! Integrates with the Fireflies.ai GraphQL API to provide meeting
//! transcripts, summaries, and search capabilities via MCP tools.

#[allow(missing_docs)]
mod client;
mod enricher;
mod types;

pub use client::FirefliesClient;
pub use enricher::FirefliesSchemaEnricher;
