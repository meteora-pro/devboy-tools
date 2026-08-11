//! BIP39 recovery envelope per [ADR-023] §3.2.
//!
//! Wraps the 32-byte vault key under a key derived from a 24-word
//! BIP39 mnemonic phrase. The phrase is shown to the user **once** at
//! vault creation (per ADR-023 §3.2 acknowledgement step) and is the
//! escape hatch when both passphrase and Touch ID unlocks are lost.
//!
//! # Algorithm
//!
//! - **Mnemonic.** 24 English BIP39 words (256 bits of entropy by
//!   construction). Generated via `bip39::Mnemonic::generate_in`,
//!   which uses an OS-backed CSPRNG.
//! - **Seed.** Standard BIP39 seed derivation: PBKDF2-HMAC-SHA512,
//!   2048 iterations, salt = `"mnemonic"` (no extra user passphrase
//!   per ADR-023 §3.2). Yields 64 bytes of high-entropy material.
//! - **Wrap key.** HKDF-SHA256 over the BIP39 seed with the
//!   per-envelope salt and a fixed `info` string. Why HKDF and not
//!   another Argon2id pass: the seed is already maximum-entropy
//!   (256 bits), so brute-force resistance from a memory-hard KDF is
//!   wasted; HKDF is fast and standard for "stretch high-entropy
//!   keying material into a fresh key".
//! - **AEAD.** XChaCha20-Poly1305 from [`crate::aead`] with
//!   `aad = "devboy-vault-envelope:recovery:v1"`. The kind-bound AAD
//!   prevents a swap attack that would move a Passphrase wrapped_key
//!   into a Recovery slot.
//! - **Layout.** The envelope's `wrapped_key` field is
//!   `base64-no-pad(nonce || ciphertext)` — same shape as the
//!   passphrase envelope from P3.3.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::fmt;

use bip39::{Language, Mnemonic};
use hkdf::Hkdf;
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::aead::{self, AeadError, KEY_LEN};
use crate::format::{Envelope, b64_decode, b64_encode};

/// AAD bound into the recovery envelope's AEAD wrap. Picked so that
/// a wrapped key from a Passphrase envelope cannot decrypt under a
/// Recovery envelope's slot.
pub const RECOVERY_ENVELOPE_AAD: &str = "devboy-vault-envelope:recovery:v1";

/// HKDF-SHA256 `info` parameter for recovery-key derivation. Provides
/// domain separation in case the BIP39 seed is ever reused for
/// another HKDF derivation (e.g. a future "vault-export" envelope).
pub const RECOVERY_HKDF_INFO: &[u8] = b"devboy-vault-recovery-key-v1";

/// Word count for the recovery phrase. ADR-023 §3.2 specifies 24
/// words → 256 bits of entropy.
pub const MNEMONIC_WORD_COUNT: usize = 24;

// =============================================================================
// RecoveryPhrase wrapper
// =============================================================================

/// Wrapper around a BIP39 mnemonic phrase that redacts on `Debug` and
/// zeroizes on `Drop` (via the underlying [`SecretString`]).
///
/// Construct via [`generate_recovery_phrase`] (fresh) or
/// [`parse_recovery_phrase`] (user-typed). The wrapped string is
/// validated against the BIP39 wordlist by both constructors, so
/// downstream code can rely on `expose_words()` returning a parseable
/// mnemonic.
pub struct RecoveryPhrase {
    words: SecretString,
}

impl RecoveryPhrase {
    /// Borrow the underlying space-separated word string.
    ///
    /// Use sparingly — the returned `&str` is the secret. Most
    /// callers want [`derive_recovery_key`] or [`unwrap_recovery`]
    /// instead, both of which accept a `&RecoveryPhrase` and never
    /// expose the string.
    pub fn expose_words(&self) -> &str {
        self.words.expose_secret()
    }
}

impl fmt::Debug for RecoveryPhrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryPhrase")
            .field("words", &"<redacted 24-word BIP39 phrase>")
            .finish()
    }
}

