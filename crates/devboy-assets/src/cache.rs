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

/// Maximum length for the sanitized asset ID component in a cache
/// filename. Together with the 8-char hash and `MAX_NAME_LEN`, the
/// total leaf stays well under the 255-byte filesystem limit.
const MAX_ID_LEN: usize = 80;

/// Maximum length for the sanitized filename component.
const MAX_NAME_LEN: usize = 120;

// Layout: {safe_id}-{8_hash}-{safe_name} + 2 dashes = MAX_ID_LEN + 8 + MAX_NAME_LEN + 2 = 210 < 255

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
    /// Both `asset_id` and `filename` are sanitized before becoming a single
    /// path component — any directory separators or `..` sequences in the
    /// inputs are replaced with `_`, so calling `path_for` with hostile
    /// input can never escape the per-context directory.
    ///
    /// An 8-character SHA-256 prefix of the raw asset_id is embedded in
    /// the filename to avoid collisions between IDs that differ only in
    /// characters collapsed by sanitization. 8 hex chars = 32 bits gives
    /// ~4 billion buckets — collision probability via birthday paradox is
    /// negligible for a rotated local cache (< 0.0001% with 100 files per
    /// context). We intentionally keep the hash short to stay well within
    /// the 255-char filename limit on ext4 / NTFS / APFS.
    pub fn path_for(&self, context: &AssetContext, asset_id: &str, filename: &str) -> PathBuf {
        let safe_id = truncate_component(&sanitize_component(asset_id), MAX_ID_LEN);
        let safe_name = truncate_component(&sanitize_filename(filename), MAX_NAME_LEN);
        // Append a short hash of the *raw* (pre-sanitization) asset_id so
        // that two IDs differing only in characters collapsed by
        // sanitization (e.g. `a/b` → `a_b` vs `a?b` → `a_b`) never map
        // to the same on-disk path.
        let id_hash = &sha256_hex(asset_id.as_bytes())[..8];
        let leaf = format!("{safe_id}-{id_hash}-{safe_name}");
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
            AssetContext::MergeRequest { mr_id } => {
                self.root.join(DIR_MERGE_REQUESTS).join(sanitize_key(mr_id))
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
        let parent = path
            .parent()
            .ok_or_else(|| AssetError::cache_dir(format!("no parent for {path:?}")))?;
        std::fs::create_dir_all(parent)?;

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
    pub size: u64,
    /// SHA-256 checksum in lower-case hex.
    pub checksum_sha256: String,
}

/// Validate that a cached-asset `local_path` stays under `root` and return
/// the absolute path on success.
///
/// The index is trusted to hold **relative** paths produced by
/// [`CacheManager::store`]. This helper defends against corrupted or
/// tampered `index.json` entries that try to point elsewhere:
///
/// - Absolute paths are rejected (because `PathBuf::join` would discard
///   `root` for any absolute RHS).
/// - Paths containing `..` components are rejected — we never generate
///   them, so anything with traversal came from outside the crate.
/// - Lexical containment: the joined path's components must start with
///   the root's components.
/// - **Symlink guard**: when the resolved path exists on disk, both
///   `root` and the resolved path are [`std::path::Path::canonicalize`]d
///   so that any symlink within the cache directory is dereferenced. The
///   canonicalized resolved path must still start with the canonicalized
///   root; if it doesn't (e.g. a symlink inside the cache dir points
///   outside), the path is rejected.
///
/// Returns `None` when the path is unsafe; callers drop the index entry
/// instead of touching the filesystem.
pub fn resolve_under_root(root: &Path, relative: &Path) -> Option<PathBuf> {
    if relative.is_absolute() {
        return None;
    }
    for component in relative.components() {
        match component {
            std::path::Component::ParentDir => return None,
            std::path::Component::Prefix(_) | std::path::Component::RootDir => return None,
            _ => {}
        }
    }
    let joined = root.join(relative);

    // Lexical containment — fast path for non-existent files (stale
    // entries) where canonicalize would fail.
    let root_components: Vec<_> = root.components().collect();
    let joined_components: Vec<_> = joined.components().collect();
    if joined_components.len() < root_components.len() {
        return None;
    }
    for (a, b) in root_components.iter().zip(joined_components.iter()) {
        if a != b {
            return None;
        }
    }

    // Symlink guard — when both paths exist, canonicalize to resolve
    // any intermediate symlinks and re-verify containment so a symlink
    // inside the cache dir that points outside can't be followed.
    if joined.exists()
        && let (Ok(canon_root), Ok(canon_target)) = (root.canonicalize(), joined.canonicalize())
        && !canon_target.starts_with(&canon_root)
    {
        return None;
    }

    Some(joined)
}

/// Compute SHA-256 of a byte slice, returned as lower-case hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        // `let _ =` is intentional: `<String as fmt::Write>::write_fmt` is
        // infallible — its `write_str` impl is just `self.push_str(s); Ok(())`
        // (see https://doc.rust-lang.org/std/string/struct.String.html#impl-Write-for-String).
        // The only theoretical failure is OOM, which aborts the process
        // rather than returning `Err`. We suppress the `#[must_use]` lint
        // with `let _ =` instead of `.unwrap()` to avoid emitting a dead
        // panic path for an unreachable case.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Restrict a filename to characters that are safe to write on any FS.
///
/// The input is first stripped of anything before the final `/` or `\` to
/// prevent traversal via `../` or Windows `..\\`. The remaining basename is
/// then passed through [`sanitize_component`] so the result is always a
/// single, FS-safe path component.
fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim();
    let after_fwd = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let base = after_fwd.rsplit('\\').next().unwrap_or(after_fwd);
    sanitize_component(base)
}

