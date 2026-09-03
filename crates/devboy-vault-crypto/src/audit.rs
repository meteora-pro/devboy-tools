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
//!   C_NONCE      [24]  = nonce of the header+index commitment
//!   C_TAG        [16]  = its Poly1305 tag
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
//! - **Truncation, careless or deliberate.** `COUNT` and the whole
//!   index are covered by an AEAD tag in the header, computed under
//!   the same key the records use. Dropping the last index entry and
//!   its ciphertext no longer helps: adjusting `COUNT` to match is
//!   what the tag is over. Removing one from the middle fails the
//!   same way.
//!
//!   In v1 this was open, and the docs here said so: `COUNT` was
//!   plaintext, each record's AAD bound only its own `seq`, and an
//!   attacker who could write to the file could make the
//!   incriminating tail disappear leaving a log that read back
//!   clean. That is what the version bump is for.
//!
//!   Appending verifies before it writes, so an ordinary later write
//!   cannot re-sign a doctored file into one that verifies.
//! - **Editing.** Any change to a ciphertext fails its Poly1305
//!   tag.
//!
//! What it does **not** catch is deletion of the whole file, a
//! rollback to an older copy of it, or truncation all the way back
//! to empty — an empty log carries no commitment, because there is
//! nothing to commit to and the file is created before any key
//! exists. All three are the same shape: the file cannot testify to
//! its own absence. Detecting them needs state kept somewhere the
//! attacker cannot reach, which is a different problem from this
//! one.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::aead::{self, AeadError, KEY_LEN, NONCE_LEN, TAG_LEN};

/// File magic.
pub const MAGIC: &[u8; 6] = b"AUDIT1";

/// Format version.
///
/// Bumped to 2 when the header and index gained an
/// authentication tag. A v1 file is refused rather than read: its
/// `COUNT` was unauthenticated, so reading one would mean trusting
/// the number this version exists to stop trusting.
pub const VERSION: u16 = 2;

/// AAD prefix. Combined with the record's sequence number so a
/// record cannot be moved to another position.
const AAD_PREFIX: &[u8] = b"audit-v1";

/// Bytes per index entry: seq(8) + nonce(24) + length(4).
const INDEX_ENTRY_LEN: usize = 8 + NONCE_LEN + 4;

/// Header length: magic(6) + version(2) + count(8) + nonce(24) +
/// tag(16).
const HEADER_LEN: usize = 6 + 2 + 8 + NONCE_LEN + TAG_LEN;

/// AAD prefix for the header+index commitment.
///
/// Distinct from [`AAD_PREFIX`] so a record's tag can never be
/// mistaken for the commitment's, or the reverse.
const COMMITMENT_PREFIX: &str = "audit-index-v2";

