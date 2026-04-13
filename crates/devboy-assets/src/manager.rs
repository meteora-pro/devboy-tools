//! High-level orchestrator combining the cache, index, and rotator.
//!
//! [`AssetManager`] is the entry point used by the rest of devboy-tools. It
//! hides the split between the physical cache directory and the in-memory
//! index, and enforces the rotation policy on every write.
//!
//! ## Concurrency and blocking
//!
//! The manager wraps its mutable state in a [`std::sync::Mutex`] so it can
//! be freely cloned via an `Arc` (e.g. to be shared across tokio tasks).
//! The mutex serializes updates to the in-memory [`AssetIndex`] and
//! rotation bookkeeping: reads, writes, touches, rotation, and index
//! persistence all synchronize through the same critical section.
//!
//! **Several code paths also perform filesystem I/O while holding the
//! lock**, including:
//!
//! - [`AssetManager::get`] — `is_file()` existence check on the target
//!   file before returning the metadata
//! - [`AssetManager::delete`] — `CacheManager::delete` on the target file
//! - [`AssetManager::store`] — rotation may delete evicted files, and
//!   `AssetIndex::save` writes `index.json` atomically (temp file + rename)
//! - [`AssetManager::rotate_now`] — same rotation + persist path
//!
//! The only filesystem write that intentionally runs **before** the lock
//! is acquired is the initial `CacheManager::store` inside
//! [`AssetManager::store`], which writes the new blob to a path derived
//! from the caller-supplied `asset_id` and `filename`. Assuming the
//! caller provides unique `asset_id` values (which is part of the API
//! contract), two concurrent writes target different paths and cannot
//! collide. Reusing the same `asset_id` concurrently is unsupported and
//! may race on the underlying file.
//!
//! Because of the above, callers must not assume these methods are free
//! from blocking filesystem work — if they are invoked from an async
//! context, wrap them in `tokio::task::spawn_blocking` (or equivalent)
//! so the executor's worker thread isn't stalled on disk I/O.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use devboy_core::asset::AssetContext;

use crate::cache::{CacheManager, resolve_under_root};
use crate::config::{AssetConfig, ResolvedAssetConfig};
use crate::error::{AssetError, Result};
use crate::index::{AssetIndex, CachedAsset, INDEX_FILENAME, NewCachedAsset};
use crate::rotation::{RotationStats, Rotator};

/// Top-level asset cache.
#[derive(Debug, Clone)]
pub struct AssetManager {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    config: ResolvedAssetConfig,
    cache: CacheManager,
    rotator: Rotator,
    state: Mutex<AssetIndex>,
}

impl AssetManager {
    /// Build a new manager from a raw [`AssetConfig`].
    pub fn from_config(config: AssetConfig) -> Result<Self> {
        let resolved = config.resolve()?;
        Self::from_resolved(resolved)
    }

    /// Build a new manager from a validated [`ResolvedAssetConfig`].
    ///
    /// Startup flow:
    ///
    /// 1. Load the on-disk index (recovering an empty one if the file is
    ///    missing or corrupt).
    /// 2. **Integrity check** — drop entries whose file is gone or whose
    ///    stored path resolves outside the cache root.
    /// 3. **Rotate** — enforce `max_file_age` and `max_cache_size` on the
    ///    loaded state so a cache that was closed over budget comes back
    ///    up within limits before any new writes.
    /// 4. Persist the resulting index.
    pub fn from_resolved(config: ResolvedAssetConfig) -> Result<Self> {
        let cache = CacheManager::new(config.cache_dir.clone())?;
        let rotator = Rotator::new(&config);
        let mut index = AssetIndex::load(&config.cache_dir)?;
        let pruned = prune_stale_entries(&mut index, &cache);
        let rotated = rotator.rotate(&mut index, &cache)?;
        if pruned > 0 || rotated.removed() > 0 {
            tracing::debug!(
                pruned,
                aged_out = rotated.aged_out,
                size_evicted = rotated.size_evicted,
                bytes_freed = rotated.bytes_freed,
                "asset cache startup cleanup",
            );
        }
        index.save(&config.cache_dir)?;

        Ok(Self {
            inner: Arc::new(Inner {
                config,
                cache,
                rotator,
                state: Mutex::new(index),
            }),
        })
    }

