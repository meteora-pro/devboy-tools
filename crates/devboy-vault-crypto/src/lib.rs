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

pub mod format;

pub use format::{
    EntryMeta, Envelope, EnvelopeKdfParams, FormatError, HEADER_LEN, Header, KdfParams, MAGIC,
    VERSION_V1, VaultFile, b64_decode, b64_encode,
};
