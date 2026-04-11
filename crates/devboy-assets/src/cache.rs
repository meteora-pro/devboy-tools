//! Filesystem-level cache manager.
//!
//! Responsibilities:
//!
//! - Deterministically map [`AssetContext`] values to on-disk paths
//! - Store / load / delete file blobs
//! - Compute SHA-256 checksums
//!
//! The cache manager is unaware of the index — the higher-level
//! [`crate::manager::AssetManager`] combines the two. This split keeps the
//! filesystem concerns testable in isolation.

use devboy_core::asset::AssetContext;
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::error::{AssetError, Result};

/// Directory name used under the cache root for issue attachments.
pub const DIR_ISSUES: &str = "issues";
/// Directory name used for issue comment attachments.
pub const DIR_ISSUE_COMMENTS: &str = "issue-comments";
/// Directory name used for merge request attachments.
pub const DIR_MERGE_REQUESTS: &str = "merge-requests";
/// Directory name used for MR note/comment attachments.
pub const DIR_MR_COMMENTS: &str = "mr-comments";
/// Directory name used for messenger chat attachments.
pub const DIR_CHATS: &str = "chats";
/// Directory name used for knowledge base attachments.
pub const DIR_KB: &str = "kb";

/// Manages the physical cache directory layout and file I/O.
#[derive(Debug, Clone)]
pub struct CacheManager {
    root: PathBuf,
}

impl CacheManager {
    /// Create a new manager rooted at `root`. The directory is created if
    /// it does not already exist.
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Absolute path to the cache root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Compute the on-disk path for an asset given its context and filename.
    ///
    /// Layout:
    /// ```text
    /// {root}/{context_dir}/{context_id}/{asset_id}-{safe_filename}
    /// ```
    ///
    /// `asset_id` is prefixed onto the filename to prevent collisions when
    /// two attachments share a name within the same context.
    pub fn path_for(&self, context: &AssetContext, asset_id: &str, filename: &str) -> PathBuf {
        let safe = sanitize_filename(filename);
        let leaf = format!("{asset_id}-{safe}");
        let dir = self.dir_for(context);
        dir.join(leaf)
    }

    /// Directory for a given context (relative to the cache root, joined).
    pub fn dir_for(&self, context: &AssetContext) -> PathBuf {
        match context {
            AssetContext::Issue { key } => self.root.join(DIR_ISSUES).join(sanitize_key(key)),
            AssetContext::IssueComment { key, comment_id } => self
                .root
                .join(DIR_ISSUE_COMMENTS)
                .join(sanitize_key(key))
                .join(sanitize_key(comment_id)),
            AssetContext::MergeRequest { id } => {
                self.root.join(DIR_MERGE_REQUESTS).join(sanitize_key(id))
            }
            AssetContext::MrComment { mr_id, note_id } => self
                .root
                .join(DIR_MR_COMMENTS)
                .join(sanitize_key(mr_id))
                .join(sanitize_key(note_id)),
            AssetContext::Chat {
                chat_id,
                message_id,
            } => self
                .root
                .join(DIR_CHATS)
                .join(sanitize_key(chat_id))
                .join(sanitize_key(message_id)),
            AssetContext::KbPage { page_id } => self.root.join(DIR_KB).join(sanitize_key(page_id)),
        }
    }

    /// Store bytes for an asset and return the absolute path where they were
    /// written along with the SHA-256 checksum.
    ///
    /// Parent directories are created as needed. Writes go through a temp
    /// file + rename so partial writes are never observable.
    pub fn store(
        &self,
        context: &AssetContext,
        asset_id: &str,
        filename: &str,
        data: &[u8],
    ) -> Result<StoredFile> {
        let path = self.path_for(context, asset_id, filename);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let parent = path
            .parent()
            .ok_or_else(|| AssetError::cache_dir(format!("no parent for {path:?}")))?;

        let mut tmp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| AssetError::cache_dir(format!("temp file: {e}")))?;
        tmp.write_all(data)?;
        tmp.flush()?;
        tmp.persist(&path)
            .map_err(|e| AssetError::cache_dir(format!("persist file: {e}")))?;

        let checksum = sha256_hex(data);