    /// Convenience constructor for tests, standalone usage, and minimal
    /// environments (containers, CI) where `dirs::cache_dir()` may return
    /// `None`.
    ///
    /// Unlike [`AssetManager::from_config`], this bypasses OS cache-dir
    /// discovery entirely and uses sensible defaults for all other
    /// settings (1 GiB budget, 7-day TTL, LRU eviction).
    pub fn with_root(root: PathBuf) -> Result<Self> {
        use crate::config::{
            DEFAULT_MAX_CACHE_SIZE, DEFAULT_MAX_FILE_AGE, EvictionPolicy, ResolvedAssetConfig,
            parse_duration, parse_size,
        };
        let resolved = ResolvedAssetConfig {
            cache_dir: root,
            max_cache_size: parse_size(DEFAULT_MAX_CACHE_SIZE)
                .expect("default cache size is valid"),
            max_file_age: parse_duration(DEFAULT_MAX_FILE_AGE).expect("default file age is valid"),
            eviction_policy: EvictionPolicy::Lru,
        };
        Self::from_resolved(resolved)
    }

    /// Absolute path to the cache directory.
    pub fn cache_dir(&self) -> &Path {
        &self.inner.config.cache_dir
    }

    /// Full resolved configuration.
    pub fn config(&self) -> &ResolvedAssetConfig {
        &self.inner.config
    }

