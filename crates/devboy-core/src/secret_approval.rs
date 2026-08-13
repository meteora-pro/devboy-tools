//! Per-session approve-on-use cache for `@secret:<path>`
//! resolution per [ADR-023] §3.7 (P25.4).
//!
//! When a manifest entry's `approve_on_use` is `Session` or
//! `PerCall`, every alias resolve must surface the
//! `secrets_request_use_approval` dialog before the value
//! reaches the consumer. The agent picks one of three
//! decisions:
//!
//! - `Once` — single resolve, no caching.
//! - `AlwaysSession` — cache the approval for the chosen TTL.
//! - `Deny` — refuse the resolve.
//!
//! [`SessionApprovalCache`] holds the `AlwaysSession` decisions
//! for the lifetime of one process. The cache is intentionally
//! *advisory*: it lives in `devboy-core` (the lowest leaf of
//! the dependency graph) so any consumer — config loader,
//! router, MCP server — can reuse the same gate logic without
//! pulling in `devboy-storage` or the dialog crate.
//!
//! The dialog and the storage manifest both stay decoupled
//! from this module: `devboy-storage` exposes the
//! `ApproveOnUse` enum on its `IndexEntry`, and a small
//! [`From`] bridge in that crate turns it into the local
//! [`ApproveOnUsePolicy`] enum so this cache stays
//! dependency-free.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Mirror of `devboy_storage::index::ApproveOnUse` exposed
/// here so the cache is reachable from `devboy-core` without a
/// circular dependency. `devboy-storage` provides a `From` impl
/// from its own enum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ApproveOnUsePolicy {
    /// Default — zero-prompt resolve. Cache is bypassed.
    #[default]
    Never,
    /// One approval covers the rest of the session, capped by
    /// the TTL the dialog returns.
    Session,
    /// Every resolve prompts; cache is bypassed even if a
    /// matching entry exists.
    PerCall,
}

/// What a consumer must do before resolving a `@secret:<path>`
/// alias. Returned by [`SessionApprovalCache::evaluate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalGate {
    /// Policy is `Never` — proceed straight to resolve, no
    /// dialog, no caching.
    NotRequired,
    /// Policy is `Session` AND a non-expired approval exists
    /// in the cache — the consumer may resolve without
    /// prompting again.
    AlreadyApproved,
    /// Either no cached approval, or policy is `PerCall`.
    /// The consumer must surface the
    /// `secrets_request_use_approval` dialog and observe the
    /// reply before resolving.
    PromptRequired,
}

#[derive(Debug, Clone)]
struct ApprovedAt {
    at: Instant,
    ttl: Duration,
}

impl ApprovedAt {
    fn is_live(&self) -> bool {
        self.at.elapsed() < self.ttl
    }
}

/// Process-lifetime cache of `AlwaysSession` approvals,
/// keyed by ADR-020 path. Mutex-guarded — accesses are
/// infrequent (one per resolve at most) and short.
#[derive(Debug, Default)]
pub struct SessionApprovalCache {
    entries: Mutex<HashMap<String, ApprovedAt>>,
}