// =============================================================================
// Errors
// =============================================================================

/// Failure modes for recovery-envelope operations.
#[derive(Debug, Error)]
pub enum RecoveryError {
    /// The OS-level CSPRNG ([`getrandom`]) refused to produce bytes
    /// during mnemonic generation or AEAD nonce generation.
    #[error("CSPRNG unavailable: {0}")]
    RngUnavailable(getrandom::Error),

    /// `bip39` library rejected the mnemonic — wrong word count, an
    /// unknown word, or a bad checksum.
    #[error("invalid BIP39 mnemonic: {0}")]
    Bip39(#[from] bip39::Error),

    /// HKDF expand step refused to produce the requested key length.
    /// Should be unreachable for our 32-byte target with SHA-256
    /// (limit is 8160 bytes), but worth surfacing rather than
    /// panicking.
    #[error("HKDF expand failed (requested too many bytes)")]
    HkdfFailed,

    /// `bip39_salt` or `wrapped_key` is not valid base64.
    #[error("envelope payload is not valid base64: {0}")]
    Base64Decode(#[from] base64::DecodeError),

    /// `bip39_salt` decoded to fewer / more bytes than the salt this
    /// crate accepts (must be 32 bytes per [`crate::format::Header::salt`]
    /// length convention).
    #[error("envelope salt has wrong length: expected 32 bytes, got {got}")]
    SaltLength {
        /// Length of the decoded salt buffer.
        got: usize,
    },

    /// The envelope was decrypted successfully but yielded a buffer
    /// whose length is not 32 bytes.
    #[error("envelope yielded a vault key of wrong length: expected 32 bytes, got {got}")]
    VaultKeyLength {
        /// Length of the decrypted payload.
        got: usize,
    },

    /// The supplied `Envelope` is not [`Envelope::Recovery`].
    #[error("envelope kind '{kind}' is not 'recovery'")]
    WrongKind {
        /// The variant the caller actually supplied.
        kind: &'static str,
    },

    /// AEAD wrap or unwrap operation failed. For [`unwrap_recovery`]
    /// this is the expected signal of a wrong recovery phrase, a
    /// tampered envelope, or a wrong AAD. The variant is
    /// intentionally opaque (matches the [`AeadError::AeadFailed`]
    /// policy in [`crate::aead`]).
    #[error("AEAD operation failed (wrong phrase, tampered envelope, or wrong AAD)")]
    AeadFailed,
}

impl From<AeadError> for RecoveryError {
    fn from(e: AeadError) -> Self {
        match e {
            AeadError::AeadFailed => Self::AeadFailed,
            AeadError::RngUnavailable(rng_err) => Self::RngUnavailable(rng_err),
        }
    }
}

// =============================================================================
// Mnemonic generation / parsing
// =============================================================================

/// Generate a fresh 24-word recovery phrase from the OS CSPRNG.
///
/// Per ADR-023 §3.2 the phrase is shown to the user **once** with an
/// explicit acknowledgement; it is never persisted by the framework.
///
/// Sources 32 bytes of entropy from `getrandom` (256 bits → 24 BIP39
/// words by the standard mapping) and hands them to
/// `bip39::Mnemonic::from_entropy_in`. Going through `from_entropy`
/// rather than the crate's optional `Mnemonic::generate` keeps us off
/// the `rand` feature flag and uses the same OS CSPRNG that
/// [`crate::aead::random_nonce`] uses.
pub fn generate_recovery_phrase() -> Result<RecoveryPhrase, RecoveryError> {
    // 24 words ↔ 256 bits ↔ 32 bytes of raw entropy.
    let mut entropy = Zeroizing::new([0u8; 32]);
    getrandom::getrandom(entropy.as_mut()).map_err(RecoveryError::RngUnavailable)?;
    let mnemonic = Mnemonic::from_entropy_in(Language::English, entropy.as_ref())
        .map_err(RecoveryError::Bip39)?;
    Ok(RecoveryPhrase {
        words: SecretString::from(mnemonic.to_string()),
    })
}

/// Validate `words` against the BIP39 English wordlist and wrap as a
/// [`RecoveryPhrase`].
///
/// Whitespace is normalised by `bip39::Mnemonic::parse_in` — extra
/// spaces between words are accepted; the round-trip preserves the
/// input form so the user sees their typed text in any subsequent
/// error message.
pub fn parse_recovery_phrase(words: &str) -> Result<RecoveryPhrase, RecoveryError> {
    let _validated = Mnemonic::parse_in(Language::English, words).map_err(RecoveryError::Bip39)?;
    Ok(RecoveryPhrase {
        words: SecretString::from(words.to_owned()),
    })
}

// =============================================================================
// Recovery-key derivation
// =============================================================================

/// Derive a 32-byte AEAD wrap key from `phrase` and `salt`.
///
/// Two-stage derivation:
///
/// 1. BIP39 mnemonic → 64-byte seed (PBKDF2-HMAC-SHA512, 2048 iters,
///    salt = `"mnemonic"`).
/// 2. seed + per-envelope `salt` → 32-byte HKDF-SHA256 output.
///
/// The returned bytes are wrapped in [`Zeroizing`] so the buffer is
/// erased from RAM when the caller drops it.
pub fn derive_recovery_key(
    phrase: &RecoveryPhrase,
    salt: &[u8],
) -> Result<Zeroizing<[u8; KEY_LEN]>, RecoveryError> {
    let mnemonic = Mnemonic::parse_in(Language::English, phrase.expose_words())
        .map_err(RecoveryError::Bip39)?;
    // BIP39 spec: passphrase argument is appended to the literal
    // "mnemonic" salt for the PBKDF2 step. We use empty per ADR-023.
    let seed: [u8; 64] = mnemonic.to_seed("");

    let hkdf = Hkdf::<Sha256>::new(Some(salt), &seed);
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    hkdf.expand(RECOVERY_HKDF_INFO, out.as_mut())
        .map_err(|_| RecoveryError::HkdfFailed)?;
    Ok(out)
}

// =============================================================================
// Envelope create / unwrap
// =============================================================================

/// Wrap `vault_key` in a recovery envelope.
///
/// The envelope is fully self-contained — it carries the per-vault
/// HKDF salt and the wrapped key — so a future call to
/// [`unwrap_recovery`] needs only the envelope plus the recovery
/// phrase. (The user must keep the phrase outside the vault for
/// recovery to be possible at all; the framework never persists it.)
pub fn create_recovery_envelope(
    vault_key: &[u8; KEY_LEN],
    phrase: &RecoveryPhrase,
    salt: [u8; 32],
) -> Result<Envelope, RecoveryError> {
    let wrap_key = derive_recovery_key(phrase, &salt)?;
    let packed = aead::encrypt_packed(&wrap_key, RECOVERY_ENVELOPE_AAD, vault_key.as_ref())?;
    Ok(Envelope::Recovery {
        bip39_salt: b64_encode(&salt),
        wrapped_key: b64_encode(&packed),
    })
}

/// Unwrap a recovery envelope and return the vault key.
///
/// Returns [`RecoveryError::AeadFailed`] for any of: wrong recovery
/// phrase, tampered envelope, wrong AAD. The variant is intentionally
/// opaque — see the policy on [`AeadError::AeadFailed`].
pub fn unwrap_recovery(
    envelope: &Envelope,
    phrase: &RecoveryPhrase,
) -> Result<Zeroizing<[u8; KEY_LEN]>, RecoveryError> {
    let (bip39_salt, wrapped_key) = match envelope {
        Envelope::Recovery {
            bip39_salt,
            wrapped_key,
        } => (bip39_salt, wrapped_key),
        Envelope::Passphrase { .. } => {
            return Err(RecoveryError::WrongKind { kind: "passphrase" });
        }
        Envelope::Totp { .. } => return Err(RecoveryError::WrongKind { kind: "totp" }),
        Envelope::Keyfile { .. } => return Err(RecoveryError::WrongKind { kind: "keyfile" }),
    };

    let salt_bytes = b64_decode(bip39_salt)?;
    if salt_bytes.len() != 32 {
        return Err(RecoveryError::SaltLength {
            got: salt_bytes.len(),
        });
    }
    let packed = b64_decode(wrapped_key)?;

    let wrap_key = derive_recovery_key(phrase, &salt_bytes)?;
    let plaintext = aead::decrypt_packed(&wrap_key, RECOVERY_ENVELOPE_AAD, &packed)?;

    if plaintext.len() != KEY_LEN {
        return Err(RecoveryError::VaultKeyLength {
            got: plaintext.len(),
        });
    }
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    out.copy_from_slice(&plaintext);
    Ok(out)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passphrase::PASSPHRASE_ENVELOPE_AAD;

    fn fixed_vault_key() -> [u8; KEY_LEN] {
        let mut k = [0u8; KEY_LEN];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i + 0x20) as u8;
        }
        k
    }

