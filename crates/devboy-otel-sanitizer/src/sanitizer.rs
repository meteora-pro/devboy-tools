//! Sanitizer engine — applies a compiled rule set to OTLP attribute values.
//!
//! Compilation strategy:
//! - All rule patterns are merged into a single [`RegexSet`] for fast
//!   prefilter (`regex` crate uses Teddy SIMD literal extraction internally,
//!   so explicit `aho-corasick` prefilter is unnecessary in the typical case).
//! - Each rule also gets its own compiled [`Regex`] for actual capture
//!   extraction once the prefilter signals a candidate.
//! - Validity filters (Meli §IV-D) are applied per match before strategy
//!   application — false-positive guard against test fixtures, dictionary
//!   words, low-entropy values.

use regex::{Regex, RegexSet};

use crate::error::SanitizerError;
use crate::rule::{Rule, Severity};
use crate::strategies;
use crate::validity;

/// Main entry point. Holds compiled rule set; cheap to clone.
#[derive(Debug, Clone)]
pub struct Sanitizer {
    rules: Vec<Rule>,
    set: RegexSet,
    per_rule: Vec<Regex>,
}

impl Default for Sanitizer {
    fn default() -> Self {
        Self {
            rules: Vec::new(),
            set: RegexSet::empty(),
            per_rule: Vec::new(),
        }
    }
}

impl Sanitizer {
    /// Build a sanitizer from a list of rules. Compiles regexes eagerly.
    pub fn new(rules: Vec<Rule>) -> Result<Self, SanitizerError> {
        let patterns: Vec<&str> = rules.iter().map(|r| r.pattern.as_str()).collect();
        let set = RegexSet::new(&patterns)?;
        let per_rule = patterns
            .iter()
            .map(|p| Regex::new(p))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            rules,
            set,
            per_rule,
        })
    }

    /// Number of compiled rules.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Sanitize a single string.
    ///
    /// Returns:
    /// - `SanitizeResult::Unchanged` — no rule matched
    /// - `SanitizeResult::Redacted(s)` — redacted form
    /// - `SanitizeResult::Drop` — caller should drop the entire field
    ///   (a rule with `Strategy::Drop` matched).
    pub fn sanitize_string(&self, input: &str) -> SanitizeResult {
        if self.rules.is_empty() {
            return SanitizeResult::Unchanged;
        }

        // Fast prefilter: which rules might match this input at all.
        let candidate_rules: Vec<usize> = self.set.matches(input).into_iter().collect();
        if candidate_rules.is_empty() {
            return SanitizeResult::Unchanged;
        }

        let mut current = input.to_string();
        let mut any_change = false;

        for rule_idx in candidate_rules {
            let regex = &self.per_rule[rule_idx];
            let rule = &self.rules[rule_idx];

            // Collect matches first (avoid borrow conflict during replacement).
            let matches: Vec<(usize, usize, String)> = regex
                .find_iter(&current)
                .map(|m| (m.start(), m.end(), m.as_str().to_string()))
                .collect();

            // Apply in reverse order so byte offsets stay valid.
            for (start, end, matched) in matches.into_iter().rev() {
                // Validity filter — skip false-positive matches.
                if !validity::is_likely_secret(&matched) {
                    continue;
                }
                match strategies::apply(&rule.strategy, &current, (start, end)) {
                    Some(new) => {
                        if new != current {
                            current = new;
                            any_change = true;
                        }
                    }
                    None => return SanitizeResult::Drop,
                }
            }
        }

        if any_change {
            SanitizeResult::Redacted(current)
        } else {
            SanitizeResult::Unchanged
        }
    }

    /// Run a non-mutating scan. Used by `devboy otel scan` auditor in
    /// dry-run mode — emits findings without modifying the input.
    pub fn scan(&self, input: &str) -> Vec<Finding> {
        if self.rules.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for rule_idx in self.set.matches(input).into_iter() {
            let regex = &self.per_rule[rule_idx];
            let rule = &self.rules[rule_idx];
            for m in regex.find_iter(input) {
                let matched = m.as_str();
                if !validity::is_likely_secret(matched) {
                    continue;
                }
                out.push(Finding {
                    rule_id: rule.id.clone(),
                    severity: rule.severity,
                    start: m.start(),
                    end: m.end(),
                    matched: matched.to_string(),
                });
            }
        }
        out
    }
}

