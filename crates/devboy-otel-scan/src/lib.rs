//! Non-mutating secret detection for structured OpenTelemetry artifacts.
//!
//! This crate deliberately owns no input/output code: SQLite, JSONL, stdin,
//! and OTLP adapters turn their data into [`serde_json::Value`] and retain
//! source metadata in [`ScanContext`]. That keeps the matching behaviour
//! identical for every transport and lets the CLI stream one record at a time.
//!
//! The matching catalogue is supplied by `devboy-secret-patterns`, the shared
//! source of truth for the OTLP sanitizer (#240) and this auditor (#242).

#![forbid(unsafe_code)]

use devboy_secret_patterns::{Catalogue, SecretPattern, Severity};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Identifies the source record currently being inspected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanContext {
    /// Path or logical name of the source artifact.
    pub source: String,
    /// One-based record or line number when the adapter can provide it.
    pub line: Option<u64>,
    /// Stable span, log-record, or input-record identifier when available.
    pub record_id: Option<String>,
}

/// A single secret-like value found in an artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Severity declared by the matched secret pattern.
    pub severity: Severity,
    /// Stable secret-pattern identifier, such as `github-pat`.
    pub category: String,
    /// Human-readable name of the matched pattern.
    pub display_name: String,
    /// Source artifact supplied by the adapter.
    pub source: String,
    /// One-based line or record number, when known.
    pub line: Option<u64>,
    /// Span, log-record, or input-record identifier, when known.
    pub record_id: Option<String>,
    /// JSON-style path to the matching scalar.
    pub attribute_path: String,
    /// Safe preview of the match. This never contains more than four original
    /// characters and must be used instead of the raw candidate in reporting.
    pub match_redacted: String,
    /// Sanitizer strategy appropriate for this category.
    pub suggested_strategy: SuggestedStrategy,
}

/// Recommended handling for a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedStrategy {
    /// Replace matching portions while preserving surrounding text.
    RegexRedact,
    /// Hash the value to retain correlation without retaining the value.
    Hash,
    /// Flag an uncertain, low-severity value for human review.
    Review,
}

/// Aggregate counters returned with each scan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSummary {
    /// Number of top-level records scanned by the caller.
    pub records: u64,
    /// Total findings emitted.
    pub findings_total: u64,
    /// Findings grouped by severity.
    pub high: u64,
    /// Findings grouped by severity.
    pub medium: u64,
    /// Findings grouped by severity.
    pub low: u64,
}

/// The result of scanning one or more records.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanReport {
    /// Findings in deterministic depth-first JSON traversal order.
    pub findings: Vec<Finding>,
    /// Aggregate counters for the scan.
    pub summary: ScanSummary,
}

/// Stateless, non-mutating scanner over a shared secret-pattern catalogue.
pub struct Scanner<'a> {
    patterns: Vec<&'a dyn SecretPattern>,
}

impl<'a> Scanner<'a> {
    /// Creates a scanner from a catalogue, including user-supplied patterns.
    pub fn new(catalogue: &'a Catalogue) -> Self {
        Self {
            patterns: catalogue.iter(),
        }
    }

    /// Scans one structured record without mutating it.
    pub fn scan_value(&self, context: &ScanContext, value: &Value) -> ScanReport {
        let mut report = ScanReport::default();
        scan_value_at(&self.patterns, context, value, "$", &mut report);
        report.summary.records = 1;
        report
    }
}

fn scan_value_at(
    patterns: &[&dyn SecretPattern],
    context: &ScanContext,
    value: &Value,
    path: &str,
    report: &mut ScanReport,
) {
    match value {
        Value::String(text) => scan_text(patterns, context, path, text, report),
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                scan_value_at(
                    patterns,
                    context,
                    child,
                    &format!("{path}[{index}]"),
                    report,
                );
            }
        }
        Value::Object(values) => {
            for (key, child) in values {
                scan_value_at(
                    patterns,
                    context,
                    child,
                    &format!("{path}.{}", escape_key(key)),
                    report,
                );
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn scan_text(
    patterns: &[&dyn SecretPattern],
    context: &ScanContext,
    path: &str,
    text: &str,
    report: &mut ScanReport,
) {
    // Catalogue regexes validate a full secret. Feed the complete scalar first
    // (private keys and URLs), then shell/JSON-shaped tokens within it.
    let mut candidates = vec![text];
    candidates.extend(text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
            )
    }));

    for candidate in candidates {
        let candidate = candidate.trim_matches(|c: char| matches!(c, '=' | ':' | '`' | '.'));
        if candidate.is_empty() {
            continue;
        }
        for pattern in patterns {
            if pattern.format_regex().is_match(candidate) {
                push_finding(report, context, path, candidate, *pattern);
            }
        }
    }
}

fn push_finding(
    report: &mut ScanReport,
    context: &ScanContext,
    attribute_path: &str,
    candidate: &str,
    pattern: &dyn SecretPattern,
) {
    let severity = pattern.severity();
    let suggested_strategy = match severity {
        Severity::High => SuggestedStrategy::RegexRedact,
        Severity::Medium => SuggestedStrategy::Hash,
        Severity::Low => SuggestedStrategy::Review,
    };
    report.findings.push(Finding {
        severity,
        category: pattern.id().to_owned(),
        display_name: pattern.display_name().to_owned(),
        source: context.source.clone(),
        line: context.line,
        record_id: context.record_id.clone(),
        attribute_path: attribute_path.to_owned(),
        match_redacted: redact_preview(candidate),
        suggested_strategy,
    });
    report.summary.findings_total += 1;
    match severity {
        Severity::High => report.summary.high += 1,
        Severity::Medium => report.summary.medium += 1,
        Severity::Low => report.summary.low += 1,
    }
}

fn escape_key(key: &str) -> String {
    key.replace('~', "~0").replace('.', "~1")
}

fn redact_preview(value: &str) -> String {
    let prefix: String = value.chars().take(4).collect();
    format!("{prefix}***")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scanner() -> Scanner<'static> {
        let catalogue = Box::leak(Box::new(Catalogue::builtins_only()));
        Scanner::new(catalogue)
    }

    #[test]
    fn reports_a_nested_token_without_leaking_its_value() {
        let token = "ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ";
        let report = scanner().scan_value(
            &ScanContext {
                source: "fixture.jsonl".into(),
                line: Some(42),
                record_id: Some("span-1".into()),
            },
            &json!({"tool_input": {"command": format!("curl -H 'Authorization: Bearer {token}'")}}),
        );

        assert!(report.findings.iter().any(|f| f.category == "github-pat"));
        assert_eq!(report.findings[0].attribute_path, "$.tool_input.command");
        assert_eq!(report.summary.high, 1);
        assert!(!serde_json::to_string(&report).unwrap().contains(token));
    }

    #[test]
    fn scans_a_url_as_a_whole_scalar() {
        let value = "postgres://user:p4ssw0rd@db.example.test:5432/appdb";
        let report = scanner().scan_value(&ScanContext::default(), &json!({"db.url": value}));
        assert!(report.findings.iter().any(|f| f.category == "postgres-url"));
    }
}
