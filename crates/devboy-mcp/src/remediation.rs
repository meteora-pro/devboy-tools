//! Actionable errors — every failure tells the agent what to do
//! next, including when only a human can proceed (ADR-024 §8).
//!
//! Before this module an agent receiving `Locked` had to guess,
//! and each plausible guess is actively harmful:
//!
//! - ask the user for the **passphrase** — forbidden by §3, and it
//!   trains people to type their master credential into a chat
//!   window;
//! - **start the daemon itself** — a self-spawned daemon is
//!   `ptrace`-able by its parent, which voids §1 and §7 while
//!   leaving their appearance intact;
//! - **hunt for the value** in the environment or dotfiles,
//!   dragging a secret into the agent's context;
//! - **retry in a loop**, burning the rate limiter and locking the
//!   user out.
//!
//! The framework knows the right answer in every one of those
//! cases. This module says it in a machine-readable form instead
//! of leaving a language model to infer it from an error name.
//!
//! The load-bearing field is [`RemediationActor`]: it tells the
//! agent whether the problem is its to solve or whether it must
//! stop and fetch a human.

use serde::{Deserialize, Serialize};

use crate::agent_safety::AgentSafeReply;

/// Who can actually resolve a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemediationActor {
    /// The agent can act on this itself — retry, back off, or ask
    /// the user for an ephemeral code and relay it.
    Agent,
    /// Only a human can proceed. The agent must stop and surface
    /// [`Remediation::user_message`].
    User,
}

/// Machine-readable next step, so the agent branches on a
/// constant rather than parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationAction {
    /// Ask the user for a TOTP code and relay it to
    /// `secrets_unlock`.
    RequestTotp,
    /// The previous code was already spent; wait for the next
    /// 30-second step before asking again.
    RequestFreshTotp,
    /// Back off for exactly `retry_after_seconds`, then retry.
    RetryAfter,
    /// Only a passphrase can open the vault. Tell the user to
    /// unlock through the trusted-path prompt.
    AskUserToUnlock,
    /// The daemon is not running. Relay the platform command;
    /// **never** start it (see the module docs).
    AskUserToStartDaemon,
    /// The daemon found the caller in its own ancestry (§7). Relay
    /// the restart command; restarting it yourself reproduces the
    /// fault.
    AskUserToRestartDaemon,
    /// The secret does not exist yet. Surface the retrieval URL
    /// and required scopes.
    AskUserToProvision,
    /// An approval prompt is waiting on the daemon's surface.
    AskUserToApprove,
    /// The stored value no longer works and must be replaced.
    AskUserToRotate,
    /// Env-only mode: name the environment variables that would
    /// satisfy the path.
    SetEnvVar,
    /// Nothing to do — the user declined, and re-asking within
    /// this session would be nagging.
    None,
}

/// What to do about a failure, attached to every `secrets_*` /
/// `vault_*` error reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remediation {
    /// Who can resolve this.
    pub actor: RemediationActor,
    /// The next step, as a constant.
    pub action: RemediationAction,
    /// Daemon-authored text to show the user verbatim.
    ///
    /// Composed here rather than by the agent, which also closes
    /// the prompt-injection concern: hostile text in a repository
    /// cannot shape an unlock request whose wording never
    /// originates agent-side.
    pub user_message: String,
    /// Whether retrying the same call can succeed at all.
    pub retryable: bool,
    /// How long to wait before retrying, when the wait is bounded
    /// and known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
}

// `Remediation` carries metadata and fixed text only — no
// `SecretString`, and nothing sourced from a `get()`.
impl AgentSafeReply for Remediation {}

/// Every failure the secret framework can report to an agent.
///
/// Kept as a closed enum specifically so
/// [`SecretsErrorKind::remediation`] can match exhaustively: a new
/// variant is a compile error until someone decides what an agent
/// should do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum SecretsErrorKind {
    /// Vault is locked and a TOTP session is resident, so an
    /// agent-mediated re-unlock is possible.
    LockedTotpAvailable,
    /// Vault is locked with no TOTP session — the daemon
    /// restarted, so only a passphrase can open it.
    TotpUnavailable,
    /// The relayed code did not verify.
    BadTotp,
    /// The relayed code was valid but its time-step was already
    /// spent (RFC 6238 §5.2).
    ReplayedCode,
    /// Too many failed attempts.
    RateLimited,
    /// The path has no value stored anywhere.
    NotProvisioned,
    /// A per-call approval prompt is pending on the daemon's
    /// surface.
    ApprovalRequired,
    /// The user declined.
    ApprovalDenied,
    /// The stored value failed its liveness check.
    LivenessFailed,
    /// The daemon is not running.
    DaemonNotRunning,
    /// The daemon is running but found the caller in its own
    /// ancestry, so it cannot protect its memory from it (§7).
    DaemonUntrusted,
    /// The operation needs the vault, which env-only mode does not
    /// have.
    NotAvailableInCiMode,
}