    fn fixed_salt() -> [u8; 32] {
        let mut s = [0u8; 32];
        for (i, b) in s.iter_mut().enumerate() {
            *b = (i + 0x40) as u8;
        }
        s
    }

    /// A deterministic, well-formed BIP39 24-word phrase. Used so the
    /// tests are reproducible (generation uses the OS RNG and is not).
    /// All words from the official English BIP39 wordlist.
    const SAMPLE_PHRASE: &str = "abandon abandon abandon abandon abandon abandon \
        abandon abandon abandon abandon abandon abandon \
        abandon abandon abandon abandon abandon abandon \
        abandon abandon abandon abandon abandon art";

    fn sample_phrase() -> RecoveryPhrase {
        parse_recovery_phrase(SAMPLE_PHRASE).expect("sample phrase must be valid BIP39")
    }

    // -- Generation / parsing ------------------------------------------------

    #[test]
    fn generated_phrase_has_24_words() {
        let p = generate_recovery_phrase().unwrap();
        let count = p.expose_words().split_whitespace().count();
        assert_eq!(count, MNEMONIC_WORD_COUNT);
    }

    #[test]
    fn generated_phrase_is_parseable_again() {
        let p = generate_recovery_phrase().unwrap();
        let reparsed = parse_recovery_phrase(p.expose_words()).unwrap();
        assert_eq!(reparsed.expose_words(), p.expose_words());
    }

