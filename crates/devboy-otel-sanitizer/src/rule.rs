//! Rule definition and severity.

use serde::{Deserialize, Serialize};

use crate::strategy::Strategy;

/// Severity level of a rule. Fixed per-rule; surfaces in scan reports
/// (text, JSON, SARIF).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Highest priority — known secret formats (AWS keys, JWT, OAuth).
    /// CI gate should fail on findings of this severity.
    High,
    /// PII or quasi-secret data (emails, IPs, generic keys).
    Medium,
    /// Heuristic matches — high entropy strings, looks-like-credential.
    Low,
}

/// Single sanitization rule.
///
/// In TOML form (excerpt of `default_rules.toml`):
///
/// ```toml
/// [[rules]]
/// id = "aws_access_key"
/// description = "AWS access key id"
/// pattern = "AKIA[0-9A-Z]{16}"
/// severity = "high"
/// category = "cloud_credential"
/// applies_to = ["span_attribute", "log_body", "metric_attribute"]
///
/// [rules.strategy]
/// kind = "mask"
/// replacement = "[AWS_KEY_REDACTED]"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Stable rule identifier — used in findings, deny-list extensions.
    pub id: String,

    /// Human-readable rule description.
    pub description: String,

    /// Regex pattern. Compiled lazily by [`Sanitizer`].
    pub pattern: String,

    /// Severity tier.
    pub severity: Severity,

    /// Free-form category (e.g. "cloud_credential", "oauth_token", "pii").
    pub category: String,

    /// Where this rule applies. Empty = applies everywhere.
    #[serde(default)]
    pub applies_to: Vec<RuleScope>,

    /// If `true`, skip validity filters (dictionary / repeats / entropy)
    /// for this rule's matches. Use for rules whose pattern is itself
    /// the signature (PEM markers, SSH keys, Auth headers) where the
    /// matched substring is naturally low-entropy formatting text.
    #[serde(default)]
    pub skip_validity: bool,

    /// Redaction strategy when the rule matches.
    pub strategy: Strategy,
}

/// Where a rule applies in the OTLP tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope {
    /// Span name.
    SpanName,
    /// Span attribute value.
    SpanAttribute,
    /// Resource attribute value.
    ResourceAttribute,
    /// Log body (for OTLP logs / events).
    LogBody,
    /// Metric label value.
    MetricAttribute,
}
