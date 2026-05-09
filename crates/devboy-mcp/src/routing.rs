//! Routing engine — decides which executor should handle a tool call.
//!
//! The engine combines three inputs:
//!
//! - Global + per-server [`ProxyRoutingConfig`] (strategy, fallback, per-tool overrides)
//! - A [`MatchReport`] describing which tools have both a local and a remote implementation
//! - The raw tool name requested by the client
//!
//! Out of these it produces a [`RoutingDecision`]: a primary executor target, an optional
//! fallback target for the error path, and a human-readable reason string for observability.
//!
//! # Name conventions
//!
//! Clients can address tools two ways:
//!
//! - **Unprefixed** (`get_issues`) — routing policy applies normally.
//! - **Prefixed** (`cloud__get_issues`) — the prefix is an explicit "send to that upstream"
//!   override. The engine honours it even when the strategy would otherwise pick Local.
//!
//! # Cloud priority
//!
//! The default [`RoutingStrategy`] is `Remote`. Local routing is never synthesized: it must
//! be explicitly enabled per tool, per server, or globally. When the schemas disagree,
//! matched tools degrade back to Remote automatically.

use std::time::Instant;

use devboy_core::config::{ProxyRoutingConfig, RoutingStrategy, routing_strategy_slug};
use serde_json::{Value, json};
use tracing::info;

use crate::signature_match::MatchReport;

/// Concrete executor target for a single call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingTarget {
    /// Dispatch to the in-process local `ToolHandler`.
    Local,
    /// Dispatch to the upstream proxy identified by `prefix`, using `original_name`
    /// as the unprefixed tool name forwarded to that upstream.
    Remote {
        prefix: String,
        original_name: String,
    },
    /// Neither executor can handle the tool. The caller should reject with a clear error.
    Reject,
}

/// Short reason label attached to every decision — surfaced in logs and `_meta` responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingReason {
    /// Explicit prefix in the tool name override.
    ExplicitPrefix,
    /// No upstream advertises this tool; local-only execution.
    LocalOnly,
    /// No local counterpart exists; must go upstream.
    RemoteOnly,
    /// Local and upstream both advertise the tool and schemas match.
    StrategyRemote,
    StrategyLocal,
    StrategyLocalFirst,
    StrategyRemoteFirst,
    /// Per-tool override rule fired.
    OverrideRule(String),
    /// Schemas disagree; forced back to remote.
    SchemaIncompatible,
    /// Tool name is completely unknown to both sides.
    Unknown,
}

impl RoutingReason {
    pub fn as_label(&self) -> &str {
        match self {
            Self::ExplicitPrefix => "explicit_prefix",
            Self::LocalOnly => "local_only",
            Self::RemoteOnly => "remote_only",
            Self::StrategyRemote => "strategy_remote",
            Self::StrategyLocal => "strategy_local",
            Self::StrategyLocalFirst => "strategy_local_first",
            Self::StrategyRemoteFirst => "strategy_remote_first",
            Self::OverrideRule(_) => "override_rule",
            Self::SchemaIncompatible => "schema_incompatible",
            Self::Unknown => "unknown",
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::OverrideRule(p) => Some(p.as_str()),
            _ => None,
        }
    }
}

/// Final routing plan for one invocation.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    /// Executor to try first.
    pub primary: RoutingTarget,
    /// Executor to retry on error. `None` when no fallback applies.
    pub fallback: Option<RoutingTarget>,
    /// Why this decision was made — included in telemetry and debug logs.
    pub reason: RoutingReason,
    /// The resolved unprefixed tool name after stripping any explicit prefix.
    pub resolved_name: String,
    /// Moment the decision was made. Useful downstream for timing analysis.
    pub decided_at: Instant,
}

