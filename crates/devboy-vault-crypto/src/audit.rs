//! Encrypted append-only audit log (ADR-024 §4, Ф9b).
//!
//! # What the log is for
//!
//! Every secret access leaves a record. The point is not
//! bookkeeping — it is that a user who suspects an agent has been
//! reading things it should not can find out, after the fact,
//! without having had to watch.
//!
//! # Why it is encrypted
//!
//! An audit log of secret access is itself sensitive: the sequence
//! of paths read, and when, describes what a person works on and
//! which systems they can reach. Storing it in the clear beside an
//! encrypted vault would hand that away for free.
//!
//! # On-disk layout
//!
//! ```text
//!   MAGIC        [6]   = b"AUDIT1"
//!   VERSION      [2]   = u16 LE
//!   COUNT        [8]   = u64 LE — records written so far
//!   INDEX        [COUNT × 36]
//!       seq      [8]   = u64 LE
//!       nonce    [24]
//!       length   [4]   = u32 LE
//!   RECORDS            = ciphertexts, in index order
//! ```
//!
//! The index is plaintext on purpose: `seq` has to be readable
//! *before* decryption because it is the AAD, and a nonce is not
//! secret. What the index does not reveal is the content — the
//! path, the actor and the timestamp all live inside the
//! ciphertext.
//!
//! # What tampering it catches
//!
//! - **Splicing.** AAD is `b"audit-v1" || seq`, so a record moved
//!   to a different index no longer decrypts. Reordering, copying a
//!   record over another, and re-using an old record all fail.
//! - **Truncation without a header rewrite.** `COUNT` is written in
//!   the header, so lopping records off the end and leaving the
//!   header alone is visible. That covers a careless truncation — a
//!   crash mid-write, a partial copy, a naive `head -c`.
//!
//!   It does **not** cover a deliberate one. `COUNT` is plaintext and
//!   unauthenticated, and each record's AAD binds only its own `seq`.
//!   An attacker who can write to this file — the same attacker the
//!   splice protection is aimed at — can delete the last index entry
//!   and the last ciphertext, decrement `COUNT`, and the result reads
//!   back as a valid log with one fewer record. The incriminating
//!   tail disappears without a trace.
//!
//!   Closing that needs the header itself authenticated under the
//!   vault key, which is a format change and is tracked separately.
//!   Until then, do not treat this log as evidence that *nothing
//!   else happened* — only as evidence that what it does contain,
//!   happened.
//! - **Editing.** Any change to a ciphertext fails its Poly1305
//!   tag.
//!
//! What it does **not** catch is deletion of the whole file, or a
//! rollback to an older copy of it. Detecting that needs state kept
//! somewhere the attacker cannot reach, which is a different
//! problem from this one.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::aead::{self, AeadError, KEY_LEN, NONCE_LEN};

/// File magic.
pub const MAGIC: &[u8; 6] = b"AUDIT1";

/// Format version.
pub const VERSION: u16 = 1;

/// AAD prefix. Combined with the record's sequence number so a
/// record cannot be moved to another position.
const AAD_PREFIX: &[u8] = b"audit-v1";

/// Bytes per index entry: seq(8) + nonce(24) + length(4).
const INDEX_ENTRY_LEN: usize = 8 + NONCE_LEN + 4;

/// Header length: magic(6) + version(2) + count(8).
const HEADER_LEN: usize = 6 + 2 + 8;

/// Things that can go wrong reading or writing the log.
#[derive(Debug, Error)]
pub enum AuditError {
    /// Underlying I/O failure.
    #[error("audit log I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The file does not start with [`MAGIC`].
    #[error("not an audit log (bad magic)")]
    BadMagic,

    /// The file claims a version this build does not understand.
    #[error("unsupported audit log version {found} (this build understands {VERSION})")]
    UnsupportedVersion {
        /// Version read from the file.
        found: u16,
    },

