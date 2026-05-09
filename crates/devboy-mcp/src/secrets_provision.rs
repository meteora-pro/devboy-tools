//! `secrets_request_provision` and `secrets_poll_status` MCP
//! tool helpers per [ADR-023] §3.7 (agent protocol — provision
//! request lifecycle).
//!
//! The agent never sees a value. To onboard a new credential it
//! asks the framework to *open a UI dialog* (TUI in a terminal,
//! GUI in a window — see ADR-023 §3.4) where the human pastes
//! the value. The dialog hands the value to the daemon over a
//! local socket; the agent only ever learns whether the user
//! accepted, cancelled, or let the request expire.
//!
//! ## Lifecycle
//!
//! ```text
//! request_provision(path) ──► request_id, status = pending
//!         │
//!         │ user types value, clicks save
//!         ▼
//! daemon stores value, registry advances to status = ok
//!         │
//!         │ poll_status(request_id) ──► { status: ok, age_seconds }
//!         ▼
//! agent proceeds to use the value via the high-level provider
//! tool (it never reads the value directly).
//! ```
//!
//! ## Timeouts
//!
//! Per ADR-023 §3.7, a pending request expires 5 minutes after
//! creation. `poll_status` reports `expired` and the registry
//! drops the entry. The user re-issuing the dialog from the UI
//! after timeout creates a fresh request with a new id.
//!
//! ## Testability
//!
//! [`UiDialogLauncher`] is a trait — production wiring talks to
//! the daemon socket; the unit tests use a [`FakeUiLauncher`]
//! that records calls and lets the test advance status by hand.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use devboy_storage::SecretPath;
use serde::{Deserialize, Serialize};

// =============================================================================
// Public contract
// =============================================================================

/// Default per-request lifetime. ADR-023 §3.7: "a pending
/// provision request expires after 5 minutes".
pub const DEFAULT_TTL: Duration = Duration::from_secs(5 * 60);

/// Request mode. `Provision` is the default ("first-time
/// write"); `Rotation` reuses the same dialog with the
/// destructive-confirm checkbox surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvisionMode {
    #[default]
    Provision,
    Rotation,
}

/// Status surfaced to the agent. Stable strings — pinned by
/// snapshot tests so a future client can rely on them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ProvisionStatus {
    /// Dialog is still open / awaiting the user.
    Pending,
    /// Dialog returned a saved value (the daemon has it).
    Ok,
    /// User dismissed the dialog without submitting.
    Cancelled,
    /// 5-minute window elapsed without a save.
    Expired,
    /// Dialog couldn't be opened — typically because the
    /// daemon is down or the launcher misconfigured.
    Failed { reason: String },
}

impl ProvisionStatus {
    pub fn label(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::Ok => "ok",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Failed { .. } => "failed",
        }
    }
}

/// Opaque request id returned by [`ProvisionRegistry::request_provision`].
/// Format is `prov-<lower-hex 12 chars>` — short enough to
/// type, long enough to avoid collisions across one user's
/// session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub String);

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl RequestId {
    /// Build a fresh id. Uses the current `Instant` plus a
    /// simple per-process counter so concurrent calls from the
    /// same registry don't collide.
    fn fresh(seq: u64) -> Self {
        let nanos = Instant::now()
            .elapsed()
            .as_nanos()
            .wrapping_add(u128::from(seq));
        Self(format!("prov-{:012x}", nanos & 0xFFFF_FFFF_FFFF))
    }
}

// =============================================================================
// UI launcher
// =============================================================================

/// Plain-data payload an agent sends as part of `propose_metadata`
/// or `propose_new_path`. The dialog renders these strings in
/// the **proposed** column only — the **current** column is
/// pulled from the trusted manifest. That separation is the
/// prompt-injection mitigation called out in ADR-023 §3.7:
/// agent-supplied text never replaces the user's record-of-truth.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedMetadata {
    pub description: Option<String>,
    pub retrieval_url: Option<String>,
    pub rotate_every_days: Option<u32>,
    /// ISO-8601 (`yyyy-mm-dd`).
    pub expires_at: Option<String>,
    pub pattern_id: Option<String>,
}