    #[test]
    fn two_generations_produce_different_phrases() {
        // The OS CSPRNG should not produce the same 256-bit value
        // twice in a row in any practical setting.
        let a = generate_recovery_phrase().unwrap();
        let b = generate_recovery_phrase().unwrap();
        assert_ne!(a.expose_words(), b.expose_words());
    }

    #[test]
    fn parse_rejects_bad_word_count() {
        let too_short = "abandon abandon abandon"; // 3 words
        assert!(matches!(
            parse_recovery_phrase(too_short).unwrap_err(),
            RecoveryError::Bip39(_)
        ));
    }

    #[test]
    fn parse_rejects_unknown_word() {
        let mut words: Vec<&str> = SAMPLE_PHRASE.split_whitespace().collect();
        words[0] = "notawordfrombip39";
        let bad = words.join(" ");
        assert!(matches!(
            parse_recovery_phrase(&bad).unwrap_err(),
            RecoveryError::Bip39(_)
        ));
    }

    #[test]
    fn parse_rejects_bad_checksum() {
        // Replace the last word ("art") with a different valid word
        // that breaks the BIP39 checksum.
        let bad = SAMPLE_PHRASE.trim_end_matches(" art").to_owned() + " able";
        assert!(matches!(
            parse_recovery_phrase(&bad).unwrap_err(),
            RecoveryError::Bip39(_)
        ));
    }

