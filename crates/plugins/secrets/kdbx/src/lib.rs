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

/// One value-bearing field on a KDBX entry. KeePass entries
/// can carry multiple Protected custom strings (e.g.
/// `password_admin`, `password_readonly`, `api_token`) AND
/// have the standard Password field set at the same time. We
/// preserve all of them so the UI can show every value the
/// user might want.
#[derive(Debug, Clone)]
pub struct KdbxValue {
    /// Field name (`Password`, `api_token`, `admin_password`, …).
    /// Standard KeePass fields keep their canonical casing;
    /// custom strings preserve whatever the user picked.
    pub name: String,
    /// The value itself. `SecretString` zeroizes on drop so a
    /// stray clone doesn't outlive the read.
    pub value: SecretString,
    /// `true` when KeePass stored this as a Protected custom
    /// string (or it's the standard Password). The UI uses this
    /// to mask the field by default; the user can reveal one
    /// field at a time.
    pub is_protected: bool,
}

impl PartialEq for KdbxValue {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.is_protected == other.is_protected
            && self.value.expose_secret() == other.value.expose_secret()
    }
}
impl Eq for KdbxValue {}

/// Metadata for one KeePass attachment, surfaced in the
/// snapshot WITHOUT the binary content. Lists name + size so
/// the UI can show "📎 server.pem (2 KiB)" without reading
/// potentially sensitive blobs into the inventory view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KdbxAttachmentMeta {
    /// Filename as recorded in KeePass.
    pub name: String,
    /// Body size in bytes.
    pub size_bytes: usize,
}

/// One inventory row resolved out of the KDBX file.
///
/// Held in the per-source [`KdbxSnapshot`]. `values` carries
/// every value-bearing field the entry has (multiple passwords,
/// custom tokens, etc.); the standalone fields above carry
/// metadata the UI surfaces in the inventory row + context card.
///
/// All standard KeePass fields are preserved + every custom
/// string the user added gets carried through `values` (or via
/// the `username` / `url` / `notes` convenience slots for the
/// metadata-only standard fields).
#[derive(Debug)]
pub struct KdbxEntry {
    /// Normalized ADR-020-like path (lowercase, `[a-z0-9_-]` only,
    /// `/`-separated segments). Mirrors `SecretSource::get`'s
    /// reference argument.
    pub path: String,
    /// Every value-bearing field on this entry, in stable order
    /// (standard Password first if non-empty, then custom
    /// strings alphabetised). The UI lists them all in the
    /// reveal-dialog; `SecretSource::get(path)` defaults to the
    /// first Protected entry. `SecretSource::get(path#name)`
    /// (K18.5 extension) routes by field name.
    pub values: Vec<KdbxValue>,
    /// Entry Title field — preserved verbatim for display.
    pub title: String,
    /// Optional UserName field.
    pub username: Option<String>,
    /// Optional URL field.
    pub url: Option<String>,
    /// Optional Notes field. Multiline; the UI's context card
    /// renders it as a `monospace` block.
    pub notes: Option<String>,
    /// Stable KeePass UUID (hyphenated hex). Survives renames
    /// of Title / Group, so the manifest can pin entries by
    /// UUID instead of path when the source DB is volatile.
    pub uuid: String,
    /// KeePass tags (already a `Vec<String>` in the upstream
    /// type — preserved verbatim).
    pub tags: Vec<String>,
    /// ISO 8601 string of [`keepass::Times::creation`].
    pub created_at: Option<String>,
    /// ISO 8601 string of [`keepass::Times::last_modification`].
    /// The UI uses this as a `last_rotated_at` heuristic.
    pub modified_at: Option<String>,
    /// ISO 8601 string of [`keepass::Times::expiry`], **only**
    /// when [`keepass::Times::expires`] is `Some(true)`. KeePass
    /// stores an expiry timestamp even when expiry isn't
    /// enabled; we hide that "ghost" value here so downstream
    /// rotation warnings don't fire on it.
    pub expires_at: Option<String>,
    /// Raw OTP source: `otpauth://...` URL or `secret=...` query
    /// string. Surfaced as a separate field so the UI can show
    /// a "TOTP available" chip without reading the secret.
    pub otp: Option<String>,
    /// Attachment metadata only (names + sizes). The actual
    /// bytes are never read into the snapshot — fetching them
    /// would force every consumer to handle potentially-secret
    /// binaries (PEM keys, .keytab files) and we'd rather punt
    /// that to an explicit "extract attachment" flow.
    pub attachments: Vec<KdbxAttachmentMeta>,
}

impl KdbxEntry {
    /// Primary value — the one `get(path)` defaults to. Prefers
    /// the first Protected field; falls back to the first
    /// value at all when nothing's protected. Returns `None`
    /// when the entry has no values (empty placeholder entry).
    pub fn primary_value(&self) -> Option<&KdbxValue> {
        self.values
            .iter()
            .find(|v| v.is_protected)
            .or_else(|| self.values.first())
    }

