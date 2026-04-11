//! LRU rotation / eviction policy for the asset cache.
//!
//! The rotator is stateless: every call evaluates the index and deletes
//! whichever assets need to go based on the configured
//! [`EvictionPolicy`] and size / age limits.
//!
//! Eviction priority for LRU:
//!
//! 1. Assets older than `max_file_age` (by `last_accessed`) are removed
//!    unconditionally. This implements the TTL behaviour described in the
//!    ADR.
//! 2. If the remaining cache is still over `max_cache_size`, assets are
//!    removed in LRU order (oldest `last_accessed` first) until the budget
//!    is met. Within the same timestamp bucket, larger files go first so
//!    one multi-GB video is preferred over many small logs.

use std::path::Path;
use std::time::Duration;

use crate::cache::CacheManager;
use crate::config::{EvictionPolicy, ResolvedAssetConfig};
use crate::error::Result;
use crate::index::{AssetIndex, CachedAsset, now_ms};

/// Stats produced by a rotation pass — exposed so callers can log what
/// happened and tests can assert on side-effects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RotationStats {
    /// Number of assets removed by age / TTL.
    pub aged_out: usize,
    /// Number of assets removed by LRU size enforcement.
    pub lru_evicted: usize,
    /// Total bytes freed.
    pub bytes_freed: u64,
}

impl RotationStats {
    /// Total number of assets removed.
    pub fn removed(&self) -> usize {
        self.aged_out + self.lru_evicted
    }
}

/// Applies an [`EvictionPolicy`] to an [`AssetIndex`] + cache root.
#[derive(Debug, Clone)]
pub struct Rotator {
    max_cache_size: u64,
    max_file_age: Duration,
    policy: EvictionPolicy,
}

impl Rotator {
    /// Build a rotator from a resolved config.
    pub fn new(config: &ResolvedAssetConfig) -> Self {
        Self {
            max_cache_size: config.max_cache_size,
            max_file_age: config.max_file_age,
            policy: config.eviction_policy,
        }
    }

    /// Run a rotation pass in-place. Mutates the index and deletes files
    /// from disk via the supplied [`CacheManager`].
    ///
    /// For [`EvictionPolicy::None`] this is a no-op.
    pub fn rotate(&self, index: &mut AssetIndex, cache: &CacheManager) -> Result<RotationStats> {
        let mut stats = RotationStats::default();
        if matches!(self.policy, EvictionPolicy::None) {
            return Ok(stats);
        }

        // Phase 1 — age / TTL.
        //
        // `Duration::as_millis()` returns `u128`, so very large values would
        // silently truncate on the `as u64` cast. Clamp to `u64::MAX`
        // explicitly — in practice that means "effectively infinite" and
        // the saturating subtraction below yields a `cutoff_ms` of 0,
        // disabling the TTL check.
        let max_age_ms: u64 = self.max_file_age.as_millis().min(u128::from(u64::MAX)) as u64;
        let cutoff_ms = now_ms().saturating_sub(max_age_ms);
        let expired: Vec<String> = index
            .assets
            .iter()
            .filter(|(_, a)| a.last_accessed_ms < cutoff_ms)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(asset) = index.remove(&id) {
                remove_cached_file(cache, &asset, &mut stats.bytes_freed)?;
                stats.aged_out += 1;
            }
        }

        // Phase 2 — size budget. A budget of 0 is treated as "unlimited",
        // matching the semantics enforced in `AssetManager::store`.
        if self.max_cache_size > 0 && index.total_size() > self.max_cache_size {
            let mut entries: Vec<CachedAsset> = index.assets.values().cloned().collect();
            self.sort_for_eviction(&mut entries);

            for asset in entries {
                if index.total_size() <= self.max_cache_size {
                    break;
                }
                if let Some(removed) = index.remove(&asset.id) {
                    remove_cached_file(cache, &removed, &mut stats.bytes_freed)?;
                    stats.lru_evicted += 1;
                }
            }
        }

        Ok(stats)
    }

    /// Sort entries so that the first element is the best eviction candidate.
    fn sort_for_eviction(&self, entries: &mut [CachedAsset]) {
        match self.policy {
            EvictionPolicy::Lru => {
                // Oldest `last_accessed` first, then largest size first.
                entries.sort_by(|a, b| {
                    a.last_accessed_ms
                        .cmp(&b.last_accessed_ms)
                        .then_with(|| b.size.cmp(&a.size))
                });
            }
            EvictionPolicy::Fifo => {
                // Oldest `downloaded_at` first.
                entries.sort_by(|a, b| a.downloaded_at_ms.cmp(&b.downloaded_at_ms));
            }
            EvictionPolicy::None => {}
        }
    }

    /// Check whether the index is already within the configured budget.
    ///
    /// Provided as a cheap fast-path so callers can skip `rotate` when the
    /// cache is empty / small. A budget of 0 is treated as "unlimited".
    pub fn within_budget(&self, index: &AssetIndex, _root: &Path) -> bool {
        self.max_cache_size == 0 || index.total_size() <= self.max_cache_size
    }
}

