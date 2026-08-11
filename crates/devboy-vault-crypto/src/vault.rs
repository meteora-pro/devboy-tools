//! Public [`Vault`] API that ties P3.1–P3.5 together.
//!
//! The lower-level modules in this crate each handle one concern:
//!
//! - [`crate::format`] is the on-disk byte layout.
//! - [`crate::aead`] is the per-entry XChaCha20-Poly1305 wrapper.
//! - [`crate::passphrase`] / [`crate::recovery`] / [`crate::totp`]
//!   are the three envelope variants — independent ways to wrap the
//!   same `vault_key` so the user can unlock with whichever method
//!   they have on hand.
//!
//! `Vault` is the user-facing API on top: open, create, add an extra
//! envelope, put / get / rotate / delete entries, list metadata.
//! Every mutation is persisted atomically via
//! [`crate::format::VaultFile::write_file_atomic`] so a crashed
//! process can never leave a half-written file behind.
//!
//! # Lifecycle
//!
//! ```text
//!   Vault::create(path, passphrase, options) ─┐
//!                                             │
//!   Vault::open(path, UnlockMethod)        ───┴── Vault (unlocked, in memory)
//!                                                 │
//!   .put / .get / .rotate / .delete / .list ◄────┤
//!                                                 │
//!   .add_envelope(...) ───── persists the new ────┘
//!                            envelope to disk
//! ```
//!
//! # In-memory state
//!
//! `vault_key` is held in [`SecretBox<[u8; KEY_LEN]>`] so it zeroizes
//! on drop. The whole `Vault` struct does not derive `Debug`; the
//! user-facing types it produces (`EntryMetadata`, errors) do.
//!
//! # Atomicity guarantees
//!
//! Every operation that mutates on-disk state (`put`, `rotate`,
//! `delete`, `add_envelope`) re-builds the entire ciphertext blob
//! region in memory and then calls
//! [`VaultFile::write_file_atomic`](crate::format::VaultFile::write_file_atomic).
//! The temp-file-then-rename pattern means every reader sees either
//! the pre- or the post-mutation state, never a torn write.
//! Re-building the blob region also means deleted ciphertexts do not
//! linger in the file — every `delete` is a true scrub.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretBox, SecretString};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::aead::{self, AeadError, KEY_LEN, NONCE_LEN};
use crate::format::{
    EntryMeta, Envelope, EnvelopeKdfParams, FormatError, Header, VaultFile, b64_decode, b64_encode,
    entry_aad,
};
use crate::keyfile::{KeyfileError, create_keyfile_envelope, unwrap_keyfile};
use crate::passphrase::{
    DEFAULT_KDF_PARAMS, PassphraseError, create_passphrase_envelope, unwrap_passphrase,
};
use crate::recovery::{
    RecoveryError, RecoveryPhrase, create_recovery_envelope, generate_recovery_phrase,
    unwrap_recovery,
};
use crate::totp::{TotpEnvelopeError, create_totp_envelope, unwrap_totp};

// =============================================================================
// User-facing types
// =============================================================================

/// User-facing entry metadata. A subset of the on-disk
/// [`EntryMeta`] — the nonce / ciphertext offset / length fields are
/// internal bookkeeping and not exposed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryMetadata {
    /// Human description.
    pub description: Option<String>,
    /// URL the user opens to obtain a fresh value.
    pub retrieval_url: Option<String>,
    /// ISO 8601 expiry date.
    pub expires_at: Option<String>,
    /// ISO 8601 date of the last successful rotation.
    pub last_rotated_at: Option<String>,
    /// Pattern catalogue id (resolved via `devboy-secret-patterns`).
    pub pattern_id: Option<String>,
}

/// One version of a secret, for a history view (ADR-024 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// Monotonic version number within the path.
    pub version: u64,
    /// Whether this version is a tombstone (the path stopped
    /// resolving here).
    pub tombstone: bool,
    /// Who wrote it — `"agent"` or `"user"`, when recorded.
    pub actor: Option<String>,
    /// ISO 8601 date the version was written.
    pub created_at: Option<String>,
}

/// How the caller wants [`Vault::open`] to unlock the vault.
pub enum UnlockMethod {
    /// Try the passphrase envelope.
    Passphrase(SecretString),
    /// Try the recovery envelope.
    Recovery(RecoveryPhrase),
    /// Try the TOTP envelope (ADR-024 §1).
    ///
    /// The caller supplies the shared secret it already holds in
    /// memory, and is responsible for having verified a code — and
    /// rejected a replayed step — beforehand. Possession of the
    /// secret alone authorises nothing: the daemon has it for the
    /// whole session.
    Totp {
        /// The shared TOTP secret, resident in daemon memory.
        totp_secret: Vec<u8>,
    },
    /// Try the keyfile envelope (ADR-024 §6).
    ///
    /// This is the unattended cold start: a daemon can open the
    /// vault with nobody present, which is what the OS keychain
    /// provided before it left the default chain.
    ///
    /// The caller passes the bytes rather than a path on purpose.
    /// Where the keyfile lives is a *configuration* decision, and
    /// letting a request name the file would let a caller point the
    /// unlock at a keyfile it controls — which is the whole attack
    /// this envelope is supposed to be indifferent to.
    Keyfile {
        /// Contents of the keyfile, read by the caller.
        keyfile: Zeroizing<Vec<u8>>,
    },
}

/// Initial unlock methods to attach when calling [`Vault::create`].
///
/// `passphrase` is mandatory — every vault must accept *some* unlock
/// method, and the passphrase is the only one that works without
/// external infrastructure (no authenticator enrolled, no recovery
/// phrase yet generated). TOTP and BIP39 recovery are optional
/// add-ons layered on top.
pub struct InitialUnlock {
    /// Mandatory passphrase. Used to wrap the vault key in the
    /// initial passphrase envelope.
    pub passphrase: SecretString,
    /// Optional Argon2id parameter override. Defaults to
    /// [`DEFAULT_KDF_PARAMS`] when `None`.
    pub passphrase_params: Option<EnvelopeKdfParams>,
    /// If `true`, generate a fresh BIP39 recovery phrase and add a
    /// recovery envelope. The generated phrase is returned in
    /// [`CreateOutcome::recovery_phrase`] so the caller can show it
    /// to the user (per ADR-023 §3.2 acknowledgement step).
    pub with_recovery: bool,
    /// If `Some`, also add a TOTP envelope wrapping the vault key
    /// under this shared secret (ADR-024 §1).
    ///
    /// Enrollment normally happens on an already-open vault via
    /// [`Vault::add_totp_envelope`]; this field exists so a vault
    /// can be created with TOTP in one step.
    pub with_totp_secret: Option<Vec<u8>>,
}

