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

use std::fmt;
use std::io::{self, BufRead};

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

impl ScanReport {
    /// Incorporates a per-record report into this report.
    pub fn extend(&mut self, other: Self) {
        self.findings.extend(other.findings);
        self.summary.records += other.summary.records;
        self.summary.findings_total += other.summary.findings_total;
        self.summary.high += other.summary.high;
        self.summary.medium += other.summary.medium;
        self.summary.low += other.summary.low;
    }
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

    /// Redacts catalogue matches from a structured record in place.
    ///
    /// This is the streaming transform used by `redacted-jsonl`. It shares the
    /// scanner's catalogue, but deliberately returns no findings so callers
    /// cannot accidentally write raw candidate values to an output stream.
    pub fn redact_value(&self, value: &mut Value) {
        redact_value_at(&self.patterns, value);
    }
}

/// Error while reading a JSONL artifact.
///
/// Its display text deliberately omits input content, which could contain a
/// secret and may be printed by callers in CI logs.
#[derive(Debug)]
pub enum JsonlScanError {
    /// The input stream could not be read.
    Read(io::Error),
    /// A non-blank line did not contain a JSON value.
    InvalidJson {
        /// One-based line number in the source.
        line: u64,
    },
}

impl fmt::Display for JsonlScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(_) => f.write_str("failed to read JSONL input"),
            Self::InvalidJson { line } => write!(f, "invalid JSON on line {line}"),
        }
    }
}

impl std::error::Error for JsonlScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::InvalidJson { .. } => None,
        }
    }
}

/// Scans a JSONL input stream without loading it fully into memory.
///
/// Each non-blank line must be an independent JSON value. Finding locations
/// retain the physical source line number so they can be located directly.
pub fn scan_jsonl<R: BufRead>(
    scanner: &Scanner<'_>,
    source: impl Into<String>,
    reader: R,
) -> Result<ScanReport, JsonlScanError> {
    let source = source.into();
    let mut report = ScanReport::default();

    for (index, line) in reader.lines().enumerate() {
        let line_number = u64::try_from(index).expect("line index always fits u64") + 1;
        let line = line.map_err(JsonlScanError::Read)?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line)
            .map_err(|_| JsonlScanError::InvalidJson { line: line_number })?;
        let context = ScanContext {
            source: source.clone(),
            line: Some(line_number),
            record_id: record_id(&value),
        };
        report.extend(scanner.scan_value(&context, &value));
    }

    Ok(report)
}

fn record_id(value: &Value) -> Option<String> {
    ["span_id", "log_record_id", "record_id", "id"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("data")
                .and_then(|data| data.get("span_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
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

fn redact_value_at(patterns: &[&dyn SecretPattern], value: &mut Value) {
    match value {
        Value::String(text) => redact_text(patterns, text),
        Value::Array(values) => {
            for child in values {
                redact_value_at(patterns, child);
            }
        }
        Value::Object(values) => {
            for child in values.values_mut() {
                redact_value_at(patterns, child);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn redact_text(patterns: &[&dyn SecretPattern], text: &mut String) {
    for pattern in patterns {
        if pattern.format_regex().is_match(text) {
            *text = format!("[REDACTED:{}]", pattern.id());
            return;
        }
    }

    // Catalogue patterns are full-value validators. Replace the token-shaped
    // pieces in command strings while retaining surrounding context.
    let mut output = String::with_capacity(text.len());
    let mut token = String::new();
    for character in text.chars() {
        if character.is_whitespace()
            || matches!(
                character,
                ',' | ';' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
            )
        {
            redact_token(patterns, &mut output, &mut token);
            output.push(character);
        } else {
            token.push(character);
        }
    }
    redact_token(patterns, &mut output, &mut token);
    *text = output;
}

fn redact_token(patterns: &[&dyn SecretPattern], output: &mut String, token: &mut String) {
    let trimmed = token.trim_matches(|character: char| matches!(character, '=' | ':' | '`' | '.'));
    if let Some(pattern) = patterns
        .iter()
        .find(|pattern| pattern.format_regex().is_match(trimmed))
    {
        let prefix_length = token.find(trimmed).unwrap_or(0);
        output.push_str(&token[..prefix_length]);
        output.push_str(&format!("[REDACTED:{}]", pattern.id()));
        output.push_str(&token[prefix_length + trimmed.len()..]);
    } else {
        output.push_str(token);
    }
    token.clear();
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

    #[test]
    fn scans_jsonl_one_record_at_a_time_with_source_locations() {
        let token = "ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ";
        let input = format!(
            "\n{{\"span_id\":\"abc123\",\"body\":\"safe\"}}\n{{\"body\":\"Bearer {token}\"}}\n"
        );
        let report = scan_jsonl(&scanner(), "fixture.jsonl", std::io::Cursor::new(input))
            .expect("valid JSONL");

        assert_eq!(report.summary.records, 2);
        let finding = report
            .findings
            .iter()
            .find(|finding| finding.category == "github-pat")
            .expect("GitHub PAT finding");
        assert_eq!(finding.source, "fixture.jsonl");
        assert_eq!(finding.line, Some(3));
        assert!(!serde_json::to_string(&report).unwrap().contains(token));
    }

    #[test]
    fn malformed_jsonl_is_reported_without_echoing_the_input() {
        let malformed = "{ definitely-not-json and possibly-secret=ghp_should_not_appear }\n";
        let error = scan_jsonl(&scanner(), "fixture.jsonl", std::io::Cursor::new(malformed))
            .expect_err("must reject malformed JSON");
        assert_eq!(error.to_string(), "invalid JSON on line 1");
        assert!(!error.to_string().contains("possibly-secret"));
    }

    #[test]
    fn redacts_nested_and_embedded_tokens_without_changing_safe_fields() {
        let token = "ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ";
        let mut value = json!({
            "safe": "unchanged",
            "token": token,
            "command": format!("curl -H 'Authorization: Bearer {token}'"),
        });
        scanner().redact_value(&mut value);

        assert_eq!(value["safe"], "unchanged");
        assert_eq!(value["token"], "[REDACTED:github-pat]");
        assert!(!value.to_string().contains(token));
        assert!(
            value["command"]
                .as_str()
                .unwrap()
                .contains("[REDACTED:github-pat]")
        );
    }
}
