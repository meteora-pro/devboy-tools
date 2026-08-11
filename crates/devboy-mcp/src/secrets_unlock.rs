//! `secrets_unlock` and `secrets_status` — the agent's view of the
//! vault's lock state (ADR-024 §1/§3, Ф6d-2).
//!
//! # What an agent is allowed to know
//!
//! Both replies are metadata only. `secrets_status` says whether
//! the vault is open, how long the current unlock lasts, which
//! methods are worth trying and how much the daemon's own position
//! is worth. `secrets_unlock` forwards a six-digit code and reports
//! whether it worked. Neither carries a secret, and neither can:
//! the values live behind the daemon, and the reply types are
//! fenced by [`crate::agent_safety::AgentSafeReply`].
//!
//! # Why the refusals are not one refusal
//!
//! An agent that receives a flat "denied" retries. The four
//! outcomes here want four different next moves — wait for the next
//! code, ask the user for the passphrase, look at the authenticator
//! again, or stop entirely — so they arrive as distinct kinds with
//! the remediation attached, per §8.
//!
//! # Why `available_methods` matters
//!
//! It is the difference between an agent that asks for a TOTP code
//! the user cannot supply and one that asks for the passphrase.
//! The daemon drops `totp` from the list when no secret is resident
//! *or* when it was started by the agent — in the second case a
//! code proves nothing, because the secret is readable from the
//! daemon's memory.

use serde::{Deserialize, Serialize};

use crate::agent_safety::AgentSafeReply;
use crate::remediation::{Remediation, RemediationContext, SecretsErrorKind};

/// Arguments for `secrets_unlock`.
#[derive(Debug, Clone, Deserialize)]
pub struct SecretsUnlockArgs {
    /// The six-digit code from the user's authenticator.
    pub totp: String,
    /// How long the unlock should last. Clamped by the daemon to
    /// the user's configured ceiling — an agent cannot ask for a
    /// longer window than the user allowed.
    #[serde(default)]
    pub duration_seconds: Option<u64>,
}

/// Reply from `secrets_unlock`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SecretsUnlockReply {
    /// Whether the vault is now open.
    pub unlocked: bool,
    /// How long the granted window lasts, when one was granted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted_seconds: Option<u64>,
    /// What to do next when the unlock did not happen.
    ///
    /// Absent on success, so its presence is itself the signal that
    /// something needs acting on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<Remediation>,
}

impl AgentSafeReply for SecretsUnlockReply {}

/// Reply from `secrets_status`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SecretsStatusReply {
    /// `"locked"` or `"unlocked"`.
    pub state: String,
    /// Seconds left on the current unlock, when open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_seconds: Option<u64>,
    /// Unlock methods worth attempting right now.
    pub available_methods: Vec<String>,
    /// How much the daemon's own position is worth (ADR-024 §7).
    pub trust_level: String,
}

impl AgentSafeReply for SecretsStatusReply {}

/// Map a daemon JSON-RPC error code onto its agent-facing kind.
///
/// The daemon's codes and the agent's kinds are deliberately
/// separate vocabularies — one is a wire protocol, the other is
/// advice — and this is the single place they meet.
pub fn kind_for_daemon_code(code: i32) -> SecretsErrorKind {
    use devboy_secrets_agent::rpc;

    match code {
        rpc::TOTP_UNAVAILABLE => SecretsErrorKind::TotpUnavailable,
        rpc::BAD_TOTP => SecretsErrorKind::BadTotp,
        rpc::REPLAYED_TOTP => SecretsErrorKind::ReplayedCode,
        rpc::TOTP_RATE_LIMITED => SecretsErrorKind::RateLimited,
        rpc::VAULT_LOCKED => SecretsErrorKind::LockedTotpAvailable,
        rpc::DAEMON_UNTRUSTED => SecretsErrorKind::DaemonUntrusted,
        // Anything unrecognised is treated as the daemon not being
        // usable rather than as a bad code: guessing "your code was
        // wrong" from an unknown failure would send the agent back
        // to the user for no reason.
        _ => SecretsErrorKind::DaemonNotRunning,
    }
}

/// Build the failure reply for a daemon refusal.
pub fn unlock_failure(code: i32, ctx: &RemediationContext) -> SecretsUnlockReply {
    SecretsUnlockReply {
        unlocked: false,
        granted_seconds: None,
        remediation: Some(kind_for_daemon_code(code).remediation(ctx)),
    }
}

/// Build the success reply.
pub fn unlock_success(granted_seconds: u64) -> SecretsUnlockReply {
    SecretsUnlockReply {
        unlocked: true,
        granted_seconds: Some(granted_seconds),
        remediation: None,
    }
}

