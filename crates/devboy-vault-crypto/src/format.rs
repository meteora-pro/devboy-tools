//! On-disk file format for the local vault per [ADR-023] §3.1.
//!
//! The vault lives at `~/.devboy/secrets/local-vault.dvb` and looks
//! like:
//!
//! ```text
//! HEADER (53 bytes, fixed):
//!   MAGIC       [4]   = b"DVB1"
//!   VERSION     [1]   = 0x01
//!   KDF_PARAMS [16]   = (m_cost u32 LE, t_cost u32 LE, p_cost u32 LE, salt_len u32 LE)
//!   SALT       [32]   = random per vault, used by the passphrase envelope's KDF
//!
//! UNLOCK_ENVELOPES (length-prefixed TOML, each envelope independently
//! wraps the vault key under one unlock method — passphrase, keychain,
//! recovery):
//!   [envelopes_len: u32 LE][envelopes_bytes...]
//!
//! ENTRIES_INDEX (length-prefixed TOML; metadata only — never values):
//!   [entries_len: u32 LE][entries_bytes...]
//!
//! AEAD_BLOBS (length-prefixed concatenation of per-entry ciphertexts):
//!   [blobs_len: u64 LE][blob_bytes...]
//! ```
//!
//! # Scope of this module
//!
//! This module owns the **file format**: how to read and write the
//! byte layout above, and how to atomically replace an existing
//! vault file. It does **not** perform any encryption — the
//! `wrapped_key` fields on envelopes and the `ciphertext_blobs` byte
//! buffer are opaque payloads that later epic phases (P3.2 AEAD,
//! P3.3 Argon2id, P3.4 BIP39, P3.5 Keychain) populate.
//!
//! Plaintext metadata is intentional per ADR-023 §3.1: `description`,
//! `retrieval_url`, `expires_at` are all readable without an unlock
//! step. The discovery and rotation-reminder flows need this; the
//! threat model already grants any local process read access to
//! metadata, and encrypting it would gate every `secrets list` on a
//! PIN prompt for no real benefit.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD as B64;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// =============================================================================
// Constants
// =============================================================================

/// Magic bytes at the start of every vault file (`b"DVB1"` — DevBoy
/// Vault format v1).
pub const MAGIC: [u8; 4] = *b"DVB1";

/// First (and currently only) version of the file format.
pub const VERSION_V1: u8 = 0x01;

/// Total byte length of the fixed-width header
/// (4 magic + 1 version + 16 kdf_params + 32 salt).
pub const HEADER_LEN: usize = 4 + 1 + 16 + 32;

// =============================================================================
// Errors
// =============================================================================

/// Failure modes when reading or writing a vault file.
#[derive(Debug, Error)]
pub enum FormatError {
    /// Underlying I/O error.
    #[error("vault file I/O error: {0}")]
    Io(#[from] io::Error),

    /// File is shorter than expected at the indicated stage.
    #[error("vault file is truncated at {stage}")]
    Truncated {
        /// Which section of the file was being parsed when the read
        /// fell short.
        stage: &'static str,
    },

    /// Magic bytes do not match [`MAGIC`].
    #[error(
        "vault file magic mismatch: expected {:?} ('DVB1'), got {got:?}",
        MAGIC
    )]
    MagicMismatch {
        /// The bytes actually read.
        got: [u8; 4],
    },

    /// Version byte is not one this crate knows how to read.
    #[error("vault file version {got} is not supported (this build understands {VERSION_V1})")]
    VersionUnsupported {
        /// The version byte read from the file.
        got: u8,
    },

    /// One of the embedded TOML sections (envelopes or entries) failed
    /// to parse.
    #[error("vault file {section} TOML section failed to parse: {source}")]
    TomlParse {
        /// Either `"envelopes"` or `"entries"`.
        section: &'static str,
        /// Underlying TOML deserialization error.
        #[source]
        source: toml::de::Error,
    },

    /// Serialising one of the in-memory TOML sections failed.
    #[error("vault file {section} TOML section failed to serialise: {source}")]
    TomlSerialize {
        /// Either `"envelopes"` or `"entries"`.
        section: &'static str,
        /// Underlying TOML serialization error.
        #[source]
        source: toml::ser::Error,
    },

    /// Atomic-write asked to persist into a path with no parent
    /// directory, or whose final component cannot be used as a file
    /// name. Happens for `/`, `.`, etc.
    #[error("vault file path {path:?} cannot be written atomically (no usable parent / file name)")]
    InvalidPath {
        /// The offending path.
        path: std::path::PathBuf,
    },
}