/// Resolve and delete a cached file belonging to an index entry, bumping
/// `bytes_freed` **only** when a file that actually existed was removed
/// from disk. Stale entries whose file has already been removed externally,
/// or whose stored path resolves outside the cache root, are dropped from
/// the index but their size is not counted as "bytes freed".
///
/// The resolution goes through [`crate::cache::resolve_under_root`], which
/// validates that the target stays inside the cache directory — a safety
/// guard against tampered `index.json` entries with absolute or traversing
/// paths.
fn remove_cached_file(
    cache: &CacheManager,
    asset: &CachedAsset,
    bytes_freed: &mut u64,
) -> Result<()> {
    let Some(abs) = crate::cache::resolve_under_root(cache.root(), &asset.local_path) else {
        tracing::warn!(
            asset_id = asset.id.as_str(),
            path = ?asset.local_path,
            "skipping eviction — index entry points outside the cache root",
        );
        return Ok(());
    };

    // Only count bytes we actually freed. `CacheManager::delete` is
    // idempotent and treats `NotFound` as success, so we need to check
    // upfront whether the file is really there.
    let existed = cache.exists(&abs);
    cache.delete(&abs)?;
    if existed {
        *bytes_freed += asset.size;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheManager;
    use crate::config::ResolvedAssetConfig;
    use crate::index::{CachedAsset, NewCachedAsset};
    use devboy_core::asset::AssetContext;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn cfg(cache_dir: PathBuf, max_size: u64, max_age: Duration) -> ResolvedAssetConfig {
        ResolvedAssetConfig {
            cache_dir,
            max_cache_size: max_size,
            max_file_age: max_age,
            eviction_policy: EvictionPolicy::Lru,
        }
    }

    fn store_asset(cache: &CacheManager, id: &str, size: u64) -> CachedAsset {
        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };
        let data = vec![0u8; size as usize];
        let stored = cache
            .store(&ctx, id, &format!("{id}.bin"), &data)
            .expect("store");
        let rel = stored
            .path
            .strip_prefix(cache.root())
            .unwrap()
            .to_path_buf();
        CachedAsset::new(NewCachedAsset {
            id: id.into(),
            filename: format!("{id}.bin"),
            mime_type: None,
            size: stored.size,
            local_path: rel,
            context: ctx,
            checksum_sha256: stored.checksum_sha256,
            remote_url: None,
        })
    }

    #[test]
    fn policy_none_is_noop() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let mut index = AssetIndex::empty();

        let asset = store_asset(&cache, "a1", 10);
        index.upsert(asset);

        let mut resolved = cfg(tmp.path().to_path_buf(), 0, Duration::from_secs(1));
        resolved.eviction_policy = EvictionPolicy::None;
        let rotator = Rotator::new(&resolved);

        let stats = rotator.rotate(&mut index, &cache).unwrap();
        assert_eq!(stats.removed(), 0);
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn age_based_eviction() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let mut index = AssetIndex::empty();

        // old asset — last accessed 10 minutes ago
        let mut old = store_asset(&cache, "old", 5);
        old.last_accessed_ms = now_ms() - 600_000;
        index.upsert(old);

        // fresh asset — just accessed
        index.upsert(store_asset(&cache, "fresh", 5));

        let resolved = cfg(
            tmp.path().to_path_buf(),
            1024 * 1024,
            Duration::from_secs(60),
        );
        let rotator = Rotator::new(&resolved);

        let stats = rotator.rotate(&mut index, &cache).unwrap();
        assert_eq!(stats.aged_out, 1);
        assert_eq!(stats.lru_evicted, 0);
        assert!(index.get("old").is_none());
        assert!(index.get("fresh").is_some());
    }

    #[test]
    fn size_based_lru_eviction() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let mut index = AssetIndex::empty();

        let mut a = store_asset(&cache, "a", 100);
        a.last_accessed_ms = 1_000;
        index.upsert(a);

        let mut b = store_asset(&cache, "b", 100);
        b.last_accessed_ms = 2_000;
        index.upsert(b);

        let mut c = store_asset(&cache, "c", 100);
        c.last_accessed_ms = 3_000;
        index.upsert(c);

        // Budget = 150 bytes. We expect `a` and `b` to be evicted (oldest),
        // leaving `c` (100 bytes) which fits.
        let resolved = cfg(
            tmp.path().to_path_buf(),
            150,
            Duration::from_secs(100 * 365 * 86_400),
        );
        let rotator = Rotator::new(&resolved);

        let stats = rotator.rotate(&mut index, &cache).unwrap();
        assert_eq!(stats.aged_out, 0);
        assert_eq!(stats.lru_evicted, 2);
        assert!(index.get("a").is_none());
        assert!(index.get("b").is_none());
        assert!(index.get("c").is_some());
        assert!(index.total_size() <= 150);
    }

    #[test]
    fn fifo_orders_by_download_time() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let mut index = AssetIndex::empty();

        let mut a = store_asset(&cache, "a", 100);
        a.downloaded_at_ms = 1_000;
        a.last_accessed_ms = 9_000; // FIFO should ignore this
        index.upsert(a);

        let mut b = store_asset(&cache, "b", 100);
        b.downloaded_at_ms = 2_000;
        b.last_accessed_ms = 1_000;
        index.upsert(b);

        let mut resolved = cfg(
            tmp.path().to_path_buf(),
            150,
            Duration::from_secs(100 * 365 * 86_400),
        );
        resolved.eviction_policy = EvictionPolicy::Fifo;
        let rotator = Rotator::new(&resolved);

        rotator.rotate(&mut index, &cache).unwrap();
        // FIFO keeps `b` (newer download), evicts `a` (older download).
        assert!(index.get("a").is_none());
        assert!(index.get("b").is_some());
    }

    #[test]
    fn within_budget_fast_path() {
        let tmp = tempdir().unwrap();
        let mut index = AssetIndex::empty();
        let resolved = cfg(
            tmp.path().to_path_buf(),
            1_000,
            Duration::from_secs(100 * 365 * 86_400),
        );
        let rotator = Rotator::new(&resolved);
        assert!(rotator.within_budget(&index, tmp.path()));

        index.upsert(CachedAsset::new(NewCachedAsset {
            id: "a".into(),
            filename: "a.bin".into(),
            mime_type: None,
            size: 500,
            local_path: PathBuf::from("issues/x/a.bin"),
            context: AssetContext::Issue { key: "x".into() },
            checksum_sha256: "deadbeef".into(),
            remote_url: None,
        }));
        assert!(rotator.within_budget(&index, tmp.path()));
    }

    #[test]
    fn age_ties_prefer_larger_files() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let mut index = AssetIndex::empty();

        let mut small = store_asset(&cache, "small", 10);
        small.last_accessed_ms = 1_000;
        index.upsert(small);

        let mut big = store_asset(&cache, "big", 1_000);
        big.last_accessed_ms = 1_000;
        index.upsert(big);

        // Budget tiny so at least one has to go; ties → larger first.
        let resolved = cfg(
            tmp.path().to_path_buf(),
            100,
            Duration::from_secs(100 * 365 * 86_400),
        );
        let rotator = Rotator::new(&resolved);
        rotator.rotate(&mut index, &cache).unwrap();
        // The big one should be evicted first.
        assert!(index.get("big").is_none());
        assert!(index.get("small").is_some());
    }

    #[test]
    fn bytes_freed_only_counts_existing_files() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let mut index = AssetIndex::empty();

        // Real, on-disk asset — eviction should count its bytes.
        let mut real = store_asset(&cache, "real", 100);
        real.last_accessed_ms = 1_000;
        index.upsert(real);

        // Phantom asset — index entry exists but the file is gone. The
        // rotator must still drop it from the index but must not count it
        // as "freed" since there was nothing on disk to free.
        let phantom = CachedAsset::new(NewCachedAsset {
            id: "phantom".into(),
            filename: "phantom.bin".into(),
            mime_type: None,
            size: 999,
            local_path: PathBuf::from("issues/ghost/phantom.bin"),
            context: AssetContext::Issue {
                key: "ghost".into(),
            },
            checksum_sha256: "abcd".into(),
            remote_url: None,
        });
        index.upsert(CachedAsset {
            last_accessed_ms: 1_000,
            ..phantom
        });

        // Budget tiny so both entries get considered; ages equal.
        let resolved = cfg(
            tmp.path().to_path_buf(),
            50,
            Duration::from_secs(100 * 365 * 86_400),
        );
        let rotator = Rotator::new(&resolved);
        let stats = rotator.rotate(&mut index, &cache).unwrap();

        // Both were dropped from the index…
        assert!(index.get("real").is_none());
        assert!(index.get("phantom").is_none());
        // …but only the real file counted.
        assert_eq!(stats.bytes_freed, 100);
    }

    #[test]
    fn bytes_freed_skips_unsafe_paths() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let mut index = AssetIndex::empty();

        // Tampered index entry pointing outside the cache root.
        index.upsert(CachedAsset {
            last_accessed_ms: 1_000,
            ..CachedAsset::new(NewCachedAsset {
                id: "hostile".into(),
                filename: "passwd".into(),
                mime_type: None,
                size: 4096,
                local_path: PathBuf::from("../../etc/passwd"),
                context: AssetContext::Issue { key: "x".into() },
                checksum_sha256: "dead".into(),
                remote_url: None,
            })
        });

        let resolved = cfg(
            tmp.path().to_path_buf(),
            1,
            Duration::from_secs(100 * 365 * 86_400),
        );
        let rotator = Rotator::new(&resolved);
        let stats = rotator.rotate(&mut index, &cache).unwrap();

        // Entry dropped from the index but bytes_freed stays at 0 — we
        // never touched the outside path.
        assert!(index.get("hostile").is_none());
        assert_eq!(stats.bytes_freed, 0);
    }
}
