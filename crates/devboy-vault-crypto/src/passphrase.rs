//! Passphrase envelope per [ADR-023] §3.2.
//!
//! Wraps the 32-byte vault key under a key derived from the user's
//! passphrase via Argon2id. Adding a passphrase envelope is the
//! mandatory first step of [`crate::format::VaultFile`] creation —
//! every other unlock method (Touch ID, BIP39 recovery) is optional
//! and bolted on later by adding more entries to
//! [`crate::format::VaultFile::envelopes`].
//!
//! # Algorithm
//!
//! - **KDF.** Argon2id, version 1.3 (`Version::V0x13`), with
//!   `m_cost`, `t_cost`, `p_cost` taken from the envelope's stored
//!   parameters. The default ([`EnvelopeKdfParams::DEFAULT`]) is
//!   `m=64 MiB, t=3, p=1` — the OWASP-recommended baseline cited in
//!   ADR-023 §3.2 and tuned to ~250 ms on a 2024-class laptop.
//! - **Wrap.** XChaCha20-Poly1305 from [`crate::aead`], with
//!   `aad = "devboy-vault-envelope:passphrase:v1"` so a swap attack
//!   that moves a Recovery wrapped-key into a Passphrase envelope
//!   slot fails authentication.
//! - **Layout.** The envelope's `wrapped_key` field is
//!   `base64-no-pad(nonce || ciphertext)` per the ADR's "single
//!   base64 string per envelope" shape.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::num::NonZeroU32;

use argon2::{Algorithm, Argon2, Params, Version};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::aead::{self, AeadError, KEY_LEN};
use crate::format::{Envelope, EnvelopeKdfParams, b64_decode, b64_encode};

/// AAD bound into the passphrase envelope's AEAD wrap. Picked so that
/// a wrapped key from a Recovery envelope cannot decrypt under a
/// Passphrase envelope's slot (the AEAD authenticates the AAD bytes
/// alongside the ciphertext; mismatched AAD → auth failure).
pub const PASSPHRASE_ENVELOPE_AAD: &str = "devboy-vault-envelope:passphrase:v1";

/// Default Argon2id parameters for new passphrase envelopes.
/// Matches [`crate::format::KdfParams::DEFAULT`] but reshaped into the
/// `{ m, t, p }` form the on-disk envelope uses.
pub const DEFAULT_KDF_PARAMS: EnvelopeKdfParams = EnvelopeKdfParams {
    m: 64 * 1024,
    t: 3,
    p: 1,
};

// =============================================================================
// Errors
// =============================================================================

/// Failure modes for passphrase-envelope operations.
#[derive(Debug, Error)]
pub enum PassphraseError {
    /// `params.m_cost`, `params.t_cost`, or `params.p_cost` is outside
    /// what the underlying Argon2 implementation accepts.
    #[error("Argon2id parameters are invalid: {0}")]
    Argon2InvalidParams(argon2::Error),

    /// The KDF refused to derive bytes (e.g. allocation failed
    /// because `m_cost` was too large for the current host).
    #[error("Argon2id key derivation failed: {0}")]
    Argon2KdfFailed(argon2::Error),