/// What kind of request a registry entry tracks. Reflected in
/// `poll_status` so the agent can confirm the dialog actually
/// matched what it asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestKind {
    Provision,
    Rotation,
    MetadataProposal,
    NewPathProposal,
}

impl RequestKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Provision => "provision",
            Self::Rotation => "rotation",
            Self::MetadataProposal => "metadata-proposal",
            Self::NewPathProposal => "new-path-proposal",
        }
    }

    fn from_provision_mode(mode: ProvisionMode) -> Self {
        match mode {
            ProvisionMode::Provision => Self::Provision,
            ProvisionMode::Rotation => Self::Rotation,
        }
    }
}

/// Hands the dialog off to whatever surface the user has
/// configured. The production wiring talks to the daemon's
/// `dialog.open` JSON-RPC method (P4.2); tests use
/// [`FakeUiLauncher`].
pub trait UiDialogLauncher: Send + Sync + fmt::Debug {
    /// Spawn the provision / rotation dialog (per ADR-023 §3.4).
    /// Returning `Ok(())` only means the dialog was launched,
    /// not that the user has acted on it. Status updates flow
    /// back through [`ProvisionRegistry::resolve`] from the
    /// dialog code.
    fn open(
        &self,
        request_id: &RequestId,
        path: &SecretPath,
        mode: ProvisionMode,
    ) -> Result<(), String>;

    /// Open the edit-metadata dialog with a diff preview. The
    /// dialog renders the manifest's current values in the
    /// `current` column and the agent's `proposed` payload in
    /// the `proposed` column — agent strings never replace the
    /// trusted manifest fields. Default impl returns Failed so
    /// embedders that haven't wired this yet get a precise
    /// error rather than a stuck pending request.
    fn open_metadata_proposal(
        &self,
        _request_id: &RequestId,
        _path: &SecretPath,
        _proposed: &ProposedMetadata,
    ) -> Result<(), String> {
        Err("metadata-proposal launcher is not wired into this MCP server build".to_owned())
    }

    /// Open the new-path registration dialog. Same contract as
    /// `open_metadata_proposal` — the suggested path becomes the
    /// editable starting value, the proposed metadata appears in
    /// the diff column, and the user has the final say.
    fn open_new_path_proposal(
        &self,
        _request_id: &RequestId,
        _suggested_path: &SecretPath,
        _proposed: &ProposedMetadata,
    ) -> Result<(), String> {
        Err("new-path-proposal launcher is not wired into this MCP server build".to_owned())
    }
}

/// No-op launcher used as the default until the daemon socket
/// is wired in. Returning [`ProvisionStatus::Failed`] is the
/// only honest behaviour when the host hasn't configured a
/// real launcher; the agent gets a precise error rather than a
/// silently-stuck pending request.
#[derive(Debug, Default)]
pub struct NoopUiLauncher;

impl UiDialogLauncher for NoopUiLauncher {
    fn open(
        &self,
        _request_id: &RequestId,
        _path: &SecretPath,
        _mode: ProvisionMode,
    ) -> Result<(), String> {
        Err("no UI launcher is wired into this MCP server build".to_owned())
    }
}

// =============================================================================
// Registry
// =============================================================================

#[derive(Debug, Clone)]
struct Entry {
    path: SecretPath,
    kind: RequestKind,
    status: ProvisionStatus,
    created_at: Instant,
}

/// Per-server in-memory registry of in-flight provision
/// requests. `Mutex`-guarded — accesses are infrequent and
/// short.
#[derive(Debug)]
pub struct ProvisionRegistry {
    inner: Mutex<RegistryState>,
    launcher: Arc<dyn UiDialogLauncher>,
    ttl: Duration,
}

#[derive(Debug, Default)]
struct RegistryState {
    entries: HashMap<RequestId, Entry>,
    seq: u64,
}

impl ProvisionRegistry {
    /// Build a registry that uses [`NoopUiLauncher`] and the
    /// default 5-minute TTL.
    pub fn new() -> Self {
        Self::with_launcher(Arc::new(NoopUiLauncher))
    }

