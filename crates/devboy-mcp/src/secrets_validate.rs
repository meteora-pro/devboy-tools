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
//! - **Format** is offline. The daemon matches the stored value
//!   against the pattern its `pattern_id` resolves to. Cheap, and
//!   catches the common "pasted the wrong thing" case.
//! - **Liveness** is opt-in, because it costs a network round trip
//!   and shows up in the provider's audit log. It resolves the
//!   value server-side, makes one cheap authenticated call, and
//!   returns whether the credential was accepted. A pattern that
//!   declares no endpoint answers `unsupported`.
//!
//! # The rule is the daemon's, not the caller's
//!
//! There is no way to hand in a `format_regex`, and that is not an
//! oversight. A method that answers yes/no about a secret against a
//! rule the caller chooses is a value oracle: ask `^g.*`, then
//! `^gl.*`, then `^gla.*`, and the secret comes out one character
//! at a time. So the rule is the one the daemon stored with the
//! entry — which includes any pattern the user declared in
//! `<config>/secrets/patterns.d/`, so an in-house token shape is
//! checkable here without anyone teaching the tool about a vendor.
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

/// Arguments for `secrets_validate`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecretsValidateArgs {
    /// The ADR-020 path to check.
    pub path: String,
    /// Also ask the provider whether the credential still works.
    ///
    /// Off by default: it costs a network round trip and leaves a
    /// line in the provider's own audit log, neither of which an
    /// agent should spend without meaning to.
    #[serde(default)]
    pub liveness: bool,
}

/// Translate the daemon's verdict word into the agent-facing enum.
///
/// Anything unrecognised becomes `Unknown`, never `Ok`. A daemon
/// newer than this build could invent a fourth verdict, and the one
/// reading it must not answer "fine" to a word it has never seen.
/// Split out from [`validate`] so this can be checked without a
/// running daemon.
/// Only `validate` calls this, and only UNIX has a daemon socket to
/// call it about — but the mapping rule is not platform-specific and
/// its test runs everywhere, so it stays compiled rather than being
/// cfg'd away and going untested on the platform it still ships to.
#[cfg_attr(not(unix), allow(dead_code))]
fn format_from_wire(word: Option<&str>) -> FormatVerdict {
    match word {
        Some("ok") => FormatVerdict::Ok,
        Some("invalid") => FormatVerdict::Invalid,
        _ => FormatVerdict::Unknown,
    }
}

/// Translate the daemon's liveness word.
///
/// An unrecognised word becomes `Unreachable`, not `Ok` and not
/// `Invalid`. "We could not establish anything" is the truthful
/// reading of an answer this build cannot parse, and it is the one
/// verdict that neither claims the credential is fine nor sends
/// someone rotating it.
/// Only `validate` calls this, and only UNIX has a daemon socket to
/// call it about — but the mapping rule is not platform-specific and
/// its test runs everywhere, so it stays compiled rather than being
/// cfg'd away and going untested on the platform it still ships to.
#[cfg_attr(not(unix), allow(dead_code))]
fn liveness_from_wire(word: Option<&str>) -> LivenessVerdict {
    match word {
        Some("ok") => LivenessVerdict::Ok,
        Some("invalid") => LivenessVerdict::Invalid,
        Some("unsupported") => LivenessVerdict::Unsupported,
        _ => LivenessVerdict::Unreachable,
    }
}

/// Ask the daemon about a path.
///
/// Note what is not sent: any rule. The daemon validates against
/// the `pattern_id` it stored with the entry, because a
/// caller-supplied regex would let anyone reading the socket
/// extract the value one character at a time from a run of yes/no
/// answers.
///
/// A daemon that is not running is reported as `Unknown` with the
/// advice to start it, rather than as a bad secret — the agent's
/// next move is to ask the user to start the daemon, not to rotate
/// a credential that was never examined.
#[cfg(unix)]
pub fn validate(args: &SecretsValidateArgs, ctx: &RemediationContext) -> SecretsValidateReply {
    use devboy_secrets_agent::AgentClient;

    let unreachable = || SecretsValidateReply {
        path: args.path.clone(),
        format: FormatVerdict::Unknown,
        liveness: None,
        expires_at: None,
        remediation: Some(SecretsErrorKind::DaemonNotRunning.remediation(ctx)),
    };

    let Some(client) = AgentClient::new() else {
        return unreachable();
    };
    let Ok(result) = client.secret_validate(&args.path, args.liveness) else {
        return unreachable();
    };

    let format = format_from_wire(result.get("format").and_then(|v| v.as_str()));

    let liveness = args
        .liveness
        .then(|| liveness_from_wire(result.get("liveness").and_then(|v| v.as_str())));

    let mut ctx = ctx.clone();
    if ctx.expires_at_hint.is_none() {
        ctx.expires_at_hint = result
            .get("expires_at")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
    }

    SecretsValidateReply::new(args.path.clone(), format, liveness, &ctx)
}

/// Off UNIX there is no daemon socket, so there is nothing to ask.
#[cfg(not(unix))]
pub fn validate(args: &SecretsValidateArgs, ctx: &RemediationContext) -> SecretsValidateReply {
    SecretsValidateReply {
        path: args.path.clone(),
        format: FormatVerdict::Unknown,
        liveness: None,
        expires_at: None,
        remediation: Some(SecretsErrorKind::DaemonNotRunning.remediation(ctx)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same rule as the format word, with a different safe default:
    /// an unparseable liveness answer must neither bless the
    /// credential nor condemn it.
    #[test]
    fn an_unrecognised_liveness_word_is_inconclusive() {
        assert_eq!(liveness_from_wire(Some("ok")), LivenessVerdict::Ok);
        assert_eq!(
            liveness_from_wire(Some("invalid")),
            LivenessVerdict::Invalid
        );
        assert_eq!(
            liveness_from_wire(Some("unsupported")),
            LivenessVerdict::Unsupported
        );
        assert_eq!(
            liveness_from_wire(Some("unreachable")),
            LivenessVerdict::Unreachable
        );

        for word in [Some("looks-fine"), None] {
            assert_eq!(
                liveness_from_wire(word),
                LivenessVerdict::Unreachable,
                "{word:?} was read as a verdict about the credential"
            );
        }
    }

    /// The daemon's three words, and the rule for a fourth.
    #[test]
    fn a_verdict_word_this_build_does_not_know_is_never_a_pass() {
        assert_eq!(format_from_wire(Some("ok")), FormatVerdict::Ok);
        assert_eq!(format_from_wire(Some("invalid")), FormatVerdict::Invalid);
        assert_eq!(format_from_wire(Some("unknown")), FormatVerdict::Unknown);

        // A daemon newer than this build.
        assert_eq!(
            format_from_wire(Some("probably-fine")),
            FormatVerdict::Unknown,
            "an unrecognised verdict must not read as a pass"
        );
        assert_eq!(format_from_wire(None), FormatVerdict::Unknown);
    }

    /// The arguments an agent sends. `liveness` has to be optional,
    /// or every caller is forced to opt out of a network call it
    /// never wanted.
    #[test]
    fn liveness_is_off_unless_asked_for() {
        let args: SecretsValidateArgs =
            serde_json::from_value(serde_json::json!({"path": "team/gitlab/token"})).unwrap();

        assert_eq!(args.path, "team/gitlab/token");
        assert!(!args.liveness);
    }

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