    // -- Debug redaction -----------------------------------------------------

    #[test]
    fn debug_redacts_phrase_words() {
        let p = sample_phrase();
        let dbg = format!("{p:?}");
        assert!(dbg.contains("redacted"));
        assert!(!dbg.contains("abandon"));
    }

    // -- Key derivation ------------------------------------------------------

    #[test]
    fn derive_is_deterministic_for_same_inputs() {
        let p = sample_phrase();
        let salt = fixed_salt();
        let a = derive_recovery_key(&p, &salt).unwrap();
        let b = derive_recovery_key(&p, &salt).unwrap();
        assert_eq!(a.as_ref(), b.as_ref());
    }

    #[test]
    fn derive_differs_when_salt_differs() {
        let p = sample_phrase();
        let a = derive_recovery_key(&p, &[0xAA; 32]).unwrap();
        let b = derive_recovery_key(&p, &[0xBB; 32]).unwrap();
        assert_ne!(a.as_ref(), b.as_ref());
    }

    #[test]
    fn derive_differs_when_phrase_differs() {
        let salt = fixed_salt();
        // Two different valid phrases.
        let p1 = sample_phrase();
        // Replace last word with a different valid checksum word —
        // this changes entropy bytes too.
        let p2_words = "legal winner thank year wave sausage worth useful legal \
            winner thank year wave sausage worth useful legal winner thank year \
            wave sausage worth title";
        let p2 = parse_recovery_phrase(p2_words).unwrap();

        let a = derive_recovery_key(&p1, &salt).unwrap();
        let b = derive_recovery_key(&p2, &salt).unwrap();
        assert_ne!(a.as_ref(), b.as_ref());
    }

    // -- Envelope round trip -------------------------------------------------

    #[test]
    fn create_then_unwrap_recovers_vault_key() {
        let vk = fixed_vault_key();
        let phrase = sample_phrase();
        let env = create_recovery_envelope(&vk, &phrase, fixed_salt()).unwrap();
        let unwrapped = unwrap_recovery(&env, &phrase).unwrap();
        assert_eq!(unwrapped.as_ref(), &vk);
    }

    #[test]
    fn create_returns_recovery_variant_with_b64_fields() {
        let env =
            create_recovery_envelope(&fixed_vault_key(), &sample_phrase(), fixed_salt()).unwrap();
        match env {
            Envelope::Recovery {
                bip39_salt,
                wrapped_key,
            } => {
                assert_eq!(b64_decode(&bip39_salt).unwrap(), fixed_salt());
                // wrapped_key = base64(nonce 24 + ct 32 + tag 16) = 72 raw bytes.
                let raw = b64_decode(&wrapped_key).unwrap();
                assert_eq!(raw.len(), aead::NONCE_LEN + KEY_LEN + aead::TAG_LEN);
            }
            other => panic!("expected Envelope::Recovery, got {other:?}"),
        }
    }

    #[test]
    fn roundtrip_with_freshly_generated_phrase() {
        let vk = fixed_vault_key();
        let phrase = generate_recovery_phrase().unwrap();
        let env = create_recovery_envelope(&vk, &phrase, fixed_salt()).unwrap();
        let unwrapped = unwrap_recovery(&env, &phrase).unwrap();
        assert_eq!(unwrapped.as_ref(), &vk);
    }

    // -- Negative cases ------------------------------------------------------

    #[test]
    fn unwrap_with_wrong_phrase_fails() {
        let vk = fixed_vault_key();
        let right = sample_phrase();
        let env = create_recovery_envelope(&vk, &right, fixed_salt()).unwrap();
        let wrong_words = "legal winner thank year wave sausage worth useful legal \
            winner thank year wave sausage worth useful legal winner thank year \
            wave sausage worth title";
        let wrong = parse_recovery_phrase(wrong_words).unwrap();
        let err = unwrap_recovery(&env, &wrong).unwrap_err();
        assert!(matches!(err, RecoveryError::AeadFailed));
    }