// =============================================================================
// Header
// =============================================================================

/// Argon2id KDF parameters. Stored verbatim in the header so the reader
/// can derive the same key as the writer used.
///
/// `m_cost` is in KiB (Argon2 convention); `t_cost` is the iteration
/// count; `p_cost` is the parallelism factor; `salt_len` is the byte
/// length of the salt that lives in [`Header::salt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub m_cost: u32,
    /// Iteration count.
    pub t_cost: u32,
    /// Parallelism factor.
    pub p_cost: u32,
    /// Salt length in bytes (informational; the salt itself is the
    /// fixed [`Header::salt`] field).
    pub salt_len: u32,
}

impl KdfParams {
    /// Default OWASP-recommended Argon2id parameters for 2024-class
    /// hardware (m=64MiB, t=3, p=1, salt 32B).
    pub const DEFAULT: KdfParams = KdfParams {
        m_cost: 64 * 1024,
        t_cost: 3,
        p_cost: 1,
        salt_len: 32,
    };
}

/// Fixed-width binary header at the start of every vault file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Always [`MAGIC`].
    pub magic: [u8; 4],
    /// Format version. Currently always [`VERSION_V1`].
    pub version: u8,
    /// Argon2id parameters.
    pub kdf_params: KdfParams,
    /// 32-byte random salt used by the passphrase envelope's KDF. Per
    /// ADR-023 §3.2 the same salt is reused across vault re-keying so
    /// envelope addition does not invalidate existing wrapped_keys.
    pub salt: [u8; 32],
}

impl Header {
    /// Build a header with [`KdfParams::DEFAULT`] and the supplied
    /// salt. Convenience constructor for tests and `Vault::create`
    /// (P3.6).
    pub fn new(salt: [u8; 32]) -> Self {
        Self {
            magic: MAGIC,
            version: VERSION_V1,
            kdf_params: KdfParams::DEFAULT,
            salt,
        }
    }

    fn read(reader: &mut impl Read) -> Result<Self, FormatError> {
        let mut buf = [0u8; HEADER_LEN];
        reader.read_exact(&mut buf).map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => FormatError::Truncated { stage: "header" },
            _ => FormatError::Io(e),
        })?;

        let magic: [u8; 4] = buf[0..4].try_into().expect("4 bytes");
        if magic != MAGIC {
            return Err(FormatError::MagicMismatch { got: magic });
        }

        let version = buf[4];
        if version != VERSION_V1 {
            return Err(FormatError::VersionUnsupported { got: version });
        }

        let kdf_params = KdfParams {
            m_cost: u32::from_le_bytes(buf[5..9].try_into().unwrap()),
            t_cost: u32::from_le_bytes(buf[9..13].try_into().unwrap()),
            p_cost: u32::from_le_bytes(buf[13..17].try_into().unwrap()),
            salt_len: u32::from_le_bytes(buf[17..21].try_into().unwrap()),
        };

        let salt: [u8; 32] = buf[21..53].try_into().expect("32 bytes");

        Ok(Self {
            magic,
            version,
            kdf_params,
            salt,
        })
    }

    fn write(&self, writer: &mut impl Write) -> Result<(), FormatError> {
        writer.write_all(&self.magic)?;
        writer.write_all(&[self.version])?;
        writer.write_all(&self.kdf_params.m_cost.to_le_bytes())?;
        writer.write_all(&self.kdf_params.t_cost.to_le_bytes())?;
        writer.write_all(&self.kdf_params.p_cost.to_le_bytes())?;
        writer.write_all(&self.kdf_params.salt_len.to_le_bytes())?;
        writer.write_all(&self.salt)?;
        Ok(())
    }
}

