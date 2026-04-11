//! On-disk index of cached assets (`index.json`).
//!
//! The index is a plain JSON file persisted alongside the cache directory.
//! It stores the metadata that must survive process restarts:
//!
//! - Map of `asset_id -> CachedAsset` (filename, size, checksum, context, ...)
//! - `last_accessed` timestamps used by the LRU rotator
//!
//! Writes go through a temp file + rename for atomicity, so that a crash
//! mid-write cannot leave a half-written index on disk.

use devboy_core::asset::AssetContext;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{AssetError, Result};

/// Filename of the on-disk index, relative to the cache root.
pub const INDEX_FILENAME: &str = "index.json";

/// Current schema version — bumped when breaking changes are made.
pub const INDEX_VERSION: u32 = 1;

/// A single cached asset as persisted in the index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachedAsset {
    /// Stable identifier (UUID or provider id).
    pub id: String,
    /// Original filename.
    pub filename: String,
    /// MIME type if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Size in bytes.
    pub size: u64,
    /// Path relative to the cache root.
    pub local_path: PathBuf,
    /// Context the asset belongs to.
    pub context: AssetContext,
    /// SHA-256 checksum in hex.
    pub checksum_sha256: String,
    /// Remote URL at the provider, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// UNIX epoch milliseconds — when the file was first downloaded.
    pub downloaded_at_ms: u64,
    /// UNIX epoch milliseconds — when the file was last accessed.
    pub last_accessed_ms: u64,
}

/// Parameters accepted by [`CachedAsset::new`].
///
/// Introduced to keep the constructor below the clippy
/// `too_many_arguments` lint threshold.
#[derive(Debug, Clone)]
pub struct NewCachedAsset {
    /// Stable identifier (UUID or provider id).
    pub id: String,
    /// Original filename.
    pub filename: String,
    /// MIME type if known.
    pub mime_type: Option<String>,
    /// Size in bytes.
    pub size: u64,
    /// Path relative to the cache root.
    pub local_path: PathBuf,
    /// Context the asset belongs to.
    pub context: AssetContext,
    /// SHA-256 checksum in hex.
    pub checksum_sha256: String,
    /// Remote URL at the provider if any.
    pub remote_url: Option<String>,
}

impl CachedAsset {
    /// Convenience constructor — wraps [`NewCachedAsset`] and stamps
    /// `downloaded_at` / `last_accessed` to the current time.
    pub fn new(params: NewCachedAsset) -> Self {
        let now = now_ms();
        Self {
            id: params.id,
            filename: params.filename,
            mime_type: params.mime_type,
            size: params.size,
            local_path: params.local_path,
            context: params.context,
            checksum_sha256: params.checksum_sha256,
            remote_url: params.remote_url,
            downloaded_at_ms: now,
            last_accessed_ms: now,
        }
    }
}

/// In-memory representation of the `index.json` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetIndex {
    /// Schema version.
    pub version: u32,
    /// All known assets keyed by id.
    #[serde(default)]
    pub assets: HashMap<String, CachedAsset>,
}

impl Default for AssetIndex {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            assets: HashMap::new(),
        }
    }
}

impl AssetIndex {
    /// Create an empty index at the current schema version.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load the index from `cache_dir/index.json`, returning an empty index
    /// if the file does not exist.
    ///
    /// If the file exists but cannot be parsed, an empty index is returned
    /// and a warning is logged — this is the first line of the integrity
    /// check and is preferable to refusing to start.
    pub fn load(cache_dir: &Path) -> Result<Self> {
        let path = cache_dir.join(INDEX_FILENAME);
        if !path.exists() {
            return Ok(Self::empty());
        }

        let bytes = std::fs::read(&path)?;
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(mut index) => {
                if index.version != INDEX_VERSION {
                    tracing::warn!(
                        expected = INDEX_VERSION,
                        found = index.version,
                        "asset index version mismatch, rebuilding"
                    );
                    index = Self::empty();
                }
                Ok(index)
            }
            Err(err) => {
                tracing::warn!(?err, "failed to parse asset index, starting fresh");
                Ok(Self::empty())
            }
        }
    }

    /// Persist the index to `cache_dir/index.json` atomically via
    /// temp file + rename.
    pub fn save(&self, cache_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(cache_dir)?;
        let path = cache_dir.join(INDEX_FILENAME);

        let bytes = serde_json::to_vec_pretty(self)?;

        // NamedTempFile in the same directory guarantees the final rename
        // stays on the same filesystem.
        let mut tmp = tempfile::NamedTempFile::new_in(cache_dir)
            .map_err(|e| AssetError::cache_dir(format!("temp file: {e}")))?;
        tmp.write_all(&bytes)?;
        tmp.flush()?;
        tmp.persist(&path)
            .map_err(|e| AssetError::cache_dir(format!("persist index: {e}")))?;
        Ok(())
    }

    /// Insert or replace an asset entry.
    pub fn upsert(&mut self, asset: CachedAsset) {
        self.assets.insert(asset.id.clone(), asset);
    }

    /// Remove an asset entry, returning the old value if any.
    pub fn remove(&mut self, id: &str) -> Option<CachedAsset> {
        self.assets.remove(id)
    }

    /// Look up an asset by id.
    pub fn get(&self, id: &str) -> Option<&CachedAsset> {
        self.assets.get(id)
    }

    /// Mutably look up an asset by id.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut CachedAsset> {
        self.assets.get_mut(id)
    }

    /// Mark `last_accessed` on an asset as "now".
    pub fn touch(&mut self, id: &str) -> bool {
        if let Some(asset) = self.assets.get_mut(id) {
            asset.last_accessed_ms = now_ms();
            true
        } else {
            false
        }
    }

    /// Total size in bytes of all tracked assets.
    pub fn total_size(&self) -> u64 {
        self.assets.values().map(|a| a.size).sum()
    }

    /// Number of tracked assets.
    pub fn len(&self) -> usize {
        self.assets.len()
    }

    /// Whether the index contains no assets.
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }
}

