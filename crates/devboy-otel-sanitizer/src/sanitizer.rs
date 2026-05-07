//! Sanitizer engine — applies a compiled rule set to OTLP attribute values.

use crate::error::SanitizerError;
use crate::rule::{Rule, Severity};

/// Main entry point. Holds a compiled rule set; cheap to clone (rules
/// are stored in an `Arc` once implementation lands).
#[derive(Debug, Clone, Default)]
pub struct Sanitizer {
    rules: Vec<Rule>,
}

impl Sanitizer {
    /// Build a sanitizer from a list of rules. Compiles regexes eagerly
    /// so subsequent calls are zero-allocation hot path.
    ///
    /// Returns [`SanitizerError::RegexCompile`] if any pattern is invalid.
    pub fn new(rules: Vec<Rule>) -> Result<Self, SanitizerError> {
        // TODO(#240): compile rules into RegexSet (Teddy SIMD prefilter)
        // and per-rule individual `Regex` (for capture extraction).
        Ok(Self { rules })
    }

    /// Sanitize a single string in place.
    ///
    /// Returns `Some(redacted)` if any rule matched, `None` if the input
    /// passed through unchanged.
    pub fn sanitize_string(&self, _input: &str) -> Option<String> {
        // TODO(#240): RegexSet match → per-match strategy application.
        None
    }

    /// Run a non-mutating scan over the input. Returns all findings.
    /// Used by the `devboy otel scan` auditor in dry-run mode.
    pub fn scan(&self, _input: &str) -> Vec<Finding> {
        // TODO(#240): full match enumeration for audit reports.
        Vec::new()
    }

    /// Return the number of compiled rules.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

/// One match found during scan / sanitize.
#[derive(Debug, Clone)]
pub struct Finding {
    /// Rule id that matched.
    pub rule_id: String,
    /// Severity of the matched rule.
    pub severity: Severity,
    /// Byte offset (start) in the original input.
    pub start: usize,
    /// Byte offset (end-exclusive) in the original input.
    pub end: usize,
    /// Redacted replacement that was (or would be) substituted.
    pub redacted: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sanitizer_passes_through() {
        let s = Sanitizer::default();
        assert_eq!(s.rule_count(), 0);
        assert_eq!(s.sanitize_string("hello world"), None);
        assert!(s.scan("hello").is_empty());
    }
}