// =============================================================================
// Envelopes (TOML section #1)
// =============================================================================

/// Argon2id parameters as recorded inside a passphrase envelope. Kept
/// separate from [`KdfParams`] (the binary-header form) so the on-disk
/// TOML representation can match ADR-023 §3.1's `{ m, t, p }` shape
/// directly. Both are populated from the same logical values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvelopeKdfParams {
    /// Memory cost (KiB).
    pub m: u32,
    /// Iteration count.
    pub t: u32,
    /// Parallelism factor.
    pub p: u32,
}

/// One unlock envelope. Each envelope independently wraps the vault
/// key under a different unlock method — adding a new envelope
/// (Touch ID, recovery phrase) is a header-only write that never
/// touches the per-entry ciphertext blobs.
///
/// `wrapped_key` and the salt fields are opaque base64-encoded byte
/// arrays at this layer; the AEAD/KDF mechanics are handled by P3.2 +
/// P3.3 + P3.4 + P3.5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Envelope {
    /// Passphrase envelope — vault key wrapped under
    /// `Argon2id(passphrase, argon2_salt, argon2_params)`.
    Passphrase {
        /// Per-envelope salt (base64 no-pad).
        argon2_salt: String,
        /// Argon2id parameters used to derive the wrap key.
        argon2_params: EnvelopeKdfParams,
        /// AEAD-wrapped vault key (base64 no-pad).
        wrapped_key: String,
    },
    /// Keychain envelope — vault key wrapped under a key stored in the
    /// macOS Keychain under `keychain_account` and protected by
    /// Touch ID.
    Keychain {
        /// `kSecAttrAccount` value used to look up the wrap key.
        keychain_account: String,
        /// AEAD-wrapped vault key (base64 no-pad).
        wrapped_key: String,
    },
    /// Recovery envelope — vault key wrapped under
    /// `HKDF(BIP39_seed, bip39_salt)`.
    Recovery {
        /// Per-envelope salt (base64 no-pad).
        bip39_salt: String,
        /// AEAD-wrapped vault key (base64 no-pad).
        wrapped_key: String,
    },
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EnvelopesFile {
    #[serde(default, rename = "envelope", skip_serializing_if = "Vec::is_empty")]
    envelope: Vec<Envelope>,
}

// =============================================================================
// Entries (TOML section #2)
// =============================================================================

/// Per-entry index record. Plaintext metadata + a pointer
/// (`ct_offset`, `ct_length`) into the ciphertext blob region. The
/// nonce is stored alongside the metadata so decryption can proceed
/// without unwrapping the ciphertext header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryMeta {
    /// ADR-020 path string. Validated against [`devboy_storage::SecretPath`]
    /// upstream (this layer keeps it as a free-form `String` to avoid
    /// the cross-crate dep — the storage layer is the gatekeeper).
    pub path: String,
    /// Per-entry AEAD nonce (base64 no-pad). 24 bytes for the
    /// XChaCha20-Poly1305 algorithm chosen in ADR-023 §3.1.
    pub nonce: String,
    /// Byte offset into the file's `AEAD_BLOBS` section where this
    /// entry's ciphertext begins.
    pub ct_offset: u64,
    /// Length in bytes of this entry's ciphertext (including the
    /// 16-byte Poly1305 tag).
    pub ct_length: u32,

    /// Free-text description (per ADR-020 §3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// URL the user opens to obtain a fresh value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_url: Option<String>,
    /// ISO 8601 (`YYYY-MM-DD`) expiry date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// ISO 8601 date of the last successful rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rotated_at: Option<String>,
    /// Catalogue pattern id (resolved through `devboy-secret-patterns`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_id: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EntriesFile {
    #[serde(default, rename = "entry", skip_serializing_if = "Vec::is_empty")]
    entry: Vec<EntryMeta>,
}