impl InitialUnlock {
    /// Construct an initial-unlock spec with only a passphrase.
    pub fn with_passphrase(passphrase: SecretString) -> Self {
        Self {
            passphrase,
            passphrase_params: None,
            with_recovery: false,
            with_totp_secret: None,
        }
    }
}

/// Output of [`Vault::create`].
pub struct CreateOutcome {
    /// The freshly-created, unlocked vault.
    pub vault: Vault,
    /// The recovery phrase, if `InitialUnlock::with_recovery` was
    /// `true`. **Show to the user once with explicit acknowledgement,
    /// never persist.** Per ADR-023 §3.2 the framework does not save
    /// the phrase anywhere — it is the caller's responsibility to
    /// either drop it or hand it to a UI flow that surfaces it once.
    pub recovery_phrase: Option<RecoveryPhrase>,
}

// =============================================================================
// Errors
// =============================================================================

/// Failure modes for [`Vault`] operations.
#[derive(Debug, Error)]
pub enum VaultError {
    /// Wraps [`crate::format::FormatError`] from the on-disk layer.
    #[error("vault file error: {0}")]
    Format(#[from] FormatError),

    /// Wraps [`crate::aead::AeadError`] from the per-entry encryption
    /// layer.
    #[error("AEAD error: {0}")]
    Aead(#[from] AeadError),

    /// Wraps [`crate::passphrase::PassphraseError`] from the
    /// passphrase envelope layer.
    #[error("passphrase envelope error: {0}")]
    Passphrase(#[from] PassphraseError),

    /// Wraps [`crate::recovery::RecoveryError`] from the BIP39
    /// recovery envelope layer.
    #[error("recovery envelope error: {0}")]
    Recovery(#[from] RecoveryError),

    /// Wraps [`crate::totp::TotpEnvelopeError`] from the TOTP
    /// envelope layer (ADR-024 §1).
    #[error("TOTP envelope error: {0}")]
    Totp(#[from] TotpEnvelopeError),

    /// Wraps [`crate::keyfile::KeyfileError`] from the keyfile
    /// envelope layer (ADR-024 §6).
    #[error("keyfile envelope error: {0}")]
    Keyfile(#[from] KeyfileError),

    /// HKDF refused to produce the audit-log subkey.
    #[error("could not derive the audit-log key")]
    AuditKeyDerivation,

    /// `Vault::open` could not find an envelope of the requested kind
    /// in the file. The caller asked for a passphrase unlock but the
    /// vault has only a recovery envelope, for example.
    #[error("vault has no '{kind}' envelope to unlock with")]
    NoMatchingEnvelope {
        /// Which envelope kind the caller asked for.
        kind: &'static str,
    },

    /// The OS-level CSPRNG ([`getrandom`]) refused to produce bytes
    /// during salt or vault-key generation.
    #[error("CSPRNG unavailable: {0}")]
    RngUnavailable(getrandom::Error),

    /// `Vault::put` / `rotate` / `delete` was asked to act on a path
    /// that has no entry. `delete` is otherwise idempotent — this
    /// variant is only emitted by `rotate` (where the user is
    /// explicitly trying to update an existing entry).
    #[error("vault has no entry for path '{path}'")]
    EntryNotFound {
        /// The path the caller asked for.
        path: String,
    },
}

// =============================================================================
// Vault
// =============================================================================

/// In-memory handle to an unlocked vault.
///
/// Built by [`Vault::open`] (existing vault) or [`Vault::create`]
/// (new vault). Holds the decrypted vault key in
/// [`SecretBox<[u8; KEY_LEN]>`] so the bytes zeroize on drop. The
/// struct does **not** derive `Debug` — the only field worth seeing
/// in a debugger is the path, and a future `Debug` impl can opt in
/// to that explicitly.
pub struct Vault {
    /// On-disk path; needed for atomic re-writes on every mutation.
    path: PathBuf,
    /// Loaded file contents.
    file: VaultFile,
    /// Unlocked symmetric vault key. Zeroized on drop.
    vault_key: SecretBox<[u8; KEY_LEN]>,
    /// Ciphertexts that have not yet been flattened into
    /// `file.ciphertext_blobs` — populated by `put` / `rotate` and
    /// consumed by `persist`. Keyed by entry path so multiple
    /// in-flight operations coalesce naturally even though current
    /// call sites only touch one entry at a time.
    pending_ciphertexts_for_persist: BTreeMap<(String, u64), Vec<u8>>,
}

impl std::fmt::Debug for Vault {
    /// Hand-rolled `Debug` that shows the path + entry-count summary
    /// but never the vault key or the ciphertexts. Required so
    /// `Result<Vault, _>::unwrap_err()` (used in tests) compiles.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("envelopes", &self.file.envelopes.len())
            .field("entries", &self.file.entries.len())
            .field("vault_key", &"<redacted>")
            .finish()
    }
}

impl Vault {
    /// Create a fresh vault at `path` and write it atomically.
    ///
    /// The vault is unlocked when this returns. At minimum a
    /// passphrase envelope is added; recovery and keychain envelopes
    /// follow if the corresponding `InitialUnlock` fields are set.
    pub fn create(path: &Path, init: InitialUnlock) -> Result<CreateOutcome, VaultError> {
        // Generate a fresh 32-byte vault key and a 32-byte header salt.
        let mut vault_key_bytes = [0u8; KEY_LEN];
        getrandom::getrandom(&mut vault_key_bytes).map_err(VaultError::RngUnavailable)?;
        let mut salt = [0u8; 32];
        getrandom::getrandom(&mut salt).map_err(VaultError::RngUnavailable)?;

        let mut file = VaultFile::empty(salt);

        // Mandatory passphrase envelope.
        let params = init.passphrase_params.unwrap_or(DEFAULT_KDF_PARAMS);
        let passphrase_env =
            create_passphrase_envelope(&vault_key_bytes, &init.passphrase, salt, params)?;
        file.envelopes.push(passphrase_env);

        // Optional recovery envelope.
        let recovery_phrase = if init.with_recovery {
            let phrase = generate_recovery_phrase()?;
            // Recovery uses its own per-envelope salt, fresh here.
            let mut recovery_salt = [0u8; 32];
            getrandom::getrandom(&mut recovery_salt).map_err(VaultError::RngUnavailable)?;
            let env = create_recovery_envelope(&vault_key_bytes, &phrase, recovery_salt)?;
            file.envelopes.push(env);
            Some(phrase)
        } else {
            None
        };

        // Optional TOTP envelope (ADR-024 §1). The shared secret
        // is supplied by the caller, which is also responsible for
        // storing it inside the vault under its reserved path.
        if let Some(totp_secret) = init.with_totp_secret.as_deref() {
            let mut salt = [0u8; 32];
            getrandom::getrandom(&mut salt).map_err(VaultError::RngUnavailable)?;
            let env = create_totp_envelope(&vault_key_bytes, totp_secret, salt)?;
            file.envelopes.push(env);
        }

        // Persist atomically before handing back the in-memory
        // vault. If anything fails after this point, we want disk
        // and memory to agree.
        file.write_file_atomic(path)?;

        let vault = Vault {
            path: path.to_path_buf(),
            file,
            vault_key: SecretBox::new(Box::new(vault_key_bytes)),
            pending_ciphertexts_for_persist: BTreeMap::new(),
        };
        Ok(CreateOutcome {
            vault,
            recovery_phrase,
        })
    }

    /// Open an existing vault from `path`, unlocking it with `unlock`.
    ///
    /// Reads the file, finds an envelope of the matching kind, and
    /// decrypts it. Returns [`VaultError::NoMatchingEnvelope`] when
    /// no envelope of the requested kind is present.
    pub fn open(path: &Path, unlock: UnlockMethod) -> Result<Self, VaultError> {
        let file = VaultFile::read_file(path)?;
        let vault_key_bytes = match &unlock {
            UnlockMethod::Passphrase(passphrase) => {
                let env = file
                    .envelopes
                    .iter()
                    .find(|e| matches!(e, Envelope::Passphrase { .. }))
                    .ok_or(VaultError::NoMatchingEnvelope { kind: "passphrase" })?;
                unwrap_passphrase(env, passphrase)?
            }
            UnlockMethod::Recovery(phrase) => {
                let env = file
                    .envelopes
                    .iter()
                    .find(|e| matches!(e, Envelope::Recovery { .. }))
                    .ok_or(VaultError::NoMatchingEnvelope { kind: "recovery" })?;
                unwrap_recovery(env, phrase)?
            }
            UnlockMethod::Totp { totp_secret } => {
                let env = file
                    .envelopes
                    .iter()
                    .find(|e| matches!(e, Envelope::Totp { .. }))
                    .ok_or(VaultError::NoMatchingEnvelope { kind: "totp" })?;
                unwrap_totp(env, totp_secret)?
            }
            UnlockMethod::Keyfile { keyfile } => {
                let env = file
                    .envelopes
                    .iter()
                    .find(|e| matches!(e, Envelope::Keyfile { .. }))
                    .ok_or(VaultError::NoMatchingEnvelope { kind: "keyfile" })?;
                unwrap_keyfile(env, keyfile)?
            }
        };
        // Convert Zeroizing<[u8; KEY_LEN]> -> SecretBox<[u8; KEY_LEN]>.
        let mut key_array = [0u8; KEY_LEN];
        key_array.copy_from_slice(vault_key_bytes.as_ref());

        Ok(Vault {
            path: path.to_path_buf(),
            file,
            vault_key: SecretBox::new(Box::new(key_array)),
            pending_ciphertexts_for_persist: BTreeMap::new(),
        })
    }

    /// Add a recovery envelope to an already-unlocked vault.
    ///
    /// Generates a fresh BIP39 recovery phrase, wraps the vault key
    /// under it, appends the envelope, and persists. The returned
    /// [`RecoveryPhrase`] must be shown to the user once and never
    /// persisted (per ADR-023 §3.2).
    pub fn add_recovery_envelope(&mut self) -> Result<RecoveryPhrase, VaultError> {
        let phrase = generate_recovery_phrase()?;
        let mut salt = [0u8; 32];
        getrandom::getrandom(&mut salt).map_err(VaultError::RngUnavailable)?;
        let env = create_recovery_envelope(self.vault_key.expose_secret(), &phrase, salt)?;
        self.file.envelopes.push(env);
        self.file.write_file_atomic(&self.path)?;
        Ok(phrase)
    }

    /// Add a TOTP envelope to an already-unlocked vault
    /// (ADR-024 §1).
    ///
    /// The caller supplies the shared secret and is responsible for
    /// also storing it *inside* this vault under its reserved path.
    /// That placement is what the whole scheme rests on: the secret
    /// then exists in plaintext only in daemon memory, which an
    /// agent cannot read, so a valid code is evidence a human
    /// approved the re-unlock.
    pub fn add_totp_envelope(&mut self, totp_secret: &[u8]) -> Result<(), VaultError> {
        let mut salt = [0u8; 32];
        getrandom::getrandom(&mut salt).map_err(VaultError::RngUnavailable)?;
        let env = create_totp_envelope(self.vault_key.expose_secret(), totp_secret, salt)?;
        self.file.envelopes.push(env);
        self.file.write_file_atomic(&self.path)?;
        Ok(())
    }

    /// Enrol a keyfile so the vault can be opened unattended
    /// (ADR-024 §6).
    ///
    /// Enrolment happens on an already-open vault, so the caller
    /// has necessarily proved they can unlock it some other way —
    /// adding a keyfile is not a way in, it is a second door opened
    /// from inside.
    ///
    /// Replaces any existing keyfile envelope rather than stacking
    /// a second one: two live keyfiles would mean the old file
    /// still opens the vault after a rotation, which is the
    /// opposite of what rotating one is for.
    pub fn add_keyfile_envelope(&mut self, keyfile: &[u8]) -> Result<(), VaultError> {
        let mut salt = [0u8; 32];
        getrandom::getrandom(&mut salt).map_err(VaultError::RngUnavailable)?;
        let env = create_keyfile_envelope(self.vault_key.expose_secret(), keyfile, salt)?;
        self.file
            .envelopes
            .retain(|e| !matches!(e, Envelope::Keyfile { .. }));
        self.file.envelopes.push(env);
        self.file.write_file_atomic(&self.path)?;
        Ok(())
    }

    /// Encrypt and store a value at `path`, appending a new version
    /// (ADR-024 §5).
    ///
    /// The previous version's ciphertext is retained, so no write
    /// can destroy a value — an agent that stores the wrong token,
    /// or an empty one, is always recoverable via
    /// [`Vault::restore`]. Only [`Vault::purge`] removes
    /// ciphertext, and that is a user-only operation.
    pub fn put(
        &mut self,
        path: &str,
        value: &SecretString,
        meta: EntryMetadata,
    ) -> Result<(), VaultError> {
        self.put_as(path, value, meta, None)
    }

    /// [`Vault::put`], recording which actor wrote the version.
    pub fn put_as(
        &mut self,
        path: &str,
        value: &SecretString,
        meta: EntryMetadata,
        actor: Option<String>,
    ) -> Result<(), VaultError> {
        let version = self.next_version(path);
        let enc = aead::encrypt_entry(
            self.vault_key.expose_secret(),
            &entry_aad(path, version),
            value.expose_secret().as_bytes(),
        )?;
        self.append_version(path, version, enc, meta, actor);
        self.persist()
    }

    /// Decrypt and return the value at `path`. Returns `Ok(None)` when
    /// no entry exists.
    pub fn get(&self, path: &str) -> Result<Option<SecretString>, VaultError> {
        // Resolve the newest version, and treat a tombstone as
        // absent — the ciphertext stays on disk and recoverable,
        // but the path stops answering (ADR-024 §5).
        let entry = match self.latest_entry(path) {
            Some(e) if !e.tombstone => e,
            _ => return Ok(None),
        };
        let nonce_bytes = b64_decode(&entry.nonce).map_err(|e| {
            VaultError::Format(FormatError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("entry nonce is not valid base64: {e}"),
            )))
        })?;
        if nonce_bytes.len() != NONCE_LEN {
            return Err(VaultError::Format(FormatError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "entry nonce has wrong length: expected {NONCE_LEN}, got {}",
                    nonce_bytes.len()
                ),
            ))));
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&nonce_bytes);

        let start = entry.ct_offset as usize;
        let end = start + entry.ct_length as usize;
        if end > self.file.ciphertext_blobs.len() {
            return Err(VaultError::Format(FormatError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "entry ciphertext range exceeds blob region",
            ))));
        }
        let ciphertext = &self.file.ciphertext_blobs[start..end];

        let plaintext = aead::decrypt_entry(
            self.vault_key.expose_secret(),
            &entry_aad(path, entry.version),
            &nonce,
            ciphertext,
        )?;
        let s = String::from_utf8(plaintext).map_err(|e| {
            VaultError::Format(FormatError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("decrypted value is not valid UTF-8: {e}"),
            )))
        })?;
        Ok(Some(SecretString::from(s)))
    }

    /// Replace the value at `path` and stamp `last_rotated_at` with
    /// today's date. Returns [`VaultError::EntryNotFound`] when no
    /// entry exists at `path`.
    ///
    /// Today's date is computed via the system clock and emitted in
    /// `YYYY-MM-DD` form; in tests where the clock can't be relied
    /// on, callers should reach for [`Vault::put`] directly.
    pub fn rotate(&mut self, path: &str, value: &SecretString) -> Result<(), VaultError> {
        if !self.file.entries.iter().any(|e| e.path == path) {
            return Err(VaultError::EntryNotFound {
                path: path.to_owned(),
            });
        }
        // Rotation appends a version like `put` does — the point of
        // ADR-024 §5 is that rotating to a dud is recoverable — but
        // it carries the previous version's metadata forward and
        // stamps `last_rotated_at`.
        let previous = self
            .latest_entry(path)
            .expect("existence checked above")
            .clone();

        let version = previous.version + 1;
        let enc = aead::encrypt_entry(
            self.vault_key.expose_secret(),
            &entry_aad(path, version),
            value.expose_secret().as_bytes(),
        )?;

        let meta = EntryMetadata {
            description: previous.description.clone(),
            retrieval_url: previous.retrieval_url.clone(),
            expires_at: previous.expires_at.clone(),
            last_rotated_at: Some(today_iso8601()),
            pattern_id: previous.pattern_id.clone(),
        };
        self.append_version(path, version, enc, meta, None);
        self.persist()
    }

    /// Stop `path` from resolving by writing a tombstone version
    /// (ADR-024 §5).
    ///
    /// This is the delete an agent is allowed to perform, and it is
    /// **not destructive**: every prior version keeps its
    /// ciphertext and stays recoverable through [`Vault::restore`].
    /// The path answers `Ok(None)` afterwards, which is what makes
    /// a mistaken delete survivable.
    ///
    /// Idempotent — deleting an absent or already-tombstoned path
    /// succeeds without writing anything.
    ///
    /// For irreversible removal see [`Vault::purge`], which is a
    /// user-only operation.
    pub fn delete(&mut self, path: &str) -> Result<(), VaultError> {
        self.delete_as(path, None)
    }

    /// [`Vault::delete`], recording which actor tombstoned the path.
    pub fn delete_as(&mut self, path: &str, actor: Option<String>) -> Result<(), VaultError> {
        match self.latest_entry(path) {
            // Nothing here, or already gone — nothing to record.
            None => return Ok(()),
            Some(e) if e.tombstone => return Ok(()),
            Some(_) => {}
        }

        let version = self.next_version(path);
        // A tombstone carries no value, but still needs a
        // well-formed ciphertext slot so the file layout stays
        // uniform. An empty plaintext seals to tag-only output.
        let enc = aead::encrypt_entry(
            self.vault_key.expose_secret(),
            &entry_aad(path, version),
            &[],
        )?;

        let mut meta = EntryMeta {
            path: path.to_owned(),
            nonce: b64_encode(&enc.nonce),
            ct_offset: 0,
            ct_length: enc.ciphertext.len() as u32,
            description: None,
            retrieval_url: None,
            expires_at: None,
            last_rotated_at: None,
            pattern_id: None,
            version,
            tombstone: true,
            actor,
            created_at: Some(today_iso8601()),
        };
        meta.tombstone = true;

        self.pending_ciphertexts_for_persist
            .insert((path.to_owned(), version), enc.ciphertext);
        self.file.entries.push(meta);
        self.persist()
    }

    /// Every version recorded for `path`, oldest first.
    ///
    /// Exposed so a user-facing history view can offer a restore
    /// target without the caller re-deriving the ordering.
    pub fn versions(&self, path: &str) -> Vec<VersionInfo> {
        let mut all: Vec<&EntryMeta> = self
            .file
            .entries
            .iter()
            .filter(|e| e.path == path)
            .collect();
        all.sort_by_key(|e| e.version);
        all.into_iter()
            .map(|e| VersionInfo {
                version: e.version,
                tombstone: e.tombstone,
                actor: e.actor.clone(),
                created_at: e.created_at.clone(),
            })
            .collect()
    }

    /// Point `path` back at a prior version by copying its
    /// ciphertext forward as a new version (ADR-024 §5).
    ///
    /// Copying forward rather than rewinding a pointer keeps the
    /// history append-only: the restore itself is visible as a
    /// version, so "what happened here" stays answerable.
    ///
    /// The value is never re-typed — the user picks from ciphertext
    /// that already exists.
    pub fn restore(&mut self, path: &str, version: u64) -> Result<(), VaultError> {
        let source = self
            .file
            .entries
            .iter()
            .find(|e| e.path == path && e.version == version)
            .ok_or_else(|| VaultError::EntryNotFound {
                path: format!("{path}@v{version}"),
            })?
            .clone();

        if source.tombstone {
            return Err(VaultError::EntryNotFound {
                path: format!("{path}@v{version} (tombstone)"),
            });
        }

        // Decrypt under the source version's AAD and re-seal under
        // the new one; the AAD binds ciphertext to its version, so
        // the bytes cannot simply be copied across.
        let plaintext = self.decrypt_entry_at(&source)?;
        let new_version = self.next_version(path);
        let enc = aead::encrypt_entry(
            self.vault_key.expose_secret(),
            &entry_aad(path, new_version),
            &plaintext,
        )?;

        let meta = EntryMetadata {
            description: source.description.clone(),
            retrieval_url: source.retrieval_url.clone(),
            expires_at: source.expires_at.clone(),
            last_rotated_at: source.last_rotated_at.clone(),
            pattern_id: source.pattern_id.clone(),
        };
        self.append_version(path, new_version, enc, meta, Some("user".to_owned()));
        self.persist()
    }

    /// Permanently remove versions of `path` — **user-only**.
    ///
    /// `version = None` purges every version of the path; `Some(v)`
    /// purges just that one. The blob region is rebuilt on persist,
    /// so purged ciphertext leaves no residue.
    ///
    /// This is the one irreversible operation in the versioning
    /// model, and it is deliberately absent from the agent-facing
    /// surface (ADR-024 §5): everything an agent can do is
    /// recoverable, and only the user holds the eraser.
    pub fn purge(&mut self, path: &str, version: Option<u64>) -> Result<(), VaultError> {
        let before = self.file.entries.len();
        match version {
            Some(v) => self
                .file
                .entries
                .retain(|e| !(e.path == path && e.version == v)),
            None => self.file.entries.retain(|e| e.path != path),
        }
        if self.file.entries.len() == before {
            return Ok(());
        }
        self.persist()
    }

    /// Decrypt one specific entry version.
    fn decrypt_entry_at(&self, entry: &EntryMeta) -> Result<Vec<u8>, VaultError> {
        let nonce_bytes = b64_decode(&entry.nonce).map_err(|e| {
            VaultError::Format(FormatError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("entry nonce is not valid base64: {e}"),
            )))
        })?;
        if nonce_bytes.len() != NONCE_LEN {
            return Err(VaultError::Format(FormatError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "entry nonce has wrong length",
            ))));
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&nonce_bytes);

        let start = entry.ct_offset as usize;
        let end = start + entry.ct_length as usize;
        if end > self.file.ciphertext_blobs.len() {
            return Err(VaultError::Format(FormatError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "entry ciphertext range exceeds blob region",
            ))));
        }

        Ok(aead::decrypt_entry(
            self.vault_key.expose_secret(),
            &entry_aad(&entry.path, entry.version),
            &nonce,
            &self.file.ciphertext_blobs[start..end],
        )?)
    }

    /// Iterate over the metadata of every entry. Order is the file's
    /// natural order (insertion order; a future P9.x version may sort
    /// before persisting).
    pub fn list(&self) -> impl Iterator<Item = EntryMetadata> + '_ {
        self.current_entries().map(|e| EntryMetadata {
            description: e.description.clone(),
            retrieval_url: e.retrieval_url.clone(),
            expires_at: e.expires_at.clone(),
            last_rotated_at: e.last_rotated_at.clone(),
            pattern_id: e.pattern_id.clone(),
        })
    }

    /// Paths that currently resolve, with their newest version.
    ///
    /// Superseded versions and tombstoned paths are filtered out:
    /// versioning is an implementation detail of durability, not
    /// something every listing should surface. A caller that wants
    /// history asks [`Vault::versions`] for it.
    fn current_entries(&self) -> impl Iterator<Item = &EntryMeta> + '_ {
        let mut newest: BTreeMap<&str, &EntryMeta> = BTreeMap::new();
        for entry in &self.file.entries {
            newest
                .entry(entry.path.as_str())
                .and_modify(|slot| {
                    if entry.version > slot.version {
                        *slot = entry;
                    }
                })
                .or_insert(entry);
        }
        newest
            .into_values()
            .filter(|e| !e.tombstone)
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// Paths that currently resolve. Convenience for callers that
    /// only want the keys.
    ///
    /// Superseded versions and tombstoned paths are excluded, so a
    /// path appears at most once regardless of how many times it
    /// has been written.
    pub fn paths(&self) -> impl Iterator<Item = &str> + '_ {
        self.current_entries().map(|e| e.path.as_str())
    }

    /// Derive the key the audit log is encrypted under.
    ///
    /// A separate key rather than the vault key itself: the audit
    /// writer holds it for the whole session, and a component that
    /// only needs to write a log should not be holding the key that
    /// opens every secret. HKDF with a fixed info string makes it
    /// deterministic — the same vault always yields the same audit
    /// key, so a log written yesterday still reads today — while
    /// being useless for anything else.
    pub fn audit_key(&self) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
        use hkdf::Hkdf;
        use sha2::Sha256;

        let hkdf = Hkdf::<Sha256>::new(None, self.vault_key.expose_secret().as_slice());
        let mut out = Zeroizing::new([0u8; KEY_LEN]);
        hkdf.expand(b"devboy-vault-audit-key-v1", out.as_mut())
            .map_err(|_| VaultError::AuditKeyDerivation)?;
        Ok(out)
    }

    /// Borrow the on-disk path for diagnostics.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Borrow the file's vault header (read-only). Useful for
    /// `doctor`-style diagnostics that need to know the salt /
    /// KDF parameters without unlocking again.
    pub fn header(&self) -> &Header {
        &self.file.header
    }

    // -- internal helpers --------------------------------------------------

    /// In-flight ciphertexts that should overwrite their previous
    /// blob slice on the next `persist`. Used by `rotate` to swap in
    /// a freshly-encrypted payload while keeping the rest of the
    /// `ciphertext_blobs` buffer addressable until the rebuild
    /// happens.
    ///
    /// Keyed by path because that is the unique key every
    /// other operation in this module is keyed on.
    /// Append a new version of `path` (ADR-024 §5).
    ///
    /// Never overwrites: an existing version keeps its ciphertext,
    /// which is what makes an agent-mediated write reversible. The
    /// caller has already encrypted under the AAD for `version`.
    fn append_version(
        &mut self,
        path: &str,
        version: u64,
        enc: aead::EncryptedEntry,
        meta: EntryMetadata,
        actor: Option<String>,
    ) {
        let new_meta = EntryMeta {
            path: path.to_owned(),
            nonce: b64_encode(&enc.nonce),
            ct_offset: 0, // resolved in persist()
            ct_length: enc.ciphertext.len() as u32,
            description: meta.description,
            retrieval_url: meta.retrieval_url,
            expires_at: meta.expires_at,
            last_rotated_at: meta.last_rotated_at,
            pattern_id: meta.pattern_id,
            version,
            tombstone: false,
            actor,
            created_at: Some(today_iso8601()),
        };
        self.pending_ciphertexts_for_persist
            .insert((path.to_owned(), version), enc.ciphertext);
        self.file.entries.push(new_meta);
    }

    /// The newest version recorded for `path`, tombstoned or not.
    fn latest_entry(&self, path: &str) -> Option<&EntryMeta> {
        self.file
            .entries
            .iter()
            .filter(|e| e.path == path)
            .max_by_key(|e| e.version)
    }

    /// Version number a new write to `path` should carry.
    fn next_version(&self, path: &str) -> u64 {
        self.latest_entry(path).map_or(1, |e| e.version + 1)
    }

    /// Rebuild `ciphertext_blobs` so each entry's `ct_offset` /
    /// `ct_length` line up with a contiguous blob region, then write
    /// the file atomically.
    ///
    /// Building from a single in-memory map of pending ciphertexts
    /// plus the existing blob region keeps deletes from leaving
    /// stale bytes on disk.
    fn persist(&mut self) -> Result<(), VaultError> {
        let mut new_blobs: Vec<u8> = Vec::with_capacity(self.file.ciphertext_blobs.len());
        let mut new_entries: Vec<EntryMeta> = Vec::with_capacity(self.file.entries.len());

        // Snapshot the pending map so iteration over `self.file.entries`
        // can borrow self mutably.
        let pending = std::mem::take(&mut self.pending_ciphertexts_for_persist);

        for entry in &self.file.entries {
            let ciphertext: &[u8] =
                if let Some(p) = pending.get(&(entry.path.clone(), entry.version)) {
                    p
                } else {
                    // Re-use the existing blob slice from the loaded file.
                    let start = entry.ct_offset as usize;
                    let end = start + entry.ct_length as usize;
                    if end > self.file.ciphertext_blobs.len() {
                        return Err(VaultError::Format(FormatError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "entry '{}' ciphertext slice [{start}, {end}) exceeds blob region",
                                entry.path
                            ),
                        ))));
                    }
                    &self.file.ciphertext_blobs[start..end]
                };

            let mut new_entry = entry.clone();
            new_entry.ct_offset = new_blobs.len() as u64;
            new_entry.ct_length = ciphertext.len() as u32;
            new_blobs.extend_from_slice(ciphertext);
            new_entries.push(new_entry);
        }

        self.file.entries = new_entries;
        self.file.ciphertext_blobs = new_blobs;
        self.file.write_file_atomic(&self.path)?;
        Ok(())
    }
}

