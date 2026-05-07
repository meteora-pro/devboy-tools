//! Redaction strategies.
//!
//! Eight strategies cover the practical redaction space for OTLP data.
//! Strategy choice is per-rule and is fixed in TOML.

use serde::{Deserialize, Serialize};

/// Redaction strategy applied when a rule matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Strategy {
    /// Keep the value as-is (used for explicit allowlist rules).
    Allow,

    /// Drop the entire span / attribute / field.
    Drop,

    /// Replace the matched value with `sha256(value)` truncated to 16 hex chars.
    Hash,

    /// Replace the matched value with a fixed mask (default `[REDACTED]`).
    Mask {
        /// Replacement string (default `[REDACTED]` if `None`).
        replacement: Option<String>,
    },

    /// Replace only the captured groups of the matched regex with a mask,
    /// leaving the surrounding text intact.
    RegexRedact {
        /// Replacement string for matched groups.
        replacement: Option<String>,
    },

    /// Truncate the value to N characters, appending an ellipsis marker.
    Truncate {
        /// Maximum length after truncation.
        max_chars: usize,
    },

    /// Replace absolute filesystem paths with workspace-relative ones.
    /// E.g. `/Users/alice/projects/foo/bar.rs` → `<workspace>/foo/bar.rs`.
    ScopeToWorkspace,

    /// Drop only if Shannon entropy of the matched value exceeds the
    /// threshold. Useful as a secondary check for high-entropy strings
    /// that don't match a specific known pattern.
    EntropyFilter {
        /// Minimum entropy (bits per char) to trigger redaction.
        min_entropy: f64,
    },
}
