//! XChaCha20-Poly1305 AEAD wrapper for per-entry encryption per
//! [ADR-023] §3.1.
//!
//! # Algorithm choice
//!
//! XChaCha20-Poly1305 (RFC 8439 ChaCha20 extended via the
//! XSalsa20-style HChaCha20 nonce derivation) was picked over
//! AES-GCM and over the IETF 12-byte-nonce ChaCha20-Poly1305 variant
//! per ADR-023:
//!
//! - **Constant-time on every supported target**, including ARM
//!   Linux without AES-NI.
//! - **24-byte random nonces** are safe to generate without a counter:
//!   the birthday bound for nonce collision is `2^96`, vs. `2^32` for
//!   the 12-byte IETF variant. The vault re-encrypts on every write
//!   (each entry gets a fresh nonce); under that pattern the 12-byte
//!   nonce would be at risk after `~2^32` entry writes per key.
//! - RustCrypto provides a vetted pure-Rust implementation
//!   ([`chacha20poly1305::XChaCha20Poly1305`]).
//!
//! # AAD = path
//!
//! Per ADR-023 §3.1 *Critical invariants*: the entry's path bytes
//! are passed as Associated Authenticated Data. A swap attack that
//! moves the ciphertext blob from `team/gitlab/...` under the index
//! entry for `personal/github/...` fails decryption because the AAD
//! no longer matches what the encrypter committed to.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use thiserror::Error;

// =============================================================================
// Constants
// =============================================================================

/// Length in bytes of the symmetric key (XChaCha20 = 256 bits).
pub const KEY_LEN: usize = 32;

/// Length in bytes of the per-entry nonce (XChaCha20 = 192 bits).
pub const NONCE_LEN: usize = 24;

/// Length in bytes of the Poly1305 authentication tag appended to the
/// ciphertext by [`encrypt_entry`].
pub const TAG_LEN: usize = 16;

// =============================================================================
// Errors
// =============================================================================

/// Failure modes for [`encrypt_entry`] / [`decrypt_entry`].
#[derive(Debug, Error)]
pub enum AeadError {
    /// The OS-level CSPRNG ([`getrandom`]) refused to produce bytes —
    /// extremely unusual but worth surfacing rather than panicking.
    #[error("failed to generate a random nonce: {0}")]
    RngUnavailable(#[from] getrandom::Error),

    /// The underlying AEAD operation rejected the inputs. For
    /// [`encrypt_entry`] this is a low-probability internal error
    /// (the crate guarantees correct-shape outputs given correct-shape
    /// inputs); for [`decrypt_entry`] this is the *expected* signal of
    /// a tampered ciphertext / wrong key / wrong AAD / wrong nonce.
    /// The variant is intentionally opaque so the caller cannot
    /// distinguish a swap attack from a wrong key — both are
    /// equivalent failures and exposing the difference would leak a
    /// side channel.
    #[error("AEAD operation failed (decryption rejected the input)")]
    AeadFailed,
}

// =============================================================================
// Output of `encrypt_entry`
// =============================================================================

/// Result of encrypting a single entry.
///
/// `ciphertext` already includes the 16-byte Poly1305 tag at the end
/// (the standard RustCrypto convention); `nonce` is the per-entry
/// random value the caller should store alongside the ciphertext so
/// decryption can find it. The tuple matches the
/// [`crate::format::EntryMeta`] layout — `nonce` lives in the
/// plaintext index, `ciphertext` lives in the AEAD blob region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedEntry {
    /// Per-entry random nonce.
    pub nonce: [u8; NONCE_LEN],
    /// AEAD ciphertext including the trailing Poly1305 tag.
    pub ciphertext: Vec<u8>,
}

// =============================================================================
// Public API
// =============================================================================

/// Encrypt `plaintext` under `key`, binding the result to `path` via
/// AAD. Generates a fresh 24-byte random nonce on every call.
///
/// # Errors
///
/// Returns [`AeadError::RngUnavailable`] if the OS CSPRNG is broken,
/// or [`AeadError::AeadFailed`] if the underlying primitive rejects
/// the inputs (extremely unlikely for the encrypt direction).
pub fn encrypt_entry(
    key: &[u8; KEY_LEN],
    path: &str,
    plaintext: &[u8],
) -> Result<EncryptedEntry, AeadError> {
    let nonce = random_nonce()?;
    let ciphertext = encrypt_with_nonce(key, &nonce, path, plaintext)?;
    Ok(EncryptedEntry { nonce, ciphertext })
}