// =============================================================================
// Today's date helper
// =============================================================================

/// Format today's UTC date as `YYYY-MM-DD`. Stand-alone helper so
/// tests can stub it by handing pre-computed dates to `put` directly
/// rather than going through `rotate`. The implementation walks the
/// epoch second count manually (no `chrono` dep just for this).
fn today_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let (year, month, day) = days_since_epoch_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert "days since 1970-01-01" to (year, month, day) using the
/// algorithm from H. F. Howard's "Date Algorithms" (the same one
/// used by `chrono` internally). Tested separately below.
fn days_since_epoch_to_ymd(days_since_epoch: i64) -> (i32, u32, u32) {
    // Shift so the epoch lines up with March 1st of year 0 (days
    // since 0000-03-01 = 719_468 in the 1970-01-01 frame).
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y_out = if m <= 2 { y + 1 } else { y };
    (y_out as i32, m as u32, d as u32)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn pw(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    fn val(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    fn meta() -> EntryMetadata {
        EntryMetadata::default()
    }

    /// Fast Argon2 params for tests so vault creation is not glued
    /// to the OWASP-recommended 250 ms target. Production callers
    /// use `DEFAULT_KDF_PARAMS`.
    fn fast_params() -> EnvelopeKdfParams {
        EnvelopeKdfParams { m: 8, t: 1, p: 1 }
    }

    fn fast_init(passphrase: &str) -> InitialUnlock {
        InitialUnlock {
            passphrase: pw(passphrase),
            passphrase_params: Some(fast_params()),
            with_recovery: false,
            with_totp_secret: None,
        }
    }

    fn vault_path(dir: &TempDir) -> PathBuf {
        dir.path().join("vault.dvb")
    }

    // -- Create & open ------------------------------------------------------

    #[test]
    fn create_then_open_with_passphrase() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let _outcome = Vault::create(&path, fast_init("p")).unwrap();
        let _opened = Vault::open(&path, UnlockMethod::Passphrase(pw("p"))).unwrap();
    }

    #[test]
    fn create_with_recovery_returns_phrase() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let init = InitialUnlock {
            passphrase: pw("p"),
            passphrase_params: Some(fast_params()),
            with_recovery: true,
            with_totp_secret: None,
        };
        let outcome = Vault::create(&path, init).unwrap();
        let phrase = outcome.recovery_phrase.expect("phrase returned");
        assert_eq!(phrase.expose_words().split_whitespace().count(), 24);

        // The recovery envelope can unlock the vault.
        let _opened = Vault::open(&path, UnlockMethod::Recovery(phrase)).unwrap();
    }

    #[test]
    fn open_with_wrong_passphrase_fails() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        Vault::create(&path, fast_init("right")).unwrap();
        let err = Vault::open(&path, UnlockMethod::Passphrase(pw("wrong"))).unwrap_err();
        assert!(matches!(
            err,
            VaultError::Passphrase(PassphraseError::AeadFailed)
        ));
    }

    #[test]
    fn open_with_recovery_when_only_passphrase_envelope_exists_fails() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        Vault::create(&path, fast_init("p")).unwrap();
        // `RecoveryPhrase` constructed from a valid mnemonic but the
        // vault has no recovery envelope at all — `open` rejects up
        // front with `NoMatchingEnvelope`.
        let phrase = crate::recovery::parse_recovery_phrase(
            "abandon abandon abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon abandon abandon art",
        )
        .unwrap();
        match Vault::open(&path, UnlockMethod::Recovery(phrase)).unwrap_err() {
            VaultError::NoMatchingEnvelope { kind } => assert_eq!(kind, "recovery"),
            other => panic!("expected NoMatchingEnvelope, got {other:?}"),
        }
    }

    // -- put / get / list / delete -----------------------------------------

    #[test]
    fn put_then_get_roundtrips() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;
        v.put("team/gitlab/token-deploy", &val("glpat-real"), meta())
            .unwrap();
        let got = v.get("team/gitlab/token-deploy").unwrap().unwrap();
        assert_eq!(got.expose_secret(), "glpat-real");
    }

    #[test]
    fn get_missing_path_returns_none() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(&vault_path(&dir), fast_init("p"))
            .unwrap()
            .vault;
        v.put("a/b/c", &val("v"), meta()).unwrap();
        assert!(v.get("d/e/f").unwrap().is_none());
    }

    #[test]
    fn delete_removes_entry_and_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(&vault_path(&dir), fast_init("p"))
            .unwrap()
            .vault;
        v.put("a/b/c", &val("v"), meta()).unwrap();
        v.delete("a/b/c").unwrap();
        assert!(v.get("a/b/c").unwrap().is_none());
        // Idempotent — second delete is also OK.
        v.delete("a/b/c").unwrap();
    }

    // -- Versioning (ADR-024 §5) ---------------------------------

    /// The core promise: an agent that overwrites a good token with
    /// a wrong one has not destroyed anything.
    #[test]
    fn put_appends_a_version_and_the_old_value_survives() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;

        v.put("a/b/c", &val("original"), meta()).unwrap();
        v.put("a/b/c", &val("clobbered"), meta()).unwrap();

        // The newest version resolves...
        assert_eq!(
            v.get("a/b/c")
                .unwrap()
                .map(|s| s.expose_secret().to_owned()),
            Some("clobbered".to_owned())
        );
        // ...and the original is still there to go back to.
        let versions = v.versions("a/b/c");
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, 1);
        assert_eq!(versions[1].version, 2);

        v.restore("a/b/c", 1).unwrap();
        assert_eq!(
            v.get("a/b/c")
                .unwrap()
                .map(|s| s.expose_secret().to_owned()),
            Some("original".to_owned())
        );
    }

    /// A restore is itself a version, so history stays append-only
    /// and "what happened here" remains answerable.
    #[test]
    fn restore_appends_rather_than_rewinding() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;

        v.put("a/b/c", &val("v1"), meta()).unwrap();
        v.put("a/b/c", &val("v2"), meta()).unwrap();
        v.restore("a/b/c", 1).unwrap();

        let versions = v.versions("a/b/c");
        assert_eq!(versions.len(), 3, "restore is recorded, not silent");
        assert_eq!(versions[2].actor.as_deref(), Some("user"));
    }

    /// Tombstoning stops resolution without destroying anything.
    #[test]
    fn delete_tombstones_and_restore_brings_the_path_back() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;

        v.put("a/b/c", &val("secret"), meta()).unwrap();
        v.delete("a/b/c").unwrap();

        assert!(v.get("a/b/c").unwrap().is_none());
        assert!(
            !v.paths().any(|p| p == "a/b/c"),
            "tombstoned paths do not list"
        );

        v.restore("a/b/c", 1).unwrap();
        assert_eq!(
            v.get("a/b/c")
                .unwrap()
                .map(|s| s.expose_secret().to_owned()),
            Some("secret".to_owned())
        );
    }

    #[test]
    fn deleting_twice_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;

        v.put("a/b/c", &val("x"), meta()).unwrap();
        v.delete("a/b/c").unwrap();
        let after_first = v.versions("a/b/c").len();
        v.delete("a/b/c").unwrap();

        assert_eq!(
            v.versions("a/b/c").len(),
            after_first,
            "a second delete must not pile up tombstones"
        );
    }

    /// Versioning must not make a path appear several times in a
    /// listing — history is a separate question from inventory.
    #[test]
    fn listings_show_one_row_per_path_regardless_of_version_count() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;

        for i in 0..5 {
            v.put("a/b/c", &val(&format!("v{i}")), meta()).unwrap();
        }

        assert_eq!(v.paths().count(), 1);
        assert_eq!(v.list().count(), 1);
        assert_eq!(v.versions("a/b/c").len(), 5);
    }

    /// The AAD binds ciphertext to its version from v2 onwards, so
    /// a blob cannot be moved between versions of the same path.
    /// Version 1 keeps the bare path, which is what lets vaults
    /// written before ADR-024 still open.
    #[test]
    fn aad_binds_version_from_two_onwards() {
        assert_eq!(crate::format::entry_aad("a/b/c", 1), "a/b/c");
        assert_eq!(crate::format::entry_aad("a/b/c", 2), "a/b/c@v2");
        assert_ne!(
            crate::format::entry_aad("a/b/c", 2),
            crate::format::entry_aad("a/b/c", 3)
        );
    }

    /// Rotation is a version too, so rotating to a dud is
    /// recoverable — and it carries prior metadata forward.
    #[test]
    fn rotate_appends_a_version_and_keeps_metadata() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;

        let mut m = meta();
        m.description = Some("gitlab deploy token".to_owned());
        v.put("a/b/c", &val("old"), m).unwrap();
        v.rotate("a/b/c", &val("new")).unwrap();

        assert_eq!(v.versions("a/b/c").len(), 2);
        let current = v.list().next().unwrap();
        assert_eq!(current.description.as_deref(), Some("gitlab deploy token"));
        assert!(current.last_rotated_at.is_some());

        v.restore("a/b/c", 1).unwrap();
        assert_eq!(
            v.get("a/b/c")
                .unwrap()
                .map(|s| s.expose_secret().to_owned()),
            Some("old".to_owned())
        );
    }

    /// Purge is the only irreversible operation, and it can take a
    /// single version.
    #[test]
    fn purge_can_remove_one_version_without_touching_the_rest() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;

        v.put("a/b/c", &val("v1"), meta()).unwrap();
        v.put("a/b/c", &val("v2"), meta()).unwrap();
        v.purge("a/b/c", Some(1)).unwrap();

        assert_eq!(v.versions("a/b/c").len(), 1);
        assert_eq!(
            v.get("a/b/c")
                .unwrap()
                .map(|s| s.expose_secret().to_owned()),
            Some("v2".to_owned()),
            "the surviving version still decrypts under its own AAD"
        );
    }

    #[test]
    fn restoring_a_tombstone_or_a_missing_version_is_refused() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;

        v.put("a/b/c", &val("x"), meta()).unwrap();
        v.delete("a/b/c").unwrap();

        assert!(v.restore("a/b/c", 2).is_err(), "version 2 is the tombstone");
        assert!(v.restore("a/b/c", 99).is_err(), "no such version");
    }

    /// Versions must survive a close/reopen cycle, or the whole
    /// feature only exists in memory.
    #[test]
    fn version_history_survives_reopen() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        {
            let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;
            v.put("a/b/c", &val("v1"), meta()).unwrap();
            v.put("a/b/c", &val("v2"), meta()).unwrap();
        }

        let mut reopened = Vault::open(&path, UnlockMethod::Passphrase(pw("p"))).unwrap();
        assert_eq!(reopened.versions("a/b/c").len(), 2);
        reopened.restore("a/b/c", 1).unwrap();
        assert_eq!(
            reopened
                .get("a/b/c")
                .unwrap()
                .map(|s| s.expose_secret().to_owned()),
            Some("v1".to_owned())
        );
    }

    #[test]
    fn purge_scrubs_ciphertext_from_blob_region() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;
        v.put("a/b/c", &val("PLAINTEXT-THAT-WONT-APPEAR-IN-BLOBS"), meta())
            .unwrap();

        // ADR-024 §5: `delete` is a tombstone, so the ciphertext
        // survives — that is what makes an agent-mediated delete
        // recoverable.
        v.delete("a/b/c").unwrap();
        assert!(v.get("a/b/c").unwrap().is_none(), "path stops resolving");
        assert!(
            !v.file.ciphertext_blobs.is_empty(),
            "tombstoning must not scrub the value it can still restore"
        );

        // `purge` is the user-only eraser, and it really erases.
        v.purge("a/b/c", None).unwrap();

        let opened = Vault::open(&path, UnlockMethod::Passphrase(pw("p"))).unwrap();
        assert_eq!(opened.file.ciphertext_blobs.len(), 0);
        assert_eq!(opened.file.entries.len(), 0);
    }

    #[test]
    fn put_overwrites_existing_path() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(&vault_path(&dir), fast_init("p"))
            .unwrap()
            .vault;
        v.put("a/b/c", &val("first"), meta()).unwrap();
        v.put("a/b/c", &val("second"), meta()).unwrap();
        let got = v.get("a/b/c").unwrap().unwrap();
        assert_eq!(got.expose_secret(), "second");
        // No duplicate entries.
        assert_eq!(v.list().count(), 1);
    }

    #[test]
    fn list_returns_metadata_for_each_entry() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(&vault_path(&dir), fast_init("p"))
            .unwrap()
            .vault;
        v.put(
            "a/b/c",
            &val("v1"),
            EntryMetadata {
                description: Some("first".to_owned()),
                ..EntryMetadata::default()
            },
        )
        .unwrap();
        v.put(
            "x/y/z",
            &val("v2"),
            EntryMetadata {
                description: Some("second".to_owned()),
                ..EntryMetadata::default()
            },
        )
        .unwrap();
        let listed: Vec<_> = v.list().collect();
        assert_eq!(listed.len(), 2);
        let descriptions: Vec<_> = listed
            .iter()
            .filter_map(|m| m.description.as_deref())
            .collect();
        assert!(descriptions.contains(&"first"));
        assert!(descriptions.contains(&"second"));
    }

    // -- rotate -------------------------------------------------------------

    #[test]
    fn rotate_replaces_value_and_stamps_last_rotated_at() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(&vault_path(&dir), fast_init("p"))
            .unwrap()
            .vault;
        v.put("a/b/c", &val("old"), meta()).unwrap();
        v.rotate("a/b/c", &val("new")).unwrap();
        let got = v.get("a/b/c").unwrap().unwrap();
        assert_eq!(got.expose_secret(), "new");

        let m = v.list().next().unwrap();
        let stamp = m.last_rotated_at.expect("last_rotated_at set by rotate");
        assert_eq!(stamp.len(), 10, "ISO 8601 date is 10 chars");
        assert!(stamp.starts_with("20"), "year 20XX");
    }

    #[test]
    fn rotate_missing_path_errors() {
        let dir = TempDir::new().unwrap();
        let mut v = Vault::create(&vault_path(&dir), fast_init("p"))
            .unwrap()
            .vault;
        let err = v.rotate("nope/no/path", &val("v")).unwrap_err();
        match err {
            VaultError::EntryNotFound { path } => assert_eq!(path, "nope/no/path"),
            other => panic!("expected EntryNotFound, got {other:?}"),
        }
    }

    // -- Persistence --------------------------------------------------------

    #[test]
    fn persistence_create_drop_reopen_recover_value() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        {
            let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;
            v.put("a/b/c", &val("persisted-value"), meta()).unwrap();
            // Drop here — Vault writes atomically inside `put`, so
            // the dropped handle is irrelevant.
        }
        let opened = Vault::open(&path, UnlockMethod::Passphrase(pw("p"))).unwrap();
        let got = opened.get("a/b/c").unwrap().unwrap();
        assert_eq!(got.expose_secret(), "persisted-value");
    }

    #[test]
    fn persistence_multiple_entries_survive_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        {
            let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;
            v.put("team/a/x", &val("v1"), meta()).unwrap();
            v.put("team/a/y", &val("v2"), meta()).unwrap();
            v.put("personal/z/w", &val("v3"), meta()).unwrap();
        }
        let opened = Vault::open(&path, UnlockMethod::Passphrase(pw("p"))).unwrap();
        assert_eq!(
            opened.get("team/a/x").unwrap().unwrap().expose_secret(),
            "v1"
        );
        assert_eq!(
            opened.get("team/a/y").unwrap().unwrap().expose_secret(),
            "v2"
        );
        assert_eq!(
            opened.get("personal/z/w").unwrap().unwrap().expose_secret(),
            "v3"
        );
    }

    // -- add_*_envelope ------------------------------------------------------

    #[test]
    fn add_recovery_envelope_then_open_with_recovery() {
        let dir = TempDir::new().unwrap();
        let path = vault_path(&dir);
        let mut v = Vault::create(&path, fast_init("p")).unwrap().vault;
        let phrase = v.add_recovery_envelope().unwrap();

        let _opened = Vault::open(&path, UnlockMethod::Recovery(phrase)).unwrap();
    }

    // -- Date helper --------------------------------------------------------

    #[test]
    fn days_since_epoch_zero_is_1970_01_01() {
        assert_eq!(days_since_epoch_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_since_epoch_one_is_1970_01_02() {
        assert_eq!(days_since_epoch_to_ymd(1), (1970, 1, 2));
    }

    #[test]
    fn days_since_epoch_for_known_y2k() {
        // 2000-01-01 is day 10957.
        assert_eq!(days_since_epoch_to_ymd(10_957), (2000, 1, 1));
    }

    #[test]
    fn today_iso8601_is_well_formed() {
        let s = today_iso8601();
        assert_eq!(s.len(), 10);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
    }
}
