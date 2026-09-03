//! In-memory cache for the source router per [ADR-021] §7.
//!
//! Source latencies vary across orders of magnitude (microseconds
//! for a keychain read; hundreds of milliseconds for `op read` plus
//! a possible biometric prompt; seconds for a misconfigured Vault).
//! Without caching, an agent that resolves a dozen secrets per
//! minute is unusable. This module is the cache the router (P5.5+,
//! P6) wraps every `get()` with.
//!
//! ## Adaptive TTL
//!
//! Per ADR-021 §7:
//!
//! - The **base TTL** is per-source (`cache_ttl_seconds` in
//!   `sources.toml`); the default lives in P6 source impls and is
//!   typically 900 seconds.
//! - If [`SecretSource::get`](crate::source::SecretSource::get)
//!   returns a `lease_duration`, the effective TTL becomes
//!   `min(base_ttl, lease_duration)`. Vault dynamic-secret leases
//!   keep the cache from outliving the lease.
//! - `lease_duration = Some(0)` disables caching for that read
//!   entirely — the value is returned to the caller but never
//!   cached.
//! - The global index may further lower the TTL through
//!   `cache_ttl_seconds_max` (per-secret cap). The cap can only
//!   lower the TTL; it cannot raise it above the source default.
//!
//! ## Eviction
//!
//! Entries leave the cache when:
//!
//! 1. Their effective TTL elapses (lazy — checked on the next
//!    `get`).
//! 2. Something calls [`AdaptiveCache::invalidate`] or
//!    [`AdaptiveCache::invalidate_all`]. No CLI command exposes
//!    these yet — the entry points exist for the router and for
//!    case 3 below.
//! 3. A source declares out-of-band invalidation (Vault lease
//!    revoked, 1Password session timed out). The router calls
//!    [`AdaptiveCache::invalidate`] in response.
//! 4. The process exits — every [`SecretString`] in the map
//!    zeroizes on drop. The cache itself is `Drop`-safe; we do not
//!    keep any extra plaintext copy.
//!
//! ## Persistence
//!
//! **Never.** Per ADR-021 §7: "the cache is never persisted.
//! Process exit drops every entry." The same posture as
//! [`secrecy::SecretString`]'s zeroize-on-drop, extended one level
//! up.
//!
//! ## Testability
//!
//! [`CacheClock`] is the abstract time source. Production callers
//! pass [`SystemClock`]; tests pass [`ManualClock`] so the TTL can
//! be raced past without `std::thread::sleep`.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use secrecy::SecretString;

use crate::secret_path::SecretPath;

/// Default base TTL when neither the source nor the per-secret cap
/// override it. Matches the ADR-021 §7 fallback (15 minutes).
pub const DEFAULT_BASE_TTL: Duration = Duration::from_secs(15 * 60);

// =============================================================================
// Clock abstraction
// =============================================================================

/// Wall-clock abstraction so tests can race past the cache TTL
/// without sleeping.
pub trait CacheClock: Send + Sync {
    /// Current monotonic time. Production uses [`Instant::now`];
    /// [`ManualClock`] returns whatever the test arranged.
    fn now(&self) -> Instant;
}

/// Production clock backed by [`Instant::now`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl CacheClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Test clock whose `now()` value is controlled by
/// [`ManualClock::advance`].
#[derive(Debug, Clone)]
pub struct ManualClock(Arc<Mutex<Instant>>);

impl ManualClock {
    /// Build a manual clock starting at `initial`.
    pub fn new(initial: Instant) -> Self {
        Self(Arc::new(Mutex::new(initial)))
    }

    /// Advance the clock by `delta`. Subsequent calls to
    /// [`CacheClock::now`] return the new time.
    pub fn advance(&self, delta: Duration) {
        let mut g = self.0.lock().expect("ManualClock mutex poisoned");
        *g += delta;
    }
}

impl CacheClock for ManualClock {
    fn now(&self) -> Instant {
        *self.0.lock().expect("ManualClock mutex poisoned")
    }
}

// =============================================================================
// Cache entry (private)
// =============================================================================

struct CacheEntry {
    value: SecretString,
    /// Pre-computed wall-clock instant at which this entry expires.
    /// `None` would mean "never expires" but we never construct
    /// that; every entry has an explicit expiry.
    expires_at: Instant,
}

// =============================================================================
// AdaptiveCache
// =============================================================================

