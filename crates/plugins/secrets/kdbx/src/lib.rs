//! KDBX 4 (KeePass) `SecretSource` implementation.
//!
//! Opens a `.kdbx` file with a user-provided passphrase + optional
//! keyfile and reads Password fields out of the matched entries.
//!
//! See the crate README and [ADR-021] §8 for context.
//!
//! ## Architecture
//!
//! The unlocked database is held in an in-process snapshot
//! ([`KdbxSnapshot`]) — a list of `(path, password, metadata)`
//! triples decrypted from the on-disk file. The snapshot is built
//! the first time `is_available` or `get` runs after the user
//! supplies the passphrase, then cached for the lifetime of the
//! process so we don't re-derive the Argon2id key on every read.
//!
//! ### Why an in-process snapshot
//!
//! `keepass::Database` is `!Sync` (it holds `Rc`s in its tree
//! traversal helpers) and the router is async + multi-threaded.
//! Snapshotting once + handing out `SecretString` clones over the
//! `SecretSource` trait is both simpler and matches the
//! agent-blindness rule: the snapshot lives only inside the
//! process that opened the file (the UI binary), the router-side
//! callers see only the wire-shaped `GetOutcome`.
//!
//! ### Path mapping
//!
//! Each KDBX entry becomes one logical secret with a path of the
//! form `<group-breadcrumb>/<entry-title>`. Groups are joined with
//! `/`; every segment is lowercased and stripped of characters
//! outside `[a-z0-9_-]` so the result lines up with what
//! `SecretPath::parse` would accept downstream. The Root group is
//! skipped so paths don't start with `root/`.
//!
//! Example mapping:
//!
//! ```text
//! KeePass:   Root / Personal / Cloud / "AWS Access Key"
//! ADR-020:   personal/cloud/aws-access-key
//! ```
//!
//! Entries whose path collapses to fewer than three segments after
//! normalization are prefixed with `kdbx/<n>/...` so they still
//! parse as a valid path (rather than dropping silently).
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use std::path::PathBuf;

use async_trait::async_trait;
use devboy_storage::source::{Capabilities, GetOutcome, SecretSource, SourceError, SourceStatus};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use tokio::sync::Mutex;

/// Errors specific to the KDBX source plugin.
#[derive(Debug, Error)]
pub enum KdbxSourceError {
    /// Couldn't open / parse the on-disk `.kdbx` file. Reasons
    /// include wrong passphrase, missing keyfile, corrupt body,
    /// unsupported schema version.
    #[error("could not open KDBX file at {path}: {reason}")]
    OpenFailed {
        /// Path the open was attempted against.
        path: PathBuf,
        /// Human-readable reason — typically the keepass crate's own
        /// error message.
        reason: String,
    },

    /// The reference string didn't match any entry in the database.
    /// Returned via `Option<GetOutcome>::None` so the router can
    /// fall through to a fallback source per ADR-021 §2.
    #[error("KDBX source `{source_name}` has no entry at reference `{reference}`")]
    NoSuchEntry {
        /// Source name (matches `SecretSource::name`).
        source_name: String,
        /// The reference the router asked for.
        reference: String,
    },
}

/// One inventory row resolved out of the KDBX file.
///
/// Held in the per-source [`KdbxSnapshot`]. The `password` field is
/// the value the router hands the caller via `get()`; `title` /
/// `username` / `url` are metadata the UI surfaces in the
/// inventory row + the provision dialog's context card.
#[derive(Debug)]
pub struct KdbxEntry {
    /// Normalized ADR-020-like path (lowercase, `[a-z0-9_-]` only,
    /// `/`-separated segments). Mirrors `SecretSource::get`'s
    /// reference argument.
    pub path: String,
    /// Password field. `None` when the KDBX entry has no Password
    /// set (rare — empty entries used as placeholders).
    pub password: Option<SecretString>,
    /// Entry Title field — preserved verbatim for display.
    pub title: String,
    /// Optional UserName field.
    pub username: Option<String>,
    /// Optional URL field.
    pub url: Option<String>,
    /// Optional Notes field.
    pub notes: Option<String>,
}

