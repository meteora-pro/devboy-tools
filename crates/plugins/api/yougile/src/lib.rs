//! YouGile provider implementation for devboy-tools.
//!
//! This crate provides integration with the YouGile REST API for issues/tasks.
//! It includes issue operations, comment support, board-column status
//! mapping, and schema enrichment for YouGile-specific issue workflows.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]

mod client;
pub mod enricher;
pub mod liveness;
pub mod metadata;
mod types;

pub use client::YouGileClient;
pub use enricher::YouGileSchemaEnricher;
pub use metadata::YouGileMetadata;
pub use types::*;

pub const DEFAULT_YOUGILE_URL: &str = "https://yougile.com/api-v2";