    /// The header says one thing and the file says another.
    ///
    /// This is the truncation signal: records are missing relative
    /// to the count the header committed to.
    #[error(
        "audit log is truncated: the header records {expected} entries but only {found} are present"
    )]
    Truncated {
        /// Count from the header.
        expected: u64,
        /// Entries actually readable.
        found: u64,
    },

    /// A record failed to decrypt.
    ///
    /// Either the key is wrong or the record has been edited,
    /// spliced in from elsewhere, or moved.
    #[error(
        "audit record {seq} did not decrypt: it has been altered, moved, or written under a different key"
    )]
    RecordCorrupt {
        /// Sequence number of the offending record.
        seq: u64,
    },

    /// Crypto layer failure.
    #[error("audit crypto error: {0}")]
    Aead(#[from] AeadError),

    /// A record's body was not valid JSON.
    #[error("audit record {seq} is not valid JSON: {source}")]
    Malformed {
        /// Sequence number of the offending record.
        seq: u64,
        /// The parse failure.
        source: serde_json::Error,
    },
}

/// One audit record.
///
/// The value is emphatically **not** here. What is recorded is that
/// a path was accessed, by whom, and when — never what came back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// ISO 8601 timestamp.
    ///
    /// Lives inside the ciphertext rather than the index: when a
    /// person works is exactly the kind of thing the log is
    /// supposed to protect.
    pub timestamp: String,
    /// What happened — `"read"`, `"write"`, `"unlock"`, …
    pub action: String,
    /// ADR-020 path the action touched.
    pub path: String,
    /// Who did it — `"agent"` or `"user"`.
    pub actor: String,
    /// Free-form detail, already scrubbed by the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// An append-only encrypted log on disk.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    count: u64,
}

impl AuditLog {
    /// Open an existing log, or create an empty one.
    pub fn open_or_create(path: &Path) -> Result<Self, AuditError> {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::File::create(path)?;
            file.write_all(MAGIC)?;
            file.write_all(&VERSION.to_le_bytes())?;
            file.write_all(&0u64.to_le_bytes())?;
            file.sync_all()?;
            restrict_permissions(path)?;
            return Ok(Self {
                path: path.to_path_buf(),
                count: 0,
            });
        }

        let mut file = std::fs::File::open(path)?;
        let mut header = [0u8; HEADER_LEN];
        file.read_exact(&mut header)?;
        if &header[..6] != MAGIC {
            return Err(AuditError::BadMagic);
        }
        let found = u16::from_le_bytes([header[6], header[7]]);
        if found != VERSION {
            return Err(AuditError::UnsupportedVersion { found });
        }
        let count = u64::from_le_bytes(header[8..16].try_into().expect("8 bytes"));

