//! Encrypted local-vault primitives for `devboy-tools`.
//!
//! This crate is the implementation half of [ADR-023] section 3.1: the
//! on-disk format, the AEAD wrappers, the KDF, the envelope-encryption
//! layer, and the public `Vault` API.
//!
//! See [ADR-023](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md)
//! for the design rationale, threat model, and file-format specification.
//!
//! Status: in progress — the file-format reader/writer (P3.1) is the
//! first phase to land; AEAD (P3.2), KDF (P3.3), recovery (P3.4),
//! Keychain (P3.5), and the public `Vault` API (P3.6) follow.

#![forbid(unsafe_code)]

pub mod aead;
pub mod audit;
pub mod fingerprint;
pub mod format;
pub mod keyfile;
pub mod passphrase;
pub mod recovery;
pub mod totp;
pub mod vault;

pub use aead::{
    AeadError, EncryptedEntry, KEY_LEN, NONCE_LEN, TAG_LEN, decrypt_entry, decrypt_packed,
    encrypt_entry, encrypt_packed, random_nonce,
};
pub use format::{
    EntryMeta, Envelope, EnvelopeKdfParams, FormatError, HEADER_LEN, Header, KdfParams, MAGIC,
    VERSION_V1, VaultFile, b64_decode, b64_encode,
};
pub use passphrase::{
    DEFAULT_KDF_PARAMS, PASSPHRASE_ENVELOPE_AAD, PassphraseError, create_passphrase_envelope,
    derive_wrap_key, unwrap_passphrase,
};
pub use recovery::{
    MNEMONIC_WORD_COUNT, RECOVERY_ENVELOPE_AAD, RECOVERY_HKDF_INFO, RecoveryError, RecoveryPhrase,
    create_recovery_envelope, derive_recovery_key, generate_recovery_phrase, parse_recovery_phrase,
    unwrap_recovery,
};
pub use vault::{CreateOutcome, EntryMetadata, InitialUnlock, UnlockMethod, Vault, VaultError};