/// Snapshot of the unlocked KDBX file — every entry flattened to
/// the wire representation the router expects.
///
/// Built once on first read (after the user supplies the
/// passphrase) and cached for the lifetime of the process.
#[derive(Debug, Default)]
pub struct KdbxSnapshot {
    /// All entries, in traversal order. Lookup is linear today;
    /// once we exceed a few hundred entries the cost is still
    /// negligible compared to the Argon2id derive that gated the
    /// open.
    pub entries: Vec<KdbxEntry>,
}

/// `SecretSource` backed by a single KDBX 4 file.
pub struct KdbxSource {
    /// Logical name from `[[source]] name = "..."` in `sources.toml`.
    name: String,
    /// Absolute path to the `.kdbx` file on disk.
    path: PathBuf,
    /// User-supplied passphrase. Wrapped in `SecretString` so it
    /// zeroizes on drop and doesn't leak into `Debug`. `None` means
    /// the UI has not collected it yet — `is_available` reports
    /// `Locked` in that case.
    passphrase: Mutex<Option<SecretString>>,
    /// Optional path to a keyfile companion (KeePass two-factor
    /// unlock). `None` for passphrase-only databases.
    keyfile: Option<PathBuf>,
    /// Decrypted entries cached after the first successful open.
    /// Wrapped in a Mutex so the async `get` / `is_available`
    /// methods can populate it lazily.
    snapshot: Mutex<Option<KdbxSnapshot>>,
}

impl std::fmt::Debug for KdbxSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KdbxSource")
            .field("name", &self.name)
            .field("path", &self.path)
            .field("passphrase", &"<redacted>")
            .field("keyfile", &self.keyfile)
            .field("snapshot", &"<redacted>")
            .finish()
    }
}

impl KdbxSource {
    /// Build a new source named `name` against the file at `path`.
    /// The passphrase + keyfile are supplied later by the UI
    /// unlock modal (or, for tests, via [`Self::set_passphrase`]).
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            passphrase: Mutex::new(None),
            keyfile: None,
            snapshot: Mutex::new(None),
        }
    }

    /// Attach a keyfile (KeePass two-factor unlock). No-op for
    /// databases that only require a passphrase.
    pub fn with_keyfile(mut self, keyfile: impl Into<PathBuf>) -> Self {
        self.keyfile = Some(keyfile.into());
        self
    }

    /// Provide the passphrase. Replaces any previously-set value
    /// AND drops the cached snapshot so the next read re-opens
    /// the file with the new key (lets the UI recover from a
    /// wrong-passphrase typo without rebuilding the source).
    pub async fn set_passphrase(&self, passphrase: SecretString) {
        *self.passphrase.lock().await = Some(passphrase);
        *self.snapshot.lock().await = None;
    }

    /// Forget any cached unlock state. Called when the UI window
    /// closes or when the user explicitly hits "lock".
    pub async fn lock(&self) {
        *self.passphrase.lock().await = None;
        *self.snapshot.lock().await = None;
    }

    /// `true` when the passphrase has been supplied this session.
    pub async fn is_unlocked(&self) -> bool {
        self.passphrase.lock().await.is_some()
    }

    /// Open the file + populate the snapshot if it isn't already
    /// cached. Internal helper called from `is_available` / `get`.
    ///
    /// Returns `Ok(())` once the snapshot is populated, or a
    /// [`KdbxSourceError`] describing why the open failed (wrong
    /// passphrase, missing file, …). The snapshot is left empty on
    /// failure so the next attempt re-tries cleanly.
    async fn ensure_snapshot(&self) -> Result<(), KdbxSourceError> {
        // Fast path: already populated.
        if self.snapshot.lock().await.is_some() {
            return Ok(());
        }
        let pass_guard = self.passphrase.lock().await;
        let Some(pass) = pass_guard.clone() else {
            // No passphrase yet — surface as OpenFailed so callers
            // see a typed reason. The `is_available` wrapper turns
            // this into `Locked` before it reaches the router.
            return Err(KdbxSourceError::OpenFailed {
                path: self.path.clone(),
                reason: "passphrase not set".to_owned(),
            });
        };
        drop(pass_guard);

        let path = self.path.clone();
        let keyfile = self.keyfile.clone();
        // `keepass::Database::open` is synchronous + CPU-bound
        // (Argon2id KDF). Off-load to a blocking thread so the
        // async runtime stays responsive.
        let snapshot = tokio::task::spawn_blocking(move || {
            open_kdbx_into_snapshot(&path, &pass, keyfile.as_deref())
        })
        .await
        .map_err(|join_err| KdbxSourceError::OpenFailed {
            path: self.path.clone(),
            reason: format!("blocking task panicked: {join_err}"),
        })??;
        *self.snapshot.lock().await = Some(snapshot);
        Ok(())
    }

    /// Test / introspection helper — clone the cached snapshot's
    /// entry list (titles + paths + metadata, **NOT** passwords).
    /// Returns `None` when the snapshot is not yet populated. Used
    /// by the UI's inventory view to populate rows without
    /// exposing values to the agent layer.
    pub async fn list_entry_summaries(&self) -> Option<Vec<KdbxEntrySummary>> {
        let guard = self.snapshot.lock().await;
        guard.as_ref().map(|snap| {
            snap.entries
                .iter()
                .map(|e| KdbxEntrySummary {
                    path: e.path.clone(),
                    title: e.title.clone(),
                    username: e.username.clone(),
                    url: e.url.clone(),
                })
                .collect()
        })
    }
}

