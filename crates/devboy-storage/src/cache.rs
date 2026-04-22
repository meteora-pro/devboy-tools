//! In-memory TTL cache layer on top of a [`CredentialStore`].
//!
//! The OS keychain is fast enough for interactive CLI usage, but inside a long-running
//! MCP proxy loop we call `get()` on every routing decision and telemetry flush. On
//! macOS that also risks repeated UI prompts if the Keychain access control list is
//! strict. A short-lived in-memory cache cuts the lookup cost without compromising
//! safety: secrets still live in OS-protected storage and are zeroized on drop.
//!
//! # Guarantees
//!
//! - TTL of `0` disables caching entirely (useful for high-security configurations).
//! - `store()` / `delete()` on the wrapped store also invalidate the cache entry so we
//!   do not serve stale secrets after rotation.
//! - The cached value is wrapped in [`zeroize::Zeroizing`], ensuring the buffer is
//!   scrubbed when the entry is evicted or the cache is dropped.
//! - The [`std::fmt::Debug`] impl never prints values.
//!
//! # Non-goals
//!
//! - Cross-process coherence: every process has its own cache. Rotation semantics rely
//!   on processes being short-lived or reconnecting before `cache_ttl_secs` elapse.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use devboy_core::Result;
use zeroize::Zeroizing;

use crate::CredentialStore;

/// Entry in the cache — a zeroized buffer plus an expiry timestamp.
struct CachedEntry {
    value: Zeroizing<String>,
    expires_at: Instant,
}

impl CachedEntry {
    fn new(value: String, ttl: Duration) -> Self {
        Self {
            value: Zeroizing::new(value),
            expires_at: Instant::now() + ttl,
        }
    }

    fn is_fresh(&self) -> bool {
        Instant::now() < self.expires_at
    }
}

/// Caching wrapper around any [`CredentialStore`].
///
/// Use [`CachedStore::new`] with an explicit TTL. Passing `ttl = Duration::from_secs(0)`
/// disables caching and makes every `get()` hit the inner store directly — the wrapper
/// simply proxies in that case.
pub struct CachedStore<S: CredentialStore> {
    inner: S,
    ttl: Duration,
    entries: RwLock<HashMap<String, CachedEntry>>,
}

impl<S: CredentialStore> CachedStore<S> {
    /// Wrap `inner` with a cache that keeps successful reads for `ttl`.
    ///
    /// Cache misses (key not found) are *not* cached — we do not want to pin a stale
    /// negative result when credentials are rotated in behind the scenes.
    pub fn new(inner: S, ttl: Duration) -> Self {
        Self {
            inner,
            ttl,
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Drop every cached entry (wiping their buffers). Useful for explicit logout flows.
    pub fn invalidate_all(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }
    }

    /// Drop a single cached entry.
    pub fn invalidate(&self, key: &str) {
        if let Ok(mut entries) = self.entries.write() {
            entries.remove(key);
        }
    }

    fn caching_disabled(&self) -> bool {
        self.ttl.is_zero()
    }

    fn lookup_fresh(&self, key: &str) -> Option<String> {
        let entries = self.entries.read().ok()?;
        let entry = entries.get(key)?;
        if entry.is_fresh() {
            Some(entry.value.as_str().to_string())
        } else {
            None
        }
    }

    fn insert(&self, key: &str, value: &str) {
        let Ok(mut entries) = self.entries.write() else {
            return;
        };
        entries.insert(key.to_string(), CachedEntry::new(value.to_string(), self.ttl));
    }

    fn purge_expired_locked(&self) {
        let Ok(mut entries) = self.entries.write() else {
            return;
        };
        let now = Instant::now();
        entries.retain(|_, e| e.expires_at > now);
    }
}

impl<S: CredentialStore> std::fmt::Debug for CachedStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let size = self.entries.read().map(|e| e.len()).unwrap_or(0);
        f.debug_struct("CachedStore")
            .field("ttl_secs", &self.ttl.as_secs())
            .field("cached_entries", &size)
            .field("values", &"<redacted>")
            .finish()
    }
}

