//! Finds paths that ask for per-use approval this build cannot
//! collect.
//!
//! # The trap
//!
//! `approve_on_use = session | per-call` is settable in the
//! global index and overridable per project. A path carrying
//! either value cannot be resolved at all: the gate refuses, and
//! the approval dialog it would need is not wired into a shipped
//! MCP server — the only launcher compiled in returns an error.
//!
//! Left alone, that surfaces as a resolve failure at the moment
//! an agent needs the secret, which is the worst time to discover
//! a configuration problem. This check moves the discovery to
//! `doctor`.
//!
//! # Why it is an error rather than a warning
//!
//! The path does not work. Not "works with reduced protection" —
//! does not resolve. A warning would suggest a posture that is
//! merely weaker than intended, and this is a hard stop.
//!
//! # Why the value is not quietly ignored
//!
//! Silently treating it as `never` would leave the user believing
//! a gate exists where none does. Every other part of this change
//! set removed exactly that kind of quiet substitution, and
//! reintroducing it for a security setting would be the worst
//! place to make an exception.

use async_trait::async_trait;
use devboy_storage::index::ApproveOnUse;

use crate::doctor::{CheckResult, CheckStatus, DiagnosticCheck, DiagnosticContext};

pub struct ApproveOnUseCheck;

/// One path that declares a policy nothing can honour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatedPath {
    pub path: String,
    pub policy: &'static str,
    /// Where the value came from, because the fix is a different
    /// edit in each case.
    pub source: &'static str,
}

/// Whether a policy needs a dialog that does not exist here.
///
/// `Never` is the default and the only value that resolves, so
/// this is the whole rule — kept separate from the walk so it can
/// be read without a manifest on disk.
pub fn is_unhonourable(policy: ApproveOnUse) -> bool {
    match policy {
        ApproveOnUse::Never => false,
        ApproveOnUse::Session | ApproveOnUse::PerCall => true,
    }
}

/// Human name for the policy, for the report.
pub fn policy_label(policy: ApproveOnUse) -> &'static str {
    match policy {
        ApproveOnUse::Never => "never",
        ApproveOnUse::Session => "session",
        ApproveOnUse::PerCall => "per-call",
    }
}

/// Build the report line for a set of findings.
///
/// Split out so the wording is testable without constructing a
/// diagnostic context: the message is the entire value of this
/// check, since the condition itself is a one-line comparison.
pub fn summarise(findings: &[GatedPath]) -> String {
    if findings.len() == 1 {
        let f = &findings[0];
        return format!(
            "`{}` is marked `approve_on_use = {}` in the {}, and per-use approval is not \
             available in this build — the path cannot be resolved at all. Set it to `never`.",
            f.path, f.policy, f.source
        );
    }
    format!(
        "{} paths are marked `approve_on_use` with a policy this build cannot collect, so none \
         of them can be resolved. Set them to `never`.",
        findings.len()
    )
}

#[async_trait]
impl DiagnosticCheck for ApproveOnUseCheck {
    fn id(&self) -> &'static str {
        "approve-on-use"
    }

    fn name(&self) -> &'static str {
        "Per-use approval policies"
    }

    fn category(&self) -> &'static str {
        "Secrets"
    }

    async fn run(&self, _ctx: &DiagnosticContext) -> CheckResult {
        let findings = collect_gated_paths();

        if findings.is_empty() {
            return CheckResult {
                id: self.id().to_string(),
                category: self.category().to_string(),
                name: self.name().to_string(),
                status: CheckStatus::Pass,
                message: "No path asks for per-use approval".to_string(),
                details: None,
                fix_command: None,
                fix_url: None,
            };
        }

        let details = serde_json::json!({
            "paths": findings
                .iter()
                .map(|f| serde_json::json!({
                    "path": f.path,
                    "policy": f.policy,
                    "declared_in": f.source,
                }))
                .collect::<Vec<_>>(),
        });

        CheckResult {
            id: self.id().to_string(),
            category: self.category().to_string(),
            name: self.name().to_string(),
            status: CheckStatus::Error,
            message: summarise(&findings),
            details: Some(details),
            fix_command: Some(format!(
                "devboy secrets override {} --approve-on-use never",
                findings[0].path
            )),
            fix_url: None,
        }
    }
}

/// Walk the global index and the project manifest.
///
/// A missing file is not a finding — plenty of installs have
/// neither, and the other checks already report that.
fn collect_gated_paths() -> Vec<GatedPath> {
    let mut out = Vec::new();

    if let Ok(index) = devboy_storage::GlobalIndex::load() {
        for (path, entry) in index.iter() {
            if let Some(policy) = entry.approve_on_use
                && is_unhonourable(policy)
            {
                out.push(GatedPath {
                    path: path.to_string(),
                    policy: policy_label(policy),
                    source: "global index",
                });
            }
        }
    }

    if let Ok(manifest) = devboy_storage::ProjectManifest::load() {
        for (path, entry) in &manifest.overrides {
            if let Some(policy) = entry.approve_on_use
                && is_unhonourable(policy)
            {
                out.push(GatedPath {
                    path: path.to_string(),
                    policy: policy_label(policy),
                    source: "project manifest",
                });
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Never` is the only policy that resolves, so it is the
    /// only one this check must stay quiet about.
    #[test]
    fn only_never_is_honourable() {
        assert!(!is_unhonourable(ApproveOnUse::Never));
        assert!(is_unhonourable(ApproveOnUse::Session));
        assert!(is_unhonourable(ApproveOnUse::PerCall));
    }

    /// The single-path message has to say three things: which
    /// path, which value, and what to change it to. Without the
    /// last one the report is a complaint.
    #[test]
    fn the_single_path_message_names_the_path_the_value_and_the_fix() {
        let m = summarise(&[GatedPath {
            path: "team/prod/db-password".into(),
            policy: "per-call",
            source: "global index",
        }]);

        assert!(m.contains("team/prod/db-password"), "{m}");
        assert!(m.contains("per-call"), "{m}");
        assert!(m.contains("global index"), "{m}");
        assert!(m.contains("`never`"), "{m}");
        assert!(
            m.contains("cannot be resolved"),
            "the consequence is the point — a policy that merely weakens something would not \
             deserve an error: {m}"
        );
    }

    #[test]
    fn the_many_path_message_counts_them() {
        let m = summarise(&[
            GatedPath {
                path: "a/b/c".into(),
                policy: "session",
                source: "global index",
            },
            GatedPath {
                path: "d/e/f".into(),
                policy: "per-call",
                source: "project manifest",
            },
        ]);

        assert!(m.contains('2'), "{m}");
        assert!(m.contains("`never`"), "{m}");
    }
}