    /// Look up a value by KeePass field name (case-sensitive
    /// — KeePass preserves the user's casing).
    pub fn value_by_name(&self, name: &str) -> Option<&KdbxValue> {
        self.values.iter().find(|v| v.name == name)
    }
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
        // `path` or `path#field-name`. The fragment lets the
        // router pull a specific value when a KeePass entry
        // carries several Protected strings (multiple passwords
        // per entry is common — `password_admin` +
        // `password_readonly` + …).
        let (path_ref, field_ref) = match reference.split_once('#') {
            Some((p, f)) if !f.is_empty() => (p, Some(f)),
            _ => (reference, None),
        };
        for entry in &snapshot.entries {
            if entry.path == path_ref {
                let value = match field_ref {
                    Some(name) => entry.value_by_name(name),
                    None => entry.primary_value(),
                };
                return Ok(value.map(|v| GetOutcome {
                    value: SecretString::from(v.value.expose_secret().to_string()),
                    lease_duration: None,
                }));
            }
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------
// Synchronous open + traversal (runs inside spawn_blocking)
// ---------------------------------------------------------------

/// Compute the working-copy path for `source`. Format is
/// `<source-without-ext>.devboy-working-<ISO-timestamp>.kdbx`
/// next to the original file. Timestamp uses UTC + `%Y%m%dT%H%M%S`
/// so concurrent copies don't collide (and the user can `ls`-sort
/// chronologically).
///
/// The returned path is **not** created here — call
/// [`prepare_working_copy`] to also do the file copy.
pub fn derive_working_copy_path(source: &std::path::Path) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kdbx".to_owned());
    let dir = source.parent().unwrap_or_else(|| std::path::Path::new("."));
    let now = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    dir.join(format!("{stem}.devboy-working-{now}.kdbx"))
}

/// Materialise a working copy of `source` so subsequent writes
/// don't risk the original. Idempotent in the sense that if
/// `destination` already exists with bit-identical content the
/// copy is a no-op; otherwise the source is byte-copied.
///
/// Returns the destination path on success so the caller can
/// store it in the backend state.
pub fn prepare_working_copy(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<PathBuf, KdbxSourceError> {
    if !source.exists() {
        return Err(KdbxSourceError::OpenFailed {
            path: source.to_path_buf(),
            reason: "source KDBX does not exist".to_owned(),
        });
    }
    if !destination.exists() {
        std::fs::copy(source, destination).map_err(|e| KdbxSourceError::OpenFailed {
            path: destination.to_path_buf(),
            reason: format!(
                "could not copy {} → {}: {e}",
                source.display(),
                destination.display()
            ),
        })?;
    }
    Ok(destination.to_path_buf())
}

/// Pull one attachment's binary bytes out of `path` given the
/// `passphrase` (+ optional `keyfile`), the entry's UUID, and
/// the attachment's filename. Re-opens the KDBX file every
/// call rather than caching binary bodies in the snapshot —
/// attachments can run to hundreds of megabytes (PEM
/// archives, dumps, recordings) and eager loading would
/// inflate the in-process memory footprint for every
/// inventory open.
///
/// Returns `Ok(None)` when the entry exists but doesn't carry
/// the named attachment, `Ok(Some(bytes))` on success, and
/// `Err(KdbxSourceError::OpenFailed)` when the file open / KDF
/// fails (wrong passphrase, corrupt body, missing file).
///
/// Synchronous + CPU-bound (Argon2id derive again on each
/// call). Callers in async contexts should wrap in
/// `tokio::task::spawn_blocking`.
pub fn extract_attachment(
    path: &std::path::Path,
    passphrase: &SecretString,
    keyfile: Option<&std::path::Path>,
    entry_uuid: &str,
    attachment_name: &str,
) -> Result<Option<Vec<u8>>, KdbxSourceError> {
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

    // Iterative DFS over the entire group tree — looking for
    // the entry whose UUID matches, then its named attachment.
    // Iterating by GroupId (Copy) instead of GroupRef sidesteps
    // the borrow chain: each iteration re-resolves the ref
    // from the database against a fresh GroupId.
    let mut stack: Vec<keepass::db::GroupId> = vec![db.root().id()];
    while let Some(group_id) = stack.pop() {
        let Some(group) = db.group(group_id) else {
            continue;
        };
        for entry in group.entries() {
            if entry.id().uuid().hyphenated().to_string() == entry_uuid {
                for (name, att) in entry.attachments_named() {
                    if name == attachment_name {
                        return Ok(Some(att.data.get().clone()));
                    }
                }
                return Ok(None);
            }
        }
        let sub_ids: Vec<keepass::db::GroupId> = group.group_ids().collect();
        stack.extend(sub_ids);
    }
    Ok(None)
}

/// K14 — metadata patch for one KeePass entry. Every field is
/// optional; `None` means "leave the existing value alone".
/// Setting a field to `Some(…)` overwrites it; setting to
/// `Some(empty-string / empty-vec / clearing variant)` clears it.
///
/// EXCLUDES `password` and any Protected custom string by design.
/// The agent-blindness boundary (ADR-023 §3.7) says agents can
/// rotate documentation around a secret but never alter the
/// secret bytes themselves — that flow stays in the GUI's
/// per-field reveal/copy path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataPatch {
    /// New Entry Title (the user-visible label in KeePass).
    pub title: Option<String>,
    /// New UserName. `Some(empty)` clears it.
    pub username: Option<String>,
    /// New URL. `Some(empty)` clears it.
    pub url: Option<String>,
    /// New Notes block (multiline allowed). `Some(empty)` clears.
    pub notes: Option<String>,
    /// New tag list — replaces the current set wholesale. Use an
    /// empty Vec to clear all tags.
    pub tags: Option<Vec<String>>,
    /// Expiry control:
    /// * `Some(Some(iso))` — set expiry to that timestamp +
    ///   flip `times.expires` to true
    /// * `Some(None)` — clear `times.expires` (no expiry)
    /// * `None` — leave the field alone
    ///
    /// `iso` is parsed as RFC 3339 / ISO 8601; on parse failure
    /// `edit_metadata` returns `KdbxSourceError::OpenFailed` with
    /// a "bad expires_at" message.
    pub expires_at: Option<Option<String>>,
}

/// K14b — read-only metadata projection for one entry. Same
/// surface as [`MetadataPatch`], plus the immutable identifying
/// fields the caller needs to round-trip an edit (`uuid`,
/// `created_at`, `modified_at`, attachment metadata).
///
/// Excludes the value-bearing fields (Password / Protected
/// custom strings) — agents that call `describe_metadata` see
/// exactly what `edit_metadata` will let them change, no more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KdbxEntryMetadata {
    pub uuid: String,
    pub title: String,
    pub username: Option<String>,
    pub url: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    pub expires_at: Option<String>,
    pub otp: Option<String>,
    pub attachments: Vec<KdbxAttachmentMeta>,
    /// Names of custom string fields on the entry (both Protected
    /// and Unprotected). Helps the caller know what's available
    /// to set via `set_unprotected` in a future
    /// `edit_metadata` extension; we don't expose values here
    /// because Protected ones would be secret bytes.
    pub custom_string_names: Vec<String>,
}