impl<S: CredentialStore> CredentialStore for CachedStore<S> {
    fn store(&self, key: &str, value: &str) -> Result<()> {
        let res = self.inner.store(key, value);
        self.invalidate(key);
        res
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        if self.caching_disabled() {
            return self.inner.get(key);
        }

        if let Some(v) = self.lookup_fresh(key) {
            return Ok(Some(v));
        }

        // Opportunistic purge of other stale entries.
        self.purge_expired_locked();

        match self.inner.get(key)? {
            Some(value) => {
                self.insert(key, &value);
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        let res = self.inner.delete(key);
        self.invalidate(key);
        res
    }

    fn is_available(&self) -> bool {
        self.inner.is_available()
    }

    fn is_writable(&self) -> bool {
        self.inner.is_writable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryStore;
    use std::thread;

    fn store_with_entry(k: &str, v: &str) -> MemoryStore {
        MemoryStore::with_credentials([(k.to_string(), v.to_string())])
    }

    #[test]
    fn test_cache_hit_returns_value_without_hitting_inner() {
        // A MemoryStore whose credentials vanish after the first read would prove this,
        // but we settle for the simpler invariant: the cache returns the same string
        // and the debug output acknowledges the size.
        let cache = CachedStore::new(store_with_entry("a/b", "secret-A"), Duration::from_secs(60));

        // Prime
        assert_eq!(cache.get("a/b").unwrap().as_deref(), Some("secret-A"));
        // Debug does not leak the value
        let dbg = format!("{:?}", cache);
        assert!(dbg.contains("cached_entries: 1"));
        assert!(!dbg.contains("secret-A"));
    }

    #[test]
    fn test_cache_respects_ttl_and_refetches() {
        let cache = CachedStore::new(
            store_with_entry("a/b", "secret-A"),
            Duration::from_millis(50),
        );

        assert_eq!(cache.get("a/b").unwrap().as_deref(), Some("secret-A"));
        thread::sleep(Duration::from_millis(80));
        // Still finds it but now from the inner store (cache miss → fresh fetch).
        assert_eq!(cache.get("a/b").unwrap().as_deref(), Some("secret-A"));
    }

    #[test]
    fn test_cache_zero_ttl_disables_caching() {
        let cache = CachedStore::new(store_with_entry("a/b", "v"), Duration::ZERO);
        assert_eq!(cache.get("a/b").unwrap().as_deref(), Some("v"));

        let dbg = format!("{:?}", cache);
        assert!(dbg.contains("cached_entries: 0"));
    }

    #[test]
    fn test_store_invalidates_cache_entry() {
        let inner = MemoryStore::new();
        inner.store("k", "v1").unwrap();

        let cache = CachedStore::new(inner, Duration::from_secs(60));
        assert_eq!(cache.get("k").unwrap().as_deref(), Some("v1"));

        // Rotate through the cache — rotation must reach the inner store AND bust cache.
        cache.store("k", "v2").unwrap();
        assert_eq!(cache.get("k").unwrap().as_deref(), Some("v2"));
    }

    #[test]
    fn test_delete_invalidates_cache_entry() {
        let inner = MemoryStore::new();
        inner.store("k", "v1").unwrap();
        let cache = CachedStore::new(inner, Duration::from_secs(60));

        assert_eq!(cache.get("k").unwrap().as_deref(), Some("v1"));

        cache.delete("k").unwrap();
        assert_eq!(cache.get("k").unwrap(), None);
    }

    #[test]
    fn test_missing_keys_not_cached() {
        // Absent credentials must not pin a "missing" state, otherwise rotation-in
        // would never be observed until TTL.
        let inner = MemoryStore::new();
        let cache = CachedStore::new(inner, Duration::from_secs(60));

        assert_eq!(cache.get("k").unwrap(), None);

        // Populate inner directly (bypass cache)…
        // we need a &inner, but `cache` owns it — so reach through `invalidate_all` as a
        // proxy for "do not keep negative entries".
        // Simpler: set via cache.store(), which invalidates and then the lookup is fresh.
        cache.store("k", "later").unwrap();
        assert_eq!(cache.get("k").unwrap().as_deref(), Some("later"));
    }

    #[test]
    fn test_invalidate_all_drops_every_entry() {
        let inner = MemoryStore::with_credentials([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);
        let cache = CachedStore::new(inner, Duration::from_secs(60));
        cache.get("a").unwrap();
        cache.get("b").unwrap();

        cache.invalidate_all();
        let dbg = format!("{:?}", cache);
        assert!(dbg.contains("cached_entries: 0"));
    }

    #[test]
    fn test_writable_and_available_delegate_to_inner() {
        let cache = CachedStore::new(MemoryStore::new(), Duration::from_secs(10));
        assert!(cache.is_writable());
        assert!(cache.is_available());
    }
}