    /// Store a new asset, persisting the updated index and running a
    /// rotation pass. Returns the newly created [`CachedAsset`].
    ///
    /// If the payload is larger than the configured
    /// [`ResolvedAssetConfig::max_cache_size`] this returns an
    /// [`AssetError::Config`] error *without* touching the filesystem —
    /// otherwise rotation would immediately evict the just-written file and
    /// we'd hand back an asset id that no longer resolves.
    ///
    /// A `max_cache_size` of `0` is treated as **unlimited**. Both this
    /// method and [`Rotator::rotate`] share that convention so the two
    /// cannot disagree about whether the budget is exhausted.
    pub fn store(&self, request: StoreRequest<'_>) -> Result<CachedAsset> {
        let StoreRequest {
            context,
            asset_id: asset_id_opt,
            filename,
            mime_type,
            remote_url,
            data,
        } = request;

        // Resolve the asset ID: caller-provided or content-addressed.
        let asset_id_owned: String;
        let asset_id: &str = match asset_id_opt {
            Some(id) => {
                let trimmed = id.trim();
                if trimmed.is_empty() {
                    return Err(AssetError::config("asset_id must not be empty"));
                }
                if trimmed.len() > MAX_ASSET_ID_LEN {
                    return Err(AssetError::config(format!(
                        "asset_id is {} chars, max allowed is {MAX_ASSET_ID_LEN}",
                        trimmed.len(),
                    )));
                }
                trimmed
            }
            None => {
                asset_id_owned = Self::content_id(data);
                &asset_id_owned
            }
        };

        let size = data.len() as u64;
        let max = self.inner.config.max_cache_size;
        if max > 0 && size > max {
            return Err(AssetError::config(format!(
                "asset '{asset_id}' is {size} bytes, which exceeds the cache \
                 budget of {max} bytes; increase `[assets] max_cache_size` or \
                 split the file",
            )));
        }

        let stored = self.inner.cache.store(&context, asset_id, filename, data)?;
        let rel_path = stored
            .path
            .strip_prefix(self.inner.cache.root())
            .map_err(|e| AssetError::cache_dir(format!("path outside cache root: {e}")))?
            .to_path_buf();

        let asset = CachedAsset::new(NewCachedAsset {
            id: asset_id.to_string(),
            filename: filename.to_string(),
            mime_type,
            size: stored.size,
            local_path: rel_path,
            context,
            checksum_sha256: stored.checksum_sha256,
            remote_url,
        });

        // Index upsert + rotation + persist. If any step fails we must
        // restore the in-memory index to its pre-mutation state *and*
        // remove the orphaned blob from disk so the cache stays
        // transactional and doesn't leak disk space.
        let mut deferred_delete: Option<PathBuf> = None;
        // Track whether a previous entry occupied the same on-disk path.
        // When true, CacheManager::store already overwrote the old blob
        // in-place, so the rollback path must NOT delete stored.path —
        // it's the only copy left and the restored snapshot still points
        // at it.
        let mut overwrote_same_path = false;
        let result: Result<()> = (|| {
            let mut index = self.state_lock()?;

            if let Some(previous) = index.get(asset.id.as_str()) {
                if previous.local_path == asset.local_path {
                    // Same on-disk path — blob was overwritten in-place.
                    overwrote_same_path = true;
                } else {
                    // Different path — record the old blob for deferred
                    // deletion *after* the commit succeeds.
                    deferred_delete =
                        resolve_under_root(&self.inner.config.cache_dir, &previous.local_path);
                }
            }

            // Snapshot the index so we can restore on failure.
            let snapshot = index.clone();

            index.upsert(asset.clone());
            if let Err(e) = self
                .inner
                .rotator
                .rotate(&mut index, &self.inner.cache)
                .and_then(|_| {
                    // Guard: if rotation evicted the asset we just stored,
                    // roll back rather than returning metadata that
                    // immediately misses on `get()`.
                    if index.get(asset.id.as_str()).is_none() {
                        return Err(AssetError::config(format!(
                            "asset '{}' was evicted immediately after store — \
                             the cache budget ({} bytes) is too small for this file \
                             ({} bytes) alongside existing entries",
                            asset.id, self.inner.config.max_cache_size, asset.size,
                        )));
                    }
                    index.save(&self.inner.config.cache_dir)
                })
            {
                // Restore the snapshot so list()/total_size() stay
                // consistent with what's actually on disk.
                *index = snapshot;
                deferred_delete = None; // don't delete old blob on rollback
                return Err(e);
            }
            Ok(())
        })();

        if let Err(e) = result {
            // Roll back: remove the orphaned blob — but only when it's a
            // truly new path. If the previous entry occupied the same
            // on-disk location, CacheManager::store already overwrote the
            // old bytes in-place, so deleting the file would leave the
            // restored snapshot pointing at nothing.
            //
            // Trade-off: when same-path overwrite is rolled back, the
            // file on disk contains the *new* bytes, not the original
            // ones — the pre-existing content is lost. Full byte-level
            // restoration (backup + restore, or write-to-temp + rename)
            // is intentionally not implemented: this is an ephemeral
            // cache and any file can be re-downloaded from the provider.
            if !overwrote_same_path {
                let _ = self.inner.cache.delete(&stored.path);
            }
            return Err(e);
        }

        // Commit succeeded — now safe to clean up the replaced blob.
        if let Some(old_path) = deferred_delete {
            let _ = self.inner.cache.delete(&old_path);
        }

        Ok(asset)
    }