    /// `argon2_salt` or `wrapped_key` is not valid base64.
    #[error("envelope payload is not valid base64: {0}")]
    Base64Decode(#[from] base64::DecodeError),

    /// `argon2_salt` decoded to fewer / more bytes than the salt this
    /// crate accepts (must be 32 bytes per [`crate::format::Header::salt`]).
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

    /// The supplied `Envelope` is not [`Envelope::Passphrase`]. Caller
    /// passed a Recovery or Keychain variant by mistake.
    #[error("envelope kind '{kind}' is not 'passphrase'")]
    WrongKind {
        /// The variant the caller actually supplied.
        kind: &'static str,
    },

    /// AEAD wrap or unwrap operation failed. For [`unwrap_passphrase`]
    /// this is the expected signal of a wrong passphrase or a tampered
    /// envelope. The variant is intentionally opaque (matches the
    /// [`AeadError::AeadFailed`] policy in [`crate::aead`]).
    #[error("AEAD operation failed (wrong passphrase, tampered envelope, or wrong AAD)")]
    AeadFailed,

    /// The OS-level CSPRNG ([`getrandom`]) refused to produce bytes
    /// during nonce generation. Threaded through from
    /// [`AeadError::RngUnavailable`].
    #[error("failed to generate a random nonce for envelope wrap: {0}")]
    RngUnavailable(getrandom::Error),
}

impl From<AeadError> for PassphraseError {
    fn from(e: AeadError) -> Self {
        match e {
            AeadError::AeadFailed => Self::AeadFailed,
            AeadError::RngUnavailable(rng_err) => Self::RngUnavailable(rng_err),
        }
    }
}

// =============================================================================
// KDF
// =============================================================================

/// Derive a 32-byte AEAD wrap key from `passphrase` and `salt` using
/// Argon2id with the supplied parameters.
///
/// The returned bytes are wrapped in [`Zeroizing`] so the buffer is
/// erased from RAM when the caller drops it; the wrap key is only
/// useful for the immediate AEAD operation that follows.
pub fn derive_wrap_key(
    passphrase: &SecretString,
    salt: &[u8],
    params: &EnvelopeKdfParams,
) -> Result<Zeroizing<[u8; KEY_LEN]>, PassphraseError> {
    let argon_params = Params::new(params.m, params.t, params.p, Some(KEY_LEN))
        .map_err(PassphraseError::Argon2InvalidParams)?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);

    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    argon
        .hash_password_into(passphrase.expose_secret().as_bytes(), salt, out.as_mut())
        .map_err(PassphraseError::Argon2KdfFailed)?;
    Ok(out)
}

// =============================================================================
// Envelope create / unwrap
// =============================================================================

/// Wrap `vault_key` in a passphrase envelope.
///
/// The envelope is fully self-contained — it carries the salt, the
/// Argon2id parameters, and the wrapped key — so a future call to
/// [`unwrap_passphrase`] does not need any side inputs other than the
/// envelope itself plus the passphrase.
pub fn create_passphrase_envelope(
    vault_key: &[u8; KEY_LEN],
    passphrase: &SecretString,
    salt: [u8; 32],
    params: EnvelopeKdfParams,
) -> Result<Envelope, PassphraseError> {
    let wrap_key = derive_wrap_key(passphrase, &salt, &params)?;
    let packed = aead::encrypt_packed(&wrap_key, PASSPHRASE_ENVELOPE_AAD, vault_key.as_ref())?;
    Ok(Envelope::Passphrase {
        argon2_salt: b64_encode(&salt),
        argon2_params: params,
        wrapped_key: b64_encode(&packed),
    })
}

/// Unwrap a passphrase envelope and return the vault key.
///
/// Returns [`PassphraseError::AeadFailed`] for any of: wrong
/// passphrase, tampered envelope, wrong AAD (i.e. someone tried to
/// pass a Recovery wrapped_key as a Passphrase envelope payload).
/// The variant is intentionally opaque — see the policy on
/// [`AeadError::AeadFailed`].
pub fn unwrap_passphrase(
    envelope: &Envelope,
    passphrase: &SecretString,
) -> Result<Zeroizing<[u8; KEY_LEN]>, PassphraseError> {
    let (argon2_salt, argon2_params, wrapped_key) = match envelope {
        Envelope::Passphrase {
            argon2_salt,
            argon2_params,
            wrapped_key,
        } => (argon2_salt, argon2_params, wrapped_key),
        Envelope::Keychain { .. } => return Err(PassphraseError::WrongKind { kind: "keychain" }),
        Envelope::Recovery { .. } => return Err(PassphraseError::WrongKind { kind: "recovery" }),
    };

    let salt_bytes = b64_decode(argon2_salt)?;
    if salt_bytes.len() != 32 {
        return Err(PassphraseError::SaltLength {
            got: salt_bytes.len(),
        });
    }
    let packed = b64_decode(wrapped_key)?;

    let wrap_key = derive_wrap_key(passphrase, &salt_bytes, argon2_params)?;
    let plaintext = aead::decrypt_packed(&wrap_key, PASSPHRASE_ENVELOPE_AAD, &packed)?;

    if plaintext.len() != KEY_LEN {
        return Err(PassphraseError::VaultKeyLength {
            got: plaintext.len(),
        });
    }
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    out.copy_from_slice(&plaintext);
    Ok(out)
}