/// Run `secrets_unlock` against the daemon.
///
/// A daemon that is not running is reported as such rather than as
/// a bad code: the agent's next move is to ask the user to start
/// it, not to fetch another code.
#[cfg(unix)]
pub fn unlock(args: &SecretsUnlockArgs, ctx: &RemediationContext) -> SecretsUnlockReply {
    use devboy_secrets_agent::{AgentClient, ClientError};

    let Some(client) = AgentClient::new() else {
        return SecretsUnlockReply {
            unlocked: false,
            granted_seconds: None,
            remediation: Some(SecretsErrorKind::DaemonNotRunning.remediation(ctx)),
        };
    };

    match client.totp_unlock(&args.totp, args.duration_seconds) {
        Ok(result) => unlock_success(
            result
                .get("granted_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        ),
        Err(ClientError::Daemon(e)) => unlock_failure(e.code, ctx),
        Err(_) => SecretsUnlockReply {
            unlocked: false,
            granted_seconds: None,
            remediation: Some(SecretsErrorKind::DaemonNotRunning.remediation(ctx)),
        },
    }
}

/// Read `secrets_status` from the daemon.
///
/// A stopped daemon is reported as locked with no methods
/// available, which is the truthful answer: nothing can be unlocked
/// until it runs.
#[cfg(unix)]
pub fn status() -> SecretsStatusReply {
    use devboy_secrets_agent::AgentClient;

    let unreachable = || SecretsStatusReply {
        state: "locked".to_owned(),
        expires_in_seconds: None,
        available_methods: Vec::new(),
        trust_level: "unknown".to_owned(),
    };

    let Some(client) = AgentClient::new() else {
        return unreachable();
    };
    let Ok(result) = client.status() else {
        return unreachable();
    };

    SecretsStatusReply {
        state: result
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("locked")
            .to_owned(),
        expires_in_seconds: result.get("unlock_ttl_seconds").and_then(|v| v.as_u64()),
        available_methods: result
            .get("available_methods")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|m| m.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        trust_level: result
            .get("trust_level")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_secrets_agent::rpc;

    fn ctx() -> RemediationContext {
        RemediationContext::for_path("team/github/token")
    }

    /// Four daemon refusals must stay four agent kinds. Collapsing
    /// them is how an agent ends up retrying a code that can never
    /// work.
    #[test]
    fn each_daemon_refusal_maps_to_its_own_kind() {
        let kinds = [
            kind_for_daemon_code(rpc::TOTP_UNAVAILABLE),
            kind_for_daemon_code(rpc::BAD_TOTP),
            kind_for_daemon_code(rpc::REPLAYED_TOTP),
            kind_for_daemon_code(rpc::TOTP_RATE_LIMITED),
        ];

        let unique: std::collections::BTreeSet<String> =
            kinds.iter().map(|k| format!("{k:?}")).collect();
        assert_eq!(unique.len(), 4, "kinds collapsed: {kinds:?}");
    }

    /// An unknown failure must not be reported as a bad code —
    /// that would send the agent back to the user for a fresh code
    /// when the real problem is the daemon.
    #[test]
    fn an_unknown_code_is_not_reported_as_a_bad_code() {
        let kind = kind_for_daemon_code(-99999);
        assert_ne!(
            format!("{kind:?}"),
            format!("{:?}", SecretsErrorKind::BadTotp)
        );
    }

    /// Every refusal carries advice; a bare `unlocked: false` gives
    /// the agent nothing to act on.
    #[test]
    fn every_failure_reply_carries_remediation() {
        for code in [
            rpc::TOTP_UNAVAILABLE,
            rpc::BAD_TOTP,
            rpc::REPLAYED_TOTP,
            rpc::TOTP_RATE_LIMITED,
            rpc::VAULT_LOCKED,
            -1,
        ] {
            let reply = unlock_failure(code, &ctx());
            assert!(!reply.unlocked);
            assert!(
                reply.remediation.is_some(),
                "code {code} produced a reply with no advice"
            );
        }
    }

    #[test]
    fn a_success_reply_carries_no_remediation() {
        let reply = unlock_success(900);
        assert!(reply.unlocked);
        assert_eq!(reply.granted_seconds, Some(900));
        assert!(
            reply.remediation.is_none(),
            "advice on a success is noise the agent has to filter"
        );
    }

    /// The replies are the agent's whole view, and a value reaching
    /// them would undo the boundary the rest of the framework
    /// maintains.
    #[test]
    fn neither_reply_can_carry_a_value() {
        let unlock = serde_json::to_string(&unlock_success(900)).unwrap();
        assert!(!unlock.contains("value"), "{unlock}");
        assert!(!unlock.contains("secret"), "{unlock}");

        let status = serde_json::to_string(&SecretsStatusReply {
            state: "unlocked".into(),
            expires_in_seconds: Some(900),
            available_methods: vec!["passphrase".into(), "totp".into()],
            trust_level: "independent".into(),
        })
        .unwrap();
        assert!(!status.contains("\"value\""), "{status}");
    }

    /// With no daemon running, both tools must answer truthfully
    /// rather than pretending: an agent told "bad code" would go
    /// ask the user for another one.
    #[cfg(unix)]
    #[test]
    fn a_stopped_daemon_is_reported_as_such() {
        // No daemon is running under the test harness.
        let reply = status();
        assert_eq!(reply.state, "locked");
        assert!(
            reply.available_methods.is_empty(),
            "nothing can be unlocked while the daemon is down"
        );
    }

    /// A rate-limited refusal should tell the agent to wait rather
    /// than to ask the user for anything.
    #[test]
    fn a_rate_limited_refusal_is_retryable_by_the_agent() {
        let reply = unlock_failure(rpc::TOTP_RATE_LIMITED, &ctx());
        let r = reply.remediation.expect("advice");
        assert!(
            r.retryable,
            "a cooldown ends by itself, so the agent should wait rather than escalate"
        );
    }

    /// ...whereas a missing secret needs a human, and telling the
    /// agent to retry would loop it forever.
    #[test]
    fn an_unavailable_totp_path_is_not_retryable() {
        let reply = unlock_failure(rpc::TOTP_UNAVAILABLE, &ctx());
        let r = reply.remediation.expect("advice");
        assert!(
            !r.retryable,
            "no amount of retrying makes an absent secret appear"
        );
    }
}