impl RoutingDecision {
    /// Produce a `_meta.routing` JSON blob suitable for attaching to an MCP response.
    /// Consumers pack this under `_meta` so downstream tooling (CLI debug output, logs)
    /// can inspect the decision without parsing free-form text.
    pub fn to_meta_json(&self) -> Value {
        let target_label = match &self.primary {
            RoutingTarget::Local => "local".to_string(),
            RoutingTarget::Remote { prefix, .. } => format!("remote:{}", prefix),
            RoutingTarget::Reject => "reject".to_string(),
        };
        let fallback_label = self.fallback.as_ref().map(|t| match t {
            RoutingTarget::Local => "local".to_string(),
            RoutingTarget::Remote { prefix, .. } => format!("remote:{}", prefix),
            RoutingTarget::Reject => "reject".to_string(),
        });
        let mut obj = json!({
            "resolved_name": self.resolved_name,
            "target": target_label,
            "reason": self.reason.as_label(),
        });
        if let Some(detail) = self.reason.detail() {
            obj["reason_detail"] = Value::String(detail.to_string());
        }
        if let Some(f) = fallback_label {
            obj["fallback"] = Value::String(f);
        }
        obj
    }

    /// Structured tracing log describing this decision. Emitted once per invocation at
    /// info level so operators can grep for routing outcomes without enabling debug.
    pub fn emit_tracing(&self, requested_name: &str) {
        let target = match &self.primary {
            RoutingTarget::Local => "local".to_string(),
            RoutingTarget::Remote { prefix, .. } => format!("remote:{}", prefix),
            RoutingTarget::Reject => "reject".to_string(),
        };
        info!(
            tool = requested_name,
            resolved = self.resolved_name.as_str(),
            target = target.as_str(),
            reason = self.reason.as_label(),
            reason_detail = self.reason.detail().unwrap_or(""),
            has_fallback = self.fallback.is_some(),
            "routing decision"
        );
    }

    pub fn local(name: impl Into<String>, reason: RoutingReason) -> Self {
        Self {
            primary: RoutingTarget::Local,
            fallback: None,
            reason,
            resolved_name: name.into(),
            decided_at: Instant::now(),
        }
    }

    pub fn remote(
        prefix: impl Into<String>,
        original_name: impl Into<String>,
        reason: RoutingReason,
    ) -> Self {
        let original = original_name.into();
        Self {
            primary: RoutingTarget::Remote {
                prefix: prefix.into(),
                original_name: original.clone(),
            },
            fallback: None,
            reason,
            resolved_name: original,
            decided_at: Instant::now(),
        }
    }

    pub fn reject(name: impl Into<String>, reason: RoutingReason) -> Self {
        Self {
            primary: RoutingTarget::Reject,
            fallback: None,
            reason,
            resolved_name: name.into(),
            decided_at: Instant::now(),
        }
    }

    pub fn with_fallback(mut self, fallback: RoutingTarget) -> Self {
        self.fallback = Some(fallback);
        self
    }
}

/// Routes tool calls based on config + match report.
///
/// Immutable after construction: callers rebuild the engine whenever the match report
/// changes (e.g., upstream reconnect).
pub struct RoutingEngine {
    config: ProxyRoutingConfig,
    report: MatchReport,
}

impl RoutingEngine {
    pub fn new(config: ProxyRoutingConfig, report: MatchReport) -> Self {
        Self { config, report }
    }

    /// Resolve the executor for the given tool name (with or without prefix).
    /// Emits a `tracing::info` event with structured fields for every call.
    pub fn decide(&self, requested_name: &str) -> RoutingDecision {
        let decision = self.decide_inner(requested_name);
        decision.emit_tracing(requested_name);
        decision
    }

    /// Same as [`Self::decide`], but does not emit a tracing event. Useful from tests or
    /// from diagnostic commands that render the decision themselves.
    pub fn decide_quiet(&self, requested_name: &str) -> RoutingDecision {
        self.decide_inner(requested_name)
    }