    #[test]
    fn unwrap_rejects_passphrase_envelope() {
        let env = Envelope::Passphrase {
            argon2_salt: b64_encode(&[0; 32]),
            argon2_params: crate::format::EnvelopeKdfParams { m: 8, t: 1, p: 1 },
            wrapped_key: b64_encode(&[0; 64]),
        };
        match unwrap_recovery(&env, &sample_phrase()).unwrap_err() {
            RecoveryError::WrongKind { kind } => assert_eq!(kind, "passphrase"),
            other => panic!("expected WrongKind, got {other:?}"),
        }
    }

    #[test]
    fn unwrap_rejects_totp_envelope() {
        let env = Envelope::Totp {
            totp_salt: b64_encode(&[0; 32]),
            wrapped_key: b64_encode(&[0; 64]),
        };
        assert!(matches!(
            unwrap_recovery(&env, &sample_phrase()).unwrap_err(),
            RecoveryError::WrongKind { kind: "totp" }
        ));
    }

    #[test]
    fn unwrap_rejects_short_salt() {
        let env = Envelope::Recovery {
            bip39_salt: b64_encode(&[0; 16]), // 16 bytes, expected 32
            wrapped_key: b64_encode(&[0; 64]),
        };
        match unwrap_recovery(&env, &sample_phrase()).unwrap_err() {
            RecoveryError::SaltLength { got } => assert_eq!(got, 16),
            other => panic!("expected SaltLength, got {other:?}"),
        }
    }

    #[test]
    fn unwrap_rejects_bad_base64_in_salt() {
        let env = Envelope::Recovery {
            bip39_salt: "not!valid?b64".to_owned(),
            wrapped_key: b64_encode(&[0; 64]),
        };
        assert!(matches!(
            unwrap_recovery(&env, &sample_phrase()).unwrap_err(),
            RecoveryError::Base64Decode(_)
        ));
    }

    #[test]
    fn unwrap_rejects_tampered_wrapped_key() {
        let mut env =
            create_recovery_envelope(&fixed_vault_key(), &sample_phrase(), fixed_salt()).unwrap();
        if let Envelope::Recovery {
            ref mut wrapped_key,
            ..
        } = env
        {
            let mut bytes = b64_decode(wrapped_key).unwrap();
            let mid = bytes.len() / 2;
            bytes[mid] ^= 0x01;
            *wrapped_key = b64_encode(&bytes);
        }
        assert!(matches!(
            unwrap_recovery(&env, &sample_phrase()).unwrap_err(),
            RecoveryError::AeadFailed
        ));
    }

    #[test]
    fn unwrap_rejects_passphrase_aad_swap() {
        // Build a wrapped_key under the *passphrase* AAD then put it
        // into a Recovery envelope. Decryption must fail because the
        // AAD is bound into the AEAD.
        let phrase = sample_phrase();
        let salt = fixed_salt();
        let wrap_key = derive_recovery_key(&phrase, &salt).unwrap();
        let passphrase_packed =
            aead::encrypt_packed(&wrap_key, PASSPHRASE_ENVELOPE_AAD, &fixed_vault_key()).unwrap();

        let env = Envelope::Recovery {
            bip39_salt: b64_encode(&salt),
            wrapped_key: b64_encode(&passphrase_packed),
        };
        assert!(matches!(
            unwrap_recovery(&env, &phrase).unwrap_err(),
            RecoveryError::AeadFailed
        ));
    }

    // -- Cross-checks --------------------------------------------------------

    #[test]
    fn derive_returns_32_bytes() {
        let p = sample_phrase();
        let k = derive_recovery_key(&p, &fixed_salt()).unwrap();
        assert_eq!(k.as_ref().len(), KEY_LEN);
    }

    #[test]
    fn mnemonic_word_count_constant_matches_adr() {
        // ADR-023 §3.2 specifies 24 words.
        assert_eq!(MNEMONIC_WORD_COUNT, 24);
    }
}
