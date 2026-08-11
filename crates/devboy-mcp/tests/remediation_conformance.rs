//! Conformance suite for the ADR-024 §8 error contract (T3).
//!
//! Two properties an agent implementation depends on, checked
//! against every error the framework can report:
//!
//! 1. **Completeness** — no error reaches an agent without a
//!    remediation. The exhaustive match in `remediation.rs` makes
//!    that a compile error; this suite adds the runtime half, so a
//!    variant cannot be listed and then given a hollow answer.
//! 2. **Coherence** — the fields agree with each other. A wrong
//!    `actor` is worse than no hint at all: it sends the agent
//!    looping on something only a human can fix, or stops it on
//!    something it could have retried.
//!
//! Wire names and action strings are asserted verbatim because
//! they are a contract with every agent implementation, not an
//! implementation detail.

use devboy_mcp::remediation::{
    ALL_ERROR_KINDS, RemediationAction, RemediationActor, RemediationContext, SecretsErrorKind,
};

/// Serialise a remediation the way an agent would receive it.
fn as_json(kind: SecretsErrorKind, ctx: &RemediationContext) -> serde_json::Value {
    serde_json::to_value(kind.remediation(ctx)).expect("remediation serialises")
}

/// Every error must produce a reply an agent can branch on
/// without parsing prose.
#[test]
fn every_error_serialises_with_the_contract_fields() {
    for kind in ALL_ERROR_KINDS {
        let json = as_json(*kind, &RemediationContext::for_path("team/gitlab/token"));

        for field in ["actor", "action", "user_message", "retryable"] {
            assert!(
                json.get(field).is_some(),
                "{kind:?} is missing the `{field}` field: {json}"
            );
        }

        let actor = json["actor"].as_str().expect("actor is a string");
        assert!(
            actor == "agent" || actor == "user",
            "{kind:?} has an actor outside the contract: {actor}"
        );

        let action = json["action"].as_str().expect("action is a string");
        assert!(
            action.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{kind:?} action must be snake_case for stable branching: {action}"
        );
    }
}

/// `retry_after_seconds` is omitted rather than serialised as
/// `null`, so an agent can treat its presence as "there is a
/// known wait".
#[test]
fn absent_backoff_is_omitted_not_null() {
    let json = as_json(
        SecretsErrorKind::NotProvisioned,
        &RemediationContext::empty(),
    );
    assert!(
        json.get("retry_after_seconds").is_none(),
        "an unbounded failure must not carry a null backoff: {json}"
    );

    let json = as_json(SecretsErrorKind::RateLimited, &RemediationContext::empty());
    assert!(
        json["retry_after_seconds"].as_u64().is_some(),
        "a rate limit must say how long: {json}"
    );
}

/// The load-bearing invariant of §8: an error only a human can
/// clear must never be addressed to the agent, and must never
/// invite a retry. Getting this wrong is what produces retry
/// loops against a locked vault.
#[test]
fn human_only_errors_stop_the_agent() {
    let human_only = [
        (SecretsErrorKind::TotpUnavailable, "ask_user_to_unlock"),
        (SecretsErrorKind::NotProvisioned, "ask_user_to_provision"),
        (SecretsErrorKind::ApprovalRequired, "ask_user_to_approve"),
        (SecretsErrorKind::ApprovalDenied, "none"),
        (SecretsErrorKind::LivenessFailed, "ask_user_to_rotate"),
        (
            SecretsErrorKind::DaemonNotRunning,
            "ask_user_to_start_daemon",
        ),
        (
            SecretsErrorKind::DaemonUntrusted,
            "ask_user_to_restart_daemon",
        ),
        (SecretsErrorKind::NotAvailableInCiMode, "set_env_var"),
    ];

    for (kind, expected_action) in human_only {
        let json = as_json(kind, &RemediationContext::empty());

        assert_eq!(json["actor"], "user", "{kind:?} must stop the agent");
        assert_eq!(
            json["action"], expected_action,
            "{kind:?} action changed — this is a contract with agent implementations"
        );
        assert_eq!(
            json["retryable"], false,
            "{kind:?} must not invite a retry loop"
        );
    }
}

/// The agent may only act unaided on ephemeral, bounded things.
#[test]
fn agent_actionable_errors_are_ephemeral_and_bounded() {
    let agent_actionable = [
        (SecretsErrorKind::LockedTotpAvailable, "request_totp"),
        (SecretsErrorKind::BadTotp, "request_totp"),
        (SecretsErrorKind::ReplayedCode, "request_fresh_totp"),
        (SecretsErrorKind::RateLimited, "retry_after"),
    ];

    for (kind, expected_action) in agent_actionable {
        let json = as_json(kind, &RemediationContext::empty());

        assert_eq!(json["actor"], "agent", "{kind:?} is the agent's to handle");
        assert_eq!(json["action"], expected_action, "{kind:?} action changed");
        assert_eq!(json["retryable"], true, "{kind:?} should be retryable");
    }
}