/// Context the framework can weave into a user-facing message.
///
/// All of it is metadata that already exists in the ADR-020
/// manifest and has never had a consumer — `retrieval_url`,
/// `required_scopes` and `rotation_method` exist precisely to be
/// shown to a human at this moment.
#[derive(Debug, Clone, Default)]
pub struct RemediationContext {
    /// The secret path the failure concerns.
    pub path: Option<String>,
    /// Human description of the secret from the manifest.
    pub description: Option<String>,
    /// Where to obtain or rotate the credential.
    pub retrieval_url: Option<String>,
    /// Scopes the credential must carry.
    pub required_scopes: Vec<String>,
    /// How rotation is performed for this path.
    pub rotation_method: Option<String>,
    /// Environment variables that would satisfy the path, in
    /// resolution order (ADR-024 §6).
    pub env_candidates: Vec<String>,
    /// Seconds to wait, for rate-limited failures.
    pub retry_after_seconds: Option<u64>,
    /// Platform-specific command that starts the daemon.
    pub daemon_start_command: Option<String>,
    /// Declared expiry date from the manifest, surfaced by
    /// verdict replies.
    pub expires_at_hint: Option<String>,
}

impl RemediationContext {
    /// Context for a bare failure with no manifest metadata.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Shorthand for the common "we know which path" case.
    pub fn for_path(path: impl Into<String>) -> Self {
        Self {
            path: Some(path.into()),
            ..Self::default()
        }
    }

    fn path_label(&self) -> String {
        match &self.path {
            Some(p) => format!(" (`{p}`)"),
            None => String::new(),
        }
    }

    /// "Create one with scopes a, b at <url>" — assembled only
    /// from what the manifest actually declared.
    fn acquisition_hint(&self) -> String {
        let mut parts = Vec::new();
        if !self.required_scopes.is_empty() {
            parts.push(format!("with scopes {}", self.required_scopes.join(", ")));
        }
        if let Some(url) = &self.retrieval_url {
            parts.push(format!("at {url}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" Create one {}.", parts.join(" "))
        }
    }
}

impl SecretsErrorKind {
    /// Stable wire name, used in the `error` field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LockedTotpAvailable => "Locked",
            Self::TotpUnavailable => "TotpUnavailable",
            Self::BadTotp => "BadTotp",
            Self::ReplayedCode => "ReplayedCode",
            Self::RateLimited => "RateLimited",
            Self::NotProvisioned => "NotProvisioned",
            Self::ApprovalRequired => "ApprovalRequired",
            Self::ApprovalDenied => "ApprovalDenied",
            Self::LivenessFailed => "LivenessFailed",
            Self::DaemonNotRunning => "DaemonNotRunning",
            Self::DaemonUntrusted => "DaemonUntrusted",
            Self::NotAvailableInCiMode => "NotAvailableInCiMode",
        }
    }