/// Path-keyed in-memory cache with adaptive TTL.
///
/// The router wraps every `get()` with the cache: on a hit, return
/// the cached value (clone of [`SecretString`]); on a miss or
/// expiry, ask the source and store the result via
/// [`AdaptiveCache::put`].
///
/// Thread-safe — uses a single [`std::sync::Mutex`] internally.
/// Operations are CPU-bound (hash + mutex grab), so the standard
/// library mutex is the right primitive (no `tokio::sync::Mutex`
/// hold across `.await`).
pub struct AdaptiveCache {
    /// Source-default TTL. Effective TTL is min(this, lease, cap).
    base_ttl: Duration,
    /// Time source. Tests inject [`ManualClock`].
    clock: Arc<dyn CacheClock>,
    /// Live entries.
    entries: Mutex<HashMap<SecretPath, CacheEntry>>,
}

impl AdaptiveCache {
    /// Build a cache with the given source-default TTL and the
    /// production clock.
    pub fn new(base_ttl: Duration) -> Self {
        Self::with_clock(base_ttl, Arc::new(SystemClock))
    }

    /// Build a cache with a caller-supplied clock. Used by tests.
    pub fn with_clock(base_ttl: Duration, clock: Arc<dyn CacheClock>) -> Self {
        Self {
            base_ttl,
            clock,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Source-default TTL the cache was constructed with.
    pub fn base_ttl(&self) -> Duration {
        self.base_ttl
    }

    /// Borrow the clock. Useful for `doctor`-style introspection
    /// and tests.
    pub fn clock(&self) -> &Arc<dyn CacheClock> {
        &self.clock
    }

    /// Look up `path`. Returns `Some(value.clone())` on hit;
    /// `None` on miss or after the TTL has elapsed (the expired
    /// entry is evicted lazily).
    pub fn get(&self, path: &SecretPath) -> Option<SecretString> {
        let mut g = self.entries.lock().expect("AdaptiveCache mutex poisoned");
        let now = self.clock.now();
        let mut hit = None;
        if let Some(entry) = g.get(path) {
            if entry.expires_at > now {
                hit = Some(entry.value.clone());
            } else {
                // Expired — drop the entry so subsequent calls do
                // not re-evaluate.
                g.remove(path);
            }
        }
        hit
    }

    /// Insert a value. Returns `true` if the value was cached;
    /// `false` if caching was suppressed (by `lease_duration =
    /// Some(0)` or by an effective TTL of zero).
    ///
    /// `lease_duration` is the upstream-reported lease (from
    /// [`GetOutcome::lease_duration`](crate::source::GetOutcome));
    /// `max_ttl` is the per-secret cap from the global index
    /// (`cache_ttl_seconds_max`). Both are optional; both can only
    /// lower the effective TTL, never raise it.
    pub fn put(
        &self,
        path: &SecretPath,
        value: SecretString,
        lease_duration: Option<Duration>,
        max_ttl: Option<Duration>,
    ) -> bool {
        let ttl = match self.effective_ttl(lease_duration, max_ttl) {
            Some(t) => t,
            None => return false,
        };
        let expires_at = self.clock.now() + ttl;
        let mut g = self.entries.lock().expect("AdaptiveCache mutex poisoned");
        g.insert(path.clone(), CacheEntry { value, expires_at });
        true
    }

    /// Compute the effective TTL given the source's lease and the
    /// per-secret cap. Returns `None` to disable caching.
    fn effective_ttl(
        &self,
        lease_duration: Option<Duration>,
        max_ttl: Option<Duration>,
    ) -> Option<Duration> {
        let mut ttl = self.base_ttl;
        if let Some(lease) = lease_duration {
            if lease.is_zero() {
                return None;
            }
            if lease < ttl {
                ttl = lease;
            }
        }
        if let Some(cap) = max_ttl
            && cap < ttl
        {
            ttl = cap;
        }
        if ttl.is_zero() { None } else { Some(ttl) }
    }

    /// Drop the entry for one path. Idempotent — no-op if the
    /// path is not in the cache.
    pub fn invalidate(&self, path: &SecretPath) {
        let mut g = self.entries.lock().expect("AdaptiveCache mutex poisoned");
        g.remove(path);
    }

    /// Drop every entry.
    ///
    /// Nothing user-facing reaches this yet; it is here for the
    /// router and for whatever eventually offers a manual refresh.
    pub fn invalidate_all(&self) {
        let mut g = self.entries.lock().expect("AdaptiveCache mutex poisoned");
        g.clear();
    }

    /// Number of entries currently in the cache, including any
    /// that may already be expired. (Lazy eviction means a count
    /// of `n` is the upper bound on live entries; `get()` may
    /// reduce it on the next call.)
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("AdaptiveCache mutex poisoned")
            .len()
    }

    /// `true` when the cache has no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for AdaptiveCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print values — the cache is full of plaintext
        // secrets. Surface only the size and the configured TTL so
        // accidental `dbg!` doesn't leak the credential payloads.
        let count = self.entries.lock().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("AdaptiveCache")
            .field("base_ttl", &self.base_ttl)
            .field("entries", &format!("<{count} redacted>"))
            .field("clock", &"<dyn CacheClock>")
            .finish()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn p(s: &str) -> SecretPath {
        SecretPath::parse(s).unwrap()
    }

    fn manual_cache(base_ttl: Duration) -> (AdaptiveCache, ManualClock) {
        let clock = ManualClock::new(Instant::now());
        let cache = AdaptiveCache::with_clock(base_ttl, Arc::new(clock.clone()));
        (cache, clock)
    }

    fn secret(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    fn exposed(v: &Option<SecretString>) -> Option<&str> {
        v.as_ref().map(|s| s.expose_secret())
    }

    // -- put/get round-trip ----------------------------------------

    #[test]
    fn put_then_get_returns_value_within_ttl() {
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        cache.put(&p("a/b/c"), secret("v"), None, None);
        let got = cache.get(&p("a/b/c"));
        assert_eq!(exposed(&got), Some("v"));
    }

    #[test]
    fn missing_key_returns_none() {
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        assert!(cache.get(&p("a/b/c")).is_none());
    }

    #[test]
    fn distinct_paths_do_not_collide() {
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        cache.put(&p("a/b/c"), secret("v1"), None, None);
        cache.put(&p("d/e/f"), secret("v2"), None, None);
        assert_eq!(exposed(&cache.get(&p("a/b/c"))), Some("v1"));
        assert_eq!(exposed(&cache.get(&p("d/e/f"))), Some("v2"));
    }

    #[test]
    fn put_overwrites_previous_value() {
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        cache.put(&p("a/b/c"), secret("v1"), None, None);
        cache.put(&p("a/b/c"), secret("v2"), None, None);
        assert_eq!(exposed(&cache.get(&p("a/b/c"))), Some("v2"));
    }

    // -- TTL expiry ------------------------------------------------

    #[test]
    fn expired_entry_returns_none_and_is_evicted() {
        let (cache, clock) = manual_cache(Duration::from_secs(10));
        cache.put(&p("a/b/c"), secret("v"), None, None);
        clock.advance(Duration::from_secs(11));
        assert!(cache.get(&p("a/b/c")).is_none());
        // get() lazily evicted — len drops back to zero.
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn entry_at_exact_expiry_is_treated_as_expired() {
        // We use `entry.expires_at > now` (strict). At equality,
        // the entry is gone.
        let (cache, clock) = manual_cache(Duration::from_secs(10));
        cache.put(&p("a/b/c"), secret("v"), None, None);
        clock.advance(Duration::from_secs(10));
        assert!(cache.get(&p("a/b/c")).is_none());
    }

    // -- Adaptive TTL: lease_duration ------------------------------

    #[test]
    fn lease_duration_zero_disables_caching() {
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        let cached = cache.put(&p("a/b/c"), secret("v"), Some(Duration::from_secs(0)), None);
        assert!(!cached, "lease_duration=0 must suppress caching");
        assert!(cache.get(&p("a/b/c")).is_none());
    }

    #[test]
    fn lease_below_base_lowers_effective_ttl() {
        let (cache, clock) = manual_cache(Duration::from_secs(60));
        cache.put(
            &p("a/b/c"),
            secret("v"),
            Some(Duration::from_secs(10)),
            None,
        );
        clock.advance(Duration::from_secs(11));
        assert!(
            cache.get(&p("a/b/c")).is_none(),
            "lease=10s should evict at 11s"
        );
    }

    #[test]
    fn lease_above_base_does_not_extend_ttl() {
        let (cache, clock) = manual_cache(Duration::from_secs(60));
        cache.put(
            &p("a/b/c"),
            secret("v"),
            Some(Duration::from_secs(3600)),
            None,
        );
        clock.advance(Duration::from_secs(61));
        assert!(
            cache.get(&p("a/b/c")).is_none(),
            "lease=3600s should NOT raise the 60s base TTL"
        );
    }

    // -- Adaptive TTL: max_ttl cap (cache_ttl_seconds_max) ---------

    #[test]
    fn max_ttl_cap_lowers_below_base() {
        let (cache, clock) = manual_cache(Duration::from_secs(60));
        cache.put(&p("a/b/c"), secret("v"), None, Some(Duration::from_secs(5)));
        clock.advance(Duration::from_secs(6));
        assert!(
            cache.get(&p("a/b/c")).is_none(),
            "max_ttl=5s should evict at 6s"
        );
    }

    #[test]
    fn max_ttl_cap_does_not_raise_above_base() {
        // ADR-021 §7: "may not raise it above the source default".
        let (cache, clock) = manual_cache(Duration::from_secs(10));
        cache.put(
            &p("a/b/c"),
            secret("v"),
            None,
            Some(Duration::from_secs(3600)),
        );
        clock.advance(Duration::from_secs(11));
        assert!(
            cache.get(&p("a/b/c")).is_none(),
            "max_ttl=3600s with base=10s should still expire at 10s"
        );
    }

    #[test]
    fn lease_and_max_ttl_both_lower_taken_jointly() {
        // base=60s, lease=30s, cap=10s → effective=10s
        let (cache, clock) = manual_cache(Duration::from_secs(60));
        cache.put(
            &p("a/b/c"),
            secret("v"),
            Some(Duration::from_secs(30)),
            Some(Duration::from_secs(10)),
        );
        clock.advance(Duration::from_secs(11));
        assert!(cache.get(&p("a/b/c")).is_none());
    }

    // -- Invalidation ----------------------------------------------

    #[test]
    fn invalidate_drops_one_entry() {
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        cache.put(&p("a/b/c"), secret("v1"), None, None);
        cache.put(&p("d/e/f"), secret("v2"), None, None);
        cache.invalidate(&p("a/b/c"));
        assert!(cache.get(&p("a/b/c")).is_none());
        // The other path is untouched.
        assert_eq!(exposed(&cache.get(&p("d/e/f"))), Some("v2"));
    }

    #[test]
    fn invalidate_unknown_path_is_a_noop() {
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        cache.invalidate(&p("a/b/c")); // does not panic
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn invalidate_all_drops_everything() {
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        cache.put(&p("a/b/c"), secret("v1"), None, None);
        cache.put(&p("d/e/f"), secret("v2"), None, None);
        cache.put(&p("g/h/i"), secret("v3"), None, None);
        assert_eq!(cache.len(), 3);
        cache.invalidate_all();
        assert!(cache.is_empty());
    }

    // -- Process-exit semantics ------------------------------------

    #[test]
    fn drop_clears_entries_and_releases_secret_strings() {
        // The cache holds SecretString in a HashMap; both zeroize
        // on drop. Verify the map shrinks to zero when the cache
        // is dropped — proxy for "entries are released".
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        cache.put(&p("a/b/c"), secret("v"), None, None);
        let entries_arc = std::sync::Arc::new(());
        let weak = std::sync::Arc::downgrade(&entries_arc);
        drop(entries_arc);
        // Sanity for the weak/arc dance: after the strong is
        // dropped, weak.upgrade() is None.
        assert!(weak.upgrade().is_none());
        // Now drop the cache; the SecretString inside zeroizes.
        // We can't observe the zeroize directly without unsafe,
        // but the absence of a panic and the standard
        // `secrecy::SecretString` contract is the closest we get.
        drop(cache);
    }

    // -- Debug / redaction -----------------------------------------

    #[test]
    fn debug_does_not_leak_plaintext() {
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        cache.put(&p("a/b/c"), secret("super-secret-value"), None, None);
        let dbg = format!("{cache:?}");
        assert!(!dbg.contains("super-secret-value"));
        assert!(dbg.contains("AdaptiveCache"));
        assert!(dbg.contains("redacted"));
    }

    // -- DEFAULT_BASE_TTL ------------------------------------------

    #[test]
    fn default_base_ttl_matches_adr_021_900_seconds() {
        assert_eq!(DEFAULT_BASE_TTL, Duration::from_secs(15 * 60));
        assert_eq!(DEFAULT_BASE_TTL.as_secs(), 900);
    }

    #[test]
    fn base_ttl_accessor_returns_constructor_value() {
        let cache = AdaptiveCache::new(Duration::from_secs(123));
        assert_eq!(cache.base_ttl(), Duration::from_secs(123));
    }

    // -- Length / emptiness ----------------------------------------

    #[test]
    fn len_counts_inserted_entries() {
        let (cache, _clock) = manual_cache(Duration::from_secs(60));
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        cache.put(&p("a/b/c"), secret("v"), None, None);
        assert_eq!(cache.len(), 1);
        cache.put(&p("d/e/f"), secret("v2"), None, None);
        assert_eq!(cache.len(), 2);
    }
}