/// Decrypt `ciphertext` (which must include the trailing Poly1305 tag)
/// under `key`, with `path` as the expected AAD and `nonce` as the
/// per-entry nonce.
///
/// # Errors
///
/// Returns [`AeadError::AeadFailed`] for any of: wrong key, wrong AAD
/// (i.e. a swap attack), wrong nonce, or tampered ciphertext. The
/// variant is intentionally opaque to avoid leaking which condition
/// failed.
pub fn decrypt_entry(
    key: &[u8; KEY_LEN],
    path: &str,
    nonce: &[u8; NONCE_LEN],
    ciphertext: &[u8],
) -> Result<Vec<u8>, AeadError> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let xnonce = XNonce::from_slice(nonce);
    cipher
        .decrypt(
            xnonce,
            Payload {
                msg: ciphertext,
                aad: path.as_bytes(),
            },
        )
        .map_err(|_| AeadError::AeadFailed)
}

/// Source 24 bytes of CSPRNG output for a fresh nonce.
///
/// Exposed so the caller can pre-generate nonces for benchmarks and
/// for the round-trip tests that assert nonces are non-deterministic.
pub fn random_nonce() -> Result<[u8; NONCE_LEN], getrandom::Error> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce)?;
    Ok(nonce)
}

// =============================================================================
// Internal helpers
// =============================================================================