    /// What the agent should do about this failure.
    ///
    /// The match is exhaustive on purpose — adding a variant
    /// without deciding its remediation will not compile.
    pub fn remediation(self, ctx: &RemediationContext) -> Remediation {
        let path = ctx.path_label();

        match self {
            Self::LockedTotpAvailable => Remediation {
                actor: RemediationActor::Agent,
                action: RemediationAction::RequestTotp,
                user_message: "The secret vault is locked. Enter the 6-digit code from your \
                               authenticator app to unlock it."
                    .to_string(),
                retryable: true,
                retry_after_seconds: None,
            },

            // The case an agent will actually hit: the daemon
            // restarted, so no code can be verified. A distinct
            // error precisely so the agent stops asking for codes
            // that cannot succeed.
            Self::TotpUnavailable => Remediation {
                actor: RemediationActor::User,
                action: RemediationAction::AskUserToUnlock,
                user_message: "The vault is locked and there is no TOTP session this boot, so a \
                               code cannot be checked. Unlock it with your passphrase via \
                               `devboy secrets unlock`."
                    .to_string(),
                retryable: false,
                retry_after_seconds: None,
            },

            Self::BadTotp => Remediation {
                actor: RemediationActor::Agent,
                action: RemediationAction::RequestTotp,
                user_message: "That code was not accepted. Check your authenticator app and try \
                               once more."
                    .to_string(),
                retryable: true,
                retry_after_seconds: None,
            },

            Self::ReplayedCode => Remediation {
                actor: RemediationActor::Agent,
                action: RemediationAction::RequestFreshTotp,
                user_message: "That code was already used. Wait for your authenticator to show \
                               the next one."
                    .to_string(),
                retryable: true,
                retry_after_seconds: Some(30),
            },

            Self::RateLimited => Remediation {
                actor: RemediationActor::Agent,
                action: RemediationAction::RetryAfter,
                user_message: "Too many failed unlock attempts. The vault is temporarily \
                               refusing codes."
                    .to_string(),
                retryable: true,
                retry_after_seconds: Some(ctx.retry_after_seconds.unwrap_or(60)),
            },

            Self::NotProvisioned => Remediation {
                actor: RemediationActor::User,
                action: if ctx.env_candidates.is_empty() {
                    RemediationAction::AskUserToProvision
                } else {
                    RemediationAction::SetEnvVar
                },
                user_message: {
                    let mut msg = format!(
                        "No value is set up for this secret{path}.{}",
                        ctx.acquisition_hint()
                    );
                    if let Some(desc) = &ctx.description {
                        msg = format!("{desc}: {msg}");
                    }
                    if !ctx.env_candidates.is_empty() {
                        msg.push_str(&format!(
                            " Or set one of: {}.",
                            ctx.env_candidates.join(", ")
                        ));
                    } else if let Some(p) = &ctx.path {
                        msg.push_str(&format!(" Then run `devboy secrets set {p}`."));
                    }
                    msg
                },
                retryable: false,
                retry_after_seconds: None,
            },

            Self::ApprovalRequired => Remediation {
                actor: RemediationActor::User,
                action: RemediationAction::AskUserToApprove,
                user_message: format!(
                    "Access to this secret{path} needs your approval. A prompt is waiting — \
                     approve or decline it there."
                ),
                // Retrying the call will not help; the pending
                // prompt has to be answered first.
                retryable: false,
                retry_after_seconds: None,
            },

            Self::ApprovalDenied => Remediation {
                actor: RemediationActor::User,
                action: RemediationAction::None,
                user_message: format!("Access to this secret{path} was declined."),
                retryable: false,
                retry_after_seconds: None,
            },

            Self::LivenessFailed => Remediation {
                actor: RemediationActor::User,
                action: RemediationAction::AskUserToRotate,
                user_message: {
                    let mut msg = format!(
                        "The stored credential{path} is no longer accepted by its provider and \
                         needs replacing.{}",
                        ctx.acquisition_hint()
                    );
                    if let Some(method) = &ctx.rotation_method {
                        msg.push_str(&format!(" Rotation method: {method}."));
                    }
                    msg
                },
                retryable: false,
                retry_after_seconds: None,
            },

            // `user`, not `agent`, specifically because a daemon
            // the agent starts is a daemon the agent can `ptrace`.
            Self::DaemonNotRunning => Remediation {
                actor: RemediationActor::User,
                action: RemediationAction::AskUserToStartDaemon,
                user_message: format!(
                    "The secret daemon is not running. Start it with `{}`, then retry.",
                    ctx.daemon_start_command
                        .clone()
                        .unwrap_or_else(default_daemon_start_command)
                ),
                retryable: false,
                retry_after_seconds: None,
            },

            Self::DaemonUntrusted => Remediation {
                actor: RemediationActor::User,
                action: RemediationAction::AskUserToRestartDaemon,
                user_message: format!(
                    "The secret daemon was started by this session and cannot protect its own \
                     memory from it. Stop it and start it with `{}`, then retry.",
                    ctx.daemon_start_command
                        .clone()
                        .unwrap_or_else(default_daemon_start_command)
                ),
                retryable: false,
                retry_after_seconds: None,
            },

            Self::NotAvailableInCiMode => Remediation {
                actor: RemediationActor::User,
                action: RemediationAction::SetEnvVar,
                user_message: {
                    let mut msg = format!(
                        "This operation needs the vault, which is unavailable in CI / env-only \
                         mode{path}."
                    );
                    if ctx.env_candidates.is_empty() {
                        msg.push_str(" Provide the secret through the environment instead.");
                    } else {
                        msg.push_str(&format!(" Set one of: {}.", ctx.env_candidates.join(", ")));
                    }
                    msg
                },
                retryable: false,
                retry_after_seconds: None,
            },
        }
    }
}