    /// Look up an asset by id and return the absolute path on disk if it
    /// is still cached. Also touches `last_accessed` and persists the
    /// index if the asset was found.
    ///
    /// The returned [`ResolvedAsset::asset`] reflects the *post-touch*
    /// state, so `asset.last_accessed_ms` is the timestamp just written
    /// to the index rather than the stale pre-touch value.
    pub fn get(&self, asset_id: &str) -> Result<Option<ResolvedAsset>> {
        let mut index = self.state_lock()?;
        // Resolve the path using a borrowed lookup first — no clone yet.
        let (abs_path, remove_stale) = match index.get(asset_id) {
            Some(asset) => {
                match resolve_under_root(&self.inner.config.cache_dir, &asset.local_path) {
                    Some(abs) => (Some(abs), false),
                    None => {
                        tracing::warn!(
                            asset_id,
                            path = ?asset.local_path,
                            "dropping index entry with unsafe local_path",
                        );
                        (None, true)
                    }
                }
            }
            None => return Ok(None),
        };

        if remove_stale {
            index.remove(asset_id);
            index.save(&self.inner.config.cache_dir)?;
            return Ok(None);
        }
        let abs_path = abs_path.expect("abs_path set when remove_stale is false");

        if !abs_path.is_file() {
            // File vanished — drop the stale entry.
            index.remove(asset_id);
            index.save(&self.inner.config.cache_dir)?;
            return Ok(None);
        }

        // Touch first, then clone so the returned metadata carries the
        // freshly-written `last_accessed_ms`.
        index.touch(asset_id);
        index.save(&self.inner.config.cache_dir)?;
        let asset = index
            .get(asset_id)
            .cloned()
            .expect("asset still present after touch");
        Ok(Some(ResolvedAsset {
            asset,
            absolute_path: abs_path,
        }))
    }

    /// Delete an asset from the cache (both the index entry and the file).
    /// Returns `true` if the asset was present.
    pub fn delete(&self, asset_id: &str) -> Result<bool> {
        let mut index = self.state_lock()?;
        let Some(asset) = index.remove(asset_id) else {
            return Ok(false);
        };
        if let Some(abs_path) = resolve_under_root(&self.inner.config.cache_dir, &asset.local_path)
        {
            self.inner.cache.delete(&abs_path)?;
        } else {
            tracing::warn!(
                asset_id,
                path = ?asset.local_path,
                "skipping filesystem delete for unsafe local_path",
            );
        }
        index.save(&self.inner.config.cache_dir)?;
        Ok(true)
    }

    /// List all assets currently tracked.
    ///
    /// Returns [`AssetError::Poisoned`] if the in-memory index mutex is
    /// poisoned; callers that want a best-effort snapshot can
    /// `.unwrap_or_default()` on the result.
    pub fn list(&self) -> Result<Vec<CachedAsset>> {
        Ok(self.state_lock()?.assets.values().cloned().collect())
    }

    /// Total tracked bytes in the cache.
    ///
    /// Returns [`AssetError::Poisoned`] if the in-memory index mutex is
    /// poisoned.
    pub fn total_size(&self) -> Result<u64> {
        Ok(self.state_lock()?.total_size())
    }

    /// Force a rotation pass immediately.
    pub fn rotate_now(&self) -> Result<RotationStats> {
        let mut index = self.state_lock()?;
        let stats = self.inner.rotator.rotate(&mut index, &self.inner.cache)?;
        index.save(&self.inner.config.cache_dir)?;
        Ok(stats)
    }

    /// Re-check index vs filesystem. Returns the number of dropped entries.
    pub fn integrity_check(&self) -> Result<usize> {
        let mut index = self.state_lock()?;
        let removed = prune_stale_entries(&mut index, &self.inner.cache);
        if removed > 0 {
            index.save(&self.inner.config.cache_dir)?;
        }
        Ok(removed)
    }

    /// Path to the on-disk index file. Useful for diagnostics and tests.
    pub fn index_path(&self) -> PathBuf {
        self.inner.config.cache_dir.join(INDEX_FILENAME)
    }

    /// Compute a content-addressed asset ID from raw bytes.
    ///
    /// The returned string has the form `sha256:{16 hex chars}` (64-bit
    /// prefix of the SHA-256 digest). This is the same ID that
    /// [`AssetManager::store`] generates when `asset_id` is `None`.
    ///
    /// Use this to check whether a file is already cached before
    /// downloading it:
    ///
    /// ```ignore
    /// let id = AssetManager::content_id(data);
    /// if manager.get(&id)?.is_some() { /* already cached */ }
    /// ```
    pub fn content_id(data: &[u8]) -> String {
        let hash = crate::cache::sha256_hex(data);
        format!("sha256:{}", &hash[..16])
    }