/// Current time as UNIX epoch milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::asset::AssetContext;
    use tempfile::tempdir;

    fn make_asset(id: &str, size: u64) -> CachedAsset {
        CachedAsset::new(NewCachedAsset {
            id: id.into(),
            filename: format!("{id}.txt"),
            mime_type: Some("text/plain".into()),
            size,
            local_path: PathBuf::from(format!("files/{id}.txt")),
            context: AssetContext::Issue {
                key: "DEV-1".into(),
            },
            checksum_sha256: "abcd".into(),
            remote_url: None,
        })
    }

    #[test]
    fn upsert_get_remove() {
        let mut index = AssetIndex::empty();
        index.upsert(make_asset("a1", 10));
        index.upsert(make_asset("a2", 20));
        assert_eq!(index.len(), 2);
        assert_eq!(index.total_size(), 30);

        assert_eq!(index.get("a1").unwrap().size, 10);
        let removed = index.remove("a1").unwrap();
        assert_eq!(removed.id, "a1");
        assert_eq!(index.len(), 1);
        assert!(index.get("a1").is_none());
    }

    #[test]
    fn touch_updates_last_accessed() {
        let mut index = AssetIndex::empty();
        index.upsert(make_asset("a1", 10));
        let original = index.get("a1").unwrap().last_accessed_ms;

        // Small sleep to ensure ms tick; spin-loop is enough for the test.
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(index.touch("a1"));
        assert!(index.get("a1").unwrap().last_accessed_ms > original);
        assert!(!index.touch("missing"));
    }

    #[test]
    fn load_missing_returns_empty() {
        let tmp = tempdir().unwrap();
        let index = AssetIndex::load(tmp.path()).unwrap();
        assert!(index.is_empty());
        assert_eq!(index.version, INDEX_VERSION);
    }

    #[test]
    fn save_and_reload_roundtrip() {
        let tmp = tempdir().unwrap();
        let mut index = AssetIndex::empty();
        index.upsert(make_asset("a1", 42));
        index.save(tmp.path()).unwrap();

        let reloaded = AssetIndex::load(tmp.path()).unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded.get("a1").unwrap().size, 42);
    }

    #[test]
    fn corrupt_index_falls_back_to_empty() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join(INDEX_FILENAME), b"not json").unwrap();
        let index = AssetIndex::load(tmp.path()).unwrap();
        assert!(index.is_empty(), "corrupt index should fall back to empty");
    }

    #[test]
    fn version_mismatch_falls_back_to_empty() {
        let tmp = tempdir().unwrap();
        std::fs::write(
            tmp.path().join(INDEX_FILENAME),
            br#"{"version":999,"assets":{}}"#,
        )
        .unwrap();
        let index = AssetIndex::load(tmp.path()).unwrap();
        assert_eq!(index.version, INDEX_VERSION);
        assert!(index.is_empty());
    }

    #[test]
    fn save_is_atomic_under_overwrite() {
        let tmp = tempdir().unwrap();
        let mut index = AssetIndex::empty();
        index.upsert(make_asset("a1", 1));
        index.save(tmp.path()).unwrap();

        // Overwrite with different content; no stale temp files should linger.
        index.upsert(make_asset("a2", 2));
        index.save(tmp.path()).unwrap();

        let reloaded = AssetIndex::load(tmp.path()).unwrap();
        assert_eq!(reloaded.len(), 2);

        // Check that no temp files are left behind.
        let stragglers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name != INDEX_FILENAME
            })
            .collect();
        assert!(stragglers.is_empty(), "unexpected files: {stragglers:?}");
    }
}