        Ok(StoredFile {
            path,
            size: data.len() as u64,
            checksum_sha256: checksum,
        })
    }

    /// Read a file from the cache by absolute path. Returns `NotFound`
    /// if the file is missing (via [`AssetError::Io`]).
    pub fn load(&self, absolute: &Path) -> Result<Vec<u8>> {
        Ok(std::fs::read(absolute)?)
    }

    /// Delete a file from the cache. Missing files are treated as success
    /// so that retries / idempotent deletes work as expected.
    pub fn delete(&self, absolute: &Path) -> Result<()> {
        match std::fs::remove_file(absolute) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AssetError::Io(e)),
        }
    }

    /// Check whether a file exists in the cache.
    pub fn exists(&self, absolute: &Path) -> bool {
        absolute.is_file()
    }
}

/// Metadata returned from [`CacheManager::store`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    /// Absolute path where the file was written.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 checksum in lower-case hex.
    pub checksum_sha256: String,
}

/// Compute SHA-256 of a byte slice, returned as lower-case hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Restrict a filename to characters that are safe to write on any FS.
/// Non-ASCII letters / digits are replaced with `_`.
fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim().trim_matches('/');
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let mut out = String::with_capacity(base.len());
    for ch in base.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unnamed".to_string()
    } else {
        out
    }
}

/// Same rules as [`sanitize_filename`] but applied to context keys.
fn sanitize_key(key: &str) -> String {
    sanitize_filename(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::asset::AssetContext;
    use tempfile::tempdir;

    #[test]
    fn sha256_matches_known_vector() {
        // Well-known test vector for an empty input.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sanitize_strips_traversal_and_bad_chars() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("hello world!.png"), "hello_world_.png");
        assert_eq!(sanitize_filename("/"), "unnamed");
        assert_eq!(sanitize_filename("привет.txt"), "______.txt");
    }

    #[test]
    fn dir_for_layouts() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();

        let issue_dir = cache.dir_for(&AssetContext::Issue {
            key: "DEV-1".into(),
        });
        assert!(issue_dir.ends_with("issues/DEV-1"));

        let mr_dir = cache.dir_for(&AssetContext::MergeRequest { id: "42".into() });
        assert!(mr_dir.ends_with("merge-requests/42"));

        let kb_dir = cache.dir_for(&AssetContext::KbPage {
            page_id: "p1".into(),
        });
        assert!(kb_dir.ends_with("kb/p1"));
    }

    #[test]
    fn store_load_delete_roundtrip() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();

        let ctx = AssetContext::Issue {
            key: "DEV-1".into(),
        };
        let payload = b"hello world";
        let stored = cache.store(&ctx, "asset-1", "hello.txt", payload).unwrap();

        assert_eq!(stored.size, payload.len() as u64);
        assert_eq!(stored.checksum_sha256, sha256_hex(payload));
        assert!(cache.exists(&stored.path));

        let loaded = cache.load(&stored.path).unwrap();
        assert_eq!(loaded, payload);

        cache.delete(&stored.path).unwrap();
        assert!(!cache.exists(&stored.path));

        // Second delete is a no-op, not an error.
        cache.delete(&stored.path).unwrap();
    }

    #[test]
    fn store_creates_nested_directories() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();

        let ctx = AssetContext::MrComment {
            mr_id: "42".into(),
            note_id: "7".into(),
        };
        let stored = cache.store(&ctx, "a1", "x.bin", b"x").unwrap();
        let rel = stored.path.strip_prefix(tmp.path()).unwrap();
        assert!(rel.to_string_lossy().contains("mr-comments/42/7"));
    }

    #[test]
    fn store_rejects_nothing_and_handles_empty_file() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let ctx = AssetContext::Issue { key: "k".into() };
        let stored = cache.store(&ctx, "id", "empty", &[]).unwrap();
        assert_eq!(stored.size, 0);
        assert_eq!(
            stored.checksum_sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn path_for_prefixes_asset_id() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let ctx = AssetContext::Issue { key: "k".into() };
        let path = cache.path_for(&ctx, "abc123", "report.log");
        assert!(path.to_string_lossy().ends_with("abc123-report.log"));
    }
}
