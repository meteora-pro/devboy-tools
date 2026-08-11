//! `secrets_validate` — verdict-only validation (ADR-024 §3).
//!
//! # The gap this closes
//!
//! ADR-020 defined liveness validation, but only `doctor` and
//! `validate` exercised it. An agent that had just provisioned a
//! token could confirm it worked in exactly one way: make a real
//! provider call and interpret the error. That is slow, noisy, and
//! occasionally destructive.
//!
//! # The contract
//!
//! The agent asks about a **path**; everything happens
//! server-side; only a verdict comes back. The value never crosses
//! the MCP wire — which is what lets an agent confirm a secret
//! works without ever being trusted with it.
//!
//! Two independent checks:
//!
//! - **Format** is offline. It matches the stored value against
//!   the entry's `format_regex`, or against the pattern its
//!   `pattern_id` resolves to. Cheap, and catches the common
//!   "pasted the wrong thing" case.
//! - **Liveness** is opt-in, because it costs a network round trip
//!   and shows up in the provider's audit log. It resolves the
//!   value server-side, makes one cheap authenticated call, and
//!   returns whether the credential was accepted.
//!
//! A liveness failure is reported as a verdict, not an error:
//! "this token is no longer accepted" is a fact about the world,
//! and the remediation attached to it points the user at rotation.

use serde::{Deserialize, Serialize};

use crate::agent_safety::AgentSafeReply;
use crate::remediation::{Remediation, RemediationContext, SecretsErrorKind};

/// Result of the offline format check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatVerdict {
    /// The value matches the declared shape.
    Ok,
    /// The value does not match.
    Invalid,
    /// No shape was declared, so there was nothing to check.
    ///
    /// Distinct from `Ok` on purpose: "we checked and it passed"
    /// and "nobody said what this should look like" are different
    /// facts, and an agent that conflates them will report false
    /// confidence.
    Unknown,
}

/// Result of the authenticated liveness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LivenessVerdict {
    /// The provider accepted the credential.
    Ok,
    /// The provider rejected it — expired, revoked, or wrong.
    Invalid,
    /// The provider could not be reached.
    ///
    /// Deliberately not `Invalid`: a network failure says nothing
    /// about the credential, and treating it as a rejection would
    /// send users rotating perfectly good tokens.
    Unreachable,
    /// The source or pattern declares no liveness check.
    Unsupported,
}

/// Reply for `secrets_validate`.
///
/// Carries verdicts and metadata only — no value, and nothing
/// derived from one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretsValidateReply {
    /// The path that was checked.
    pub path: String,
    /// Offline shape check.
    pub format: FormatVerdict,
    /// Authenticated check, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness: Option<LivenessVerdict>,
    /// Declared expiry from the manifest, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// What to do about a negative verdict.
    ///
    /// Present only when something needs acting on, so its absence
    /// is itself the "all good" signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<Remediation>,
}

impl AgentSafeReply for SecretsValidateReply {}

impl SecretsValidateReply {
    /// Build a reply, attaching a remediation when a verdict calls
    /// for one.
    ///
    /// The precedence matters. A dead credential is reported ahead
    /// of a malformed one: if the provider has already rejected it,
    /// "rotate this" is the actionable instruction, and arguing
    /// about its shape would be beside the point.
    pub fn new(
        path: impl Into<String>,
        format: FormatVerdict,
        liveness: Option<LivenessVerdict>,
        ctx: &RemediationContext,
    ) -> Self {
        let path = path.into();

        let remediation = match (format, liveness) {
            (_, Some(LivenessVerdict::Invalid)) => {
                Some(SecretsErrorKind::LivenessFailed.remediation(ctx))
            }
            (FormatVerdict::Invalid, _) => Some(SecretsErrorKind::LivenessFailed.remediation(ctx)),
            // Unreachable is not the credential's fault, and
            // Unsupported is not a problem at all.
            _ => None,
        };

        Self {
            path,
            format,
            liveness,
            expires_at: ctx.expires_at_hint.clone(),
            remediation,
        }
    }