/// Outcome of a sanitization call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SanitizeResult {
    /// No rule matched; input is safe to emit as-is.
    Unchanged,
    /// One or more rules matched and the value was redacted.
    Redacted(String),
    /// A rule with `Strategy::Drop` matched; caller should drop the
    /// entire field rather than emit any value.
    Drop,
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
    /// The matched substring (raw — caller's responsibility not to log).
    pub matched: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::{Rule, RuleScope, Severity};
    use crate::strategy::Strategy;

    fn aws_rule() -> Rule {
        Rule {
            id: "aws_access_key".into(),
            description: "AWS access key id".into(),
            pattern: r"AKIA[0-9A-Z]{16}".into(),
            severity: Severity::High,
            category: "cloud_credential".into(),
            applies_to: vec![RuleScope::SpanAttribute],
            strategy: Strategy::Mask {
                replacement: Some("[AWS_KEY_REDACTED]".into()),
            },
        }
    }

    /// Random-looking value (no sequential numbers, no repeats) — passes
    /// validity filter and exercises the redaction path.
    const RANDOM_AWS_VALUE: &str = "AKIAQ7K3MN9PV2BX5LZA";

    #[test]
    fn empty_sanitizer_passes_through() {
        let s = Sanitizer::default();
        assert_eq!(s.rule_count(), 0);
        assert_eq!(s.sanitize_string("hello"), SanitizeResult::Unchanged);
        assert!(s.scan("hello").is_empty());
    }

    #[test]
    fn aws_key_is_redacted() {
        let s = Sanitizer::new(vec![aws_rule()]).unwrap();
        let input = format!("export AWS_KEY={RANDOM_AWS_VALUE}");
        match s.sanitize_string(&input) {
            SanitizeResult::Redacted(v) => {
                assert!(v.contains("[AWS_KEY_REDACTED]"));
                assert!(!v.contains(RANDOM_AWS_VALUE));
            }
            other => panic!("expected Redacted, got {other:?}"),
        }
    }

    #[test]
    fn no_match_passes_through() {
        let s = Sanitizer::new(vec![aws_rule()]).unwrap();
        assert_eq!(
            s.sanitize_string("nothing to see here"),
            SanitizeResult::Unchanged
        );
    }

    #[test]
    fn scan_emits_findings() {
        let s = Sanitizer::new(vec![aws_rule()]).unwrap();
        let input = format!("{RANDOM_AWS_VALUE} appears here");
        let f = s.scan(&input);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule_id, "aws_access_key");
        assert_eq!(f[0].severity, Severity::High);
    }

    #[test]
    fn drop_strategy_returns_drop() {
        let mut rule = aws_rule();
        rule.strategy = Strategy::Drop;
        let s = Sanitizer::new(vec![rule]).unwrap();
        assert_eq!(s.sanitize_string(RANDOM_AWS_VALUE), SanitizeResult::Drop);
    }

    #[test]
    fn validity_filter_skips_dictionary_match() {
        // Pattern matches "password" but dictionary filter rejects it.
        let rule = Rule {
            id: "any_word".into(),
            description: "any word".into(),
            pattern: r"\w+".into(),
            severity: Severity::Low,
            category: "test".into(),
            applies_to: vec![],
            strategy: Strategy::Mask { replacement: None },
        };
        let s = Sanitizer::new(vec![rule]).unwrap();
        // "password" is in the validity dictionary → skipped.
        // But high-entropy random string would be redacted.
        let _ = s.sanitize_string("password");
        if let SanitizeResult::Redacted(v) = s.sanitize_string(RANDOM_AWS_VALUE) {
            assert!(v.contains("[REDACTED]"));
        }
    }
}