impl SessionApprovalCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cache a `Session`-scope approval for `path` with the
    /// given TTL. The TTL comes from the dialog's reply so a
    /// short-lived approval drops out of the cache once the
    /// agent's window expires.
    ///
    /// A second call for the same path replaces the previous
    /// entry — there is no contract on "earliest wins" or
    /// "latest wins" beyond that, but in practice the latest
    /// reply is the one the user actually saw.
    pub fn record_session(&self, path: impl Into<String>, ttl: Duration) {
        let mut state = self.entries.lock().expect("approval cache poisoned");
        state.insert(
            path.into(),
            ApprovedAt {
                at: Instant::now(),
                ttl,
            },
        );
    }

    /// `true` iff `path` has a non-expired session approval.
    /// Expired entries are dropped lazily on this call so the
    /// cache stays tidy without a background sweeper.
    pub fn is_approved(&self, path: &str) -> bool {
        let mut state = self.entries.lock().expect("approval cache poisoned");
        if let Some(entry) = state.get(path) {
            if entry.is_live() {
                return true;
            }
            state.remove(path);
        }
        false
    }

    /// Decide whether the consumer must prompt before
    /// resolving `path`. The single source of truth used by
    /// alias resolvers and the MCP proxy.
    pub fn evaluate(&self, path: &str, policy: ApproveOnUsePolicy) -> ApprovalGate {
        match policy {
            ApproveOnUsePolicy::Never => ApprovalGate::NotRequired,
            ApproveOnUsePolicy::PerCall => ApprovalGate::PromptRequired,
            ApproveOnUsePolicy::Session => {
                if self.is_approved(path) {
                    ApprovalGate::AlreadyApproved
                } else {
                    ApprovalGate::PromptRequired
                }
            }
        }
    }

    /// Drop the cached approval for `path` (if any). Call after
    /// a rotation so a freshly-rotated value re-prompts.
    /// Returns `true` if an entry was removed.
    pub fn forget(&self, path: &str) -> bool {
        let mut state = self.entries.lock().expect("approval cache poisoned");
        state.remove(path).is_some()
    }

    /// Drop every entry. Useful when the user clears the
    /// session manually from the inventory UI.
    pub fn clear(&self) {
        let mut state = self.entries.lock().expect("approval cache poisoned");
        state.clear();
    }

    /// Drop expired entries; returns the number swept.
    /// Optional housekeeping — the cache is correct without
    /// it because [`Self::is_approved`] cleans up on access.
    pub fn sweep_expired(&self) -> usize {
        let mut state = self.entries.lock().expect("approval cache poisoned");
        let before = state.len();
        state.retain(|_, e| e.is_live());
        before - state.len()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().expect("approval cache poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// =============================================================================
// ApprovalGatedResolver — enforces the cache before a resolve
// =============================================================================

use std::sync::Arc;

use crate::alias::{AliasResolverError, SecretResolver};
use secrecy::SecretString;

/// Type-safe wrapper that enforces the approve-on-use policy
/// **before** dispatching to an inner [`SecretResolver`]. This is
/// what closes the loop on the P25 protocol — a resolver that
/// is not gated through this wrapper makes the
/// `approve_on_use` field a metadata-only theatrical control.
///
/// Construction takes three values:
///
/// 1. An inner `SecretResolver` (keychain, local-vault, 1Password,
///    …).
/// 2. An [`Arc<SessionApprovalCache>`] — shared across every gated
///    resolver in the process so the user only sees one prompt
///    per session per path.
/// 3. A `policy_for_path` closure — typically reads the path's
///    `approve_on_use` field from the merged manifest. The
///    closure shape avoids a hard dependency on `devboy-storage`
///    in this crate.
///
/// On every `resolve()` call:
///
/// - `ApproveOnUsePolicy::Never` → straight to the inner resolver.
/// - `ApproveOnUsePolicy::Session` with a cache hit → straight to
///   the inner resolver.
/// - `ApproveOnUsePolicy::Session` without a cache hit, or
///   `ApproveOnUsePolicy::PerCall` → return
///   [`AliasResolverError::Backend`] with a message that names the
///   path and the policy, so the caller can surface the approval
///   dialog and retry.
pub struct ApprovalGatedResolver<R, F>
where
    R: SecretResolver,
    F: Fn(&str) -> ApproveOnUsePolicy + Send + Sync,
{
    inner: R,
    cache: Arc<SessionApprovalCache>,
    policy_for_path: F,
}

impl<R, F> ApprovalGatedResolver<R, F>
where
    R: SecretResolver,
    F: Fn(&str) -> ApproveOnUsePolicy + Send + Sync,
{
    pub fn new(inner: R, cache: Arc<SessionApprovalCache>, policy_for_path: F) -> Self {
        Self {
            inner,
            cache,
            policy_for_path,
        }
    }

    /// Underlying cache handle — exposed so the orchestration
    /// layer (which drives the approval dialog) can call
    /// `record_session` after the user clicks "Allow always
    /// (this session)".
    pub fn cache(&self) -> &Arc<SessionApprovalCache> {
        &self.cache
    }
}

impl<R, F> SecretResolver for ApprovalGatedResolver<R, F>
where
    R: SecretResolver,
    F: Fn(&str) -> ApproveOnUsePolicy + Send + Sync,
{
    fn resolve(&self, path: &str) -> Result<SecretString, AliasResolverError> {
        let policy = (self.policy_for_path)(path);
        match self.cache.evaluate(path, policy) {
            ApprovalGate::NotRequired | ApprovalGate::AlreadyApproved => self.inner.resolve(path),
            ApprovalGate::PromptRequired => {
                let label = match policy {
                    ApproveOnUsePolicy::Never => "never",
                    ApproveOnUsePolicy::Session => "session",
                    ApproveOnUsePolicy::PerCall => "per-call",
                };
                Err(AliasResolverError::Backend {
                    path: path.to_owned(),
                    message: approval_unavailable_message(label),
                })
            }
        }
    }
}

/// What to say when a path asks for per-use approval.
///
/// It used to say "surface secrets_request_use_approval and
/// retry". That tool exists, and in a shipped build it always
/// fails: the only launcher compiled in is `NoopUiLauncher`, and
/// a working one is installed solely by tests. So the agent was
/// sent for permission to a door that cannot open, and the path
/// stayed unresolvable with no way out.
///
/// The per-use approval flow is also not the product model: the
/// user unlocks the vault once and the agent works, with TOTP as
/// the re-authentication step where it is configured. So the
/// honest answer is not "ask harder", it is "this gate does not
/// exist here — take it off the path".
///
/// Deliberately not downgraded to `never` behind the user's
/// back: a security setting that silently stops applying is the
/// exact failure this whole change set has been removing.
pub fn approval_unavailable_message(policy_label: &str) -> String {
    format!(
        "this path is marked `approve_on_use = {policy_label}`, and per-use approval is not \
         available in this build — there is no dialog to answer, so the value cannot be \
         resolved. Set `approve_on_use = never` on the path (the vault is unlocked once and \
         re-authenticated with TOTP where that is configured), or remove the override."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn ttl_long() -> Duration {
        Duration::from_secs(300)
    }

    // -- evaluate ---------------------------------------------------

    /// The message an agent receives when a path asks for
    /// approval this build cannot collect. It has one job: stop
    /// the agent looping and tell the human what to change.
    #[test]
    fn the_refusal_names_the_setting_and_the_way_out() {
        let m = approval_unavailable_message("per-call");

        assert!(m.contains("approve_on_use = per-call"), "{m}");
        assert!(
            m.contains("approve_on_use = never"),
            "a refusal without a way out is just a wall: {m}"
        );
        assert!(
            !m.contains("secrets_request_use_approval"),
            "pointing at a tool that always fails is what made this a dead end: {m}"
        );
        assert!(
            m.contains("not available in this build"),
            "the reason has to be stated, or it reads as a permissions problem: {m}"
        );
    }

    /// The gate still refuses — the point is the wording, not
    /// letting the value through. Silently downgrading a
    /// security setting is the failure this replaces.
    #[test]
    fn a_gated_path_is_still_refused_not_quietly_allowed() {
        let cache = SessionApprovalCache::new();
        assert_eq!(
            cache.evaluate("team/prod/db-password", ApproveOnUsePolicy::PerCall),
            ApprovalGate::PromptRequired
        );
    }

    #[test]
    fn evaluate_never_policy_returns_not_required() {
        let cache = SessionApprovalCache::new();
        assert_eq!(
            cache.evaluate("team/jira/api-key", ApproveOnUsePolicy::Never),
            ApprovalGate::NotRequired
        );
    }

    #[test]
    fn evaluate_per_call_always_prompts_even_with_cache_hit() {
        let cache = SessionApprovalCache::new();
        cache.record_session("team/jira/api-key", ttl_long());
        assert_eq!(
            cache.evaluate("team/jira/api-key", ApproveOnUsePolicy::PerCall),
            ApprovalGate::PromptRequired
        );
    }

    #[test]
    fn evaluate_session_returns_already_approved_when_cached() {
        let cache = SessionApprovalCache::new();
        cache.record_session("team/jira/api-key", ttl_long());
        assert_eq!(
            cache.evaluate("team/jira/api-key", ApproveOnUsePolicy::Session),
            ApprovalGate::AlreadyApproved
        );
    }

    #[test]
    fn evaluate_session_prompts_when_cache_miss() {
        let cache = SessionApprovalCache::new();
        assert_eq!(
            cache.evaluate("team/jira/api-key", ApproveOnUsePolicy::Session),
            ApprovalGate::PromptRequired
        );
    }

    // -- TTL --------------------------------------------------------

    #[test]
    fn cached_approval_expires_after_ttl() {
        let cache = SessionApprovalCache::new();
        cache.record_session("team/jira/api-key", Duration::from_millis(20));
        sleep(Duration::from_millis(40));
        assert_eq!(
            cache.evaluate("team/jira/api-key", ApproveOnUsePolicy::Session),
            ApprovalGate::PromptRequired
        );
    }

    #[test]
    fn is_approved_drops_expired_entry_lazily() {
        let cache = SessionApprovalCache::new();
        cache.record_session("a/b/c", Duration::from_millis(10));
        sleep(Duration::from_millis(20));
        assert!(!cache.is_approved("a/b/c"));
        assert_eq!(cache.len(), 0, "expired entry should be evicted on access");
    }

    // -- forget / clear --------------------------------------------

    #[test]
    fn forget_evicts_existing_entry() {
        let cache = SessionApprovalCache::new();
        cache.record_session("a/b/c", ttl_long());
        assert!(cache.forget("a/b/c"));
        assert!(!cache.is_approved("a/b/c"));
    }

    #[test]
    fn forget_returns_false_for_missing_entry() {
        let cache = SessionApprovalCache::new();
        assert!(!cache.forget("a/b/c"));
    }

    #[test]
    fn clear_drops_all_entries() {
        let cache = SessionApprovalCache::new();
        cache.record_session("a/b/c", ttl_long());
        cache.record_session("d/e/f", ttl_long());
        cache.clear();
        assert!(cache.is_empty());
    }

    // -- replace ----------------------------------------------------

    #[test]
    fn record_session_replaces_existing_entry() {
        let cache = SessionApprovalCache::new();
        cache.record_session("a/b/c", Duration::from_millis(10));
        sleep(Duration::from_millis(20));
        // First entry is now stale — record a fresh long-lived
        // approval. The next is_approved call must report true.
        cache.record_session("a/b/c", ttl_long());
        assert!(cache.is_approved("a/b/c"));
    }

    // -- sweep ------------------------------------------------------

    #[test]
    fn sweep_expired_drops_only_stale_entries() {
        let cache = SessionApprovalCache::new();
        cache.record_session("stale", Duration::from_millis(10));
        cache.record_session("fresh", ttl_long());
        sleep(Duration::from_millis(20));
        assert_eq!(cache.sweep_expired(), 1);
        assert!(cache.is_approved("fresh"));
        assert!(!cache.is_approved("stale"));
    }

    // -- ApprovalGatedResolver --------------------------------------

    use crate::alias::{AliasResolverError, SecretResolver};
    use secrecy::{ExposeSecret, SecretString};
    use std::sync::Mutex;

    /// Minimal in-memory resolver for gating tests. Counts
    /// calls so we can assert the gate short-circuits.
    struct CountingResolver {
        secrets: std::collections::HashMap<String, String>,
        calls: Mutex<u32>,
    }

    impl CountingResolver {
        fn new(entries: &[(&str, &str)]) -> Self {
            Self {
                secrets: entries
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
                calls: Mutex::new(0),
            }
        }
    }

    impl SecretResolver for CountingResolver {
        fn resolve(&self, path: &str) -> Result<SecretString, AliasResolverError> {
            *self.calls.lock().unwrap() += 1;
            self.secrets
                .get(path)
                .map(|v| SecretString::from(v.clone()))
                .ok_or_else(|| AliasResolverError::NotFound {
                    path: path.to_owned(),
                })
        }
    }

    #[test]
    fn gated_resolver_passes_through_never_policy() {
        let inner = CountingResolver::new(&[("team/x/y", "value-1")]);
        let cache = Arc::new(SessionApprovalCache::new());
        let gated = ApprovalGatedResolver::new(inner, cache, |_| ApproveOnUsePolicy::Never);
        let v = gated.resolve("team/x/y").unwrap();
        assert_eq!(v.expose_secret(), "value-1");
    }

    #[test]
    fn gated_resolver_refuses_session_policy_without_cache_hit() {
        let inner = CountingResolver::new(&[("team/x/y", "value-1")]);
        let cache = Arc::new(SessionApprovalCache::new());
        let gated =
            ApprovalGatedResolver::new(inner, cache.clone(), |_| ApproveOnUsePolicy::Session);
        let err = gated.resolve("team/x/y").unwrap_err();
        match err {
            AliasResolverError::Backend { path, message } => {
                assert_eq!(path, "team/x/y");
                assert!(
                    message.contains("approve_on_use = session"),
                    "the refusal must name the setting that caused it: {message}"
                );
                assert!(
                    message.contains("approve_on_use = never"),
                    "and the way out of it: {message}"
                );
            }
            other => panic!("expected Backend gate-required error, got {other:?}"),
        }
        // Inner resolver must NOT have been touched.
        // (We can't borrow the inner directly through the gate;
        // a fresh assertion below validates the same thing with
        // an explicit count.)
    }

    #[test]
    fn gated_resolver_passes_session_policy_after_cache_record() {
        let inner = CountingResolver::new(&[("team/x/y", "value-1")]);
        let cache = Arc::new(SessionApprovalCache::new());
        cache.record_session("team/x/y", ttl_long());
        let gated = ApprovalGatedResolver::new(inner, cache, |_| ApproveOnUsePolicy::Session);
        let v = gated.resolve("team/x/y").unwrap();
        assert_eq!(v.expose_secret(), "value-1");
    }

    #[test]
    fn gated_resolver_always_refuses_per_call_even_with_cache() {
        let inner = CountingResolver::new(&[("team/x/y", "value-1")]);
        let cache = Arc::new(SessionApprovalCache::new());
        cache.record_session("team/x/y", ttl_long());
        let gated = ApprovalGatedResolver::new(inner, cache, |_| ApproveOnUsePolicy::PerCall);
        let err = gated.resolve("team/x/y").unwrap_err();
        assert!(matches!(err, AliasResolverError::Backend { .. }));
    }

    #[test]
    fn gated_resolver_does_not_touch_inner_on_refusal() {
        // Build the inner outside the gate so we can re-read its
        // call count after the refusal.
        let cache = Arc::new(SessionApprovalCache::new());
        let inner_box: Box<dyn SecretResolver> =
            Box::new(CountingResolver::new(&[("team/x/y", "value-1")]));
        // Use a sneak: build the gate on an Arc-shared resolver
        // via &dyn. A small adapter that owns nothing and just
        // proxies the call count check is simpler.
        let counter = Arc::new(Mutex::new(0u32));
        let counter_clone = Arc::clone(&counter);
        struct ProxyResolver {
            inner: Box<dyn SecretResolver>,
            counter: Arc<Mutex<u32>>,
        }
        impl SecretResolver for ProxyResolver {
            fn resolve(&self, path: &str) -> Result<SecretString, AliasResolverError> {
                *self.counter.lock().unwrap() += 1;
                self.inner.resolve(path)
            }
        }
        let proxy = ProxyResolver {
            inner: inner_box,
            counter: counter_clone,
        };
        let gated = ApprovalGatedResolver::new(proxy, cache, |_| ApproveOnUsePolicy::Session);
        let _ = gated.resolve("team/x/y").unwrap_err();
        assert_eq!(
            *counter.lock().unwrap(),
            0,
            "inner resolver must not be touched on gate refusal"
        );
    }

    #[test]
    fn gated_resolver_call_count_zero_after_refusal() {
        let cache = Arc::new(SessionApprovalCache::new());
        let inner = CountingResolver::new(&[("team/prod-db/password", "v")]);
        let gated = ApprovalGatedResolver::new(inner, cache, |path| {
            if path == "team/prod-db/password" {
                ApproveOnUsePolicy::PerCall
            } else {
                ApproveOnUsePolicy::Never
            }
        });
        let _ = gated.resolve("team/prod-db/password").unwrap_err();
        // can't observe inner.call_count() here because the
        // gate owns inner; the wrapper invariant is enforced
        // by the previous test using ProxyResolver. This test
        // just exercises the per-path policy closure shape.
    }

    #[test]
    fn gated_resolver_cache_accessor_exposes_handle_for_orchestrator() {
        let inner = CountingResolver::new(&[]);
        let cache = Arc::new(SessionApprovalCache::new());
        let gated =
            ApprovalGatedResolver::new(inner, Arc::clone(&cache), |_| ApproveOnUsePolicy::Session);
        // The orchestration layer needs to record the approval
        // after the user clicks "Allow always (this session)";
        // it does so through the cached handle.
        gated.cache().record_session("a/b/c", ttl_long());
        assert!(cache.is_approved("a/b/c"));
    }
}