/// K14 — apply a [`MetadataPatch`] to the entry identified by
/// `entry_uuid` inside the KDBX file at `path`. Re-derives the
/// Argon2id KDF on open, walks to the entry, mutates only the
/// patch-covered fields, then `Database::save` back to the same
/// path.
///
/// Working-copy safety: this function writes to `path` verbatim
/// — callers MUST pass a working-copy path (see
/// [`derive_working_copy_path`] / [`prepare_working_copy`]) and
/// then atomically swap or sync back to the user's original
/// file on their own schedule. The function does NOT auto-copy;
/// that's the orchestrating layer's job (the UI bin already
/// honours `DEVBOY_KDBX_WORKING_COPY=auto`).
///
/// Returns `KdbxSourceError::OpenFailed` on:
/// * wrong passphrase / corrupt body
/// * unknown `entry_uuid` (no walked entry matched)
/// * bad `expires_at` ISO 8601 string in the patch
/// * filesystem failure on the write side
///
/// Synchronous + CPU-bound — wrap in `spawn_blocking` from an
/// async context.
pub fn edit_metadata(
    path: &std::path::Path,
    passphrase: &SecretString,
    keyfile: Option<&std::path::Path>,
    entry_uuid: &str,
    patch: &MetadataPatch,
) -> Result<(), KdbxSourceError> {
    use keepass::db::Value;
    use keepass::{Database, DatabaseKey};
    use std::fs::File;
    use std::io::BufReader;

    // ---- open ------------------------------------------------
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
    // The key is consumed by `open`; clone what we need to feed
    // `save` again later (DatabaseKey is Clone in 0.12).
    let save_key = DatabaseKey::new().with_password(passphrase.expose_secret());
    let mut db = Database::open(&mut file, key).map_err(|e| KdbxSourceError::OpenFailed {
        path: path.to_path_buf(),
        reason: format!("{e}"),
    })?;
    drop(file);

    // ---- locate by uuid + mutate -----------------------------
    let target_id =
        find_entry_id_by_uuid(&db, entry_uuid).ok_or_else(|| KdbxSourceError::OpenFailed {
            path: path.to_path_buf(),
            reason: format!("entry uuid {entry_uuid:?} not found"),
        })?;

    // Pre-validate expires_at BEFORE mutating so a bad ISO 8601
    // doesn't leave a half-applied patch.
    let parsed_expiry: Option<Option<chrono::NaiveDateTime>> = match &patch.expires_at {
        None => None,
        Some(None) => Some(None),
        Some(Some(iso)) => Some(Some(
            iso.parse::<chrono::NaiveDateTime>()
                .or_else(|_| chrono::DateTime::parse_from_rfc3339(iso).map(|dt| dt.naive_utc()))
                .map_err(|e| KdbxSourceError::OpenFailed {
                    path: path.to_path_buf(),
                    reason: format!("bad expires_at {iso:?}: {e}"),
                })?,
        )),
    };

    // Wrap the EntryMut borrow in a scope so it ends before
    // we hand the database off to `save`. clippy's
    // `drop_non_drop` doesn't recognise EntryMut as carrying a
    // Drop impl, so an explicit `drop(entry)` would trip it.
    {
        let mut entry = db
            .entry_mut(target_id)
            .ok_or_else(|| KdbxSourceError::OpenFailed {
                path: path.to_path_buf(),
                reason: format!("entry {entry_uuid:?} disappeared mid-edit"),
            })?;

        if let Some(v) = &patch.title {
            entry.fields.insert(
                keepass::db::fields::TITLE.to_owned(),
                Value::Unprotected(v.clone()),
            );
        }
        if let Some(v) = &patch.username {
            entry.fields.insert(
                keepass::db::fields::USERNAME.to_owned(),
                Value::Unprotected(v.clone()),
            );
        }
        if let Some(v) = &patch.url {
            entry.fields.insert(
                keepass::db::fields::URL.to_owned(),
                Value::Unprotected(v.clone()),
            );
        }
        if let Some(v) = &patch.notes {
            entry.fields.insert(
                keepass::db::fields::NOTES.to_owned(),
                Value::Unprotected(v.clone()),
            );
        }
        if let Some(tags) = &patch.tags {
            entry.tags = tags.clone();
        }
        match parsed_expiry {
            None => {}
            Some(None) => {
                entry.times.expires = Some(false);
            }
            Some(Some(dt)) => {
                entry.times.expiry = Some(dt);
                entry.times.expires = Some(true);
            }
        }
        // Stamp the modification time so any other KeePass
        // client re-syncing the file sees the change.
        entry.times.last_modification = Some(keepass::db::Times::now());
    }

    // ---- save back -------------------------------------------
    let mut out = std::fs::File::create(path).map_err(|e| KdbxSourceError::OpenFailed {
        path: path.to_path_buf(),
        reason: format!("could not open for write: {e}"),
    })?;
    db.save(&mut out, save_key)
        .map_err(|e| KdbxSourceError::OpenFailed {
            path: path.to_path_buf(),
            reason: format!("save failed: {e}"),
        })?;
    Ok(())
}