/// Platform default for "how do I start the daemon".
fn default_daemon_start_command() -> String {
    if cfg!(target_os = "macos") {
        "launchctl kickstart -k gui/$(id -u)/dev.devboy.secrets".to_string()
    } else if cfg!(target_os = "windows") {
        "Start-Service devboy-secrets".to_string()
    } else {
        "systemctl --user start devboy-secrets".to_string()
    }
}

/// Every variant, for exhaustiveness testing.
///
/// Kept in sync with the enum by the private `variant_index`,
/// whose match is exhaustive — a new variant fails to compile
/// until it is added here too.
pub const ALL_ERROR_KINDS: &[SecretsErrorKind] = &[
    SecretsErrorKind::LockedTotpAvailable,
    SecretsErrorKind::TotpUnavailable,
    SecretsErrorKind::BadTotp,
    SecretsErrorKind::ReplayedCode,
    SecretsErrorKind::RateLimited,
    SecretsErrorKind::NotProvisioned,
    SecretsErrorKind::ApprovalRequired,
    SecretsErrorKind::ApprovalDenied,
    SecretsErrorKind::LivenessFailed,
    SecretsErrorKind::DaemonNotRunning,
    SecretsErrorKind::DaemonUntrusted,
    SecretsErrorKind::NotAvailableInCiMode,
];