/// Sanitize an arbitrary string into a single path component.
///
/// Used for both filenames and opaque identifiers (asset ids, context
/// keys). Rules:
///
/// - Keep ASCII alphanumerics, `.`, `-`, `_`
/// - Replace everything else — including `/`, `\`, and any non-ASCII
///   character — with `_`
/// - Reject lone / repeated `..` segments by never letting them survive
///   (the individual `.` characters remain, but the full traversal form
///   `..` becomes part of a longer, harmless name)
/// - Return the sentinel `"unnamed"` for empty / whitespace-only input
fn sanitize_component(value: &str) -> String {
    let trimmed = value.trim();
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    // A bare `..` (or any all-dot string) can still be interpreted as a
    // traversal; neutralize it by replacing every `.` with `_` in that case.
    if out.chars().all(|c| c == '.') && !out.is_empty() {
        return out.replace('.', "_");
    }
    if out.is_empty() {
        "unnamed".to_string()
    } else {
        out
    }
}

/// Same rules as [`sanitize_component`] but named for clarity at call sites.
fn sanitize_key(key: &str) -> String {
    sanitize_component(key)
}

/// Truncate a string to at most `max_len` bytes on a char boundary.
fn truncate_component(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    // Find a char boundary at or before max_len.
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
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
    fn sanitize_handles_windows_separators() {
        assert_eq!(
            sanitize_filename("..\\..\\Windows\\System32\\cmd.exe"),
            "cmd.exe",
        );
    }

    #[test]
    fn sanitize_neutralizes_dot_only_names() {
        assert_eq!(sanitize_component(".."), "__");
        assert_eq!(sanitize_component("..."), "___");
        assert_eq!(sanitize_component("."), "_");
    }

    #[test]
    fn path_for_blocks_asset_id_traversal() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let ctx = AssetContext::Issue { key: "k".into() };

        // Hostile asset id trying to escape the issue directory.
        let path = cache.path_for(&ctx, "../../escape", "file.txt");
        let rel = path.strip_prefix(tmp.path()).unwrap();
        let components: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        // The hostile id becomes a single sanitized segment; it never introduces
        // a `..` component.
        assert!(
            !components.iter().any(|c| c == ".." || c.contains('/')),
            "unexpected components: {components:?}",
        );
        assert!(path.starts_with(tmp.path()));
    }

    #[test]
    fn store_with_hostile_ids_stays_under_cache_root() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let ctx = AssetContext::Issue {
            key: "../../root".into(),
        };

        let stored = cache
            .store(&ctx, "../../../etc", "../passwd", b"secret")
            .unwrap();
        assert!(
            stored.path.starts_with(tmp.path()),
            "path escaped cache root: {:?}",
            stored.path
        );
    }

    #[test]
    fn dir_for_layouts() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();

        let issue_dir = cache.dir_for(&AssetContext::Issue {
            key: "DEV-1".into(),
        });
        assert!(issue_dir.ends_with("issues/DEV-1"));

        let mr_dir = cache.dir_for(&AssetContext::MergeRequest { mr_id: "42".into() });
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

        // Use path components so the assertion is agnostic to the OS path
        // separator (`/` on Unix, `\` on Windows).
        let components: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        assert!(
            components
                .windows(3)
                .any(|w| w == ["mr-comments", "42", "7"]),
            "unexpected path components: {components:?}",
        );
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
    fn resolve_under_root_accepts_relative_paths() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let rel = PathBuf::from("issues/DEV-1/screen.png");
        let abs = resolve_under_root(root, &rel).unwrap();
        assert!(abs.starts_with(root));
        assert!(abs.ends_with("issues/DEV-1/screen.png"));
    }

    #[test]
    fn resolve_under_root_rejects_absolute() {
        let tmp = tempdir().unwrap();
        let abs = PathBuf::from("/etc/passwd");
        assert!(resolve_under_root(tmp.path(), &abs).is_none());
    }

    #[test]
    fn resolve_under_root_rejects_parent_dir() {
        let tmp = tempdir().unwrap();
        let traversal = PathBuf::from("../../etc/passwd");
        assert!(resolve_under_root(tmp.path(), &traversal).is_none());

        let nested = PathBuf::from("issues/../../etc/passwd");
        assert!(resolve_under_root(tmp.path(), &nested).is_none());
    }

    #[test]
    fn resolve_under_root_accepts_empty_and_single_segment() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(
            resolve_under_root(root, &PathBuf::from("a.txt")).unwrap(),
            root.join("a.txt"),
        );
    }

    #[test]
    fn path_for_prefixes_asset_id_and_hash() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let ctx = AssetContext::Issue { key: "k".into() };
        let path = cache.path_for(&ctx, "abc123", "report.log");
        let leaf = path.file_name().unwrap().to_string_lossy();
        // Format: {sanitized_id}-{8-char hash}-{sanitized_filename}
        assert!(leaf.starts_with("abc123-"), "unexpected leaf: {leaf}");
        assert!(leaf.ends_with("-report.log"), "unexpected leaf: {leaf}");
        // The hash is 8 hex chars between the id and filename.
        let parts: Vec<&str> = leaf.splitn(3, '-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1].len(), 8, "hash should be 8 hex chars");
    }

    #[test]
    fn path_for_avoids_collision_on_sanitized_ids() {
        let tmp = tempdir().unwrap();
        let cache = CacheManager::new(tmp.path().to_path_buf()).unwrap();
        let ctx = AssetContext::Issue { key: "k".into() };
        // These two IDs sanitize to the same string but differ pre-sanitization.
        let p1 = cache.path_for(&ctx, "a/b", "f.txt");
        let p2 = cache.path_for(&ctx, "a?b", "f.txt");
        assert_ne!(
            p1, p2,
            "different raw IDs must produce different paths even when sanitized form matches"
        );
    }
}
