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
//! ## Public API
//!
//! - [`Sanitizer`] — main entry point. Holds compiled rule set, applies
//!   redaction to OTLP attribute values.
//! - [`Rule`] — single rule definition (regex pattern + redaction strategy).
//! - [`Strategy`] — eight redaction strategies (drop/hash/mask/regex_redact/
//!   truncate/scope_to_workspace/entropy_filter/allow).
//! - [`Severity`] — fixed per-rule severity (HIGH/MEDIUM/LOW).
//! - [`Finding`] — describes a single match (rule, location, matched value).
//! - [`SanitizeResult`] — outcome of a single sanitization call.
//! - [`load_default_rules`] — bundled rule set (placeholder; default
//!   `default_rules.toml` lands in the next commit within issue #240).
//!
//! ## Quick start
//!
//! ```no_run
//! use devboy_otel_sanitizer::{Sanitizer, SanitizeResult, load_default_rules};
//!
//! let rules = load_default_rules().expect("rules");
//! let sanitizer = Sanitizer::new(rules).expect("compile");
//!
//! match sanitizer.sanitize_string("export AWS_KEY=AKIA1234567890ABCDEF") {
//!     SanitizeResult::Redacted(s) => println!("{s}"),
//!     SanitizeResult::Drop => { /* drop the whole field */ }
//!     SanitizeResult::Unchanged => println!("clean"),
//! }
//! ```
//!
//! ## Design
//!
//! Full design rationale: see `docs/architecture/sanitizer-research.md`
//! in the repository root.

#![deny(missing_docs)]

mod error;
mod rule;
mod sanitizer;
mod strategies;
mod strategy;
pub mod validity;

pub use error::SanitizerError;
pub use rule::{Rule, RuleScope, Severity};
pub use sanitizer::{Finding, SanitizeResult, Sanitizer};
pub use strategy::Strategy;

/// Load the bundled default rule set.
///
/// **Status**: scaffold — returns empty until `default_rules.toml` lands
/// in the next commit within issue #240. The bundled set will cover
/// gitleaks defaults + Meli NDSS 2019 patterns + agent-OTEL-specific
/// rules (~150 total).
pub fn load_default_rules() -> Result<Vec<Rule>, SanitizerError> {
    Ok(Vec::new())
}