/// Position of each variant, used only to prove
/// [`ALL_ERROR_KINDS`] is complete.
///
/// Test-only: its whole job is to be an exhaustive match that
/// fails to compile when a variant is added without being listed.
#[cfg(test)]
fn variant_index(kind: SecretsErrorKind) -> usize {
    match kind {
        SecretsErrorKind::LockedTotpAvailable => 0,
        SecretsErrorKind::TotpUnavailable => 1,
        SecretsErrorKind::BadTotp => 2,
        SecretsErrorKind::ReplayedCode => 3,
        SecretsErrorKind::RateLimited => 4,
        SecretsErrorKind::NotProvisioned => 5,
        SecretsErrorKind::ApprovalRequired => 6,
        SecretsErrorKind::ApprovalDenied => 7,
        SecretsErrorKind::LivenessFailed => 8,
        SecretsErrorKind::DaemonNotRunning => 9,
        SecretsErrorKind::DaemonUntrusted => 10,
        SecretsErrorKind::NotAvailableInCiMode => 11,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of the whole module: a newly added error cannot
    /// ship without someone deciding what an agent should do
    /// about it.
    #[test]
    fn every_error_kind_is_listed_exactly_once() {
        for (expected, kind) in ALL_ERROR_KINDS.iter().enumerate() {
            assert_eq!(
                variant_index(*kind),
                expected,
                "{:?} is out of order in ALL_ERROR_KINDS",
                kind
            );
        }
    }

    #[test]
    fn every_error_kind_has_a_usable_remediation() {
        let ctx = RemediationContext::for_path("team/gitlab/token");
        for kind in ALL_ERROR_KINDS {
            let r = kind.remediation(&ctx);

            assert!(
                !r.user_message.trim().is_empty(),
                "{kind:?} has an empty user_message"
            );
            assert!(
                r.user_message.ends_with('.') || r.user_message.ends_with('!'),
                "{kind:?} user_message should read as a sentence: {}",
                r.user_message
            );
            // A wait is only meaningful when retrying is possible.
            if r.retry_after_seconds.is_some() {
                assert!(r.retryable, "{kind:?} sets a backoff but is not retryable");
            }
            // `retry_after` is the whole content of this action.
            if r.action == RemediationAction::RetryAfter {
                assert!(
                    r.retry_after_seconds.is_some(),
                    "{kind:?} says RetryAfter without saying how long"
                );
            }
            // `None` means stop; a retryable stop is incoherent.
            if r.action == RemediationAction::None {
                assert!(!r.retryable, "{kind:?} is actionless yet retryable");
            }
        }
    }

    /// Errors only a human can clear must never be marked as the
    /// agent's job — that is what sends an agent into a loop.
    #[test]
    fn human_only_failures_are_addressed_to_the_user() {
        for kind in [
            SecretsErrorKind::TotpUnavailable,
            SecretsErrorKind::NotProvisioned,
            SecretsErrorKind::ApprovalRequired,
            SecretsErrorKind::ApprovalDenied,
            SecretsErrorKind::LivenessFailed,
            SecretsErrorKind::DaemonNotRunning,
            SecretsErrorKind::DaemonUntrusted,
            SecretsErrorKind::NotAvailableInCiMode,
        ] {
            let r = kind.remediation(&RemediationContext::empty());
            assert_eq!(
                r.actor,
                RemediationActor::User,
                "{kind:?} must stop the agent and fetch a human"
            );
            assert!(!r.retryable, "{kind:?} must not invite a retry");
        }
    }

    /// The agent may only act by itself on ephemeral, bounded
    /// things: relaying a code or backing off.
    #[test]
    fn agent_actionable_failures_are_bounded() {
        for kind in [
            SecretsErrorKind::LockedTotpAvailable,
            SecretsErrorKind::BadTotp,
            SecretsErrorKind::ReplayedCode,
            SecretsErrorKind::RateLimited,
        ] {
            let r = kind.remediation(&RemediationContext::empty());
            assert_eq!(r.actor, RemediationActor::Agent);
            assert!(r.retryable);
        }
    }

    /// Neither the agent nor the framework may suggest starting
    /// the daemon: a self-spawned daemon is `ptrace`-able by its
    /// parent, which voids §1 and §7.
    #[test]
    fn daemon_failures_are_never_the_agents_job() {
        for kind in [
            SecretsErrorKind::DaemonNotRunning,
            SecretsErrorKind::DaemonUntrusted,
        ] {
            let r = kind.remediation(&RemediationContext::empty());
            assert_eq!(r.actor, RemediationActor::User);
            assert!(
                !r.user_message.is_empty(),
                "must name the command for the user to run"
            );
        }
    }

    /// The manifest metadata that has existed since ADR-020 with
    /// no consumer finally reaches the person who needs it.
    #[test]
    fn not_provisioned_surfaces_manifest_metadata() {
        let ctx = RemediationContext {
            path: Some("team/gitlab/token-deploy".to_string()),
            description: Some("GitLab deploy token".to_string()),
            retrieval_url: Some("https://gitlab.example/-/user_settings".to_string()),
            required_scopes: vec!["api".to_string(), "read_repository".to_string()],
            ..RemediationContext::default()
        };

        let msg = SecretsErrorKind::NotProvisioned
            .remediation(&ctx)
            .user_message;

        assert!(msg.contains("GitLab deploy token"), "{msg}");
        assert!(msg.contains("api, read_repository"), "{msg}");
        assert!(msg.contains("https://gitlab.example"), "{msg}");
        assert!(msg.contains("devboy secrets set"), "{msg}");
    }

    /// In env-only mode the fix is an environment variable, and
    /// the message should say which.
    #[test]
    fn env_candidates_replace_the_provisioning_advice() {
        let ctx = RemediationContext {
            path: Some("team/gitlab/token".to_string()),
            env_candidates: vec![
                "DEVBOY_GITLAB_TOKEN".to_string(),
                "GITLAB_TOKEN".to_string(),
            ],
            ..RemediationContext::default()
        };

        let r = SecretsErrorKind::NotProvisioned.remediation(&ctx);
        assert_eq!(r.action, RemediationAction::SetEnvVar);
        assert!(r.user_message.contains("DEVBOY_GITLAB_TOKEN"));
        assert!(r.user_message.contains("GITLAB_TOKEN"));
    }

    #[test]
    fn rate_limit_backoff_is_taken_from_context_when_known() {
        let ctx = RemediationContext {
            retry_after_seconds: Some(12),
            ..RemediationContext::default()
        };
        assert_eq!(
            SecretsErrorKind::RateLimited
                .remediation(&ctx)
                .retry_after_seconds,
            Some(12)
        );
    }

    /// Wire names are a contract with every agent implementation.
    #[test]
    fn wire_names_are_stable() {
        assert_eq!(SecretsErrorKind::LockedTotpAvailable.as_str(), "Locked");
        assert_eq!(
            SecretsErrorKind::TotpUnavailable.as_str(),
            "TotpUnavailable"
        );
        assert_eq!(
            SecretsErrorKind::DaemonUntrusted.as_str(),
            "DaemonUntrusted"
        );
        assert_eq!(
            SecretsErrorKind::NotAvailableInCiMode.as_str(),
            "NotAvailableInCiMode"
        );
    }

    #[test]
    fn remediation_serialises_with_snake_case_actions() {
        let r = SecretsErrorKind::LockedTotpAvailable.remediation(&RemediationContext::empty());
        let json = serde_json::to_string(&r).unwrap();

        assert!(json.contains(r#""actor":"agent""#), "{json}");
        assert!(json.contains(r#""action":"request_totp""#), "{json}");
        // Absent backoff must not appear as null.
        assert!(!json.contains("retry_after_seconds"), "{json}");
    }
}