/// Reserved for future cross-checks against `argon2`-version drift.
#[allow(dead_code)]
const _ARGON2_VERSION_TAG: NonZeroU32 = match NonZeroU32::new(0x13) {
    Some(v) => v,
    None => panic!("0x13 != 0"),
};

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Fast Argon2 params for tests. The crate's `Params::new` enforces
    /// minimums (`m_cost >= 8 KiB` and `t_cost >= 1`), so this is the
    /// floor.
    fn fast_params() -> EnvelopeKdfParams {
        EnvelopeKdfParams { m: 8, t: 1, p: 1 }
    }

    fn fixed_vault_key() -> [u8; KEY_LEN] {
        let mut k = [0u8; KEY_LEN];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i + 0x10) as u8;
        }
        k
    }

    fn fixed_salt() -> [u8; 32] {
        let mut s = [0u8; 32];
        for (i, b) in s.iter_mut().enumerate() {
            *b = (i + 0x80) as u8;
        }
        s
    }

    fn passphrase(s: &str) -> SecretString {
        SecretString::from(s.to_owned())
    }

    // -- KDF -----------------------------------------------------------------

    #[test]
    fn derive_wrap_key_is_deterministic_for_same_inputs() {
        let pw = passphrase("correct horse battery staple");
        let salt = fixed_salt();
        let p = fast_params();
        let a = derive_wrap_key(&pw, &salt, &p).unwrap();
        let b = derive_wrap_key(&pw, &salt, &p).unwrap();
        assert_eq!(a.as_ref(), b.as_ref());
    }

    #[test]
    fn derive_wrap_key_differs_when_salt_differs() {
        let pw = passphrase("same passphrase");
        let p = fast_params();
        let a = derive_wrap_key(&pw, &[0xAA; 32], &p).unwrap();
        let b = derive_wrap_key(&pw, &[0xBB; 32], &p).unwrap();
        assert_ne!(a.as_ref(), b.as_ref());
    }

    #[test]
    fn derive_wrap_key_differs_when_passphrase_differs() {
        let salt = fixed_salt();
        let p = fast_params();
        let a = derive_wrap_key(&passphrase("alpha"), &salt, &p).unwrap();
        let b = derive_wrap_key(&passphrase("beta"), &salt, &p).unwrap();
        assert_ne!(a.as_ref(), b.as_ref());
    }

    #[test]
    fn derive_wrap_key_rejects_invalid_params() {
        let pw = passphrase("x");
        // m_cost = 0 is below the Argon2 minimum.
        let bad = EnvelopeKdfParams { m: 0, t: 1, p: 1 };
        let err = derive_wrap_key(&pw, &fixed_salt(), &bad).unwrap_err();
        assert!(matches!(err, PassphraseError::Argon2InvalidParams(_)));
    }

    // -- Envelope round trip -------------------------------------------------

    #[test]
    fn create_then_unwrap_recovers_vault_key() {
        let vk = fixed_vault_key();
        let pw = passphrase("strong passphrase 1234567890");
        let env = create_passphrase_envelope(&vk, &pw, fixed_salt(), fast_params()).unwrap();
        let unwrapped = unwrap_passphrase(&env, &pw).unwrap();
        assert_eq!(unwrapped.as_ref(), &vk);
    }

    #[test]
    fn create_returns_passphrase_variant_with_b64_fields() {
        let env = create_passphrase_envelope(
            &fixed_vault_key(),
            &passphrase("x"),
            fixed_salt(),
            fast_params(),
        )
        .unwrap();
        match env {
            Envelope::Passphrase {
                argon2_salt,
                argon2_params,
                wrapped_key,
            } => {
                // Salt round-trips through base64.
                let decoded_salt = b64_decode(&argon2_salt).unwrap();
                assert_eq!(decoded_salt, fixed_salt());
                // Params preserved.
                assert_eq!(argon2_params, fast_params());
                // wrapped_key is at least nonce + 32-byte payload + tag = 24+32+16 = 72 base64-no-pad ≈ 96 chars.
                assert!(wrapped_key.len() >= 90);
            }
            other => panic!("expected Envelope::Passphrase, got {other:?}"),
        }
    }

    // -- Negative cases ------------------------------------------------------

    #[test]
    fn unwrap_with_wrong_passphrase_fails() {
        let env = create_passphrase_envelope(
            &fixed_vault_key(),
            &passphrase("right one"),
            fixed_salt(),
            fast_params(),
        )
        .unwrap();
        let err = unwrap_passphrase(&env, &passphrase("wrong one")).unwrap_err();
        assert!(matches!(err, PassphraseError::AeadFailed));
    }

    #[test]
    fn unwrap_rejects_keychain_envelope() {
        let env = Envelope::Keychain {
            keychain_account: "x".to_owned(),
            wrapped_key: b64_encode(&[0; 64]),
        };
        let err = unwrap_passphrase(&env, &passphrase("x")).unwrap_err();
        match err {
            PassphraseError::WrongKind { kind } => assert_eq!(kind, "keychain"),
            other => panic!("expected WrongKind, got {other:?}"),
        }
    }

    #[test]
    fn unwrap_rejects_recovery_envelope() {
        let env = Envelope::Recovery {
            bip39_salt: b64_encode(&[0; 32]),
            wrapped_key: b64_encode(&[0; 64]),
        };
        let err = unwrap_passphrase(&env, &passphrase("x")).unwrap_err();
        assert!(matches!(
            err,
            PassphraseError::WrongKind { kind: "recovery" }
        ));
    }

    #[test]
    fn unwrap_rejects_bad_base64_in_salt() {
        let env = Envelope::Passphrase {
            argon2_salt: "not!valid?b64".to_owned(),
            argon2_params: fast_params(),
            wrapped_key: b64_encode(&[0; 64]),
        };
        let err = unwrap_passphrase(&env, &passphrase("x")).unwrap_err();
        assert!(matches!(err, PassphraseError::Base64Decode(_)));
    }

    #[test]
    fn unwrap_rejects_short_salt() {
        // Salt that decodes to <32 bytes is invalid per the file
        // format; the unwrap function rejects it before even running
        // the KDF.
        let env = Envelope::Passphrase {
            argon2_salt: b64_encode(&[0; 16]), // 16 bytes, expected 32
            argon2_params: fast_params(),
            wrapped_key: b64_encode(&[0; 64]),
        };
        let err = unwrap_passphrase(&env, &passphrase("x")).unwrap_err();
        match err {
            PassphraseError::SaltLength { got } => assert_eq!(got, 16),
            other => panic!("expected SaltLength, got {other:?}"),
        }
    }

    #[test]
    fn unwrap_rejects_tampered_wrapped_key() {
        let mut env = create_passphrase_envelope(
            &fixed_vault_key(),
            &passphrase("p"),
            fixed_salt(),
            fast_params(),
        )
        .unwrap();
        if let Envelope::Passphrase {
            ref mut wrapped_key,
            ..
        } = env
        {
            // Flip a character that's part of the b64 alphabet to
            // ensure decode succeeds but the AEAD then fails.
            let mut bytes = b64_decode(wrapped_key).unwrap();
            let mid = bytes.len() / 2;
            bytes[mid] ^= 0x01;
            *wrapped_key = b64_encode(&bytes);
        }
        let err = unwrap_passphrase(&env, &passphrase("p")).unwrap_err();
        assert!(matches!(err, PassphraseError::AeadFailed));
    }

    #[test]
    fn unwrap_rejects_recovery_aad_swap() {
        // Build a wrapped_key under the *recovery* AAD then put it
        // into a Passphrase envelope. Decryption must fail because
        // the AAD is bound into the AEAD.
        let pw = passphrase("p");
        let salt = fixed_salt();
        let wrap_key = derive_wrap_key(&pw, &salt, &fast_params()).unwrap();
        let recovery_packed = aead::encrypt_packed(
            &wrap_key,
            "devboy-vault-envelope:recovery:v1",
            &fixed_vault_key(),
        )
        .unwrap();

        let env = Envelope::Passphrase {
            argon2_salt: b64_encode(&salt),
            argon2_params: fast_params(),
            wrapped_key: b64_encode(&recovery_packed),
        };
        let err = unwrap_passphrase(&env, &pw).unwrap_err();
        assert!(matches!(err, PassphraseError::AeadFailed));
    }

    // -- Defaults ------------------------------------------------------------

    #[test]
    fn default_kdf_params_match_owasp_recommendations() {
        assert_eq!(DEFAULT_KDF_PARAMS.m, 64 * 1024);
        assert_eq!(DEFAULT_KDF_PARAMS.t, 3);
        assert_eq!(DEFAULT_KDF_PARAMS.p, 1);
    }
}