    /// Acquire the in-memory index lock.
    ///
    /// Returns [`AssetError::Poisoned`] if the mutex was poisoned by a
    /// panic in another thread, giving callers a chance to recover or
    /// propagate the error gracefully instead of crashing.
    fn state_lock(&self) -> Result<std::sync::MutexGuard<'_, AssetIndex>> {
        self.inner
            .state
            .lock()
            .map_err(|e| AssetError::poisoned(e.to_string()))
    }
}

/// Resolved lookup result — both the cached metadata and the absolute
/// path of the file on disk.
#[derive(Debug, Clone)]
pub struct ResolvedAsset {
    /// Metadata from the index.
    pub asset: CachedAsset,
    /// Absolute path to the file on disk.
    pub absolute_path: PathBuf,
}

/// Parameters for [`AssetManager::store`].
#[derive(Debug)]
pub struct StoreRequest<'a> {
    /// Context the asset is attached to.
    pub context: AssetContext,
    /// Stable identifier for the asset.
    ///
    /// **Dual-mode:**
    /// - `Some("att-42")` — caller-provided ID, typically the provider's
    ///   native attachment identifier (ClickUp attachment id, Jira
    ///   attachment id, GitLab upload URL, etc.). This enables cache-hit
    ///   lookups when the same attachment is requested again.
    /// - `None` — auto-generate a content-addressed ID from the SHA-256
    ///   of `data`. The generated ID has the form `sha256:{16 hex chars}`
    ///   and provides natural deduplication: storing the same bytes twice
    ///   hits the same cache entry.
    ///
    /// Callers can also pre-compute a content ID via
    /// [`AssetManager::content_id`] if they want to check for existence
    /// before downloading.
    pub asset_id: Option<&'a str>,
    /// Original filename.
    pub filename: &'a str,
    /// MIME type if known.
    pub mime_type: Option<String>,
    /// Remote URL at the provider if known.
    pub remote_url: Option<String>,
    /// Raw bytes to cache.
    pub data: &'a [u8],
}

/// Maximum allowed length for an asset ID (after sanitization the
/// on-disk component will be shorter, but we reject clearly absurd
/// inputs early).
const MAX_ASSET_ID_LEN: usize = 200;