// =============================================================================
// VaultFile aggregate
// =============================================================================

/// In-memory representation of a vault file. Constructed by
/// [`VaultFile::read_from`] / [`VaultFile::read_file`] when reading,
/// and by the higher-level `Vault` API (P3.6) when writing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultFile {
    /// Fixed-width binary header.
    pub header: Header,
    /// Unlock envelopes (one per unlock method).
    pub envelopes: Vec<Envelope>,
    /// Per-entry index records (plaintext metadata).
    pub entries: Vec<EntryMeta>,
    /// Concatenated ciphertext blobs. Each entry's
    /// `[ct_offset, ct_offset + ct_length)` slice is its individual
    /// ciphertext.
    pub ciphertext_blobs: Vec<u8>,
}

impl VaultFile {
    /// Build an empty vault — no envelopes, no entries, no blobs.
    /// Useful as the starting point for `Vault::create` (P3.6).
    pub fn empty(salt: [u8; 32]) -> Self {
        Self {
            header: Header::new(salt),
            envelopes: Vec::new(),
            entries: Vec::new(),
            ciphertext_blobs: Vec::new(),
        }
    }

    /// Read a vault from any reader implementing [`Read`].
    pub fn read_from(reader: &mut impl Read) -> Result<Self, FormatError> {
        let header = Header::read(reader)?;

        let envelopes_bytes = read_length_prefixed_u32(reader, "envelopes_len")?;
        let envelopes_file: EnvelopesFile =
            toml::from_str(std::str::from_utf8(&envelopes_bytes).map_err(|e| {
                FormatError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("envelopes section is not valid UTF-8: {e}"),
                ))
            })?)
            .map_err(|source| FormatError::TomlParse {
                section: "envelopes",
                source,
            })?;

        let entries_bytes = read_length_prefixed_u32(reader, "entries_len")?;
        let entries_file: EntriesFile =
            toml::from_str(std::str::from_utf8(&entries_bytes).map_err(|e| {
                FormatError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("entries section is not valid UTF-8: {e}"),
                ))
            })?)
            .map_err(|source| FormatError::TomlParse {
                section: "entries",
                source,
            })?;

        let blobs = read_length_prefixed_u64(reader, "blobs_len")?;

        Ok(Self {
            header,
            envelopes: envelopes_file.envelope,
            entries: entries_file.entry,
            ciphertext_blobs: blobs,
        })
    }

    /// Write a vault to any writer implementing [`Write`].
    pub fn write_to(&self, writer: &mut impl Write) -> Result<(), FormatError> {
        self.header.write(writer)?;

        let envelopes_file = EnvelopesFile {
            envelope: self.envelopes.clone(),
        };
        let envelopes_toml =
            toml::to_string(&envelopes_file).map_err(|source| FormatError::TomlSerialize {
                section: "envelopes",
                source,
            })?;
        write_length_prefixed_u32(writer, envelopes_toml.as_bytes())?;

        let entries_file = EntriesFile {
            entry: self.entries.clone(),
        };
        let entries_toml =
            toml::to_string(&entries_file).map_err(|source| FormatError::TomlSerialize {
                section: "entries",
                source,
            })?;
        write_length_prefixed_u32(writer, entries_toml.as_bytes())?;

        write_length_prefixed_u64(writer, &self.ciphertext_blobs)?;

        Ok(())
    }

    /// Read a vault from a path on disk. Equivalent to opening the
    /// file and calling [`VaultFile::read_from`].
    pub fn read_file(path: &Path) -> Result<Self, FormatError> {
        let mut f = fs::File::open(path)?;
        Self::read_from(&mut f)
    }

    /// Atomically replace the file at `path` with this vault.
    ///
    /// Writes the new content to a sibling temp file (same parent
    /// directory so the eventual rename is on the same filesystem),
    /// fsyncs it, then renames over the destination. On any failure
    /// the temp file is cleaned up so a partial write never leaves a
    /// stale `*.tmp` behind.
    pub fn write_file_atomic(&self, path: &Path) -> Result<(), FormatError> {
        let parent = path.parent().ok_or_else(|| FormatError::InvalidPath {
            path: path.to_path_buf(),
        })?;
        // `tempfile::NamedTempFile::new_in` creates a uniquely-named
        // file in the requested directory; `.persist` then renames it
        // over the target atomically (on every supported platform).
        // Drop deletes the temp file if `persist` was not called, so
        // any error in `write_to` is self-cleaning.
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        self.write_to(&mut tmp)?;
        // Flush + fsync so the rename observes the full contents.
        tmp.as_file().sync_all()?;
        tmp.persist(path)
            .map_err(|e: tempfile::PersistError| FormatError::Io(e.error))?;
        Ok(())
    }
}

