//! # devboy-otel-sanitizer
//!
//! Generic, vendor-neutral OTLP sanitizer library.
//!
//! Designed for use as a pluggable middleware in any OTLP pipeline
//! (forward proxy, server-side ingestion, scan auditor). Works with
//! any OTLP backend (Honeycomb, Jaeger, Datadog, Langfuse, self-hosted
//! OpenTelemetry Collector — anything that speaks OTLP HTTP/protobuf
//! or HTTP/JSON).
//!
//! ## Public API surface (scaffold)
//!
//! - [`Sanitizer`] — main entry point. Holds compiled rule set, applies
//!   redaction to OTLP attribute trees.
//! - [`Rule`] — single rule definition (regex pattern + redaction strategy).
//! - [`Strategy`] — eight redaction strategies (drop/hash/mask/regex_redact/
//!   truncate/scope_to_workspace/entropy_filter/allow).
//! - [`Severity`] — fixed per-rule severity (HIGH/MEDIUM/LOW).
//! - [`Finding`] — describes a single match (rule, location, redacted value).
//! - [`load_default_rules`] — bundled rule set (~150 rules from gitleaks
//!   defaults + Meli NDSS 2019 patterns + agent-OTEL-specific rules).
//!
//! ## Status
//!
//! Scaffold — public API shape only. Rule engine, validity filters, and
//! default rule set are subsequent commits within issue #240.
//!
//! Full design rationale: see `docs/architecture/sanitizer-research.md`
//! in the repository root.

#![deny(missing_docs)]

mod error;
mod rule;
mod sanitizer;
mod strategy;

pub use error::SanitizerError;
pub use rule::{Rule, Severity};
pub use sanitizer::{Finding, Sanitizer};
pub use strategy::Strategy;

/// Load the bundled default rule set.
///
/// Includes ~150 rules covering common secret formats (AWS keys, OAuth
/// tokens, JWTs, private keys, database connection strings) plus
/// agent-OTEL-specific rules (Anthropic API keys, OpenAI API keys,
/// MCP tokens).
///
/// Implementation deferred — placeholder returns empty set.
pub fn load_default_rules() -> Result<Vec<Rule>, SanitizerError> {
    // TODO(#240): bundle `default_rules.toml` via `include_str!` and parse
    Ok(Vec::new())
}