/// Drop index entries whose underlying file is missing (or whose stored
/// path is unsafe). Returns the count of removed entries.
///
/// Used by both `from_resolved` (startup check) and
/// [`AssetManager::integrity_check`].
fn prune_stale_entries(index: &mut AssetIndex, cache: &CacheManager) -> usize {
    let stale: Vec<String> = index
        .assets
        .iter()
        .filter_map(
            |(id, asset)| match resolve_under_root(cache.root(), &asset.local_path) {
                Some(abs) if cache.exists(&abs) => None,
                Some(_) => Some(id.clone()),
                None => {
                    tracing::warn!(
                        asset_id = id.as_str(),
                        path = ?asset.local_path,
                        "dropping index entry with unsafe local_path",
                    );
                    Some(id.clone())
                }
            },
        )
        .collect();
    let count = stale.len();
    for id in stale {
        index.remove(&id);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EvictionPolicy;
    use devboy_core::asset::AssetContext;
    use std::time::Duration;
    use tempfile::tempdir;

    fn manager(root: PathBuf) -> AssetManager {
        let cfg = ResolvedAssetConfig {
            cache_dir: root,
            max_cache_size: 10_000,
            max_file_age: Duration::from_secs(100 * 86_400),
            eviction_policy: EvictionPolicy::Lru,
        };
        AssetManager::from_resolved(cfg).unwrap()
    }

    fn store_simple<'a>(
        context: AssetContext,
        asset_id: &'a str,
        filename: &'a str,
        data: &'a [u8],
    ) -> StoreRequest<'a> {
        StoreRequest {
            context,
            asset_id: Some(asset_id),
            filename,
            mime_type: None,
            remote_url: None,
            data,
        }
    }

    #[test]
    fn store_get_delete_roundtrip() {
        let tmp = tempdir().unwrap();
        let mgr = manager(tmp.path().to_path_buf());
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };

        let asset = mgr
            .store(StoreRequest {
                context: ctx.clone(),
                asset_id: Some("a1"),
                filename: "file.txt",
                mime_type: Some("text/plain".into()),
                remote_url: None,
                data: b"hello",
            })
            .unwrap();
        assert_eq!(asset.size, 5);
        assert_eq!(mgr.total_size().unwrap(), 5);

        let resolved = mgr.get("a1").unwrap().expect("asset present");
        assert_eq!(resolved.asset.id, "a1");
        assert!(resolved.absolute_path.is_file());
        assert_eq!(std::fs::read(&resolved.absolute_path).unwrap(), b"hello");

        assert!(mgr.delete("a1").unwrap());
        assert!(mgr.get("a1").unwrap().is_none());
        assert!(!mgr.delete("a1").unwrap(), "second delete is a no-op");
        assert_eq!(mgr.total_size().unwrap(), 0);
    }

    #[test]
    fn store_persists_across_reopen() {
        let tmp = tempdir().unwrap();
        {
            let mgr = manager(tmp.path().to_path_buf());
            let ctx = AssetContext::Issue {
                key: "DEV-1".into(),
            };
            mgr.store(store_simple(ctx, "a1", "x.bin", b"xyz")).unwrap();
        }

        let mgr = manager(tmp.path().to_path_buf());
        let list = mgr.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "a1");
        let resolved = mgr.get("a1").unwrap().unwrap();
        assert_eq!(std::fs::read(&resolved.absolute_path).unwrap(), b"xyz");
    }

    #[test]
    fn integrity_check_removes_missing_files() {
        let tmp = tempdir().unwrap();
        let mgr = manager(tmp.path().to_path_buf());
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };
        let asset = mgr.store(store_simple(ctx, "a1", "x.bin", b"xyz")).unwrap();

        // Remove the file out-of-band.
        let abs = tmp.path().join(&asset.local_path);
        std::fs::remove_file(&abs).unwrap();

        let removed = mgr.integrity_check().unwrap();
        assert_eq!(removed, 1);
        assert!(mgr.list().unwrap().is_empty());
    }

    #[test]
    fn get_drops_stale_entry_and_returns_none() {
        let tmp = tempdir().unwrap();
        let mgr = manager(tmp.path().to_path_buf());
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };
        let asset = mgr.store(store_simple(ctx, "a1", "x.bin", b"xyz")).unwrap();

        std::fs::remove_file(tmp.path().join(&asset.local_path)).unwrap();

        assert!(mgr.get("a1").unwrap().is_none());
        assert!(mgr.list().unwrap().is_empty());
    }

    #[test]
    fn rotate_now_enforces_budget() {
        let tmp = tempdir().unwrap();
        let cfg = ResolvedAssetConfig {
            cache_dir: tmp.path().to_path_buf(),
            max_cache_size: 100,
            max_file_age: Duration::from_secs(100 * 86_400),
            eviction_policy: EvictionPolicy::Lru,
        };
        let mgr = AssetManager::from_resolved(cfg).unwrap();
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };

        mgr.store(store_simple(ctx.clone(), "a", "a.bin", &[0u8; 60]))
            .unwrap();
        // Second store triggers rotation automatically. Both fit (60 + 60 > 100)
        // so one of them should be evicted.
        mgr.store(store_simple(ctx, "b", "b.bin", &[0u8; 60]))
            .unwrap();
        assert!(mgr.total_size().unwrap() <= 100);
        assert_eq!(mgr.list().unwrap().len(), 1);

        // Explicit rotate_now is a no-op now that we are within budget.
        let stats = mgr.rotate_now().unwrap();
        assert_eq!(stats.removed(), 0);
    }

    #[test]
    fn index_path_points_at_cache_dir() {
        let tmp = tempdir().unwrap();
        let mgr = manager(tmp.path().to_path_buf());
        assert_eq!(mgr.index_path(), tmp.path().join(INDEX_FILENAME));
        assert_eq!(mgr.cache_dir(), tmp.path());
    }

    #[test]
    fn store_treats_zero_max_cache_size_as_unlimited() {
        let tmp = tempdir().unwrap();
        let cfg = ResolvedAssetConfig {
            cache_dir: tmp.path().to_path_buf(),
            max_cache_size: 0, // "unlimited"
            max_file_age: Duration::from_secs(100 * 86_400),
            eviction_policy: EvictionPolicy::Lru,
        };
        let mgr = AssetManager::from_resolved(cfg).unwrap();
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };

        // Storing a multi-megabyte blob under a zero budget must succeed
        // and stay in the cache (no rotation eviction).
        let big = vec![0u8; 2_000_000];
        mgr.store(store_simple(ctx, "big", "big.bin", &big))
            .unwrap();
        assert_eq!(mgr.total_size().unwrap(), big.len() as u64);
        assert_eq!(mgr.list().unwrap().len(), 1);

        // Explicit rotate_now is a no-op under the zero budget.
        let stats = mgr.rotate_now().unwrap();
        assert_eq!(stats.removed(), 0);
    }

    #[test]
    fn store_rejects_oversized_payload() {
        let tmp = tempdir().unwrap();
        let cfg = ResolvedAssetConfig {
            cache_dir: tmp.path().to_path_buf(),
            max_cache_size: 10,
            max_file_age: Duration::from_secs(100 * 86_400),
            eviction_policy: EvictionPolicy::Lru,
        };
        let mgr = AssetManager::from_resolved(cfg).unwrap();
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };

        let err = mgr
            .store(store_simple(ctx, "a1", "big.bin", &[0u8; 100]))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exceeds the cache"), "unexpected msg: {msg}");

        // Nothing should have been written or tracked.
        assert!(mgr.list().unwrap().is_empty());
        assert_eq!(mgr.total_size().unwrap(), 0);
    }

    #[test]
    fn get_returns_fresh_last_accessed() {
        let tmp = tempdir().unwrap();
        let mgr = manager(tmp.path().to_path_buf());
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };
        let stored = mgr.store(store_simple(ctx, "a1", "a.bin", b"xyz")).unwrap();
        let stored_at = stored.last_accessed_ms;

        // Wait a few ms so the touch produces a strictly newer timestamp.
        std::thread::sleep(std::time::Duration::from_millis(5));

        let resolved = mgr.get("a1").unwrap().expect("asset present");
        assert!(
            resolved.asset.last_accessed_ms > stored_at,
            "expected ResolvedAsset to reflect the post-touch timestamp: \
             stored={stored_at}, returned={}",
            resolved.asset.last_accessed_ms,
        );

        // And the index value matches what `get` returned.
        let from_list = mgr
            .list()
            .unwrap()
            .into_iter()
            .find(|a| a.id == "a1")
            .unwrap();
        assert_eq!(from_list.last_accessed_ms, resolved.asset.last_accessed_ms);
    }

    #[test]
    fn from_resolved_rotates_on_startup() {
        let tmp = tempdir().unwrap();

        // Seed the cache with two entries under a generous budget.
        {
            let mgr = manager(tmp.path().to_path_buf());
            let ctx = AssetContext::Issue {
                key: "DEV-1".into(),
            };
            mgr.store(store_simple(ctx.clone(), "a", "a.bin", &[0u8; 60]))
                .unwrap();
            mgr.store(store_simple(ctx, "b", "b.bin", &[0u8; 60]))
                .unwrap();
            assert_eq!(mgr.total_size().unwrap(), 120);
        }

        // Re-open with a tight budget — startup rotation should trim the
        // cache back under the limit *before* we hand it to the caller.
        let tight = ResolvedAssetConfig {
            cache_dir: tmp.path().to_path_buf(),
            max_cache_size: 100,
            max_file_age: Duration::from_secs(100 * 86_400),
            eviction_policy: EvictionPolicy::Lru,
        };
        let mgr = AssetManager::from_resolved(tight).unwrap();
        assert!(
            mgr.total_size().unwrap() <= 100,
            "cache still over budget on open"
        );
        assert_eq!(mgr.list().unwrap().len(), 1);
    }

    #[test]
    fn with_root_uses_defaults() {
        let tmp = tempdir().unwrap();
        let mgr = AssetManager::with_root(tmp.path().to_path_buf()).unwrap();
        assert_eq!(mgr.cache_dir(), tmp.path());
        assert!(mgr.config().max_cache_size > 0);
    }

    // =================================================================
    // Dual-mode asset_id tests
    // =================================================================

    #[test]
    fn store_auto_generates_content_addressed_id() {
        let tmp = tempdir().unwrap();
        let mgr = manager(tmp.path().to_path_buf());
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };

        let asset = mgr
            .store(StoreRequest {
                context: ctx,
                asset_id: None, // auto-generate
                filename: "trace.log",
                mime_type: None,
                remote_url: None,
                data: b"stack trace here",
            })
            .unwrap();

        assert!(
            asset.id.starts_with("sha256:"),
            "auto-generated id should have sha256: prefix, got: {}",
            asset.id,
        );
        assert_eq!(asset.id.len(), 7 + 16); // "sha256:" + 16 hex chars

        // Same content → same id (dedup).
        let expected = AssetManager::content_id(b"stack trace here");
        assert_eq!(asset.id, expected);

        // Retrievable by the generated id.
        let resolved = mgr.get(&asset.id).unwrap().expect("should be cached");
        assert_eq!(
            std::fs::read(&resolved.absolute_path).unwrap(),
            b"stack trace here",
        );
    }

    #[test]
    fn store_deduplicates_by_content_id() {
        let tmp = tempdir().unwrap();
        let mgr = manager(tmp.path().to_path_buf());
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };

        let a = mgr
            .store(StoreRequest {
                context: ctx.clone(),
                asset_id: None,
                filename: "a.log",
                mime_type: None,
                remote_url: None,
                data: b"same content",
            })
            .unwrap();

        let b = mgr
            .store(StoreRequest {
                context: ctx,
                asset_id: None,
                filename: "b.log",
                mime_type: None,
                remote_url: None,
                data: b"same content",
            })
            .unwrap();

        // Same content → same id, single cache entry.
        assert_eq!(a.id, b.id);
        assert_eq!(mgr.list().unwrap().len(), 1);
    }

    #[test]
    fn store_rejects_empty_asset_id() {
        let tmp = tempdir().unwrap();
        let mgr = manager(tmp.path().to_path_buf());
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };

        let err = mgr
            .store(StoreRequest {
                context: ctx,
                asset_id: Some(""),
                filename: "x.txt",
                mime_type: None,
                remote_url: None,
                data: b"x",
            })
            .unwrap_err();
        assert!(err.to_string().contains("empty"), "unexpected: {err}");
    }

    #[test]
    fn store_rejects_overly_long_asset_id() {
        let tmp = tempdir().unwrap();
        let mgr = manager(tmp.path().to_path_buf());
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };

        let long_id = "x".repeat(MAX_ASSET_ID_LEN + 1);
        let err = mgr
            .store(StoreRequest {
                context: ctx,
                asset_id: Some(&long_id),
                filename: "x.txt",
                mime_type: None,
                remote_url: None,
                data: b"x",
            })
            .unwrap_err();
        assert!(err.to_string().contains("200"), "unexpected: {err}");
    }

    #[test]
    fn content_id_is_deterministic() {
        let a = AssetManager::content_id(b"hello");
        let b = AssetManager::content_id(b"hello");
        assert_eq!(a, b);
        assert!(a.starts_with("sha256:"));

        let c = AssetManager::content_id(b"world");
        assert_ne!(a, c);
    }
}