    fn decide_inner(&self, requested_name: &str) -> RoutingDecision {
        // 1. Explicit prefix → always remote.
        if let Some((prefix, unprefixed)) = split_prefix(requested_name) {
            return RoutingDecision::remote(prefix, unprefixed, RoutingReason::ExplicitPrefix);
        }

        let match_info = self.report.get(requested_name);

        // 2. Unknown to both sides.
        let Some(m) = match_info else {
            return RoutingDecision::reject(requested_name, RoutingReason::Unknown);
        };

        // 3. One-sided: trivial routing.
        if m.local_present && !m.remote_present {
            return RoutingDecision::local(requested_name, RoutingReason::LocalOnly);
        }
        if m.remote_present && !m.local_present {
            let prefix = m.upstream_prefix.clone().unwrap_or_default();
            return RoutingDecision::remote(prefix, requested_name, RoutingReason::RemoteOnly);
        }

        // 4. Matched pair — consult strategy, with schema-compat as a hard gate.
        if m.schema_compatible == Some(false) {
            let prefix = m.upstream_prefix.clone().unwrap_or_default();
            return RoutingDecision::remote(
                prefix,
                requested_name,
                RoutingReason::SchemaIncompatible,
            );
        }

        let (strategy, override_pattern) = self.resolved_strategy_with_override(requested_name);

        let base_reason = match override_pattern {
            Some(pat) => RoutingReason::OverrideRule(pat),
            None => strategy_label(strategy),
        };

        let local_target = RoutingTarget::Local;
        let remote_target = RoutingTarget::Remote {
            prefix: m.upstream_prefix.clone().unwrap_or_default(),
            original_name: requested_name.to_string(),
        };

        match strategy {
            RoutingStrategy::Remote => RoutingDecision {
                primary: remote_target,
                fallback: None,
                reason: base_reason,
                resolved_name: requested_name.to_string(),
                decided_at: Instant::now(),
            },
            RoutingStrategy::Local => RoutingDecision {
                primary: local_target,
                fallback: None,
                reason: base_reason,
                resolved_name: requested_name.to_string(),
                decided_at: Instant::now(),
            },
            RoutingStrategy::LocalFirst => RoutingDecision {
                primary: local_target,
                fallback: if self.config.fallback_on_error {
                    Some(remote_target)
                } else {
                    None
                },
                reason: base_reason,
                resolved_name: requested_name.to_string(),
                decided_at: Instant::now(),
            },
            RoutingStrategy::RemoteFirst => RoutingDecision {
                primary: remote_target,
                fallback: if self.config.fallback_on_error {
                    Some(local_target)
                } else {
                    None
                },
                reason: base_reason,
                resolved_name: requested_name.to_string(),
                decided_at: Instant::now(),
            },
        }
    }

    /// Expose the underlying config for observability / debug commands.
    pub fn config(&self) -> &ProxyRoutingConfig {
        &self.config
    }

    /// Expose the match report for observability / debug commands.
    pub fn report(&self) -> &MatchReport {
        &self.report
    }

    fn resolved_strategy_with_override(
        &self,
        tool_name: &str,
    ) -> (RoutingStrategy, Option<String>) {
        for rule in &self.config.tool_overrides {
            if devboy_core::config::matches_glob(&rule.pattern, tool_name) {
                return (rule.strategy, Some(rule.pattern.clone()));
            }
        }
        (self.config.strategy, None)
    }
}

/// If `name` contains `__`, split at the first occurrence; the left side is the upstream
/// prefix, the right side is the unprefixed tool name. Otherwise returns `None`.
fn split_prefix(name: &str) -> Option<(String, String)> {
    name.split_once("__")
        .map(|(p, t)| (p.to_string(), t.to_string()))
}

fn strategy_label(s: RoutingStrategy) -> RoutingReason {
    match s {
        RoutingStrategy::Remote => RoutingReason::StrategyRemote,
        RoutingStrategy::Local => RoutingReason::StrategyLocal,
        RoutingStrategy::LocalFirst => RoutingReason::StrategyLocalFirst,
        RoutingStrategy::RemoteFirst => RoutingReason::StrategyRemoteFirst,
    }
}