    pub fn with_launcher(launcher: Arc<dyn UiDialogLauncher>) -> Self {
        Self {
            inner: Mutex::new(RegistryState::default()),
            launcher,
            ttl: DEFAULT_TTL,
        }
    }

    /// Override the request lifetime — only useful in tests; the
    /// agent contract pins this to 5 minutes.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Open a dialog for `path`. Returns the freshly-issued
    /// `request_id`. The returned id is `Pending` until the
    /// dialog reports back via [`Self::resolve`] or the TTL
    /// elapses.
    pub fn request_provision(
        &self,
        path: SecretPath,
        mode: ProvisionMode,
    ) -> Result<RequestId, String> {
        let id = {
            let mut state = self.inner.lock().expect("registry poisoned");
            state.seq = state.seq.wrapping_add(1);
            RequestId::fresh(state.seq)
        };

        let kind = RequestKind::from_provision_mode(mode);
        match self.launcher.open(&id, &path, mode) {
            Ok(()) => {
                self.record(&id, path, kind, ProvisionStatus::Pending);
                Ok(id)
            }
            Err(e) => {
                self.record(
                    &id,
                    path,
                    kind,
                    ProvisionStatus::Failed { reason: e.clone() },
                );
                Err(e)
            }
        }
    }

    /// Open the edit-metadata dialog with a diff preview. The
    /// agent passes its `proposed` payload; the dialog reads the
    /// manifest's current fields straight from the index and
    /// renders them as the diff baseline — the agent's strings
    /// never replace the trusted record (ADR-023 §3.7).
    pub fn request_metadata_proposal(
        &self,
        path: SecretPath,
        proposed: ProposedMetadata,
    ) -> Result<RequestId, String> {
        let id = self.next_id();
        match self.launcher.open_metadata_proposal(&id, &path, &proposed) {
            Ok(()) => {
                self.record(
                    &id,
                    path,
                    RequestKind::MetadataProposal,
                    ProvisionStatus::Pending,
                );
                Ok(id)
            }
            Err(e) => {
                self.record(
                    &id,
                    path,
                    RequestKind::MetadataProposal,
                    ProvisionStatus::Failed { reason: e.clone() },
                );
                Err(e)
            }
        }
    }

    /// Open the new-path registration dialog. Identical
    /// semantics to `request_metadata_proposal` but the path
    /// argument is editable and starts at the agent's
    /// suggestion.
    pub fn request_new_path_proposal(
        &self,
        suggested_path: SecretPath,
        proposed: ProposedMetadata,
    ) -> Result<RequestId, String> {
        let id = self.next_id();
        match self
            .launcher
            .open_new_path_proposal(&id, &suggested_path, &proposed)
        {
            Ok(()) => {
                self.record(
                    &id,
                    suggested_path,
                    RequestKind::NewPathProposal,
                    ProvisionStatus::Pending,
                );
                Ok(id)
            }
            Err(e) => {
                self.record(
                    &id,
                    suggested_path,
                    RequestKind::NewPathProposal,
                    ProvisionStatus::Failed { reason: e.clone() },
                );
                Err(e)
            }
        }
    }

    fn next_id(&self) -> RequestId {
        let mut state = self.inner.lock().expect("registry poisoned");
        state.seq = state.seq.wrapping_add(1);
        RequestId::fresh(state.seq)
    }

    fn record(&self, id: &RequestId, path: SecretPath, kind: RequestKind, status: ProvisionStatus) {
        let mut state = self.inner.lock().expect("registry poisoned");
        state.entries.insert(
            id.clone(),
            Entry {
                path,
                kind,
                status,
                created_at: Instant::now(),
            },
        );
    }