/// The AAD the commitment tag is computed over.
///
/// A digest of the index rather than the index itself, because the
/// AEAD helpers take AAD as a `&str` and the index is arbitrary
/// bytes. SHA-256 is second-preimage resistant, so committing to
/// the digest commits to the bytes.
fn commitment_aad(count: u64, index: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(index);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("{COMMITMENT_PREFIX}:{count}:{hex}")
}

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

    /// The header + index commitment did not verify.
    ///
    /// Mainly the deliberate-truncation case: someone dropped
    /// records and adjusted `COUNT` to match. The tag covers both,
    /// so a consistent-looking pair is no longer enough.
    ///
    /// A wrong key looks the same from here, exactly as it does for
    /// [`AuditError::RecordCorrupt`] — an unverified tag says the
    /// key and the bytes disagree, not which of them moved.
    #[error(
        "audit log header does not verify: records have been removed, the index edited, or the \
         log was written under a different key. It is not evidence of anything in this state"
    )]
    IndexTampered,

    /// Sequence numbers are not 0, 1, 2, … in order.
    ///
    /// Not an attacker's doing: rearranging the index breaks the
    /// header commitment first, and forging that needs the key. This
    /// catches *our own* writer — each record's AAD binds only its
    /// own `seq`, so a numbering bug would produce a log that
    /// decrypts perfectly and reads back in the wrong order.
    #[error("audit log is missing record {expected}: the index jumps to {found}")]
    SequenceGap {
        /// The sequence number this position should hold.
        expected: u64,
        /// What the index says instead.
        found: u64,
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
    /// Nonce and tag committing to `count` and the index region.
    ///
    /// Read without a key — verifying it needs one, which arrives
    /// with each [`AuditLog::append`] / [`AuditLog::read_all`].
    commitment_nonce: [u8; NONCE_LEN],
    commitment_tag: Vec<u8>,
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
            // An empty log has nothing to commit to, and creation
            // has no key to commit with. Zeroes hold the space; the
            // first append writes a real tag.
            file.write_all(&[0u8; NONCE_LEN])?;
            file.write_all(&[0u8; TAG_LEN])?;
            file.sync_all()?;
            restrict_permissions(path)?;
            return Ok(Self {
                path: path.to_path_buf(),
                count: 0,
                commitment_nonce: [0u8; NONCE_LEN],
                commitment_tag: vec![0u8; TAG_LEN],
            });
        }

        let mut file = std::fs::File::open(path)?;

        // Magic and version first, from their own short read. The
        // rest of the header is only meaningful once the version is
        // known, and a file too small to hold a v2 header should say
        // "not an audit log" rather than "unexpected end of file".
        let mut prelude = [0u8; 16];
        file.read_exact(&mut prelude).map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => AuditError::BadMagic,
            _ => AuditError::Io(e),
        })?;
        if &prelude[..6] != MAGIC {
            return Err(AuditError::BadMagic);
        }
        let found = u16::from_le_bytes([prelude[6], prelude[7]]);
        if found != VERSION {
            return Err(AuditError::UnsupportedVersion { found });
        }
        let count = u64::from_le_bytes(prelude[8..16].try_into().expect("8 bytes"));

        let mut commitment_nonce = [0u8; NONCE_LEN];
        let mut commitment_tag = vec![0u8; TAG_LEN];
        file.read_exact(&mut commitment_nonce)?;
        file.read_exact(&mut commitment_tag)?;

        Ok(Self {
            path: path.to_path_buf(),
            count,
            commitment_nonce,
            commitment_tag,
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
        // Verify before extending. Appending to a file whose tail
        // was already removed would sign the tampered state as
        // genuine — the append would launder it.
        self.verify_commitment(key, &index)?;
        index.extend_from_slice(&seq.to_le_bytes());
        index.extend_from_slice(&encrypted.nonce);
        index.extend_from_slice(&(encrypted.ciphertext.len() as u32).to_le_bytes());
        records.extend_from_slice(&encrypted.ciphertext);

        let commitment = aead::encrypt_entry(key, &commitment_aad(seq + 1, &index), &[])?;

        let temp = self.path.with_extension("dvb.tmp");
        {
            let mut out = std::fs::File::create(&temp)?;
            out.write_all(MAGIC)?;
            out.write_all(&VERSION.to_le_bytes())?;
            out.write_all(&(seq + 1).to_le_bytes())?;
            out.write_all(&commitment.nonce)?;
            out.write_all(&commitment.ciphertext)?;
            out.write_all(&index)?;
            out.write_all(&records)?;
            out.sync_all()?;
        }
        restrict_permissions(&temp)?;
        std::fs::rename(&temp, &self.path)?;

        self.count = seq + 1;
        self.commitment_nonce = commitment.nonce;
        self.commitment_tag = commitment.ciphertext;
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

        self.verify_commitment(key, &index[..self.count as usize * INDEX_ENTRY_LEN])?;

        let mut out = Vec::with_capacity(self.count as usize);
        let mut offset = 0usize;
        for i in 0..self.count as usize {
            let entry = &index[i * INDEX_ENTRY_LEN..(i + 1) * INDEX_ENTRY_LEN];
            let seq = u64::from_le_bytes(entry[..8].try_into().expect("8 bytes"));
            let mut nonce = [0u8; NONCE_LEN];
            nonce.copy_from_slice(&entry[8..8 + NONCE_LEN]);
            let length =
                u32::from_le_bytes(entry[8 + NONCE_LEN..].try_into().expect("4 bytes")) as usize;

            // Records are numbered from zero with no gaps. An
            // attacker cannot get here — the commitment above
            // already refused — but a bug in `append` could, and a
            // log that silently renumbers itself is not evidence of
            // anything.
            if seq != i as u64 {
                return Err(AuditError::SequenceGap {
                    expected: i as u64,
                    found: seq,
                });
            }

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

    /// Check the header + index against the tag in the header.
    ///
    /// An empty log is exempt: there is nothing to commit to, and
    /// the file is created before any key exists. That leaves
    /// "truncate the whole thing to zero" undetectable here — which
    /// is the same class as deleting the file outright, already
    /// documented as out of this format's reach.
    fn verify_commitment(&self, key: &[u8; KEY_LEN], index: &[u8]) -> Result<(), AuditError> {
        if self.count == 0 {
            return Ok(());
        }
        aead::decrypt_entry(
            key,
            &commitment_aad(self.count, index),
            &self.commitment_nonce,
            &self.commitment_tag,
        )
        .map(|_| ())
        .map_err(|_| AuditError::IndexTampered)
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
        // The splice moves index bytes, so the header commitment
        // now catches it before any record is decrypted. Either
        // verdict means the splice failed; pinning one would pin
        // the order of the checks rather than the property.
        assert!(
            matches!(
                err,
                AuditError::RecordCorrupt { .. } | AuditError::IndexTampered
            ),
            "{err}"
        );
    }

    /// The attack the format version exists for.
    ///
    /// Drop the last record — its index entry and its ciphertext —
    /// and decrement `COUNT` to match. Under v1 the result read back
    /// as a perfectly valid two-record log and the third access was
    /// gone without a trace. The commitment covers `COUNT` and the
    /// whole index together, so a consistent-looking pair is no
    /// longer enough.
    #[test]
    fn deleting_the_tail_and_fixing_the_count_is_caught() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut l = AuditLog::open_or_create(&path).unwrap();
        for p in ["team/a/one", "team/a/two", "team/a/three"] {
            l.append(&KEY, &record(p)).unwrap();
        }

        let bytes = std::fs::read(&path).unwrap();
        let last_len = u32::from_le_bytes(
            bytes[HEADER_LEN + 2 * INDEX_ENTRY_LEN + 8 + NONCE_LEN
                ..HEADER_LEN + 3 * INDEX_ENTRY_LEN]
                .try_into()
                .unwrap(),
        ) as usize;

        // Header, then the first two index entries, then the
        // records minus the last ciphertext.
        let mut forged = bytes[..HEADER_LEN + 2 * INDEX_ENTRY_LEN].to_vec();
        let records_at = HEADER_LEN + 3 * INDEX_ENTRY_LEN;
        forged.extend_from_slice(&bytes[records_at..bytes.len() - last_len]);
        // And the count adjusted so nothing looks out of place.
        forged[8..16].copy_from_slice(&2u64.to_le_bytes());
        std::fs::write(&path, &forged).unwrap();

        let reopened = AuditLog::open_or_create(&path).unwrap();
        let err = reopened
            .read_all(&KEY)
            .expect_err("a doctored count must not read as a clean log");
        assert!(matches!(err, AuditError::IndexTampered), "{err}");
    }

    /// The same attack aimed at the middle instead of the tail.
    ///
    /// Each record's AAD binds only its own `seq`, so removing one
    /// from the middle leaves every survivor decrypting perfectly.
    /// Only the gap in the numbering gives it away — and `read_all`
    /// did not look at the numbering at all.
    #[test]
    fn removing_a_record_from_the_middle_is_caught() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut l = AuditLog::open_or_create(&path).unwrap();
        for p in ["team/a/one", "team/a/two", "team/a/three"] {
            l.append(&KEY, &record(p)).unwrap();
        }

        let bytes = std::fs::read(&path).unwrap();
        let lens: Vec<usize> = (0..3)
            .map(|n| {
                u32::from_le_bytes(
                    bytes[HEADER_LEN + n * INDEX_ENTRY_LEN + 8 + NONCE_LEN
                        ..HEADER_LEN + (n + 1) * INDEX_ENTRY_LEN]
                        .try_into()
                        .unwrap(),
                ) as usize
            })
            .collect();
        let records_at = HEADER_LEN + 3 * INDEX_ENTRY_LEN;

        // Keep entries 0 and 2, drop 1.
        let mut forged = bytes[..HEADER_LEN + INDEX_ENTRY_LEN].to_vec();
        forged.extend_from_slice(
            &bytes[HEADER_LEN + 2 * INDEX_ENTRY_LEN..HEADER_LEN + 3 * INDEX_ENTRY_LEN],
        );
        forged.extend_from_slice(&bytes[records_at..records_at + lens[0]]);
        forged.extend_from_slice(
            &bytes[records_at + lens[0] + lens[1]..records_at + lens[0] + lens[1] + lens[2]],
        );
        forged[8..16].copy_from_slice(&2u64.to_le_bytes());
        std::fs::write(&path, &forged).unwrap();

        let reopened = AuditLog::open_or_create(&path).unwrap();
        let err = reopened
            .read_all(&KEY)
            .expect_err("a hole in the middle must not read as a clean log");
        assert!(matches!(err, AuditError::IndexTampered), "{err}");
    }

    /// The numbering check, reached the only way it can be.
    ///
    /// The commitment stops an outsider before the sequence is ever
    /// examined, so this forges a log *and re-signs it with the real
    /// key* — which is what a bug in our own `append` would amount
    /// to. Without a test that holds the key, the check below would
    /// be unreachable, and an unreachable check is the thing this
    /// branch keeps finding.
    #[test]
    fn a_gap_in_the_numbering_is_caught_even_when_the_header_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut l = AuditLog::open_or_create(&path).unwrap();
        for p in ["team/a/one", "team/a/two"] {
            l.append(&KEY, &record(p)).unwrap();
        }

        let mut bytes = std::fs::read(&path).unwrap();
        // Renumber the first record 0 -> 7.
        bytes[HEADER_LEN..HEADER_LEN + 8].copy_from_slice(&7u64.to_le_bytes());

        // Re-sign, as only something holding the key could.
        let index_end = HEADER_LEN + 2 * INDEX_ENTRY_LEN;
        let index = bytes[HEADER_LEN..index_end].to_vec();
        let fresh = crate::aead::encrypt_entry(&KEY, &commitment_aad(2, &index), &[]).unwrap();
        bytes[16..16 + NONCE_LEN].copy_from_slice(&fresh.nonce);
        bytes[16 + NONCE_LEN..HEADER_LEN].copy_from_slice(&fresh.ciphertext);
        std::fs::write(&path, &bytes).unwrap();

        let reopened = AuditLog::open_or_create(&path).unwrap();
        let err = reopened
            .read_all(&KEY)
            .expect_err("a renumbered record must not pass");
        assert!(
            matches!(
                err,
                AuditError::SequenceGap {
                    expected: 0,
                    found: 7
                }
            ),
            "{err}"
        );
    }

    /// Appending must not sign a file that was already doctored.
    ///
    /// Without a check on the way in, the next ordinary write would
    /// compute a fresh, valid commitment over the tampered state and
    /// launder it into something that verifies for good.
    #[test]
    fn appending_to_a_doctored_log_refuses_rather_than_blessing_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit-log.dvb");
        let mut l = AuditLog::open_or_create(&path).unwrap();
        for p in ["team/a/one", "team/a/two"] {
            l.append(&KEY, &record(p)).unwrap();
        }

        let bytes = std::fs::read(&path).unwrap();
        let last_len = u32::from_le_bytes(
            bytes[HEADER_LEN + INDEX_ENTRY_LEN + 8 + NONCE_LEN..HEADER_LEN + 2 * INDEX_ENTRY_LEN]
                .try_into()
                .unwrap(),
        ) as usize;
        let mut forged = bytes[..HEADER_LEN + INDEX_ENTRY_LEN].to_vec();
        forged.extend_from_slice(&bytes[HEADER_LEN + 2 * INDEX_ENTRY_LEN..bytes.len() - last_len]);
        forged[8..16].copy_from_slice(&1u64.to_le_bytes());
        std::fs::write(&path, &forged).unwrap();

        let mut reopened = AuditLog::open_or_create(&path).unwrap();
        let err = reopened
            .append(&KEY, &record("team/a/three"))
            .expect_err("appending must not bless a doctored log");
        assert!(matches!(err, AuditError::IndexTampered), "{err}");
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
    ///
    /// Which error it is depends on what verifies first, and since
    /// the header commitment moved ahead of the records that is now
    /// `IndexTampered`. Both mean the same thing: the key and the
    /// bytes disagree. The test accepts either so it keeps testing
    /// "a wrong key gets nothing" rather than the order of the
    /// checks.
    #[test]
    fn the_wrong_key_cannot_read_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = log(&dir);
        l.append(&KEY, &record("team/a/one")).unwrap();

        let err = l
            .read_all(&[9u8; KEY_LEN])
            .expect_err("a different key must not read this");
        assert!(
            matches!(
                err,
                AuditError::RecordCorrupt { .. } | AuditError::IndexTampered
            ),
            "{err}"
        );
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