/// Human-friendly summary of the current routing state. Used by the `proxy status` CLI
/// command and any dashboard that wants to inspect how the proxy is configured.
#[derive(Debug, Clone)]
pub struct ProxyStatus {
    pub strategy: RoutingStrategy,
    pub fallback_on_error: bool,
    pub total_tools: usize,
    pub routable_locally: Vec<String>,
    pub remote_only: Vec<String>,
    pub local_only: Vec<String>,
    pub incompatible: Vec<IncompatibleTool>,
    pub override_rules: Vec<(String, RoutingStrategy)>,
}

/// One row in the "schema disagrees" table.
#[derive(Debug, Clone)]
pub struct IncompatibleTool {
    pub tool: String,
    pub upstream_prefix: Option<String>,
    pub reason: Option<String>,
}

impl ProxyStatus {
    /// Compute a status snapshot from the engine's current config + match report.
    pub fn from_engine(engine: &RoutingEngine) -> Self {
        let report = engine.report();
        let config = engine.config();

        let mut routable_locally: Vec<String> = report
            .routable_locally()
            .iter()
            .map(|m| m.tool_name.clone())
            .collect();
        routable_locally.sort();

        let mut remote_only: Vec<String> = report
            .remote_only()
            .iter()
            .map(|m| m.tool_name.clone())
            .collect();
        remote_only.sort();

        let mut local_only: Vec<String> = report
            .local_only()
            .iter()
            .map(|m| m.tool_name.clone())
            .collect();
        local_only.sort();

        let mut incompatible: Vec<IncompatibleTool> = report
            .incompatible_pairs()
            .iter()
            .map(|m| IncompatibleTool {
                tool: m.tool_name.clone(),
                upstream_prefix: m.upstream_prefix.clone(),
                reason: m.schema_mismatch.clone(),
            })
            .collect();
        incompatible.sort_by(|a, b| a.tool.cmp(&b.tool));

        let override_rules: Vec<(String, RoutingStrategy)> = config
            .tool_overrides
            .iter()
            .map(|r| (r.pattern.clone(), r.strategy))
            .collect();

        Self {
            strategy: config.strategy,
            fallback_on_error: config.fallback_on_error,
            total_tools: report.len(),
            routable_locally,
            remote_only,
            local_only,
            incompatible,
            override_rules,
        }
    }

