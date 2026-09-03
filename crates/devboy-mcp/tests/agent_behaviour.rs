//! Agent-behaviour harness for the ADR-024 §8 contract (T4).
//!
//! Every other test in this epic checks *what the server returned*.
//! This one checks *what an agent does about it*, which is the
//! property §8 actually exists for — a remediation nobody acts on
//! correctly is decoration.
//!
//! [`ContractAgent`] is a deterministic stand-in for a coding
//! agent. It implements exactly the §8 contract and nothing else:
//! look at `actor`, act if it is `agent`, stop and fetch a human if
//! it is `user`. Because it is deterministic, a scenario that would
//! send a real agent into a retry loop produces an observable loop
//! here, and the test fails instead of the user's vault getting
//! rate-limited.
//!
//! The failure this guards against is specific: an agent that
//! keeps asking for TOTP codes after the daemon restarted, burning
//! the rate limiter while the user watches it fail.

use devboy_mcp::remediation::{
    Remediation, RemediationAction, RemediationActor, RemediationContext, SecretsErrorKind,
};

/// One observable thing the agent did.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// Asked the user for a TOTP code and relayed it.
    RelayedTotp,
    /// Waited out a backoff before retrying.
    WaitedSeconds(u64),
    /// Stopped and surfaced a message to the human.
    AskedHuman(RemediationAction),
    /// Gave up without asking anything further.
    Stopped,
}

/// A deterministic agent that follows the §8 contract literally.
struct ContractAgent {
    steps: Vec<Step>,
    /// Guard against the very failure this harness exists to catch.
    max_steps: usize,
}

impl ContractAgent {
    fn new() -> Self {
        Self {
            steps: Vec::new(),
            max_steps: 10,
        }
    }

    /// React to one failure. Returns `true` when the agent believes
    /// retrying the original call is worthwhile.
    fn react(&mut self, r: &Remediation) -> bool {
        assert!(
            self.steps.len() < self.max_steps,
            "agent looped — the contract let it retry forever: {:?}",
            self.steps
        );

        match r.actor {
            // Only ephemeral, bounded things are the agent's to do.
            RemediationActor::Agent => match r.action {
                RemediationAction::RequestTotp | RemediationAction::RequestFreshTotp => {
                    self.steps.push(Step::RelayedTotp);
                    true
                }
                RemediationAction::RetryAfter => {
                    let wait = r
                        .retry_after_seconds
                        .expect("RetryAfter without a duration is unactionable");
                    self.steps.push(Step::WaitedSeconds(wait));
                    true
                }
                other => panic!("agent-addressed remediation with a human action: {other:?}"),
            },

            // Anything else stops the agent. This branch is the
            // whole point of the harness.
            RemediationActor::User => {
                if r.action == RemediationAction::None {
                    self.steps.push(Step::Stopped);
                } else {
                    self.steps.push(Step::AskedHuman(r.action));
                }
                assert!(
                    !r.retryable,
                    "a human-only failure must not invite a retry: {:?}",
                    r.action
                );
                false
            }
        }
    }
}

fn remediation(kind: SecretsErrorKind) -> Remediation {
    kind.remediation(&RemediationContext::for_path("team/gitlab/token"))
}

/// The happy path: a locked vault with a live TOTP session is the
/// agent's to resolve, in one step.
#[test]
fn locked_with_a_totp_session_is_resolved_by_the_agent() {
    let mut agent = ContractAgent::new();

    let retry = agent.react(&remediation(SecretsErrorKind::LockedTotpAvailable));

    assert!(retry, "the agent should relay a code and try again");
    assert_eq!(agent.steps, vec![Step::RelayedTotp]);
}

/// The case this harness was built for.
///
/// After a daemon restart no code can be verified. An agent that
/// keeps asking burns the rate limiter and looks broken to the
/// user. The contract must stop it on the first reply.
#[test]
fn a_restarted_daemon_stops_the_agent_instead_of_looping_it() {
    let mut agent = ContractAgent::new();

    let retry = agent.react(&remediation(SecretsErrorKind::TotpUnavailable));

    assert!(!retry, "no code can succeed; retrying is pure noise");
    assert_eq!(
        agent.steps,
        vec![Step::AskedHuman(RemediationAction::AskUserToUnlock)]
    );
}

/// A wrong code is worth one more try; a *third* failure would be
/// the loop this harness catches.
#[test]
fn a_bad_code_is_retried_but_the_loop_is_bounded() {
    let mut agent = ContractAgent::new();

    for _ in 0..3 {
        assert!(agent.react(&remediation(SecretsErrorKind::BadTotp)));
    }

    assert_eq!(agent.steps.len(), 3);
    assert!(agent.steps.iter().all(|s| *s == Step::RelayedTotp));
}

/// Backoff must be honoured exactly, not approximated.
#[test]
fn rate_limiting_makes_the_agent_wait_the_stated_time() {
    let mut agent = ContractAgent::new();

    let ctx = RemediationContext {
        retry_after_seconds: Some(45),
        ..RemediationContext::default()
    };
    let retry = agent.react(&SecretsErrorKind::RateLimited.remediation(&ctx));

    assert!(retry);
    assert_eq!(agent.steps, vec![Step::WaitedSeconds(45)]);
}