/// K14b — read-only companion to [`edit_metadata`]. Opens the
/// KDBX, locates the entry by UUID, and projects its
/// non-value-bearing fields into a [`KdbxEntryMetadata`].
///
/// Excludes Password / any Protected custom string from the
/// return so an agent calling this can't fish for secrets via
/// the metadata channel.
pub fn describe_metadata(
    path: &std::path::Path,
    passphrase: &SecretString,
    keyfile: Option<&std::path::Path>,
    entry_uuid: &str,
) -> Result<Option<KdbxEntryMetadata>, KdbxSourceError> {
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

    let Some(entry_id) = find_entry_id_by_uuid(&db, entry_uuid) else {
        return Ok(None);
    };
    let entry = db
        .entry(entry_id)
        .ok_or_else(|| KdbxSourceError::OpenFailed {
            path: path.to_path_buf(),
            reason: format!("entry {entry_uuid:?} disappeared mid-read"),
        })?;

    let custom_string_names: Vec<String> = entry
        .fields
        .keys()
        .filter(|k| !is_standard_metadata_field(k) && k.as_str() != "Password")
        .cloned()
        .collect();
    let attachments: Vec<KdbxAttachmentMeta> = entry
        .attachments_named()
        .map(|(name, att)| KdbxAttachmentMeta {
            name: name.to_owned(),
            size_bytes: att.data.get().len(),
        })
        .collect();

    Ok(Some(KdbxEntryMetadata {
        uuid: entry_uuid.to_owned(),
        title: entry.get_title().unwrap_or_default().to_owned(),
        username: entry
            .get_username()
            .map(str::to_owned)
            .filter(|s| !s.is_empty()),
        url: entry.get_url().map(str::to_owned).filter(|s| !s.is_empty()),
        notes: entry
            .get(keepass::db::fields::NOTES)
            .map(str::to_owned)
            .filter(|s| !s.is_empty()),
        tags: entry.tags.clone(),
        created_at: entry
            .times
            .creation
            .map(|t| t.and_utc().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        modified_at: entry
            .times
            .last_modification
            .map(|t| t.and_utc().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        expires_at: if entry.times.expires == Some(true) {
            entry
                .times
                .expiry
                .map(|t| t.and_utc().format("%Y-%m-%dT%H:%M:%SZ").to_string())
        } else {
            None
        },
        otp: entry
            .get_raw_otp_value()
            .map(str::to_owned)
            .filter(|s| !s.is_empty()),
        attachments,
        custom_string_names,
    }))
}

/// Helper — walk every group iteratively, return the first
/// EntryId whose uuid matches. Same iteration shape as
/// `extract_attachment` to avoid borrow-checker pain.
fn find_entry_id_by_uuid(db: &keepass::Database, entry_uuid: &str) -> Option<keepass::db::EntryId> {
    let mut stack: Vec<keepass::db::GroupId> = vec![db.root().id()];
    while let Some(group_id) = stack.pop() {
        let Some(group) = db.group(group_id) else {
            continue;
        };
        for entry in group.entries() {
            if entry.id().uuid().hyphenated().to_string() == entry_uuid {
                return Some(entry.id());
            }
        }
        let sub_ids: Vec<keepass::db::GroupId> = group.group_ids().collect();
        stack.extend(sub_ids);
    }
    None
}

/// Standard KeePass field names — used by `describe_metadata` to
/// filter custom string fields out of the standard-field list.
fn is_standard_metadata_field(name: &str) -> bool {
    matches!(name, "Title" | "UserName" | "URL" | "Notes")
}

/// Open `.kdbx` at `path` with `passphrase` + optional `keyfile`
/// and flatten every entry into a [`KdbxSnapshot`].
///
/// Standalone function so it can run inside `spawn_blocking` (the
/// `keepass` crate is sync + CPU-bound on the Argon2id KDF).
///
/// Exposed as `pub` so the UI bin's `StorageBackend::unlock` path
/// can call it directly without going through `KdbxSource`'s
/// async wrapper — the UI already holds a tokio runtime and just
/// wants to materialize the snapshot on the unlock-modal submit.
pub fn open_kdbx_into_snapshot(
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

/// KeePass standard metadata-only field names — these are
/// surfaced through dedicated convenience slots on
/// [`KdbxEntry`] (`title` / `username` / `url` / `notes` /
/// `otp`) instead of being promoted into the `values` vec.
const METADATA_FIELDS: &[&str] = &["Title", "UserName", "URL", "Notes", "otp"];

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

        // K18 multi-value: collect EVERY value-bearing field
        // on the entry. KeePass commonly stores multiple
        // Protected strings per entry (`password_admin`,
        // `password_readonly`, `api_token`); we preserve all
        // of them so the UI can list + reveal each one.
        //
        // Order: standard Password first (if non-empty), then
        // custom strings alphabetised. Both Protected and
        // Unprotected customs are kept; the `is_protected`
        // flag drives masking in the UI.
        let mut values: Vec<KdbxValue> = Vec::new();
        if let Some(pw) = entry.get_password().filter(|p| !p.is_empty()) {
            values.push(KdbxValue {
                name: "Password".to_owned(),
                value: SecretString::from(pw.to_owned()),
                is_protected: true,
            });
        }
        let mut customs: Vec<(&String, &keepass::db::Value<String>)> = entry
            .fields
            .iter()
            .filter(|(k, _)| k.as_str() != "Password" && !METADATA_FIELDS.contains(&k.as_str()))
            .filter(|(_, v)| !v.as_str().is_empty())
            .collect();
        customs.sort_by_key(|(k, _)| k.as_str().to_owned());
        for (key, value) in customs {
            values.push(KdbxValue {
                name: key.clone(),
                value: SecretString::from(value.as_str().to_owned()),
                is_protected: value.is_protected(),
            });
        }

        // Times — format as ISO 8601 + suppress the ghost
        // expiry timestamp when KeePass's `Expires` flag is off.
        let created_at = entry
            .times
            .creation
            .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string());
        let modified_at = entry
            .times
            .last_modification
            .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string());
        let expires_at = if entry.times.expires == Some(true) {
            entry
                .times
                .expiry
                .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        } else {
            None
        };

        // Attachments — metadata only.
        let attachments: Vec<KdbxAttachmentMeta> = entry
            .attachments_named()
            .map(|(name, att)| KdbxAttachmentMeta {
                name: name.to_owned(),
                size_bytes: att.data.get().len(),
            })
            .collect();

        out.push(KdbxEntry {
            path,
            values,
            title,
            username: entry.get_username().map(str::to_owned),
            url: entry.get_url().map(str::to_owned),
            notes: entry.get("Notes").map(str::to_owned),
            uuid: entry.id().uuid().hyphenated().to_string(),
            tags: entry.tags.clone(),
            created_at,
            modified_at,
            expires_at,
            otp: entry.get_raw_otp_value().map(str::to_owned),
            attachments,
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

    // -- K7 — full metadata extraction -----------------------

    /// Build a synthetic KDBX with one entry that exercises
    /// every K7 metadata field: tags, custom fields (both
    /// Protected and Unprotected), Notes, ExpiryTime + Expires,
    /// raw OTP, and walk it back out via `KdbxSource`. Pins
    /// every field of the resulting `KdbxEntry` so a future
    /// refactor of the walker can't silently drop one.
    #[tokio::test]
    async fn full_metadata_extraction_round_trip() {
        use chrono::NaiveDateTime;
        use keepass::db::Value;
        use keepass::{Database, DatabaseKey};

        let tmp_dir = tempfile::TempDir::new().unwrap();
        let kdbx_path = tmp_dir.path().join("rich.kdbx");

        let mut db = Database::new();
        let uuid_seen;
        {
            let mut root = db.root_mut();
            let mut work = root.add_group();
            work.name = "Work".to_owned();
            let mut entry = work.add_entry();
            entry.set_unprotected("Title", "Stripe API");
            entry.set_unprotected("UserName", "ops@example.com");
            entry.set_unprotected("URL", "https://dashboard.stripe.com/apikeys");
            entry.set_unprotected("Notes", "Quarterly rotation; alert oncall");
            entry.set("Password", Value::Unprotected("sk_live_fixture".to_owned()));
            entry.set("client_id", Value::Unprotected("acct_1abc".to_owned()));
            entry.set_protected("webhook_secret", "whsec_zzz");
            entry.tags = vec!["ops".to_owned(), "billing".to_owned()];
            entry.times.expires = Some(true);
            entry.times.expiry = Some(
                NaiveDateTime::parse_from_str("2027-01-15T00:00:00", "%Y-%m-%dT%H:%M:%S").unwrap(),
            );
            entry.times.creation = Some(
                NaiveDateTime::parse_from_str("2026-01-01T00:00:00", "%Y-%m-%dT%H:%M:%S").unwrap(),
            );
            entry.times.last_modification = Some(
                NaiveDateTime::parse_from_str("2026-05-01T00:00:00", "%Y-%m-%dT%H:%M:%S").unwrap(),
            );
            entry.set_unprotected(
                "otp",
                "otpauth://totp/Stripe:ops%40example.com?secret=ABCD&issuer=Stripe",
            );
            uuid_seen = entry.as_ref().id().uuid().hyphenated().to_string();
        }

        let mut out = std::fs::File::create(&kdbx_path).unwrap();
        db.save(&mut out, DatabaseKey::new().with_password("p"))
            .unwrap();

        let src = KdbxSource::new("work-kp", &kdbx_path);
        src.set_passphrase(SecretString::from("p")).await;
        match src.is_available().await {
            SourceStatus::Available => {}
            other => panic!("expected Available, got {other:?}"),
        }

        // Reach into the cached snapshot directly so we can
        // assert on EVERY field (the public `get()` only
        // returns the value).
        let guard = src.snapshot.lock().await;
        let snap = guard.as_ref().expect("snapshot populated");
        assert_eq!(snap.entries.len(), 1);
        let entry = &snap.entries[0];

        assert_eq!(entry.path, "kdbx/work/stripe-api");
        assert_eq!(entry.title, "Stripe API");
        assert_eq!(entry.username.as_deref(), Some("ops@example.com"));
        assert_eq!(
            entry.url.as_deref(),
            Some("https://dashboard.stripe.com/apikeys")
        );
        assert_eq!(
            entry.notes.as_deref(),
            Some("Quarterly rotation; alert oncall")
        );
        assert_eq!(entry.uuid, uuid_seen);
        assert_eq!(entry.tags, vec!["ops", "billing"]);
        assert_eq!(entry.created_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(entry.modified_at.as_deref(), Some("2026-05-01T00:00:00Z"));
        assert_eq!(entry.expires_at.as_deref(), Some("2027-01-15T00:00:00Z"));
        assert!(
            entry
                .otp
                .as_deref()
                .map(|s| s.starts_with("otpauth://"))
                .unwrap_or(false)
        );
        // K18 multi-value: three values land in `values` —
        // Password (standard), client_id (Unprotected custom),
        // webhook_secret (Protected custom). Ordered:
        // Password first, then customs alphabetised.
        let value_names: Vec<&str> = entry.values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(value_names, vec!["Password", "client_id", "webhook_secret"]);
        // is_protected flag is preserved per field.
        assert!(entry.value_by_name("Password").unwrap().is_protected);
        assert!(!entry.value_by_name("client_id").unwrap().is_protected);
        assert!(entry.value_by_name("webhook_secret").unwrap().is_protected);
        // Values themselves come through verbatim.
        assert_eq!(
            entry
                .value_by_name("Password")
                .unwrap()
                .value
                .expose_secret(),
            "sk_live_fixture"
        );
        assert_eq!(
            entry
                .value_by_name("client_id")
                .unwrap()
                .value
                .expose_secret(),
            "acct_1abc"
        );
        assert_eq!(
            entry
                .value_by_name("webhook_secret")
                .unwrap()
                .value
                .expose_secret(),
            "whsec_zzz"
        );
        // primary_value() prefers Protected → Password wins.
        assert_eq!(entry.primary_value().unwrap().name, "Password");
    }

    /// When the standard Password field is empty AND a
    /// Protected custom string exists, the K18 model surfaces
    /// the custom string as a value — `primary_value()`
    /// promotes it transparently since it's the first
    /// Protected entry available.
    #[tokio::test]
    async fn primary_value_picks_lone_protected_custom_when_password_empty() {
        use keepass::{Database, DatabaseKey};

        let tmp_dir = tempfile::TempDir::new().unwrap();
        let kdbx_path = tmp_dir.path().join("promoted.kdbx");

        let mut db = Database::new();
        {
            let mut root = db.root_mut();
            let mut entry = root.add_entry();
            entry.set_unprotected("Title", "GitHub PAT");
            entry.set_protected("api_token", "ghp_promoted_secret");
            entry.set_unprotected("note_to_self", "rotate quarterly");
        }
        let mut out = std::fs::File::create(&kdbx_path).unwrap();
        db.save(&mut out, DatabaseKey::new().with_password("p"))
            .unwrap();

        let src = KdbxSource::new("gh", &kdbx_path);
        src.set_passphrase(SecretString::from("p")).await;
        let _ = src.is_available().await;
        let guard = src.snapshot.lock().await;
        let entry = &guard.as_ref().unwrap().entries[0];

        // values has both customs (Password is absent → not
        // in the vec); api_token (Protected) + note_to_self
        // (Unprotected), alphabetised.
        let names: Vec<&str> = entry.values.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["api_token", "note_to_self"]);
        assert!(entry.value_by_name("api_token").unwrap().is_protected);
        assert!(!entry.value_by_name("note_to_self").unwrap().is_protected);

        // primary_value picks the first Protected.
        let primary = entry.primary_value().unwrap();
        assert_eq!(primary.name, "api_token");
        assert_eq!(primary.value.expose_secret(), "ghp_promoted_secret");
    }

    /// When MULTIPLE Protected custom strings exist (with no
    /// standard Password), K18 keeps all of them. The user can
    /// pick via `path#name` syntax via `KdbxSource::get`.
    #[tokio::test]
    async fn multiple_protected_customs_all_appear_in_values() {
        use keepass::{Database, DatabaseKey};

        let tmp_dir = tempfile::TempDir::new().unwrap();
        let kdbx_path = tmp_dir.path().join("ambiguous.kdbx");

        let mut db = Database::new();
        {
            let mut root = db.root_mut();
            let mut entry = root.add_entry();
            entry.set_unprotected("Title", "Ambiguous");
            entry.set_protected("token_a", "a-value");
            entry.set_protected("token_b", "b-value");
        }
        let mut out = std::fs::File::create(&kdbx_path).unwrap();
        db.save(&mut out, DatabaseKey::new().with_password("p"))
            .unwrap();

        let src = KdbxSource::new("amb", &kdbx_path);
        src.set_passphrase(SecretString::from("p")).await;
        let _ = src.is_available().await;
        // `get(path)` → primary value (first Protected
        // alphabetised → "token_a").
        let outcome = src.get("kdbx/imported/ambiguous").await.unwrap().unwrap();
        assert_eq!(outcome.value.expose_secret(), "a-value");
        // `get(path#name)` → explicit field selection.
        let outcome = src
            .get("kdbx/imported/ambiguous#token_b")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outcome.value.expose_secret(), "b-value");
        // Unknown field name → None.
        assert!(
            src.get("kdbx/imported/ambiguous#nope")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Expiry timestamp is suppressed when KeePass's `Expires`
    /// flag is `Some(false)` even if the file carries a
    /// non-None `expiry` value (KeePass writes a ghost
    /// timestamp into every entry on save). Without this guard
    /// every entry would look like it expires.
    // -- K13 — working-copy safety ---------------------------

    #[test]
    fn derive_working_copy_path_drops_into_source_dir_with_timestamp() {
        let p = derive_working_copy_path(std::path::Path::new("/tmp/x.kdbx"));
        let s = p.to_string_lossy();
        assert!(s.starts_with("/tmp/x.devboy-working-"), "got: {s}");
        assert!(s.ends_with(".kdbx"), "got: {s}");
    }

    #[test]
    fn derive_working_copy_path_handles_files_without_extension() {
        // Some users keep KDBX files without an extension; the
        // helper must not break on those.
        let p = derive_working_copy_path(std::path::Path::new("/tmp/passwords"));
        let s = p.to_string_lossy();
        assert!(s.contains("passwords.devboy-working-"), "got: {s}");
    }

    #[test]
    fn prepare_working_copy_copies_bytes_and_is_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src.kdbx");
        let dst = dir.path().join("dst.kdbx");
        std::fs::write(&src, b"hello-kdbx").unwrap();
        let result = prepare_working_copy(&src, &dst).unwrap();
        assert_eq!(result, dst);
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello-kdbx");
        // Second call — destination already exists; no-op.
        // Mutate dst to confirm we don't re-overwrite.
        std::fs::write(&dst, b"do-not-overwrite").unwrap();
        let result = prepare_working_copy(&src, &dst).unwrap();
        assert_eq!(result, dst);
        assert_eq!(std::fs::read(&dst).unwrap(), b"do-not-overwrite");
    }

    #[test]
    fn prepare_working_copy_errors_when_source_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("nope.kdbx");
        let dst = dir.path().join("dst.kdbx");
        let err = prepare_working_copy(&src, &dst).unwrap_err();
        assert!(matches!(err, KdbxSourceError::OpenFailed { .. }));
    }

    #[tokio::test]
    async fn expires_at_is_suppressed_when_expires_flag_is_false() {
        use chrono::NaiveDateTime;
        use keepass::{Database, DatabaseKey};
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let kdbx_path = tmp_dir.path().join("ghost.kdbx");

        let mut db = Database::new();
        {
            let mut root = db.root_mut();
            let mut entry = root.add_entry();
            entry.set_unprotected("Title", "Non-Expiring");
            entry.times.expires = Some(false);
            entry.times.expiry = Some(
                NaiveDateTime::parse_from_str("2099-12-31T23:59:59", "%Y-%m-%dT%H:%M:%S").unwrap(),
            );
        }
        let mut out = std::fs::File::create(&kdbx_path).unwrap();
        db.save(&mut out, DatabaseKey::new().with_password("p"))
            .unwrap();

        let src = KdbxSource::new("ghost", &kdbx_path);
        src.set_passphrase(SecretString::from("p")).await;
        let _ = src.is_available().await;
        let guard = src.snapshot.lock().await;
        let entry = &guard.as_ref().unwrap().entries[0];
        assert!(
            entry.expires_at.is_none(),
            "ghost expiry must not leak through, got {:?}",
            entry.expires_at
        );
    }

    // -- K21 — extract_attachment ---------------------------

    /// Build a KDBX with one entry that carries two
    /// attachments, round-trip through `extract_attachment`,
    /// assert the bytes come back verbatim + a wrong-name
    /// lookup is `None`.
    #[test]
    fn extract_attachment_returns_bytes_for_matching_name() {
        use keepass::db::Value;
        use keepass::{Database, DatabaseKey};
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let kdbx_path = tmp_dir.path().join("with_attachments.kdbx");

        let mut db = Database::new();
        let uuid_seen;
        {
            let mut root = db.root_mut();
            let mut entry = root.add_entry();
            entry.set_unprotected("Title", "TLS bundle");
            entry.add_attachment(
                "server.pem".to_owned(),
                Value::Unprotected(b"-----BEGIN CERT-----\nABCDEF\n-----END CERT-----".to_vec()),
            );
            entry.add_attachment(
                "client.key".to_owned(),
                Value::Unprotected(b"client-key-bytes".to_vec()),
            );
            uuid_seen = entry.as_ref().id().uuid().hyphenated().to_string();
        }
        let mut out = std::fs::File::create(&kdbx_path).unwrap();
        db.save(&mut out, DatabaseKey::new().with_password("p"))
            .unwrap();

        // Happy path — both attachments fetchable by name.
        let bytes = extract_attachment(
            &kdbx_path,
            &SecretString::from("p"),
            None,
            &uuid_seen,
            "server.pem",
        )
        .unwrap()
        .expect("server.pem must be present");
        assert!(bytes.starts_with(b"-----BEGIN CERT-----"));

        let bytes = extract_attachment(
            &kdbx_path,
            &SecretString::from("p"),
            None,
            &uuid_seen,
            "client.key",
        )
        .unwrap()
        .expect("client.key must be present");
        assert_eq!(bytes, b"client-key-bytes");

        // Unknown attachment on a known entry → Ok(None).
        let result = extract_attachment(
            &kdbx_path,
            &SecretString::from("p"),
            None,
            &uuid_seen,
            "nope.txt",
        )
        .unwrap();
        assert!(result.is_none());

        // Unknown entry UUID → Ok(None).
        let result = extract_attachment(
            &kdbx_path,
            &SecretString::from("p"),
            None,
            "00000000-0000-0000-0000-000000000000",
            "server.pem",
        )
        .unwrap();
        assert!(result.is_none());

        // Wrong passphrase → OpenFailed error.
        let err = extract_attachment(
            &kdbx_path,
            &SecretString::from("wrong-passphrase"),
            None,
            &uuid_seen,
            "server.pem",
        )
        .unwrap_err();
        assert!(matches!(err, KdbxSourceError::OpenFailed { .. }));
    }

    // -- K14 — edit_metadata + describe_metadata ----------------

    /// Construct a synthetic KDBX with one entry pre-populated
    /// with title/username/url/notes/tags/protected password.
    /// Returns (tmp_dir, kdbx_path, entry_uuid).
    fn make_kdbx_with_entry() -> (tempfile::TempDir, std::path::PathBuf, String) {
        use keepass::{Database, DatabaseKey};
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let kdbx_path = tmp_dir.path().join("edit.kdbx");

        let mut db = Database::new();
        let uuid;
        {
            let mut root = db.root_mut();
            let mut entry = root.add_entry();
            entry.set_unprotected("Title", "API token");
            entry.set_unprotected("UserName", "ops");
            entry.set_unprotected("URL", "https://api.example.com");
            entry.set_unprotected("Notes", "original notes");
            entry.set_protected("Password", "super-secret-password");
            entry.tags = vec!["api".into(), "prod".into()];
            uuid = entry.as_ref().id().uuid().hyphenated().to_string();
        }
        let mut out = std::fs::File::create(&kdbx_path).unwrap();
        db.save(&mut out, DatabaseKey::new().with_password("p"))
            .unwrap();
        (tmp_dir, kdbx_path, uuid)
    }

    #[test]
    fn edit_metadata_round_trips_notes_tags_and_url() {
        let (_tmp, path, uuid) = make_kdbx_with_entry();

        edit_metadata(
            &path,
            &SecretString::from("p"),
            None,
            &uuid,
            &MetadataPatch {
                notes: Some("rotation runbook: ops-wiki/rotations#42".into()),
                tags: Some(vec!["api".into(), "prod".into(), "rotated-q1".into()]),
                url: Some("https://api.example.com/v2".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let snapshot = open_kdbx_into_snapshot(&path, &SecretString::from("p"), None).unwrap();
        let entry = snapshot
            .entries
            .iter()
            .find(|e| e.uuid == uuid)
            .expect("entry must survive the edit");
        assert_eq!(
            entry.notes.as_deref(),
            Some("rotation runbook: ops-wiki/rotations#42")
        );
        assert_eq!(entry.tags, vec!["api", "prod", "rotated-q1"]);
        assert_eq!(entry.url.as_deref(), Some("https://api.example.com/v2"));
        // Password must NOT have been touched.
        let pw = entry.primary_value().expect("password slot present");
        use secrecy::ExposeSecret as _;
        assert_eq!(pw.value.expose_secret(), "super-secret-password");
    }

    #[test]
    fn edit_metadata_round_trips_expires_at_set_and_clear() {
        let (_tmp, path, uuid) = make_kdbx_with_entry();

        // Set expiry.
        edit_metadata(
            &path,
            &SecretString::from("p"),
            None,
            &uuid,
            &MetadataPatch {
                expires_at: Some(Some("2027-01-15T00:00:00Z".to_owned())),
                ..Default::default()
            },
        )
        .unwrap();
        let snap = open_kdbx_into_snapshot(&path, &SecretString::from("p"), None).unwrap();
        let entry = snap.entries.iter().find(|e| e.uuid == uuid).unwrap();
        assert!(entry.expires_at.is_some());

        // Clear expiry.
        edit_metadata(
            &path,
            &SecretString::from("p"),
            None,
            &uuid,
            &MetadataPatch {
                expires_at: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        let snap = open_kdbx_into_snapshot(&path, &SecretString::from("p"), None).unwrap();
        let entry = snap.entries.iter().find(|e| e.uuid == uuid).unwrap();
        assert_eq!(entry.expires_at, None);
    }

    #[test]
    fn edit_metadata_rejects_bad_expires_at() {
        let (_tmp, path, uuid) = make_kdbx_with_entry();
        let err = edit_metadata(
            &path,
            &SecretString::from("p"),
            None,
            &uuid,
            &MetadataPatch {
                expires_at: Some(Some("not-a-date".to_owned())),
                ..Default::default()
            },
        )
        .unwrap_err();
        let KdbxSourceError::OpenFailed { reason, .. } = err else {
            panic!("expected OpenFailed");
        };
        assert!(reason.contains("bad expires_at"), "got: {reason}");
    }

    #[test]
    fn edit_metadata_rejects_unknown_uuid() {
        let (_tmp, path, _uuid) = make_kdbx_with_entry();
        let err = edit_metadata(
            &path,
            &SecretString::from("p"),
            None,
            "00000000-0000-0000-0000-000000000000",
            &MetadataPatch {
                notes: Some("doesn't matter".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        let KdbxSourceError::OpenFailed { reason, .. } = err else {
            panic!("expected OpenFailed");
        };
        assert!(reason.contains("not found"), "got: {reason}");
    }

    #[test]
    fn describe_metadata_projects_fields_without_password() {
        let (_tmp, path, uuid) = make_kdbx_with_entry();
        let meta = describe_metadata(&path, &SecretString::from("p"), None, &uuid)
            .unwrap()
            .expect("entry present");
        assert_eq!(meta.title, "API token");
        assert_eq!(meta.username.as_deref(), Some("ops"));
        assert_eq!(meta.url.as_deref(), Some("https://api.example.com"));
        assert_eq!(meta.notes.as_deref(), Some("original notes"));
        assert_eq!(meta.tags, vec!["api", "prod"]);
        assert_eq!(meta.uuid, uuid);
        // No password / no protected secret leaks out.
        assert!(!meta.custom_string_names.iter().any(|n| n == "Password"));
    }

    #[test]
    fn describe_metadata_returns_none_for_unknown_uuid() {
        let (_tmp, path, _uuid) = make_kdbx_with_entry();
        let out = describe_metadata(
            &path,
            &SecretString::from("p"),
            None,
            "00000000-0000-0000-0000-000000000000",
        )
        .unwrap();
        assert!(out.is_none());
    }
}