// =============================================================================
// Length-prefixed section helpers
// =============================================================================

fn read_length_prefixed_u32(
    reader: &mut impl Read,
    stage: &'static str,
) -> Result<Vec<u8>, FormatError> {
    let mut len_buf = [0u8; 4];
    reader
        .read_exact(&mut len_buf)
        .map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => FormatError::Truncated { stage },
            _ => FormatError::Io(e),
        })?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => FormatError::Truncated { stage },
        _ => FormatError::Io(e),
    })?;
    Ok(body)
}

fn read_length_prefixed_u64(
    reader: &mut impl Read,
    stage: &'static str,
) -> Result<Vec<u8>, FormatError> {
    let mut len_buf = [0u8; 8];
    reader
        .read_exact(&mut len_buf)
        .map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => FormatError::Truncated { stage },
            _ => FormatError::Io(e),
        })?;
    let len = u64::from_le_bytes(len_buf);
    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => FormatError::Truncated { stage },
        _ => FormatError::Io(e),
    })?;
    Ok(body)
}

fn write_length_prefixed_u32(writer: &mut impl Write, body: &[u8]) -> Result<(), FormatError> {
    let len: u32 = body.len().try_into().map_err(|_| {
        FormatError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "section exceeds u32::MAX bytes",
        ))
    })?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(body)?;
    Ok(())
}

fn write_length_prefixed_u64(writer: &mut impl Write, body: &[u8]) -> Result<(), FormatError> {
    let len: u64 = body.len() as u64;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(body)?;
    Ok(())
}