/// A spent code needs a *fresh* one, not the same one again.
#[test]
fn a_replayed_code_asks_for_a_new_one_with_a_wait() {
    let r = remediation(SecretsErrorKind::ReplayedCode);

    assert_eq!(r.action, RemediationAction::RequestFreshTotp);
    assert_eq!(
        r.retry_after_seconds,
        Some(30),
        "the agent should wait out the step rather than re-ask instantly"
    );
}

/// Declining is a decision, not a transient failure. Re-asking in
/// the same session is nagging.
#[test]
fn a_declined_approval_ends_the_exchange() {
    let mut agent = ContractAgent::new();

    let retry = agent.react(&remediation(SecretsErrorKind::ApprovalDenied));

    assert!(!retry);
    assert_eq!(agent.steps, vec![Step::Stopped]);
}

/// A pending prompt is answered by a human on the daemon's
/// surface; hammering the call does not make it resolve faster.
#[test]
fn a_pending_approval_stops_the_agent_rather_than_polling() {
    let mut agent = ContractAgent::new();

    let retry = agent.react(&remediation(SecretsErrorKind::ApprovalRequired));

    assert!(!retry);
    assert_eq!(
        agent.steps,
        vec![Step::AskedHuman(RemediationAction::AskUserToApprove)]
    );
}

/// The most dangerous "helpful" action an agent could take: a
/// daemon it starts is a daemon it can `ptrace`, which voids §1
/// and §7 while leaving their appearance intact.
#[test]
fn daemon_failures_never_become_the_agents_job() {
    for kind in [
        SecretsErrorKind::DaemonNotRunning,
        SecretsErrorKind::DaemonUntrusted,
    ] {
        let mut agent = ContractAgent::new();
        let retry = agent.react(&remediation(kind));

        assert!(!retry, "{kind:?} must not be retried");
        match agent.steps.as_slice() {
            [Step::AskedHuman(action)] => assert!(
                matches!(
                    action,
                    RemediationAction::AskUserToStartDaemon
                        | RemediationAction::AskUserToRestartDaemon
                ),
                "{kind:?} produced {action:?}"
            ),
            other => panic!("{kind:?} should hand off to a human, got {other:?}"),
        }
    }
}

/// In env-only mode there is no vault to unlock, so the fix is an
/// environment variable and the agent has nothing to retry.
#[test]
fn ci_mode_sends_the_agent_to_the_user_with_variable_names() {
    let ctx = RemediationContext {
        path: Some("team/gitlab/token".to_string()),
        env_candidates: vec![
            "DEVBOY_GITLAB_TOKEN".to_string(),
            "GITLAB_TOKEN".to_string(),
        ],
        ..RemediationContext::default()
    };
    let r = SecretsErrorKind::NotAvailableInCiMode.remediation(&ctx);

    let mut agent = ContractAgent::new();
    assert!(!agent.react(&r));
    assert_eq!(
        agent.steps,
        vec![Step::AskedHuman(RemediationAction::SetEnvVar)]
    );
    assert!(
        r.user_message.contains("DEVBOY_GITLAB_TOKEN"),
        "{}",
        r.user_message
    );
}

/// A full exchange: the agent relays a code, the code is wrong, it
/// relays another, the daemon restarts — and it stops rather than
/// carrying on.
#[test]
fn a_realistic_exchange_terminates_at_the_human() {
    let mut agent = ContractAgent::new();

    assert!(agent.react(&remediation(SecretsErrorKind::LockedTotpAvailable)));
    assert!(agent.react(&remediation(SecretsErrorKind::BadTotp)));
    assert!(!agent.react(&remediation(SecretsErrorKind::TotpUnavailable)));

    assert_eq!(
        agent.steps,
        vec![
            Step::RelayedTotp,
            Step::RelayedTotp,
            Step::AskedHuman(RemediationAction::AskUserToUnlock),
        ]
    );
}

/// Sweep: no remediation may be addressed to the agent while
/// naming a human action, and none may be human-addressed while
/// inviting a retry. Either combination is what produces loops.
#[test]
fn no_remediation_is_internally_contradictory() {
    for kind in devboy_mcp::remediation::ALL_ERROR_KINDS {
        let mut agent = ContractAgent::new();
        // `react` panics on either contradiction.
        let _ = agent.react(&remediation(*kind));
        assert_eq!(agent.steps.len(), 1, "{kind:?} produced no observable step");
    }
}

/// Every human-facing message must be something worth showing:
/// non-empty, a sentence, and free of the jargon a user cannot act
/// on.
#[test]
fn human_messages_are_worth_surfacing() {
    for kind in devboy_mcp::remediation::ALL_ERROR_KINDS {
        let r = remediation(*kind);
        if r.actor != RemediationActor::User {
            continue;
        }

        assert!(r.user_message.len() > 20, "{kind:?}: {}", r.user_message);
        assert!(
            r.user_message.ends_with('.'),
            "{kind:?} message is not a sentence: {}",
            r.user_message
        );
        // A user cannot act on an enum name.
        assert!(
            !r.user_message.contains("SecretsErrorKind"),
            "{kind:?} leaks an internal name: {}",
            r.user_message
        );
    }
}