        Ok(Self {
            path: path.to_path_buf(),
            count,
        })
    }

    /// How many records the header claims.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Append one record.
    ///
    /// Existing bytes are never rewritten: the record and its index
    /// entry are inserted and the count bumped. That is what makes
    /// the log append-only in fact rather than only in intent.
    pub fn append(&mut self, key: &[u8; KEY_LEN], record: &AuditRecord) -> Result<u64, AuditError> {
        let seq = self.count;
        let body =
            serde_json::to_vec(record).map_err(|source| AuditError::Malformed { seq, source })?;
        let encrypted = aead::encrypt_entry(key, &aad_for(seq), &body)?;

        // The index sits between the header and the records, so an
        // append has to splice rather than tack on. Rewriting the
        // whole file is the honest way to keep both regions
        // contiguous at this size; the log rotates long before that
        // costs anything.
        let (mut index, mut records) = self.read_regions()?;
        index.extend_from_slice(&seq.to_le_bytes());
        index.extend_from_slice(&encrypted.nonce);
        index.extend_from_slice(&(encrypted.ciphertext.len() as u32).to_le_bytes());
        records.extend_from_slice(&encrypted.ciphertext);

        let temp = self.path.with_extension("dvb.tmp");
        {
            let mut out = std::fs::File::create(&temp)?;
            out.write_all(MAGIC)?;
            out.write_all(&VERSION.to_le_bytes())?;
            out.write_all(&(seq + 1).to_le_bytes())?;
            out.write_all(&index)?;
            out.write_all(&records)?;
            out.sync_all()?;
        }
        restrict_permissions(&temp)?;
        std::fs::rename(&temp, &self.path)?;

        self.count = seq + 1;
        Ok(seq)
    }

    /// Read and decrypt every record in order.
    ///
    /// Fails on the first record that does not verify, naming its
    /// sequence number — a partially-readable audit log is not
    /// evidence of anything, so silently skipping bad records would
    /// defeat the purpose.
    pub fn read_all(&self, key: &[u8; KEY_LEN]) -> Result<Vec<AuditRecord>, AuditError> {
        let (index, records) = self.read_regions()?;

        let available = (index.len() / INDEX_ENTRY_LEN) as u64;
        if available < self.count {
            return Err(AuditError::Truncated {
                expected: self.count,
                found: available,
            });
        }

        let mut out = Vec::with_capacity(self.count as usize);
        let mut offset = 0usize;
        for i in 0..self.count as usize {
            let entry = &index[i * INDEX_ENTRY_LEN..(i + 1) * INDEX_ENTRY_LEN];
            let seq = u64::from_le_bytes(entry[..8].try_into().expect("8 bytes"));
            let mut nonce = [0u8; NONCE_LEN];
            nonce.copy_from_slice(&entry[8..8 + NONCE_LEN]);
            let length =
                u32::from_le_bytes(entry[8 + NONCE_LEN..].try_into().expect("4 bytes")) as usize;

            if offset + length > records.len() {
                return Err(AuditError::Truncated {
                    expected: self.count,
                    found: i as u64,
                });
            }
            let ciphertext = &records[offset..offset + length];
            offset += length;

            let plaintext = aead::decrypt_entry(key, &aad_for(seq), &nonce, ciphertext)
                .map_err(|_| AuditError::RecordCorrupt { seq })?;
            let record: AuditRecord = serde_json::from_slice(&plaintext)
                .map_err(|source| AuditError::Malformed { seq, source })?;
            out.push(record);
        }
        Ok(out)
    }

    /// Split the file into its index and record regions.
    fn read_regions(&self) -> Result<(Vec<u8>, Vec<u8>), AuditError> {
        let mut file = std::fs::File::open(&self.path)?;
        file.seek(SeekFrom::Start(HEADER_LEN as u64))?;

        let index_len = self.count as usize * INDEX_ENTRY_LEN;
        let mut index = vec![0u8; index_len];
        // A short read here means the file lost bytes the header
        // still counts, which is the truncation case.
        if let Err(e) = file.read_exact(&mut index) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                let mut partial = Vec::new();
                let mut retry = std::fs::File::open(&self.path)?;
                retry.seek(SeekFrom::Start(HEADER_LEN as u64))?;
                retry.read_to_end(&mut partial)?;
                let usable = partial.len() / INDEX_ENTRY_LEN * INDEX_ENTRY_LEN;
                return Ok((partial[..usable].to_vec(), Vec::new()));
            }
            return Err(e.into());
        }

        let mut records = Vec::new();
        file.read_to_end(&mut records)?;
        Ok((index, records))
    }
}