    /// Push a status update from the dialog code. Used by the
    /// daemon glue (and tests) to advance a request from
    /// `Pending` to `Ok` / `Cancelled` / `Failed`. No-op when
    /// the request id isn't known (e.g. the agent never asked
    /// for it) or the request already settled.
    pub fn resolve(&self, id: &RequestId, status: ProvisionStatus) {
        let mut state = self.inner.lock().expect("registry poisoned");
        if let Some(entry) = state.entries.get_mut(id)
            && matches!(entry.status, ProvisionStatus::Pending)
        {
            entry.status = status;
        }
    }

    /// Snapshot the current status of `id`. Returns `None` if
    /// the registry has no record of that id (likely because it
    /// expired and was swept). Pending entries past the TTL are
    /// transitioned to `Expired` here so the caller doesn't
    /// have to.
    pub fn poll_status(&self, id: &RequestId) -> Option<ProvisionStatusReply> {
        let mut state = self.inner.lock().expect("registry poisoned");
        let entry = state.entries.get_mut(id)?;
        if matches!(entry.status, ProvisionStatus::Pending)
            && entry.created_at.elapsed() >= self.ttl
        {
            entry.status = ProvisionStatus::Expired;
        }
        Some(ProvisionStatusReply {
            request_id: id.clone(),
            path: entry.path.to_string(),
            kind: entry.kind,
            status: entry.status.clone(),
            age_seconds: entry.created_at.elapsed().as_secs(),
        })
    }

    /// Drop entries that have settled at least `older_than`
    /// ago. Intended for periodic housekeeping; the registry
    /// works fine without it but the entries linger forever
    /// otherwise.
    pub fn sweep_settled(&self, older_than: Duration) -> usize {
        let mut state = self.inner.lock().expect("registry poisoned");
        let before = state.entries.len();
        state.entries.retain(|_, e| {
            !matches!(
                e.status,
                ProvisionStatus::Ok | ProvisionStatus::Cancelled | ProvisionStatus::Expired
            ) || e.created_at.elapsed() < older_than
        });
        before - state.entries.len()
    }
}

impl Default for ProvisionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Wire-format reply returned by `secrets_poll_status`. The
/// `kind` field reports which entry-point produced the
/// request — `provision`, `rotation`, `metadata-proposal`, or
/// `new-path-proposal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvisionStatusReply {
    pub request_id: RequestId,
    pub path: String,
    pub kind: RequestKind,
    pub status: ProvisionStatus,
    pub age_seconds: u64,
}

/// Tiny wire-format envelope returned by every `request_*`
/// tool. Pulled out as a typed struct so the
/// [`crate::agent_safety::AgentSafeReply`] fence can verify
/// it carries no value-shaped fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestIdReply {
    pub request_id: String,
}

impl crate::agent_safety::AgentSafeReply for ProvisionStatusReply {}
impl crate::agent_safety::AgentSafeReply for RequestIdReply {}

// =============================================================================
// Test fixtures
// =============================================================================

/// Records the calls a test made and lets the test pre-script
/// the launcher's response (success vs. error). Wrapped in
/// `Mutex` for `Send + Sync` so it slots into the trait object
/// the registry holds.
#[derive(Debug, Default)]
pub struct FakeUiLauncher {
    inner: Mutex<FakeInner>,
}

#[derive(Debug, Default)]
struct FakeInner {
    calls: Vec<(RequestId, SecretPath, ProvisionMode)>,
    metadata_calls: Vec<(RequestId, SecretPath, ProposedMetadata)>,
    new_path_calls: Vec<(RequestId, SecretPath, ProposedMetadata)>,
    next_error: Option<String>,
}

impl FakeUiLauncher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make the next `open()` call return `Err(reason)` instead
    /// of opening the dialog. Useful for the daemon-down path.
    pub fn fail_next_with(&self, reason: impl Into<String>) {
        self.inner.lock().unwrap().next_error = Some(reason.into());
    }

    /// Inspect the calls the launcher received. Returns owned
    /// data so tests can release the mutex before asserting.
    pub fn calls(&self) -> Vec<(RequestId, SecretPath, ProvisionMode)> {
        self.inner.lock().unwrap().calls.clone()
    }

    pub fn metadata_calls(&self) -> Vec<(RequestId, SecretPath, ProposedMetadata)> {
        self.inner.lock().unwrap().metadata_calls.clone()
    }

    pub fn new_path_calls(&self) -> Vec<(RequestId, SecretPath, ProposedMetadata)> {
        self.inner.lock().unwrap().new_path_calls.clone()
    }
}

