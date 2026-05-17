//! KDBX 4 (KeePass) `SecretSource` implementation.
//!
//! Opens a `.kdbx` file with a user-provided passphrase + optional
//! keyfile and reads Password fields out of the matched entries.
//!
//! See the crate README and [ADR-021] §8 for context.
//!
//! ## Scope of this revision (K1 — skeleton)
//!
//! Crate plumbing only: dependency on the `keepass` crate, public
//! type signatures, and a `SecretSource` impl that always returns
//! `NotInstalled`. The actual KDBX open + entry lookup lands in K2;
//! the UI passphrase prompt lands in K4. This revision exists so the
//! workspace builds + the new crate slot can be wired into the
//! `sources.toml` router before the heavier logic ships.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use std::path::PathBuf;

use async_trait::async_trait;
use devboy_storage::source::{Capabilities, GetOutcome, SecretSource, SourceError, SourceStatus};
use secrecy::SecretString;
use thiserror::Error;
use tokio::sync::Mutex;

/// Errors specific to the KDBX source plugin.
#[derive(Debug, Error)]
pub enum KdbxSourceError {
    /// Couldn't open / parse the on-disk `.kdbx` file. Reasons include
    /// wrong passphrase, missing keyfile, corrupt body, unsupported
    /// schema version. The underlying `keepass::error` is surfaced
    /// in the `Display` impl so doctor can show actionable context.
    #[error("could not open KDBX file at {path}: {reason}")]
    OpenFailed {
        /// Path the open was attempted against.
        path: PathBuf,
        /// Human-readable reason — typically the keepass crate's own
        /// error message.
        reason: String,
    },

    /// The reference string didn't match any entry in the database.
    /// Returned by `get` so the router can fall through to a
    /// fallback source per ADR-021 §2.
    #[error("KDBX source `{source_name}` has no entry at reference `{reference}`")]
    NoSuchEntry {
        /// Source name (matches `SecretSource::name`).
        source_name: String,
        /// The reference the router asked for.
        reference: String,
    },
}

/// `SecretSource` backed by a single KDBX 4 file.
///
/// Holds a per-source name + the on-disk path + the unlock material
/// the user supplied (passphrase + optional keyfile). The unlocked
/// `keepass::Database` handle is **not** cached here — K2's
/// implementation will gate caching behind a TTL + zeroize policy
/// inside `Mutex<Option<keepass::Database>>`. For the K1 skeleton
/// every operation just reports `NotInstalled` so the router can
/// already see the source slot without exercising the open path.
pub struct KdbxSource {
    /// Logical name from `[[source]] name = "..."` in `sources.toml`.
    name: String,
    /// Absolute path to the `.kdbx` file on disk.
    path: PathBuf,
    /// User-supplied passphrase. Wrapped in `SecretString` so it
    /// zeroizes on drop and doesn't leak into `Debug`. `None` means
    /// the UI has not collected it yet — `is_available` reports
    /// `Locked` in that case (K2).
    passphrase: Mutex<Option<SecretString>>,
    /// Optional path to a keyfile companion (KeePass two-factor
    /// unlock). `None` for passphrase-only databases.
    keyfile: Option<PathBuf>,
}

impl std::fmt::Debug for KdbxSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the passphrase (even the redacted SecretString
        // Debug wrapper carries the field name, which we want to
        // avoid in error contexts that include the source).
        f.debug_struct("KdbxSource")
            .field("name", &self.name)
            .field("path", &self.path)
            .field("passphrase", &"<unlocked-out-of-process>")
            .field("keyfile", &self.keyfile)
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
        }
    }

    /// Attach a keyfile (KeePass two-factor unlock). No-op for
    /// databases that only require a passphrase.
    pub fn with_keyfile(mut self, keyfile: impl Into<PathBuf>) -> Self {
        self.keyfile = Some(keyfile.into());
        self
    }

    /// Provide the passphrase. Replaces any previously-set value.
    /// Called by the UI unlock modal on submit (K4).
    pub async fn set_passphrase(&self, passphrase: SecretString) {
        let mut guard = self.passphrase.lock().await;
        *guard = Some(passphrase);
    }

    /// `true` when the passphrase has been supplied this session.
    /// Driven by `is_available`'s `Locked` / `Available` decision in
    /// K2; exposed here for the UI to gate its unlock modal.
    pub async fn is_unlocked(&self) -> bool {
        self.passphrase.lock().await.is_some()
    }
}

#[async_trait]
impl SecretSource for KdbxSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        // Read-only MVP. AUDIT_LOGGED is false because KDBX itself
        // does not record read access. WRITE / ROTATE flip on once
        // K6's follow-up wires the `save_kdbx4` path with
        // concurrent-write safety.
        Capabilities::READ | Capabilities::LIST | Capabilities::BIOMETRIC_PROMPT
    }

    async fn is_available(&self) -> SourceStatus {
        // K1 skeleton: report NotInstalled until the file exists +
        // the passphrase has been supplied. K2 expands this to
        // distinguish NotInstalled (file missing) / Locked (no
        // passphrase) / Available (open succeeds) / Error (wrong
        // passphrase, corrupt body).
        if !self.path.exists() {
            return SourceStatus::NotInstalled;
        }
        if !self.is_unlocked().await {
            return SourceStatus::Locked;
        }
        SourceStatus::Error("KDBX read path not implemented yet (lands in K2)".to_owned())
    }

    async fn get(&self, _reference: &str) -> Result<Option<GetOutcome>, SourceError> {
        // K1 skeleton: surface a typed error so the router treats us
        // as a non-functional source until K2 implements the lookup.
        Err(SourceError::Unavailable {
            name: self.name.clone(),
            reason: "KDBX get() not implemented yet (lands in K2)".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_report_read_list_and_biometric_prompt() {
        let src = KdbxSource::new("test", "/tmp/no-such-file.kdbx");
        let caps = src.capabilities();
        assert!(caps.contains(Capabilities::READ));
        assert!(caps.contains(Capabilities::LIST));
        assert!(caps.contains(Capabilities::BIOMETRIC_PROMPT));
        assert!(
            !caps.contains(Capabilities::WRITE),
            "K1 skeleton must not advertise WRITE before K6"
        );
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
        assert!(dbg.contains("<unlocked-out-of-process>"));
    }
}