/// Wire names are what agent implementations match on. A rename
/// is a breaking change and should fail loudly here rather than
/// silently in someone's integration.
#[test]
fn error_wire_names_are_frozen() {
    let expected = [
        (SecretsErrorKind::LockedTotpAvailable, "Locked"),
        (SecretsErrorKind::TotpUnavailable, "TotpUnavailable"),
        (SecretsErrorKind::BadTotp, "BadTotp"),
        (SecretsErrorKind::ReplayedCode, "ReplayedCode"),
        (SecretsErrorKind::RateLimited, "RateLimited"),
        (SecretsErrorKind::NotProvisioned, "NotProvisioned"),
        (SecretsErrorKind::ApprovalRequired, "ApprovalRequired"),
        (SecretsErrorKind::ApprovalDenied, "ApprovalDenied"),
        (SecretsErrorKind::LivenessFailed, "LivenessFailed"),
        (SecretsErrorKind::DaemonNotRunning, "DaemonNotRunning"),
        (SecretsErrorKind::DaemonUntrusted, "DaemonUntrusted"),
        (
            SecretsErrorKind::NotAvailableInCiMode,
            "NotAvailableInCiMode",
        ),
    ];

    assert_eq!(
        expected.len(),
        ALL_ERROR_KINDS.len(),
        "an error kind was added or removed without updating the frozen wire-name list"
    );

    for (kind, name) in expected {
        assert_eq!(kind.as_str(), name, "{kind:?} wire name changed");
    }
}

/// A remediation must never carry a secret value — it rides on
/// error replies and is subject to the same audit as the replies
/// themselves.
///
/// The manifest fields it *does* carry (URL, scopes, description)
/// are metadata that exists to be shown to a human.
#[test]
fn remediation_never_carries_a_value() {
    const SENTINEL: &str = "glpat-SENTINEL-must-never-appear";

    // Feed the sentinel through every context field a caller
    // could populate.
    let ctx = RemediationContext {
        path: Some(format!("team/{SENTINEL}/token")),
        description: Some(SENTINEL.to_string()),
        retrieval_url: Some(format!("https://example.test/{SENTINEL}")),
        required_scopes: vec![SENTINEL.to_string()],
        rotation_method: Some(SENTINEL.to_string()),
        env_candidates: vec![SENTINEL.to_string()],
        retry_after_seconds: Some(1),
        daemon_start_command: Some(SENTINEL.to_string()),
    };

    // The sentinel *will* appear, because every one of those
    // fields is caller-supplied metadata. The point of this test
    // is the inverse: assert that nothing else does — i.e. the
    // struct has no hidden field sourcing content from anywhere
    // but the context we passed.
    for kind in ALL_ERROR_KINDS {
        let json = as_json(*kind, &ctx);
        let obj = json.as_object().expect("remediation is an object");

        let allowed = [
            "actor",
            "action",
            "user_message",
            "retryable",
            "retry_after_seconds",
        ];
        for key in obj.keys() {
            assert!(
                allowed.contains(&key.as_str()),
                "{kind:?} grew an unaudited field `{key}` — add it to the AgentSafeReply review \
                 before allowing it on the wire"
            );
        }
    }
}

/// ADR-024 §3: no tool accepts a passphrase, so an agent has no
/// protocol-level way to ask for one.
///
/// Checked against the actual action vocabulary rather than by
/// grepping source, because what matters is the surface an agent
/// can reach.
#[test]
fn no_remediation_ever_asks_for_a_passphrase_relay() {
    for kind in ALL_ERROR_KINDS {
        let json = as_json(*kind, &RemediationContext::empty());
        let action = json["action"].as_str().unwrap();

        assert!(
            !action.contains("passphrase"),
            "{kind:?} exposes a passphrase action: {action}"
        );

        // The unlock path a human takes is described to the user,
        // but it is never an action the agent performs.
        if action == "ask_user_to_unlock" {
            assert_eq!(
                json["actor"], "user",
                "unlocking by passphrase is always the user's action"
            );
        }
    }
}

/// The `RemediationAction` vocabulary is closed and stable.
#[test]
fn action_vocabulary_is_frozen() {
    let all = [
        (RemediationAction::RequestTotp, "request_totp"),
        (RemediationAction::RequestFreshTotp, "request_fresh_totp"),
        (RemediationAction::RetryAfter, "retry_after"),
        (RemediationAction::AskUserToUnlock, "ask_user_to_unlock"),
        (
            RemediationAction::AskUserToStartDaemon,
            "ask_user_to_start_daemon",
        ),
        (
            RemediationAction::AskUserToRestartDaemon,
            "ask_user_to_restart_daemon",
        ),
        (
            RemediationAction::AskUserToProvision,
            "ask_user_to_provision",
        ),
        (RemediationAction::AskUserToApprove, "ask_user_to_approve"),
        (RemediationAction::AskUserToRotate, "ask_user_to_rotate"),
        (RemediationAction::SetEnvVar, "set_env_var"),
        (RemediationAction::None, "none"),
    ];

    for (action, wire) in all {
        let json = serde_json::to_value(action).unwrap();
        assert_eq!(json, wire, "{action:?} wire name changed");
    }
}

#[test]
fn actor_vocabulary_is_frozen() {
    assert_eq!(
        serde_json::to_value(RemediationActor::Agent).unwrap(),
        "agent"
    );
    assert_eq!(
        serde_json::to_value(RemediationActor::User).unwrap(),
        "user"
    );
}