/// Encrypt with a caller-supplied nonce. Used by [`encrypt_entry`] for
/// the random-nonce case and by tests for deterministic verification
/// (e.g. the wrong-AAD / wrong-nonce checks).
fn encrypt_with_nonce(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    path: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>, AeadError> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let xnonce = XNonce::from_slice(nonce);
    cipher
        .encrypt(
            xnonce,
            Payload {
                msg: plaintext,
                aad: path.as_bytes(),
            },
        )
        .map_err(|_| AeadError::AeadFailed)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_key() -> [u8; KEY_LEN] {
        let mut k = [0u8; KEY_LEN];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        k
    }

    fn fixed_nonce() -> [u8; NONCE_LEN] {
        let mut n = [0u8; NONCE_LEN];
        for (i, b) in n.iter_mut().enumerate() {
            *b = (i + 1) as u8;
        }
        n
    }

    // -- Round trip ----------------------------------------------------------

    #[test]
    fn roundtrip_with_random_nonce() {
        let key = fixed_key();
        let path = "team/gitlab/token-deploy";
        let plaintext = b"glpat-abcdefghij_KLMNOPQRSTU";

        let enc = encrypt_entry(&key, path, plaintext).unwrap();
        // Ciphertext should have the 16-byte Poly1305 tag appended.
        assert_eq!(enc.ciphertext.len(), plaintext.len() + TAG_LEN);

        let decrypted = decrypt_entry(&key, path, &enc.nonce, &enc.ciphertext).unwrap();
        assert_eq!(decrypted.as_slice(), plaintext);
    }

    #[test]
    fn roundtrip_empty_plaintext() {
        let key = fixed_key();
        let enc = encrypt_entry(&key, "a/b/c", b"").unwrap();
        // Even an empty plaintext produces a 16-byte tag.
        assert_eq!(enc.ciphertext.len(), TAG_LEN);
        let decrypted = decrypt_entry(&key, "a/b/c", &enc.nonce, &enc.ciphertext).unwrap();
        assert!(decrypted.is_empty());
    }

    #[test]
    fn roundtrip_large_plaintext() {
        // 1 MiB to exercise the streaming behaviour of the underlying
        // XChaCha20 implementation. Vault entries will normally be
        // far smaller, but the algorithm has no upper bound and we
        // want to be sure it works end-to-end.
        let key = fixed_key();
        let plaintext = vec![0xAB; 1024 * 1024];
        let enc = encrypt_entry(&key, "team/large/blob", &plaintext).unwrap();
        let decrypted =
            decrypt_entry(&key, "team/large/blob", &enc.nonce, &enc.ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    // -- Authentication failures --------------------------------------------

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let key = fixed_key();
        let mut wrong_key = fixed_key();
        wrong_key[0] ^= 0xFF;
        let enc = encrypt_entry(&key, "p", b"value").unwrap();
        let err = decrypt_entry(&wrong_key, "p", &enc.nonce, &enc.ciphertext).unwrap_err();
        assert!(matches!(err, AeadError::AeadFailed));
    }

    #[test]
    fn decrypt_with_wrong_path_fails_swap_attack_mitigation() {
        // ADR-023 §3.1 *Critical invariants*: AAD = path so a swap
        // attack — moving the ciphertext blob under a different
        // index entry — fails authentication.
        let key = fixed_key();
        let enc = encrypt_entry(&key, "team/gitlab/token-deploy", b"glpat-real").unwrap();
        let err =
            decrypt_entry(&key, "personal/github/pat", &enc.nonce, &enc.ciphertext).unwrap_err();
        assert!(matches!(err, AeadError::AeadFailed));
    }

    #[test]
    fn decrypt_with_wrong_nonce_fails() {
        let key = fixed_key();
        let enc = encrypt_entry(&key, "p", b"value").unwrap();
        let mut wrong_nonce = enc.nonce;
        wrong_nonce[0] ^= 0xFF;
        let err = decrypt_entry(&key, "p", &wrong_nonce, &enc.ciphertext).unwrap_err();
        assert!(matches!(err, AeadError::AeadFailed));
    }

    #[test]
    fn decrypt_with_tampered_ciphertext_fails() {
        let key = fixed_key();
        let mut enc = encrypt_entry(&key, "p", b"value-of-non-trivial-length").unwrap();
        // Flip one bit in the middle of the ciphertext (still inside
        // the message portion, before the tag).
        let mid = enc.ciphertext.len() / 2;
        enc.ciphertext[mid] ^= 0x01;
        let err = decrypt_entry(&key, "p", &enc.nonce, &enc.ciphertext).unwrap_err();
        assert!(matches!(err, AeadError::AeadFailed));
    }

    #[test]
    fn decrypt_with_tampered_tag_fails() {
        let key = fixed_key();
        let mut enc = encrypt_entry(&key, "p", b"v").unwrap();
        let last = enc.ciphertext.len() - 1;
        enc.ciphertext[last] ^= 0x01;
        let err = decrypt_entry(&key, "p", &enc.nonce, &enc.ciphertext).unwrap_err();
        assert!(matches!(err, AeadError::AeadFailed));
    }

    #[test]
    fn decrypt_truncated_ciphertext_fails() {
        // Ciphertext shorter than the 16-byte tag must fail.
        let key = fixed_key();
        let nonce = fixed_nonce();
        let too_short = vec![0u8; TAG_LEN - 1];
        let err = decrypt_entry(&key, "p", &nonce, &too_short).unwrap_err();
        assert!(matches!(err, AeadError::AeadFailed));
    }

    // -- Determinism / non-determinism --------------------------------------

    #[test]
    fn random_nonces_do_not_repeat_across_many_calls() {
        // The birthday bound for 24-byte random values is 2^96; over
        // 1 000 calls the empirical collision probability is 0. If
        // this test ever flakes, getrandom is broken.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let n = random_nonce().unwrap();
            assert!(seen.insert(n), "nonce collision after {} draws", seen.len());
        }
    }

    #[test]
    fn encrypt_with_same_inputs_produces_distinct_outputs() {
        // Different nonces → different ciphertexts even when key,
        // path, and plaintext are identical.
        let key = fixed_key();
        let a = encrypt_entry(&key, "p", b"v").unwrap();
        let b = encrypt_entry(&key, "p", b"v").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn deterministic_with_explicit_nonce() {
        // Internal helper exposed via the test module — same inputs
        // yield the same output, which is the property RustCrypto's
        // implementation guarantees and what the AAD/nonce binding
        // tests above rely on.
        let key = fixed_key();
        let nonce = fixed_nonce();
        let a = encrypt_with_nonce(&key, &nonce, "p", b"v").unwrap();
        let b = encrypt_with_nonce(&key, &nonce, "p", b"v").unwrap();
        assert_eq!(a, b);
    }

    // -- Constants ----------------------------------------------------------

    #[test]
    fn constants_match_xchacha20_poly1305() {
        assert_eq!(KEY_LEN, 32);
        assert_eq!(NONCE_LEN, 24);
        assert_eq!(TAG_LEN, 16);
    }
}