/// Convenience for callers that need to embed binary data into the
/// envelope/entry TOML — wrap and unwrap base64 with the same
/// no-padding alphabet used by [`Envelope::wrapped_key`] etc.
pub fn b64_encode(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

/// Inverse of [`b64_encode`].
pub fn b64_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    B64.decode(s)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample_envelope_passphrase() -> Envelope {
        Envelope::Passphrase {
            argon2_salt: b64_encode(&[0xAA; 32]),
            argon2_params: EnvelopeKdfParams {
                m: 65536,
                t: 3,
                p: 1,
            },
            wrapped_key: b64_encode(&[0xBB; 48]),
        }
    }

    fn sample_envelope_keychain() -> Envelope {
        Envelope::Keychain {
            keychain_account: "dev.devboy.secrets.vault.macos-touchid".to_owned(),
            wrapped_key: b64_encode(&[0xCC; 48]),
        }
    }

    fn sample_envelope_recovery() -> Envelope {
        Envelope::Recovery {
            bip39_salt: b64_encode(&[0xDD; 32]),
            wrapped_key: b64_encode(&[0xEE; 48]),
        }
    }

    fn sample_entry(path: &str, ct_offset: u64, ct_length: u32) -> EntryMeta {
        EntryMeta {
            path: path.to_owned(),
            nonce: b64_encode(&[0x11; 24]),
            ct_offset,
            ct_length,
            description: Some(format!("Description for {path}")),
            retrieval_url: Some("https://example.test/tokens".to_owned()),
            expires_at: Some("2026-08-01".to_owned()),
            last_rotated_at: Some("2026-05-02".to_owned()),
            pattern_id: Some("github-pat".to_owned()),
        }
    }

    // -- Header ----------------------------------------------------------------

    #[test]
    fn header_round_trips_through_in_memory_buffer() {
        let salt = [0x42; 32];
        let header = Header::new(salt);
        let mut buf = Vec::new();
        header.write(&mut buf).unwrap();
        assert_eq!(buf.len(), HEADER_LEN);

        let mut cursor = Cursor::new(&buf);
        let parsed = Header::read(&mut cursor).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn header_rejects_wrong_magic() {
        let mut buf = vec![b'X', b'Y', b'Z', b'!', VERSION_V1];
        buf.extend(std::iter::repeat_n(0u8, HEADER_LEN - buf.len()));
        let mut cursor = Cursor::new(&buf);
        match Header::read(&mut cursor).unwrap_err() {
            FormatError::MagicMismatch { got } => assert_eq!(got, *b"XYZ!"),
            other => panic!("expected MagicMismatch, got {other:?}"),
        }
    }

    #[test]
    fn header_rejects_unsupported_version() {
        let mut buf = Vec::with_capacity(HEADER_LEN);
        buf.extend_from_slice(&MAGIC);
        buf.push(0x99);
        buf.extend(std::iter::repeat_n(0u8, HEADER_LEN - buf.len()));
        let mut cursor = Cursor::new(&buf);
        match Header::read(&mut cursor).unwrap_err() {
            FormatError::VersionUnsupported { got } => assert_eq!(got, 0x99),
            other => panic!("expected VersionUnsupported, got {other:?}"),
        }
    }

    #[test]
    fn header_truncation_is_classified_as_truncated() {
        let mut cursor = Cursor::new(&MAGIC[..2]); // far short of HEADER_LEN
        match Header::read(&mut cursor).unwrap_err() {
            FormatError::Truncated { stage } => assert_eq!(stage, "header"),
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    // -- Envelopes / entries serialisation -----------------------------------

    #[test]
    fn envelopes_section_round_trips_each_kind() {
        let envelopes = vec![
            sample_envelope_passphrase(),
            sample_envelope_keychain(),
            sample_envelope_recovery(),
        ];
        let envelopes_file = EnvelopesFile {
            envelope: envelopes.clone(),
        };
        let toml_str = toml::to_string(&envelopes_file).unwrap();
        let reparsed: EnvelopesFile = toml::from_str(&toml_str).unwrap();
        assert_eq!(reparsed.envelope, envelopes);
    }

    #[test]
    fn entries_section_round_trips() {
        let entries = vec![
            sample_entry("team/gitlab/token-deploy", 0, 312),
            sample_entry("personal/github/pat", 312, 256),
        ];
        let entries_file = EntriesFile {
            entry: entries.clone(),
        };
        let toml_str = toml::to_string(&entries_file).unwrap();
        let reparsed: EntriesFile = toml::from_str(&toml_str).unwrap();
        assert_eq!(reparsed.entry, entries);
    }

    // -- Whole-file round trip -----------------------------------------------

    #[test]
    fn empty_vault_round_trips() {
        let vault = VaultFile::empty([0; 32]);
        let mut buf = Vec::new();
        vault.write_to(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf);
        let parsed = VaultFile::read_from(&mut cursor).unwrap();
        assert_eq!(parsed, vault);
    }

    #[test]
    fn full_vault_round_trips_in_memory() {
        let mut vault = VaultFile::empty([0x55; 32]);
        vault.envelopes.push(sample_envelope_passphrase());
        vault.envelopes.push(sample_envelope_keychain());
        vault.envelopes.push(sample_envelope_recovery());
        vault.entries.push(sample_entry("team/gitlab/x", 0, 64));
        vault
            .entries
            .push(sample_entry("personal/github/y", 64, 80));
        vault.ciphertext_blobs.extend_from_slice(&[0xAB; 64]);
        vault.ciphertext_blobs.extend_from_slice(&[0xCD; 80]);

        let mut buf = Vec::new();
        vault.write_to(&mut buf).unwrap();

        let mut cursor = Cursor::new(&buf);
        let parsed = VaultFile::read_from(&mut cursor).unwrap();
        assert_eq!(parsed, vault);
    }

    // -- Atomic file write ---------------------------------------------------

    #[test]
    fn write_file_atomic_then_read_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-vault.dvb");

        let mut vault = VaultFile::empty([0x77; 32]);
        vault.envelopes.push(sample_envelope_passphrase());
        vault.entries.push(sample_entry("team/x/y", 0, 32));
        vault.ciphertext_blobs.extend_from_slice(&[0x12; 32]);

        vault.write_file_atomic(&path).unwrap();

        let parsed = VaultFile::read_file(&path).unwrap();
        assert_eq!(parsed, vault);
    }

    #[test]
    fn write_file_atomic_leaves_no_temp_file_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-vault.dvb");
        let vault = VaultFile::empty([0; 32]);
        vault.write_file_atomic(&path).unwrap();

        // Only the final file should exist in the directory.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["local-vault.dvb".to_owned()]);
    }

    #[test]
    fn write_file_atomic_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-vault.dvb");

        // Write twice with different contents.
        let mut v1 = VaultFile::empty([0x11; 32]);
        v1.entries.push(sample_entry("a/b/c", 0, 16));
        v1.ciphertext_blobs.extend_from_slice(&[0xAA; 16]);
        v1.write_file_atomic(&path).unwrap();

        let mut v2 = VaultFile::empty([0x22; 32]);
        v2.entries.push(sample_entry("d/e/f", 0, 24));
        v2.ciphertext_blobs.extend_from_slice(&[0xBB; 24]);
        v2.write_file_atomic(&path).unwrap();

        let parsed = VaultFile::read_file(&path).unwrap();
        assert_eq!(parsed, v2);
    }

    #[test]
    fn read_file_truncated_after_header_yields_truncated_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local-vault.dvb");

        // Write only the header.
        let header = Header::new([0; 32]);
        let mut buf = Vec::new();
        header.write(&mut buf).unwrap();
        std::fs::write(&path, buf).unwrap();

        match VaultFile::read_file(&path).unwrap_err() {
            FormatError::Truncated { stage } => assert_eq!(stage, "envelopes_len"),
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    // -- Base64 helpers ------------------------------------------------------

    #[test]
    fn b64_round_trips_binary_payload() {
        let original = (0u8..=255u8).collect::<Vec<u8>>();
        let encoded = b64_encode(&original);
        let decoded = b64_decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    // -- Default KDF params --------------------------------------------------

    #[test]
    fn default_kdf_params_match_owasp_recommendations() {
        // OWASP 2024 recommendation for Argon2id — 64 MiB, 3 iters,
        // p=1. The defaults are referenced by ADR-023 §3.2 algorithms
        // section; if we change them, both this test and the ADR
        // text need to move together.
        assert_eq!(KdfParams::DEFAULT.m_cost, 64 * 1024);
        assert_eq!(KdfParams::DEFAULT.t_cost, 3);
        assert_eq!(KdfParams::DEFAULT.p_cost, 1);
        assert_eq!(KdfParams::DEFAULT.salt_len, 32);
    }
}