    /// Render a plain-text report suitable for CLI output.
    ///
    /// The strategy is formatted via [`routing_strategy_slug`] (kebab-case),
    /// consistently with `to_json()` and TOML serialization — the user sees the same
    /// spelling in every output format.
    pub fn to_text_report(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(out, "Proxy routing status");
        let _ = writeln!(out, "====================");
        let _ = writeln!(
            out,
            "strategy           : {}",
            routing_strategy_slug(self.strategy)
        );
        let _ = writeln!(out, "fallback_on_error  : {}", self.fallback_on_error);
        let _ = writeln!(out, "total_tools        : {}", self.total_tools);

        let _ = writeln!(out);
        let _ = writeln!(out, "Routable locally ({}):", self.routable_locally.len());
        for t in &self.routable_locally {
            let _ = writeln!(out, "  • {}", t);
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "Remote-only ({}):", self.remote_only.len());
        for t in &self.remote_only {
            let _ = writeln!(out, "  • {}", t);
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "Local-only ({}):", self.local_only.len());
        for t in &self.local_only {
            let _ = writeln!(out, "  • {}", t);
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "Schema incompatible ({}):", self.incompatible.len());
        for it in &self.incompatible {
            let up = it.upstream_prefix.as_deref().unwrap_or("(unknown)");
            let reason = it.reason.as_deref().unwrap_or("(no detail)");
            let _ = writeln!(out, "  • {:<32} via {:<10} — {}", it.tool, up, reason);
        }
        if !self.override_rules.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "Override rules:");
            for (pat, strat) in &self.override_rules {
                let _ = writeln!(out, "  • {:<24} → {}", pat, routing_strategy_slug(*strat));
            }
        }
        out
    }

    /// JSON form — convenient for machine-readable CLI output (`--json` flag).
    ///
    /// Strategy values are serialized in the same kebab-case form as in the TOML
    /// config (`local-first`, not `LocalFirst`); otherwise `proxy status --json | jq`
    /// clients would have to deal with two spellings of the same value.
    pub fn to_json(&self) -> Value {
        json!({
            "strategy": routing_strategy_slug(self.strategy),
            "fallback_on_error": self.fallback_on_error,
            "total_tools": self.total_tools,
            "routable_locally": self.routable_locally,
            "remote_only": self.remote_only,
            "local_only": self.local_only,
            "incompatible": self.incompatible.iter().map(|it| json!({
                "tool": it.tool,
                "upstream_prefix": it.upstream_prefix,
                "reason": it.reason,
            })).collect::<Vec<_>>(),
            "override_rules": self.override_rules.iter().map(|(p, s)| json!({
                "pattern": p,
                "strategy": routing_strategy_slug(*s),
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature_match::ToolMatch;
    use devboy_core::config::ProxyToolRule;

    fn report_with(entries: Vec<ToolMatch>) -> MatchReport {
        let mut report = MatchReport::default();
        for e in entries {
            report.matches.insert(e.tool_name.clone(), e);
        }
        report
    }

    fn matched(name: &str, compatible: bool) -> ToolMatch {
        ToolMatch {
            tool_name: name.to_string(),
            local_present: true,
            remote_present: true,
            schema_compatible: Some(compatible),
            upstream_prefix: Some("cloud".to_string()),
            schema_mismatch: None,
        }
    }

    fn local_only(name: &str) -> ToolMatch {
        ToolMatch {
            tool_name: name.to_string(),
            local_present: true,
            remote_present: false,
            schema_compatible: None,
            upstream_prefix: None,
            schema_mismatch: None,
        }
    }

    fn remote_only(name: &str, prefix: &str) -> ToolMatch {
        ToolMatch {
            tool_name: name.to_string(),
            local_present: false,
            remote_present: true,
            schema_compatible: None,
            upstream_prefix: Some(prefix.to_string()),
            schema_mismatch: None,
        }
    }

    #[test]
    fn test_explicit_prefix_forces_remote() {
        let engine = RoutingEngine::new(
            ProxyRoutingConfig::default(),
            report_with(vec![matched("get_issues", true)]),
        );
        let d = engine.decide("cloud__get_issues");
        assert_eq!(
            d.primary,
            RoutingTarget::Remote {
                prefix: "cloud".to_string(),
                original_name: "get_issues".to_string(),
            }
        );
        assert_eq!(d.reason, RoutingReason::ExplicitPrefix);
    }

    #[test]
    fn test_unknown_tool_rejects() {
        let engine = RoutingEngine::new(ProxyRoutingConfig::default(), MatchReport::default());
        let d = engine.decide("mystery_tool");
        assert_eq!(d.primary, RoutingTarget::Reject);
        assert_eq!(d.reason, RoutingReason::Unknown);
    }

    #[test]
    fn test_local_only_tool_goes_local() {
        let engine = RoutingEngine::new(
            ProxyRoutingConfig::default(),
            report_with(vec![local_only("list_contexts")]),
        );
        let d = engine.decide("list_contexts");
        assert_eq!(d.primary, RoutingTarget::Local);
        assert_eq!(d.reason, RoutingReason::LocalOnly);
    }

    #[test]
    fn test_remote_only_tool_goes_remote() {
        let engine = RoutingEngine::new(
            ProxyRoutingConfig::default(),
            report_with(vec![remote_only("cloud_specific", "cloud")]),
        );
        let d = engine.decide("cloud_specific");
        assert_eq!(
            d.primary,
            RoutingTarget::Remote {
                prefix: "cloud".to_string(),
                original_name: "cloud_specific".to_string(),
            }
        );
        assert_eq!(d.reason, RoutingReason::RemoteOnly);
    }

    #[test]
    fn test_matched_default_strategy_is_remote() {
        let engine = RoutingEngine::new(
            ProxyRoutingConfig::default(),
            report_with(vec![matched("get_issues", true)]),
        );
        let d = engine.decide("get_issues");
        assert!(matches!(d.primary, RoutingTarget::Remote { .. }));
        assert_eq!(d.reason, RoutingReason::StrategyRemote);
        assert!(d.fallback.is_none());
    }

    #[test]
    fn test_matched_local_strategy() {
        let config = ProxyRoutingConfig {
            strategy: RoutingStrategy::Local,
            ..Default::default()
        };
        let engine = RoutingEngine::new(config, report_with(vec![matched("get_issues", true)]));
        let d = engine.decide("get_issues");
        assert_eq!(d.primary, RoutingTarget::Local);
        assert_eq!(d.reason, RoutingReason::StrategyLocal);
        assert!(d.fallback.is_none());
    }

    #[test]
    fn test_local_first_with_fallback_on_error() {
        let config = ProxyRoutingConfig {
            strategy: RoutingStrategy::LocalFirst,
            fallback_on_error: true,
            ..Default::default()
        };
        let engine = RoutingEngine::new(config, report_with(vec![matched("get_issues", true)]));
        let d = engine.decide("get_issues");
        assert_eq!(d.primary, RoutingTarget::Local);
        assert_eq!(d.reason, RoutingReason::StrategyLocalFirst);
        assert_eq!(
            d.fallback,
            Some(RoutingTarget::Remote {
                prefix: "cloud".to_string(),
                original_name: "get_issues".to_string(),
            })
        );
    }

    #[test]
    fn test_remote_first_with_fallback() {
        let config = ProxyRoutingConfig {
            strategy: RoutingStrategy::RemoteFirst,
            fallback_on_error: true,
            ..Default::default()
        };
        let engine = RoutingEngine::new(config, report_with(vec![matched("get_issues", true)]));
        let d = engine.decide("get_issues");
        assert!(matches!(d.primary, RoutingTarget::Remote { .. }));
        assert_eq!(d.fallback, Some(RoutingTarget::Local));
        assert_eq!(d.reason, RoutingReason::StrategyRemoteFirst);
    }

    #[test]
    fn test_local_first_no_fallback_when_disabled() {
        let config = ProxyRoutingConfig {
            strategy: RoutingStrategy::LocalFirst,
            fallback_on_error: false,
            ..Default::default()
        };
        let engine = RoutingEngine::new(config, report_with(vec![matched("get_issues", true)]));
        let d = engine.decide("get_issues");
        assert!(d.fallback.is_none());
    }

    #[test]
    fn test_incompatible_schema_forces_remote() {
        let config = ProxyRoutingConfig {
            strategy: RoutingStrategy::LocalFirst,
            fallback_on_error: true,
            ..Default::default()
        };
        let engine = RoutingEngine::new(config, report_with(vec![matched("get_issues", false)]));
        let d = engine.decide("get_issues");
        assert!(matches!(d.primary, RoutingTarget::Remote { .. }));
        assert_eq!(d.reason, RoutingReason::SchemaIncompatible);
        assert!(d.fallback.is_none());
    }

    #[test]
    fn test_per_tool_override_wins_over_global_strategy() {
        let config = ProxyRoutingConfig {
            strategy: RoutingStrategy::Remote,
            fallback_on_error: true,
            tool_overrides: vec![ProxyToolRule {
                pattern: "get_*".to_string(),
                strategy: RoutingStrategy::Local,
            }],
        };
        let engine = RoutingEngine::new(
            config,
            report_with(vec![
                matched("get_issues", true),
                matched("create_issue", true),
            ]),
        );

        let d_get = engine.decide("get_issues");
        assert_eq!(d_get.primary, RoutingTarget::Local);
        match &d_get.reason {
            RoutingReason::OverrideRule(p) => assert_eq!(p, "get_*"),
            other => panic!("expected OverrideRule, got {:?}", other),
        }

        let d_create = engine.decide("create_issue");
        assert!(matches!(d_create.primary, RoutingTarget::Remote { .. }));
        assert_eq!(d_create.reason, RoutingReason::StrategyRemote);
    }

    #[test]
    fn test_decision_label_roundtrip() {
        assert_eq!(RoutingReason::ExplicitPrefix.as_label(), "explicit_prefix");
        assert_eq!(
            RoutingReason::OverrideRule("get_*".to_string()).as_label(),
            "override_rule"
        );
        assert_eq!(
            RoutingReason::OverrideRule("get_*".to_string()).detail(),
            Some("get_*")
        );
    }

    #[test]
    fn test_to_meta_json_shapes() {
        let d = RoutingDecision::local("get_issues", RoutingReason::StrategyLocal);
        let v = d.to_meta_json();
        assert_eq!(v["target"], "local");
        assert_eq!(v["reason"], "strategy_local");
        assert!(v.get("fallback").is_none());

        let d = RoutingDecision::remote(
            "cloud",
            "get_issues",
            RoutingReason::OverrideRule("get_*".to_string()),
        )
        .with_fallback(RoutingTarget::Local);
        let v = d.to_meta_json();
        assert_eq!(v["target"], "remote:cloud");
        assert_eq!(v["reason"], "override_rule");
        assert_eq!(v["reason_detail"], "get_*");
        assert_eq!(v["fallback"], "local");
    }

    #[test]
    fn test_proxy_status_summary() {
        let config = ProxyRoutingConfig {
            strategy: RoutingStrategy::LocalFirst,
            fallback_on_error: true,
            tool_overrides: vec![ProxyToolRule {
                pattern: "create_*".to_string(),
                strategy: RoutingStrategy::Remote,
            }],
        };
        let engine = RoutingEngine::new(
            config,
            report_with(vec![
                matched("get_issues", true),
                matched("update_issue", false),
                local_only("list_contexts"),
                remote_only("cloud_only", "cloud"),
            ]),
        );

        let status = ProxyStatus::from_engine(&engine);
        assert_eq!(status.strategy, RoutingStrategy::LocalFirst);
        assert_eq!(status.total_tools, 4);
        assert_eq!(status.routable_locally, vec!["get_issues".to_string()]);
        assert_eq!(status.remote_only, vec!["cloud_only".to_string()]);
        assert_eq!(status.local_only, vec!["list_contexts".to_string()]);
        assert_eq!(status.incompatible.len(), 1);
        assert_eq!(status.incompatible[0].tool, "update_issue");
        assert_eq!(status.override_rules.len(), 1);

        let text = status.to_text_report();
        assert!(text.contains("Routable locally (1):"));
        assert!(text.contains("get_issues"));
        // Strategy and override rules render in kebab-case, symmetric with JSON/TOML.
        assert!(
            text.contains("strategy           : local-first"),
            "missing kebab-case strategy line in:\n{}",
            text
        );
        // Override rule line exists and uses kebab-case target (whitespace is cosmetic).
        let override_line = text
            .lines()
            .find(|l| l.contains("create_*"))
            .expect("override rule line present");
        assert!(
            override_line.ends_with("→ remote"),
            "override rule must end with '→ remote' (kebab-case), got: {:?}",
            override_line
        );
        // Ensure PascalCase enum debug forms are gone.
        assert!(
            !text.contains("LocalFirst")
                && !text.contains("RemoteFirst")
                && !text.contains(": Remote\n")
                && !text.contains(": Local\n"),
            "text still contains PascalCase strategy name:\n{}",
            text
        );

        let json = status.to_json();
        assert_eq!(json["total_tools"], 4);
        assert_eq!(json["routable_locally"][0], "get_issues");
        // Strategy must be kebab-case (matches serde/TOML form), not PascalCase `Debug`.
        assert_eq!(json["strategy"], "local-first");
        assert_eq!(json["override_rules"][0]["strategy"], "remote");
    }

    #[test]
    fn test_decide_quiet_does_not_panic_and_matches_decide() {
        let engine = RoutingEngine::new(
            ProxyRoutingConfig::default(),
            report_with(vec![matched("get_issues", true)]),
        );
        let a = engine.decide_quiet("get_issues");
        let b = engine.decide_quiet("get_issues");
        assert_eq!(a.resolved_name, b.resolved_name);
        assert_eq!(a.reason, b.reason);
    }
}