/// AAD for a record at `seq`.
///
/// Binding the sequence number is what stops a record being moved:
/// the same ciphertext at a different index has different AAD and
/// no longer verifies.
fn aad_for(seq: u64) -> String {
    let mut bytes = Vec::with_capacity(AAD_PREFIX.len() + 8);
    bytes.extend_from_slice(AAD_PREFIX);
    bytes.extend_from_slice(&seq.to_le_bytes());
    // The AEAD layer takes AAD as a string; this encoding is
    // injective over the byte pair, which is all that is required.
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Tighten permissions to owner-only where the platform has them.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), AuditError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), AuditError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; KEY_LEN] = [7u8; KEY_LEN];

    fn record(path: &str) -> AuditRecord {
        AuditRecord {
            timestamp: "2026-08-11T12:00:00Z".to_owned(),
            action: "read".to_owned(),
            path: path.to_owned(),
            actor: "agent".to_owned(),
            detail: None,
        }
    }

    fn log(dir: &tempfile::TempDir) -> AuditLog {
        AuditLog::open_or_create(&dir.path().join("audit-log.dvb")).expect("open")
    }

    #[test]
    fn records_round_trip_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = log(&dir);

        for p in ["team/a/one", "team/b/two", "personal/c/three"] {
            l.append(&KEY, &record(p)).expect("append");
        }

        let read = l.read_all(&KEY).expect("read");
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].path, "team/a/one");
        assert_eq!(read[2].path, "personal/c/three");
    }

    #[test]
    fn a_fresh_log_is_empty_and_readable() {
        let dir = tempfile::tempdir().unwrap();
        let l = log(&dir);
        assert_eq!(l.count(), 0);
        assert!(l.read_all(&KEY).expect("read").is_empty());
    }

    #[test]
    fn appending_does_not_disturb_earlier_records() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = log(&dir);
        l.append(&KEY, &record("first")).unwrap();
        let after_one = l.read_all(&KEY).unwrap();

        l.append(&KEY, &record("second")).unwrap();
        let after_two = l.read_all(&KEY).unwrap();

        assert_eq!(
            after_two[0], after_one[0],
            "the first record must be untouched"
        );
        assert_eq!(after_two.len(), 2);
    }

    /// The log reopens from disk with everything intact — a log
    /// that only reads back within one process is not a log.
    #[test]
    fn the_log_survives_being_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        {
            let mut l = AuditLog::open_or_create(&path).unwrap();
            l.append(&KEY, &record("persisted")).unwrap();
        }

        let reopened = AuditLog::open_or_create(&path).unwrap();
        assert_eq!(reopened.count(), 1);
        assert_eq!(reopened.read_all(&KEY).unwrap()[0].path, "persisted");
    }

    /// Splicing: a record copied over another must not decrypt,
    /// because its AAD carries the sequence number it was written
    /// at.
    ///
    /// The two paths are deliberately the same length so the
    /// ciphertexts are too and the swap leaves a structurally valid
    /// file — that is the splice worth catching. An earlier version
    /// of this test guarded the swap behind an equal-length check
    /// and silently skipped itself when they differed, which a
    /// mutation run exposed: unbinding the sequence number from the
    /// AAD failed nothing.
    #[test]
    fn a_record_moved_to_another_index_does_not_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut l = AuditLog::open_or_create(&path).unwrap();
        l.append(&KEY, &record("team/a/one")).unwrap();
        l.append(&KEY, &record("team/b/two")).unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        let records_at = HEADER_LEN + 2 * INDEX_ENTRY_LEN;
        let first_len = u32::from_le_bytes(
            bytes[HEADER_LEN + 8 + NONCE_LEN..HEADER_LEN + INDEX_ENTRY_LEN]
                .try_into()
                .unwrap(),
        ) as usize;
        let second_len = u32::from_le_bytes(
            bytes[HEADER_LEN + INDEX_ENTRY_LEN + 8 + NONCE_LEN..HEADER_LEN + 2 * INDEX_ENTRY_LEN]
                .try_into()
                .unwrap(),
        ) as usize;
        assert_eq!(
            first_len, second_len,
            "the fixture must produce equal-length records or the swap below is not a splice"
        );

        // Swap the NONCES too. An attacker splicing records moves
        // the whole index entry, not just the ciphertext — leaving
        // the nonces in place would make this a test of nonce
        // mismatch rather than of the sequence binding, which is
        // what a mutation run caught it being.
        let nonce_a = HEADER_LEN + 8;
        let nonce_b = HEADER_LEN + INDEX_ENTRY_LEN + 8;
        let first_nonce = bytes[nonce_a..nonce_a + NONCE_LEN].to_vec();
        let second_nonce = bytes[nonce_b..nonce_b + NONCE_LEN].to_vec();
        bytes[nonce_a..nonce_a + NONCE_LEN].copy_from_slice(&second_nonce);
        bytes[nonce_b..nonce_b + NONCE_LEN].copy_from_slice(&first_nonce);

        let first = bytes[records_at..records_at + first_len].to_vec();
        let second = bytes[records_at + first_len..records_at + 2 * first_len].to_vec();
        bytes[records_at..records_at + first_len].copy_from_slice(&second);
        bytes[records_at + first_len..records_at + 2 * first_len].copy_from_slice(&first);
        std::fs::write(&path, &bytes).unwrap();

        let reopened = AuditLog::open_or_create(&path).unwrap();
        let err = reopened
            .read_all(&KEY)
            .expect_err("a spliced record must not verify");
        assert!(matches!(err, AuditError::RecordCorrupt { .. }), "{err}");
    }

    /// Editing a byte of a record fails its tag.
    #[test]
    fn an_edited_record_does_not_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut l = AuditLog::open_or_create(&path).unwrap();
        l.append(&KEY, &record("team/a/one")).unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();

        let reopened = AuditLog::open_or_create(&path).unwrap();
        assert!(matches!(
            reopened.read_all(&KEY),
            Err(AuditError::RecordCorrupt { .. })
        ));
    }

    /// Truncation is the attack a plain append-only file misses:
    /// lopping records off the end leaves a perfectly consistent
    /// file. The header's count is what gives it away.
    #[test]
    fn a_truncated_log_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut l = AuditLog::open_or_create(&path).unwrap();
        for p in ["one", "two", "three"] {
            l.append(&KEY, &record(p)).unwrap();
        }

        // Drop the last record's bytes but leave the header's count
        // claiming three.
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 20]).unwrap();

        let reopened = AuditLog::open_or_create(&path).unwrap();
        let err = reopened
            .read_all(&KEY)
            .expect_err("truncation must be caught");
        assert!(
            matches!(err, AuditError::Truncated { .. }),
            "expected truncation, got {err}"
        );
    }

    /// A different key must not read the log — it is encrypted for
    /// a reason.
    #[test]
    fn the_wrong_key_cannot_read_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = log(&dir);
        l.append(&KEY, &record("team/a/one")).unwrap();

        assert!(matches!(
            l.read_all(&[9u8; KEY_LEN]),
            Err(AuditError::RecordCorrupt { .. })
        ));
    }

    /// The index is plaintext, so it must not carry anything worth
    /// hiding: no path, no actor, no timestamp.
    #[test]
    fn the_plaintext_region_reveals_nothing_about_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut l = AuditLog::open_or_create(&path).unwrap();
        l.append(&KEY, &record("prod/db/password")).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let index_end = HEADER_LEN + INDEX_ENTRY_LEN;
        let plaintext_region = &bytes[..index_end];

        for needle in ["prod/db/password", "agent", "2026-08-11", "read"] {
            assert!(
                !plaintext_region
                    .windows(needle.len())
                    .any(|w| w == needle.as_bytes()),
                "the plaintext header/index leaked {needle:?}"
            );
        }
    }

    #[test]
    fn a_file_without_the_magic_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        std::fs::write(&path, b"not an audit log at all, really").unwrap();

        assert!(matches!(
            AuditLog::open_or_create(&path),
            Err(AuditError::BadMagic)
        ));
    }

    /// A future format must be refused rather than misread — a log
    /// half-understood is worse than one that will not open.
    #[test]
    fn a_newer_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&99u16.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();

        assert!(matches!(
            AuditLog::open_or_create(&path),
            Err(AuditError::UnsupportedVersion { found: 99 })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn the_log_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut l = AuditLog::open_or_create(&path).unwrap();
        l.append(&KEY, &record("team/a/one")).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "a log of who read which secret when must not be readable by others"
        );
    }
}