/// Inventory-row projection of [`KdbxEntry`] — every field except
/// the password. Cheap to clone + safe to send to the
/// agent-facing layer; the password stays inside the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KdbxEntrySummary {
    /// Normalized path the router uses as the lookup key.
    pub path: String,
    /// Entry Title (as displayed in KeePass).
    pub title: String,
    /// UserName field.
    pub username: Option<String>,
    /// URL field.
    pub url: Option<String>,
}

#[async_trait]
impl SecretSource for KdbxSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        // Read-only MVP. WRITE / ROTATE deferred to a follow-up;
        // AUDIT_LOGGED is false because KDBX itself does not record
        // read access.
        Capabilities::READ | Capabilities::LIST | Capabilities::BIOMETRIC_PROMPT
    }

    async fn is_available(&self) -> SourceStatus {
        if !self.path.exists() {
            return SourceStatus::NotInstalled;
        }
        if !self.is_unlocked().await {
            return SourceStatus::Locked;
        }
        match self.ensure_snapshot().await {
            Ok(()) => SourceStatus::Available,
            Err(KdbxSourceError::OpenFailed { reason, .. }) => SourceStatus::Error(reason),
            Err(KdbxSourceError::NoSuchEntry { .. }) => {
                // ensure_snapshot doesn't return NoSuchEntry, but
                // the exhaustive match is the safe future-proof.
                SourceStatus::Available
            }
        }
    }

    async fn get(&self, reference: &str) -> Result<Option<GetOutcome>, SourceError> {
        // First, ensure the file is open + cached.
        self.ensure_snapshot()
            .await
            .map_err(|e| SourceError::Upstream {
                name: self.name.clone(),
                message: e.to_string(),
            })?;
        let guard = self.snapshot.lock().await;
        let snapshot = guard.as_ref().expect("ensure_snapshot populated this");
        // Linear scan over the cached entries. Fine up to a few
        // thousand entries; if a user shows up with a KDBX of
        // 50 000+ entries we'll swap in a HashMap.
        for entry in &snapshot.entries {
            if entry.path == reference {
                return match &entry.password {
                    Some(p) => Ok(Some(GetOutcome {
                        value: SecretString::from(p.expose_secret().to_string()),
                        lease_duration: None,
                    })),
                    None => Ok(None),
                };
            }
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------
// Synchronous open + traversal (runs inside spawn_blocking)
// ---------------------------------------------------------------

/// Open `.kdbx` at `path` with `passphrase` + optional `keyfile`
/// and flatten every entry into a [`KdbxSnapshot`].
///
/// Standalone function so it can run inside `spawn_blocking` (the
/// `keepass` crate is sync + CPU-bound on the Argon2id KDF).
fn open_kdbx_into_snapshot(
    path: &std::path::Path,
    passphrase: &SecretString,
    keyfile: Option<&std::path::Path>,
) -> Result<KdbxSnapshot, KdbxSourceError> {
    use keepass::{Database, DatabaseKey};
    use std::fs::File;
    use std::io::BufReader;

    let mut file = File::open(path).map_err(|e| KdbxSourceError::OpenFailed {
        path: path.to_path_buf(),
        reason: format!("could not open file: {e}"),
    })?;
    let mut key = DatabaseKey::new().with_password(passphrase.expose_secret());
    if let Some(kf_path) = keyfile {
        let kf = File::open(kf_path).map_err(|e| KdbxSourceError::OpenFailed {
            path: kf_path.to_path_buf(),
            reason: format!("could not open keyfile: {e}"),
        })?;
        let mut kf_reader = BufReader::new(kf);
        key = key
            .with_keyfile(&mut kf_reader)
            .map_err(|e| KdbxSourceError::OpenFailed {
                path: path.to_path_buf(),
                reason: format!("keyfile parse failed: {e}"),
            })?;
    }
    let db = Database::open(&mut file, key).map_err(|e| KdbxSourceError::OpenFailed {
        path: path.to_path_buf(),
        reason: format!("{e}"),
    })?;

    let mut entries = Vec::new();
    walk_group(db.root(), &mut Vec::new(), &mut entries);
    Ok(KdbxSnapshot { entries })
}

/// Recursive traversal helper — visits every entry under
/// `group_ref`, maintaining the breadcrumb in `path_segments`.
///
/// The Root group's name is **not** added to the breadcrumb (it's
/// implicit). All other group names are pushed before descending.
fn walk_group(
    group_ref: keepass::db::GroupRef<'_>,
    path_segments: &mut Vec<String>,
    out: &mut Vec<KdbxEntry>,
) {
    for entry in group_ref.entries() {
        let title = entry.get_title().unwrap_or("(no title)").to_owned();
        let normalised_title = normalize_segment(&title);
        let mut segments = path_segments.clone();
        segments.push(normalised_title);
        let path = ensure_min_three_segments(segments);
        let password = entry
            .get_password()
            .filter(|p| !p.is_empty())
            .map(|p| SecretString::from(p.to_owned()));
        out.push(KdbxEntry {
            path,
            password,
            title,
            username: entry.get_username().map(str::to_owned),
            url: entry.get_url().map(str::to_owned),
            notes: entry.get("Notes").map(str::to_owned),
        });
    }
    for child in group_ref.groups() {
        let segment = normalize_segment(&child.name);
        path_segments.push(segment);
        walk_group(child, path_segments, out);
        path_segments.pop();
    }
}

/// Lowercase + replace runs of non-`[a-z0-9_]` with `-`, then trim
/// `-` from the ends. Mirrors what `SecretPath::parse` accepts at
/// segment level. Empty input becomes `entry` so we always
/// produce a non-empty segment.
fn normalize_segment(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_dash = true;
    for ch in lower.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        "entry".to_owned()
    } else {
        trimmed
    }
}

/// `SecretPath::parse` (ADR-020 §2) requires ≥3 slash-separated
/// segments. Two-step normalization:
///
/// 1. Pad with `imported` until we have ≥2 user-derived segments
///    (so the final 1-entry root case still produces a clean
///    three-segment path after the `kdbx` prefix lands).
/// 2. Prefix with `kdbx` so KDBX-sourced paths are easy to tell
///    apart from manifest-declared ones at a glance.
fn ensure_min_three_segments(mut segments: Vec<String>) -> String {
    while segments.len() < 2 {
        segments.insert(0, "imported".to_owned());
    }
    if segments[0] != "kdbx" {
        segments.insert(0, "kdbx".to_owned());
    }
    segments.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- normalize_segment + ensure_min_three_segments ---------

    #[test]
    fn normalize_segment_lowercases_and_dashifies() {
        assert_eq!(normalize_segment("AWS Access Key"), "aws-access-key");
        assert_eq!(normalize_segment("API_TOKEN_v2"), "api_token_v2");
        assert_eq!(normalize_segment("foo/bar"), "foo-bar");
        assert_eq!(normalize_segment("   "), "entry");
        assert_eq!(normalize_segment("--hyphenated--"), "hyphenated");
    }

    #[test]
    fn ensure_min_three_segments_namespaces_short_paths() {
        // Single segment → kdbx/imported/<x>
        assert_eq!(
            ensure_min_three_segments(vec!["token".to_owned()]),
            "kdbx/imported/token"
        );
        // Two segments → kdbx/<a>/<b>
        assert_eq!(
            ensure_min_three_segments(vec!["work".to_owned(), "github".to_owned()]),
            "kdbx/work/github"
        );
        // Three+ segments → kdbx/<...>
        assert_eq!(
            ensure_min_three_segments(vec![
                "personal".to_owned(),
                "cloud".to_owned(),
                "aws".to_owned()
            ]),
            "kdbx/personal/cloud/aws"
        );
        // Already prefixed — must not double-prefix.
        assert_eq!(
            ensure_min_three_segments(vec!["kdbx".to_owned(), "work".to_owned(), "ci".to_owned()]),
            "kdbx/work/ci"
        );
    }

    // -- SecretSource skeleton sanity --------------------------

    #[test]
    fn capabilities_report_read_list_and_biometric_prompt() {
        let src = KdbxSource::new("test", "/tmp/no-such-file.kdbx");
        let caps = src.capabilities();
        assert!(caps.contains(Capabilities::READ));
        assert!(caps.contains(Capabilities::LIST));
        assert!(caps.contains(Capabilities::BIOMETRIC_PROMPT));
        assert!(!caps.contains(Capabilities::WRITE));
    }

    #[tokio::test]
    async fn is_available_reports_not_installed_for_missing_file() {
        let src = KdbxSource::new("test", "/tmp/definitely-not-a-real-kdbx.kdbx");
        assert!(matches!(
            src.is_available().await,
            SourceStatus::NotInstalled
        ));
    }

    #[tokio::test]
    async fn is_available_reports_locked_when_file_exists_but_no_passphrase() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let src = KdbxSource::new("test", tmp.path());
        assert!(matches!(src.is_available().await, SourceStatus::Locked));
    }

    #[tokio::test]
    async fn debug_does_not_leak_passphrase() {
        let src = KdbxSource::new("test", "/tmp/x.kdbx");
        src.set_passphrase(SecretString::from("super-secret-passphrase"))
            .await;
        let dbg = format!("{src:?}");
        assert!(!dbg.contains("super-secret-passphrase"));
        assert!(dbg.contains("<redacted>"));
    }

    // -- End-to-end open + walk against a synthetic fixture ---

    /// Build a fresh KDBX 4 database in a tempdir, populate a
    /// known tree of groups + entries, save it, then open it via
    /// our source and assert the snapshot matches. Exercises the
    /// full open path: Argon2id derive, ChaCha20 decrypt, XML
    /// parse, our traversal + path normalization.
    #[tokio::test]
    async fn open_synthetic_kdbx_and_walks_groups_into_paths() {
        use keepass::db::Value;
        use keepass::{Database, DatabaseKey};

        let tmp_dir = tempfile::TempDir::new().unwrap();
        let kdbx_path = tmp_dir.path().join("synthetic.kdbx");

        let mut db = Database::new();
        {
            let mut root = db.root_mut();
            // Root-level entry — should land under kdbx/imported/...
            {
                let mut bare = root.add_entry();
                bare.set_unprotected("Title", "BareToken");
                bare.set("Password", Value::Unprotected("bare-fixture".to_owned()));
            }
            // Personal / Cloud sub-tree with two entries.
            let mut personal = root.add_group();
            personal.name = "Personal".to_owned();
            let mut cloud = personal.add_group();
            cloud.name = "Cloud".to_owned();
            {
                let mut aws = cloud.add_entry();
                aws.set_unprotected("Title", "AWS Access Key");
                aws.set_unprotected("UserName", "AKIAIOSFODNN7EXAMPLE");
                aws.set_unprotected("URL", "https://console.aws.amazon.com/");
                aws.set("Password", Value::Unprotected("aws-sk-fixture".to_owned()));
            }
            {
                let mut gcp = cloud.add_entry();
                gcp.set_unprotected("Title", "GCP Service Account");
                gcp.set("Password", Value::Unprotected("gcp-key-fixture".to_owned()));
            }
        }

        // Save to disk so our source reads it back through the
        // real file → reader path.
        let mut out = std::fs::File::create(&kdbx_path).unwrap();
        let key = DatabaseKey::new().with_password("test-passphrase");
        db.save(&mut out, key).unwrap();

        let src = KdbxSource::new("personal-keepass", &kdbx_path);
        src.set_passphrase(SecretString::from("test-passphrase"))
            .await;

        // is_available should report Available after open.
        match src.is_available().await {
            SourceStatus::Available => {}
            other => panic!("expected Available, got {other:?}"),
        }

        // Fetch the AWS entry by its normalised path.
        let outcome = src
            .get("kdbx/personal/cloud/aws-access-key")
            .await
            .unwrap()
            .expect("AWS entry should exist");
        assert_eq!(outcome.value.expose_secret(), "aws-sk-fixture");

        // GCP entry under the same Cloud group.
        let outcome = src
            .get("kdbx/personal/cloud/gcp-service-account")
            .await
            .unwrap()
            .expect("GCP entry should exist");
        assert_eq!(outcome.value.expose_secret(), "gcp-key-fixture");

        // Root-level entry — collapsed to the imported namespace.
        let outcome = src
            .get("kdbx/imported/baretoken")
            .await
            .unwrap()
            .expect("bare token should exist");
        assert_eq!(outcome.value.expose_secret(), "bare-fixture");

        // Unknown reference returns None, not an error.
        assert!(src.get("kdbx/nope/missing").await.unwrap().is_none());

        // list_entry_summaries surfaces metadata WITHOUT the
        // password field (agent-blindness boundary).
        let summaries = src.list_entry_summaries().await.unwrap();
        let aws_summary = summaries
            .iter()
            .find(|s| s.path == "kdbx/personal/cloud/aws-access-key")
            .expect("AWS summary present");
        assert_eq!(aws_summary.title, "AWS Access Key");
        assert_eq!(
            aws_summary.username.as_deref(),
            Some("AKIAIOSFODNN7EXAMPLE")
        );
        assert_eq!(
            aws_summary.url.as_deref(),
            Some("https://console.aws.amazon.com/")
        );
    }

    #[tokio::test]
    async fn wrong_passphrase_surfaces_as_source_status_error() {
        use keepass::{Database, DatabaseKey};
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let kdbx_path = tmp_dir.path().join("locked.kdbx");
        let db = Database::new();
        let mut out = std::fs::File::create(&kdbx_path).unwrap();
        db.save(
            &mut out,
            DatabaseKey::new().with_password("real-passphrase"),
        )
        .unwrap();

        let src = KdbxSource::new("locked", &kdbx_path);
        src.set_passphrase(SecretString::from("wrong-passphrase"))
            .await;
        match src.is_available().await {
            SourceStatus::Error(reason) => {
                // The keepass crate's error mentions a HMAC or AEAD
                // mismatch; we just assert it's some non-empty
                // reason so the UI has something to show the user.
                assert!(
                    !reason.is_empty(),
                    "wrong-passphrase error must carry a reason"
                );
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
