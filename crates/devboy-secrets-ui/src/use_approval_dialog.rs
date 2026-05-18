//! Use-approval dialog view-model per [ADR-023] §3.7 (P25).
//!
//! Surfaces a per-resolve approval prompt for paths whose
//! `approve_on_use` policy is set to `Session` or `PerCall`.
//! The agent never sees the secret — only whether the user
//! approved its use, and for how long.
//!
//! ## Architecture
//!
//! Same shape as [`crate::provision_dialog`]: a pure view-model
//! ([`UseApprovalState`]) with no rendering code, plus a thin
//! `render` function in the matching backend module
//! ([`crate::gui::use_approval_dialog`]). The orchestration
//! layer (P25.4) caches `Decided(AlwaysSession)` results so the
//! dialog does not re-fire for the same path within the
//! session.
//!
//! ## Trust boundary
//!
//! `path` is read from the trusted manifest. `reason` is the
//! agent-supplied string from `secrets_request_use_approval` —
//! the dialog renders it verbatim but never grants it the
//! authority to mutate the manifest, mirroring the
//! prompt-injection mitigation already in place for
//! [`crate::metadata_editor`].
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

/// What the user picked. Mapped to `ProvisionStatus::Once /
/// Session / Denied` on the MCP side by the orchestration
/// layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UseApprovalDecision {
    /// Allow this resolve only — no caching.
    Once,
    /// Allow this resolve and any further resolve of the same
    /// path within the running session.
    AlwaysSession,
    /// Reject the resolve — the consumer must surface an error
    /// to the agent.
    Deny,
}

impl UseApprovalDecision {
    /// Stable wire string mirrored by `ProvisionStatus.label()`
    /// on the MCP side; the orchestration layer relies on this
    /// pairing so the test pinning the strings on either end
    /// catches drift early.
    pub fn label(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::AlwaysSession => "session",
            Self::Deny => "denied",
        }
    }
}

/// Lifecycle of the dialog. The dialog stays open while
/// `Awaiting`; once a decision is recorded the orchestration
/// layer closes the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseApprovalStatus {
    Awaiting,
    Decided(UseApprovalDecision),
}

/// View-model held by the orchestration layer for the
/// lifetime of one approval prompt.
#[derive(Debug, Clone)]
pub struct UseApprovalState {
    path: String,
    reason: String,
    status: UseApprovalStatus,
}

impl UseApprovalState {
    /// Build a fresh prompt for `path` with the agent-supplied
    /// `reason`. Both arguments come from the trusted side:
    /// `path` from the manifest, `reason` from the
    /// `secrets_request_use_approval` call after the MCP
    /// handler validated it (non-empty).
    pub fn new(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            reason: reason.into(),
            status: UseApprovalStatus::Awaiting,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn status(&self) -> UseApprovalStatus {
        self.status
    }

    /// Record the user's pick. No-ops if the dialog has
    /// already settled — the user cannot change their mind by
    /// clicking another button after the dialog closes.
    pub fn record_decision(&mut self, decision: UseApprovalDecision) {
        if matches!(self.status, UseApprovalStatus::Awaiting) {
            self.status = UseApprovalStatus::Decided(decision);
        }
    }

    pub fn decision(&self) -> Option<UseApprovalDecision> {
        match self.status {
            UseApprovalStatus::Decided(d) => Some(d),
            UseApprovalStatus::Awaiting => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_starts_awaiting() {
        let s = UseApprovalState::new("team/jira/api-key", "deploy");
        assert_eq!(s.status(), UseApprovalStatus::Awaiting);
        assert_eq!(s.decision(), None);
        assert_eq!(s.path(), "team/jira/api-key");
        assert_eq!(s.reason(), "deploy");
    }

    #[test]
    fn record_decision_settles_state() {
        let mut s = UseApprovalState::new("team/jira/api-key", "deploy");
        s.record_decision(UseApprovalDecision::AlwaysSession);
        assert_eq!(
            s.status(),
            UseApprovalStatus::Decided(UseApprovalDecision::AlwaysSession)
        );
        assert_eq!(s.decision(), Some(UseApprovalDecision::AlwaysSession));
    }

    #[test]
    fn record_decision_is_terminal() {
        let mut s = UseApprovalState::new("team/jira/api-key", "deploy");
        s.record_decision(UseApprovalDecision::Once);
        // A second click must not overwrite the first decision.
        s.record_decision(UseApprovalDecision::Deny);
        assert_eq!(s.decision(), Some(UseApprovalDecision::Once));
    }

    #[test]
    fn decision_labels_pin_wire_format() {
        assert_eq!(UseApprovalDecision::Once.label(), "once");
        assert_eq!(UseApprovalDecision::AlwaysSession.label(), "session");
        assert_eq!(UseApprovalDecision::Deny.label(), "denied");
    }
}