    /// Whether the caller should treat this as a healthy secret.
    ///
    /// `Unreachable` counts as healthy: a network problem is not
    /// evidence against the credential.
    pub fn is_healthy(&self) -> bool {
        let format_ok = !matches!(self.format, FormatVerdict::Invalid);
        let liveness_ok = !matches!(self.liveness, Some(LivenessVerdict::Invalid));
        format_ok && liveness_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RemediationContext {
        RemediationContext {
            path: Some("team/gitlab/token".to_string()),
            retrieval_url: Some("https://gitlab.example/-/user_settings".to_string()),
            rotation_method: Some("provider-ui".to_string()),
            ..RemediationContext::default()
        }
    }

    #[test]
    fn a_healthy_secret_carries_no_remediation() {
        let r = SecretsValidateReply::new(
            "team/gitlab/token",
            FormatVerdict::Ok,
            Some(LivenessVerdict::Ok),
            &ctx(),
        );

        assert!(
            r.remediation.is_none(),
            "absence of advice is the all-clear"
        );
        assert!(r.is_healthy());
    }

    #[test]
    fn a_dead_credential_is_reported_with_rotation_advice() {
        let r = SecretsValidateReply::new(
            "team/gitlab/token",
            FormatVerdict::Ok,
            Some(LivenessVerdict::Invalid),
            &ctx(),
        );

        assert!(!r.is_healthy());
        let rem = r
            .remediation
            .clone()
            .expect("a dead credential needs advice");
        assert_eq!(
            rem.action,
            crate::remediation::RemediationAction::AskUserToRotate
        );
        assert!(
            rem.user_message.contains("gitlab.example"),
            "{}",
            rem.user_message
        );
    }

    /// A network failure says nothing about the credential.
    /// Treating it as a rejection would send users rotating
    /// perfectly good tokens.
    #[test]
    fn unreachable_is_not_treated_as_a_rejection() {
        let r = SecretsValidateReply::new(
            "team/gitlab/token",
            FormatVerdict::Ok,
            Some(LivenessVerdict::Unreachable),
            &ctx(),
        );

        assert!(r.remediation.is_none());
        assert!(r.is_healthy(), "unreachable must not imply a bad secret");
    }

    #[test]
    fn unsupported_liveness_is_not_a_problem() {
        let r = SecretsValidateReply::new(
            "team/gitlab/token",
            FormatVerdict::Ok,
            Some(LivenessVerdict::Unsupported),
            &ctx(),
        );

        assert!(r.remediation.is_none());
        assert!(r.is_healthy());
    }

    /// "Checked and passed" and "nobody declared a shape" are
    /// different facts; an agent conflating them reports false
    /// confidence.
    #[test]
    fn unknown_format_is_distinct_from_ok() {
        let unknown = SecretsValidateReply::new("p", FormatVerdict::Unknown, None, &ctx());
        let ok = SecretsValidateReply::new("p", FormatVerdict::Ok, None, &ctx());

        assert_ne!(unknown.format, ok.format);
        assert!(unknown.remediation.is_none());
        assert!(unknown.is_healthy());
    }

    #[test]
    fn a_malformed_value_is_flagged_even_without_a_liveness_check() {
        let r = SecretsValidateReply::new("p", FormatVerdict::Invalid, None, &ctx());

        assert!(r.remediation.is_some());
        assert!(!r.is_healthy());
    }

    /// A dead credential outranks a shape complaint: if the
    /// provider already rejected it, rotation is the instruction.
    #[test]
    fn liveness_failure_outranks_a_format_complaint() {
        let r = SecretsValidateReply::new(
            "p",
            FormatVerdict::Invalid,
            Some(LivenessVerdict::Invalid),
            &ctx(),
        );

        assert_eq!(
            r.remediation.unwrap().action,
            crate::remediation::RemediationAction::AskUserToRotate
        );
    }

    /// The whole point of the tool: an agent learns the verdict
    /// without ever seeing the value.
    #[test]
    fn the_reply_never_carries_a_value() {
        let r = SecretsValidateReply::new(
            "team/gitlab/token",
            FormatVerdict::Ok,
            Some(LivenessVerdict::Ok),
            &ctx(),
        );
        let json = serde_json::to_value(&r).unwrap();
        let obj = json.as_object().unwrap();

        let allowed = ["path", "format", "liveness", "expires_at", "remediation"];
        for key in obj.keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "unaudited field `{key}` on a verdict reply"
            );
        }
    }

    #[test]
    fn verdict_wire_names_are_stable() {
        assert_eq!(serde_json::to_value(FormatVerdict::Ok).unwrap(), "ok");
        assert_eq!(
            serde_json::to_value(FormatVerdict::Invalid).unwrap(),
            "invalid"
        );
        assert_eq!(
            serde_json::to_value(FormatVerdict::Unknown).unwrap(),
            "unknown"
        );
        assert_eq!(serde_json::to_value(LivenessVerdict::Ok).unwrap(), "ok");
        assert_eq!(
            serde_json::to_value(LivenessVerdict::Unreachable).unwrap(),
            "unreachable"
        );
        assert_eq!(
            serde_json::to_value(LivenessVerdict::Unsupported).unwrap(),
            "unsupported"
        );
    }

    /// An omitted liveness check must be absent rather than null,
    /// so its presence means "this was actually checked".
    #[test]
    fn an_unrequested_liveness_check_is_omitted() {
        let r = SecretsValidateReply::new("p", FormatVerdict::Ok, None, &ctx());
        let json = serde_json::to_string(&r).unwrap();

        assert!(!json.contains("liveness"), "{json}");
    }
}
