//! High-level orchestrator combining the cache, index, and rotator.
//!
//! [`AssetManager`] is the entry point used by the rest of devboy-tools. It
//! hides the split between the physical cache directory and the in-memory
//! index, and enforces the rotation policy on every write.
//!
//! ## Concurrency
//!
//! The manager wraps its mutable state in a [`std::sync::Mutex`] so it can
//! be freely cloned via an `Arc` (e.g. to be shared across tokio tasks).
//! All on-disk writes go through the mutex, which is acceptable given the
//! low frequency of cache mutations in the expected workload.

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
                lru_evicted = rotated.lru_evicted,
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

    /// Convenience constructor for tests and standalone usage.
    pub fn with_root(root: PathBuf) -> Result<Self> {
        let cfg = AssetConfig::default();
        let mut resolved = cfg.resolve()?;
        resolved.cache_dir = root;
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
            asset_id,
            filename,
            mime_type,
            remote_url,
            data,
        } = request;

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

        {
            let mut index = self.state_lock();
            index.upsert(asset.clone());
            self.inner.rotator.rotate(&mut index, &self.inner.cache)?;
            index.save(&self.inner.config.cache_dir)?;
        }

        Ok(asset)
    }

    /// Look up an asset by id and return the absolute path on disk if it
    /// is still cached. Also touches `last_accessed` and persists the
    /// index if the asset was found.
    pub fn get(&self, asset_id: &str) -> Result<Option<ResolvedAsset>> {
        let mut index = self.state_lock();
        let Some(asset) = index.get(asset_id).cloned() else {
            return Ok(None);
        };
        // Validate the stored path stays under the cache root before
        // touching the filesystem. A tampered index entry with an absolute
        // or traversing path gets dropped and reported as `None`.
        let Some(abs_path) = resolve_under_root(&self.inner.config.cache_dir, &asset.local_path)
        else {
            tracing::warn!(
                asset_id,
                path = ?asset.local_path,
                "dropping index entry with unsafe local_path",
            );
            index.remove(asset_id);
            index.save(&self.inner.config.cache_dir)?;
            return Ok(None);
        };
        if !abs_path.is_file() {
            // File vanished — drop the stale entry.
            index.remove(asset_id);
            index.save(&self.inner.config.cache_dir)?;
            return Ok(None);
        }
        index.touch(asset_id);
        index.save(&self.inner.config.cache_dir)?;
        Ok(Some(ResolvedAsset {
            asset,
            absolute_path: abs_path,
        }))
    }

    /// Delete an asset from the cache (both the index entry and the file).
    /// Returns `true` if the asset was present.
    pub fn delete(&self, asset_id: &str) -> Result<bool> {
        let mut index = self.state_lock();
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
    pub fn list(&self) -> Vec<CachedAsset> {
        self.state_lock().assets.values().cloned().collect()
    }

    /// Total tracked bytes in the cache.
    pub fn total_size(&self) -> u64 {
        self.state_lock().total_size()
    }

    /// Force a rotation pass immediately.
    pub fn rotate_now(&self) -> Result<RotationStats> {
        let mut index = self.state_lock();
        let stats = self.inner.rotator.rotate(&mut index, &self.inner.cache)?;
        index.save(&self.inner.config.cache_dir)?;
        Ok(stats)
    }

    /// Re-check index vs filesystem. Returns the number of dropped entries.
    pub fn integrity_check(&self) -> Result<usize> {
        let mut index = self.state_lock();
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

    fn state_lock(&self) -> std::sync::MutexGuard<'_, AssetIndex> {
        self.inner.state.lock().expect("asset index mutex poisoned")
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
    /// Stable id to use for the asset. Caller is responsible for uniqueness.
    pub asset_id: &'a str,
    /// Original filename.
    pub filename: &'a str,
    /// MIME type if known.
    pub mime_type: Option<String>,
    /// Remote URL at the provider if known.
    pub remote_url: Option<String>,
    /// Raw bytes to cache.
    pub data: &'a [u8],
}

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
            asset_id,
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
                asset_id: "a1",
                filename: "file.txt",
                mime_type: Some("text/plain".into()),
                remote_url: None,
                data: b"hello",
            })
            .unwrap();
        assert_eq!(asset.size, 5);
        assert_eq!(mgr.total_size(), 5);

        let resolved = mgr.get("a1").unwrap().expect("asset present");
        assert_eq!(resolved.asset.id, "a1");
        assert!(resolved.absolute_path.is_file());
        assert_eq!(std::fs::read(&resolved.absolute_path).unwrap(), b"hello");

        assert!(mgr.delete("a1").unwrap());
        assert!(mgr.get("a1").unwrap().is_none());
        assert!(!mgr.delete("a1").unwrap(), "second delete is a no-op");
        assert_eq!(mgr.total_size(), 0);
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
        let list = mgr.list();
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
        assert!(mgr.list().is_empty());
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
        assert!(mgr.list().is_empty());
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
        assert!(mgr.total_size() <= 100);
        assert_eq!(mgr.list().len(), 1);

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
        assert_eq!(mgr.total_size(), big.len() as u64);
        assert_eq!(mgr.list().len(), 1);

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
        assert!(mgr.list().is_empty());
        assert_eq!(mgr.total_size(), 0);
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
            assert_eq!(mgr.total_size(), 120);
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
        assert!(mgr.total_size() <= 100, "cache still over budget on open");
        assert_eq!(mgr.list().len(), 1);
    }

    #[test]
    fn with_root_uses_defaults() {
        let tmp = tempdir().unwrap();
        let mgr = AssetManager::with_root(tmp.path().to_path_buf()).unwrap();
        assert_eq!(mgr.cache_dir(), tmp.path());
        assert!(mgr.config().max_cache_size > 0);
    }
}