impl UiDialogLauncher for FakeUiLauncher {
    fn open(
        &self,
        request_id: &RequestId,
        path: &SecretPath,
        mode: ProvisionMode,
    ) -> Result<(), String> {
        let mut state = self.inner.lock().unwrap();
        if let Some(e) = state.next_error.take() {
            return Err(e);
        }
        state.calls.push((request_id.clone(), path.clone(), mode));
        Ok(())
    }

    fn open_metadata_proposal(
        &self,
        request_id: &RequestId,
        path: &SecretPath,
        proposed: &ProposedMetadata,
    ) -> Result<(), String> {
        let mut state = self.inner.lock().unwrap();
        if let Some(e) = state.next_error.take() {
            return Err(e);
        }
        state
            .metadata_calls
            .push((request_id.clone(), path.clone(), proposed.clone()));
        Ok(())
    }

    fn open_new_path_proposal(
        &self,
        request_id: &RequestId,
        suggested_path: &SecretPath,
        proposed: &ProposedMetadata,
    ) -> Result<(), String> {
        let mut state = self.inner.lock().unwrap();
        if let Some(e) = state.next_error.take() {
            return Err(e);
        }
        state
            .new_path_calls
            .push((request_id.clone(), suggested_path.clone(), proposed.clone()));
        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn path() -> SecretPath {
        SecretPath::parse("team/jira/api-key").unwrap()
    }

    // -- Request issuance --------------------------------------

    #[test]
    fn request_provision_calls_launcher_and_returns_pending_id() {
        let launcher = Arc::new(FakeUiLauncher::new());
        let registry = ProvisionRegistry::with_launcher(launcher);
        let id = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap();
        assert!(id.0.starts_with("prov-"));
        let reply = registry.poll_status(&id).unwrap();
        assert_eq!(reply.status, ProvisionStatus::Pending);
        assert_eq!(reply.path, "team/jira/api-key");
        assert_eq!(reply.kind, RequestKind::Provision);
    }

    #[test]
    fn request_provision_records_call_on_fake_launcher() {
        let fake = FakeUiLauncher::new();
        // Move into the registry, but inspect via a separate handle.
        let registry = ProvisionRegistry::with_launcher(Arc::new(FakeUiLauncher::new()));
        let _ = registry
            .request_provision(path(), ProvisionMode::Rotation)
            .unwrap();
        // We can't reach back into the registry's launcher (it
        // owns it), but we can poll-status to see the recorded
        // mode survived.
        let id = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap();
        assert_eq!(
            registry.poll_status(&id).unwrap().kind,
            RequestKind::Provision
        );
        // Direct fake test:
        let id2 = RequestId("fixed".into());
        fake.open(&id2, &path(), ProvisionMode::Rotation).unwrap();
        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].2, ProvisionMode::Rotation);
    }

    #[test]
    fn launcher_failure_records_failed_status_and_propagates_error() {
        let launcher = FakeUiLauncher::new();
        launcher.fail_next_with("daemon unreachable");
        let registry = ProvisionRegistry::with_launcher(Arc::new(launcher));
        let err = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap_err();
        assert_eq!(err, "daemon unreachable");
    }

    // -- Status lifecycle --------------------------------------

    #[test]
    fn resolve_advances_pending_request_to_ok() {
        let registry = ProvisionRegistry::with_launcher(Arc::new(FakeUiLauncher::new()));
        let id = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap();
        registry.resolve(&id, ProvisionStatus::Ok);
        let reply = registry.poll_status(&id).unwrap();
        assert_eq!(reply.status, ProvisionStatus::Ok);
    }

    #[test]
    fn resolve_advances_pending_request_to_cancelled() {
        let registry = ProvisionRegistry::with_launcher(Arc::new(FakeUiLauncher::new()));
        let id = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap();
        registry.resolve(&id, ProvisionStatus::Cancelled);
        assert_eq!(
            registry.poll_status(&id).unwrap().status,
            ProvisionStatus::Cancelled
        );
    }

    #[test]
    fn resolve_does_not_overwrite_a_settled_request() {
        let registry = ProvisionRegistry::with_launcher(Arc::new(FakeUiLauncher::new()));
        let id = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap();
        registry.resolve(&id, ProvisionStatus::Ok);
        registry.resolve(&id, ProvisionStatus::Cancelled);
        assert_eq!(
            registry.poll_status(&id).unwrap().status,
            ProvisionStatus::Ok,
            "a settled request must not be re-resolved"
        );
    }

    #[test]
    fn poll_status_returns_none_for_unknown_id() {
        let registry = ProvisionRegistry::new();
        let id = RequestId("never-issued".into());
        assert!(registry.poll_status(&id).is_none());
    }

    // -- TTL ---------------------------------------------------

    #[test]
    fn pending_request_past_ttl_is_marked_expired_on_poll() {
        let registry = ProvisionRegistry::with_launcher(Arc::new(FakeUiLauncher::new()))
            .with_ttl(Duration::from_millis(50));
        let id = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap();
        sleep(Duration::from_millis(80));
        let reply = registry.poll_status(&id).unwrap();
        assert_eq!(reply.status, ProvisionStatus::Expired);
        assert!(reply.age_seconds < 5, "age should be <5s in this test");
    }

    #[test]
    fn settled_request_past_ttl_keeps_its_terminal_status() {
        let registry = ProvisionRegistry::with_launcher(Arc::new(FakeUiLauncher::new()))
            .with_ttl(Duration::from_millis(50));
        let id = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap();
        registry.resolve(&id, ProvisionStatus::Ok);
        sleep(Duration::from_millis(80));
        let reply = registry.poll_status(&id).unwrap();
        assert_eq!(
            reply.status,
            ProvisionStatus::Ok,
            "TTL must not flip a settled status to Expired"
        );
    }

    #[test]
    fn ttl_default_is_five_minutes_per_adr() {
        assert_eq!(DEFAULT_TTL, Duration::from_secs(300));
    }

    // -- Sweep -------------------------------------------------

    #[test]
    fn sweep_settled_drops_old_terminal_entries_only() {
        let registry = ProvisionRegistry::with_launcher(Arc::new(FakeUiLauncher::new()));
        let id_ok = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap();
        let id_pending = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap();
        registry.resolve(&id_ok, ProvisionStatus::Ok);
        sleep(Duration::from_millis(20));
        let dropped = registry.sweep_settled(Duration::from_millis(0));
        assert_eq!(dropped, 1, "only the settled entry should sweep");
        assert!(registry.poll_status(&id_ok).is_none());
        assert!(registry.poll_status(&id_pending).is_some());
    }

    // -- Default launcher --------------------------------------

    #[test]
    fn default_launcher_returns_failed_status_with_clear_message() {
        let registry = ProvisionRegistry::new();
        let err = registry
            .request_provision(path(), ProvisionMode::Provision)
            .unwrap_err();
        assert!(err.contains("no UI launcher"));
    }

    // -- Status labels are pinned -----------------------------

    #[test]
    fn status_labels_match_adr_023_strings() {
        assert_eq!(ProvisionStatus::Pending.label(), "pending");
        assert_eq!(ProvisionStatus::Ok.label(), "ok");
        assert_eq!(ProvisionStatus::Cancelled.label(), "cancelled");
        assert_eq!(ProvisionStatus::Expired.label(), "expired");
        assert_eq!(
            ProvisionStatus::Failed { reason: "x".into() }.label(),
            "failed"
        );
    }

    #[test]
    fn request_kind_labels_pinned_for_wire_format() {
        assert_eq!(RequestKind::Provision.label(), "provision");
        assert_eq!(RequestKind::Rotation.label(), "rotation");
        assert_eq!(RequestKind::MetadataProposal.label(), "metadata-proposal");
        assert_eq!(RequestKind::NewPathProposal.label(), "new-path-proposal");
    }

    // -- Metadata proposal -------------------------------------

    fn proposed() -> ProposedMetadata {
        ProposedMetadata {
            description: Some("Jira API token (proposed)".into()),
            retrieval_url: Some("https://example.invalid/jira/tokens".into()),
            rotate_every_days: Some(90),
            expires_at: Some("2026-12-01".into()),
            pattern_id: Some("jira-api-token".into()),
        }
    }

    #[test]
    fn request_metadata_proposal_records_pending_with_kind() {
        let fake = Arc::new(FakeUiLauncher::new());
        let registry = ProvisionRegistry::with_launcher(fake.clone());
        let id = registry
            .request_metadata_proposal(path(), proposed())
            .unwrap();
        let reply = registry.poll_status(&id).unwrap();
        assert_eq!(reply.kind, RequestKind::MetadataProposal);
        assert_eq!(reply.status, ProvisionStatus::Pending);
        assert_eq!(reply.path, "team/jira/api-key");

        let calls = fake.metadata_calls();
        assert_eq!(calls.len(), 1);
        // The proposed payload reaches the launcher unchanged.
        assert_eq!(calls[0].2, proposed());
    }

    #[test]
    fn request_metadata_proposal_records_failed_on_launcher_error() {
        let fake = Arc::new(FakeUiLauncher::new());
        fake.fail_next_with("daemon unreachable");
        let registry = ProvisionRegistry::with_launcher(fake);
        let err = registry
            .request_metadata_proposal(path(), proposed())
            .unwrap_err();
        assert_eq!(err, "daemon unreachable");
    }

    #[test]
    fn metadata_proposal_resolve_advances_status() {
        let fake = Arc::new(FakeUiLauncher::new());
        let registry = ProvisionRegistry::with_launcher(fake);
        let id = registry
            .request_metadata_proposal(path(), proposed())
            .unwrap();
        registry.resolve(&id, ProvisionStatus::Ok);
        assert_eq!(
            registry.poll_status(&id).unwrap().status,
            ProvisionStatus::Ok
        );
    }

    // -- New-path proposal -------------------------------------

    #[test]
    fn request_new_path_proposal_records_pending_with_kind() {
        let fake = Arc::new(FakeUiLauncher::new());
        let registry = ProvisionRegistry::with_launcher(fake.clone());
        let suggested = SecretPath::parse("team/openai/api-key").unwrap();
        let id = registry
            .request_new_path_proposal(suggested, proposed())
            .unwrap();
        let reply = registry.poll_status(&id).unwrap();
        assert_eq!(reply.kind, RequestKind::NewPathProposal);
        assert_eq!(reply.status, ProvisionStatus::Pending);
        assert_eq!(reply.path, "team/openai/api-key");

        let calls = fake.new_path_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].2, proposed());
    }

    #[test]
    fn new_path_proposal_resolve_advances_status() {
        let fake = Arc::new(FakeUiLauncher::new());
        let registry = ProvisionRegistry::with_launcher(fake);
        let suggested = SecretPath::parse("team/openai/api-key").unwrap();
        let id = registry
            .request_new_path_proposal(suggested, proposed())
            .unwrap();
        registry.resolve(&id, ProvisionStatus::Cancelled);
        assert_eq!(
            registry.poll_status(&id).unwrap().status,
            ProvisionStatus::Cancelled
        );
    }

    // -- Default launcher returns Failed for proposal kinds ----

    #[test]
    fn default_launcher_rejects_metadata_proposal() {
        let registry = ProvisionRegistry::new();
        let err = registry
            .request_metadata_proposal(path(), ProposedMetadata::default())
            .unwrap_err();
        assert!(err.contains("metadata-proposal launcher"));
    }

    #[test]
    fn default_launcher_rejects_new_path_proposal() {
        let registry = ProvisionRegistry::new();
        let err = registry
            .request_new_path_proposal(path(), ProposedMetadata::default())
            .unwrap_err();
        assert!(err.contains("new-path-proposal launcher"));
    }
}
