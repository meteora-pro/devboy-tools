//! RFC 6238 TOTP for the agent-mediated re-unlock (ADR-024 §1).
//!
//! # What this is for, and what it is not
//!
//! TOTP here is **not** a shorter way to type the passphrase. The
//! strength of a TOTP unlock equals the strength of wherever the
//! shared secret is stored, never the ~20 bits of the code — a
//! code is always derivable from its secret. As a passphrase
//! replacement it would be strictly weaker than the passphrase.
//!
//! Its job is narrower and it can actually do it: prove that a
//! **human with a second device** approved a re-unlock, to a
//! caller that cannot derive the code itself. That property comes
//! from where the secret lives (inside the encrypted vault, then
//! in daemon memory — see the daemon crate), not from this
//! module. This module only implements the arithmetic.
//!
//! # Algorithm
//!
//! RFC 6238 with the universal defaults: HMAC-SHA1, 6 digits,
//! 30-second step. SHA-1 is not a security choice — it is what
//! every authenticator app implements, and the scheme's strength
//! rests on the 32-byte shared secret rather than on the hash's
//! collision resistance.

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::aead::{self, AeadError, KEY_LEN};
use crate::format::{Envelope, b64_decode, b64_encode};

type HmacSha1 = Hmac<Sha1>;

/// AAD bound into the TOTP envelope's AEAD wrap.
///
/// Kind-bound like the other envelopes, so a wrapped key lifted
/// from one envelope kind cannot be unwrapped as another.
pub const TOTP_ENVELOPE_AAD: &str = "devboy-vault-envelope:totp:v1";

/// HKDF `info` label for the TOTP wrap key.
pub const TOTP_HKDF_INFO: &[u8] = b"devboy-vault-totp-key-v1";

/// Seconds per TOTP step (RFC 6238 default).
pub const STEP_SECONDS: u64 = 30;

/// Digits in a generated code (RFC 6238 default).
pub const DIGITS: u32 = 6;

/// Steps of clock skew accepted on either side.
///
/// One step each way is the usual compromise: it absorbs ordinary
/// clock drift without widening the window a replayed code could
/// live in beyond ~90 seconds.
pub const SKEW_STEPS: u64 = 1;

/// Minimum shared-secret length.
///
/// RFC 4226 requires at least 128 bits; this crate generates 256
/// and refuses anything shorter than the RFC minimum, since a
/// short secret is the one parameter that actually weakens the
/// scheme.
pub const MIN_SECRET_BYTES: usize = 16;

/// Failure modes of TOTP verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TotpError {
    /// The shared secret is shorter than RFC 4226 permits.
    #[error("TOTP secret must be at least {MIN_SECRET_BYTES} bytes")]
    SecretTooShort,
    /// The submitted code is not `DIGITS` ASCII digits.
    #[error("TOTP code must be exactly {DIGITS} digits")]
    MalformedCode,
    /// A stored secret was not the base32 text this module writes.
    #[error("stored TOTP secret is not valid unpadded base32")]
    MalformedStoredSecret,
}

/// Which step a verified code belonged to.
///
/// Returned so the caller can enforce single-use: a code is only
/// as good as the replay guard that remembers it was spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedStep(pub u64);

/// The time-step containing `unix_seconds`.
pub fn step_at(unix_seconds: u64) -> u64 {
    unix_seconds / STEP_SECONDS
}

/// Generate the code for an explicit step.
///
/// Split from wall-clock time so tests can drive RFC vectors and
/// so the daemon can check neighbouring steps without a clock.
pub fn code_for_step(secret: &[u8], step: u64) -> Result<String, TotpError> {
    if secret.len() < MIN_SECRET_BYTES {
        return Err(TotpError::SecretTooShort);
    }

    let mut mac = HmacSha1::new_from_slice(secret).expect("HMAC accepts keys of any length");
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();

    // Dynamic truncation, RFC 4226 §5.3: the low nibble of the
    // last byte selects a 4-byte window, whose high bit is masked
    // off so the result is always a positive 31-bit integer.
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let binary = u32::from_be_bytes([
        digest[offset] & 0x7f,
        digest[offset + 1],
        digest[offset + 2],
        digest[offset + 3],
    ]);

    let modulus = 10u32.pow(DIGITS);
    Ok(format!(
        "{:0width$}",
        binary % modulus,
        width = DIGITS as usize
    ))
}

/// Verify `code` against `secret` at `unix_seconds`, accepting
/// [`SKEW_STEPS`] of drift.
///
/// Returns the step the code belonged to, so the caller can
/// reject a replay of that same step. Comparison is
/// constant-time; a timing oracle on a 6-digit space would let an
/// attacker recover a code digit by digit.
///
/// Every candidate step is checked even after a match, so the
/// work done does not depend on *which* step matched.
pub fn verify(secret: &[u8], code: &str, unix_seconds: u64) -> Result<VerifiedStep, TotpError> {
    if secret.len() < MIN_SECRET_BYTES {
        return Err(TotpError::SecretTooShort);
    }
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(TotpError::MalformedCode);
    }

    let current = step_at(unix_seconds);
    let mut matched: Option<u64> = None;

    for step in current.saturating_sub(SKEW_STEPS)..=current.saturating_add(SKEW_STEPS) {
        let candidate = code_for_step(secret, step)?;
        if candidate.as_bytes().ct_eq(code.as_bytes()).into() {
            // Record rather than return: an early return here
            // would make the loop's duration reveal which step
            // matched.
            matched.get_or_insert(step);
        }
    }

    matched.map(VerifiedStep).ok_or(TotpError::MalformedCode)
}

/// Render the shared secret as an `otpauth://` URI for an
/// authenticator app.
///
/// The secret is base32 without padding, which is what every
/// authenticator expects; `=` padding is rejected by several.
/// Decode a stored secret back into the key bytes.
///
/// # Why this has to exist
///
/// The key is raw bytes: [`verify`] and [`code_for_step`] take
/// `&[u8]`, and [`provisioning_uri`] base32-*encodes* them for the
/// authenticator app, which decodes and HMACs the raw bytes.
///
/// But the vault slot holding the shared secret is a `SecretString`,
/// so what gets stored is the base32 *text*. Anything reading that
/// slot must decode it before using it as a key. Skipping that step
/// yields a key made of the ASCII characters of the base32 — a
/// different key from the one in the user's phone, so every code is
/// rejected and nothing says why.
///
/// That is not hypothetical: it is exactly what the daemon did until
/// this function existed, and the tests missed it because both sides
/// of the test used the same undecoded string.
pub fn decode_secret(stored: &str) -> Result<Vec<u8>, TotpError> {
    data_encoding::BASE32_NOPAD
        .decode(stored.trim().as_bytes())
        .map_err(|_| TotpError::MalformedStoredSecret)
}

pub fn provisioning_uri(secret: &[u8], issuer: &str, account: &str) -> String {
    let encoded = data_encoding::BASE32_NOPAD.encode(secret);
    let issuer_enc = urlencode(issuer);
    let account_enc = urlencode(account);

    format!(
        "otpauth://totp/{issuer_enc}:{account_enc}?secret={encoded}&issuer={issuer_enc}\
         &algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}"
    )
}

// =============================================================================
// Envelope create / unwrap
// =============================================================================

/// Failure modes of the TOTP envelope.
#[derive(Debug, Error)]
pub enum TotpEnvelopeError {
    /// AEAD wrap or unwrap failed.
    #[error(transparent)]
    Aead(#[from] AeadError),
    /// The envelope handed in is not a TOTP envelope.
    #[error("expected a TOTP envelope, got a {kind} envelope")]
    WrongKind {
        /// The kind actually supplied.
        kind: &'static str,
    },
    /// Stored salt is not the expected length.
    #[error("TOTP envelope salt must be 32 bytes, got {got}")]
    SaltLength {
        /// Length actually decoded.
        got: usize,
    },
    /// A base64 field failed to decode.
    #[error("TOTP envelope field is not valid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    /// HKDF refused to expand.
    #[error("HKDF expansion failed")]
    HkdfFailed,
    /// The shared secret is too short (see [`MIN_SECRET_BYTES`]).
    #[error(transparent)]
    Totp(#[from] TotpError),
}

/// Derive the envelope wrap key from the shared TOTP secret.
///
/// The **secret** backs the wrap, never the code: six digits carry
/// ~20 bits and would be brute-forced instantly. The code's role is
/// only to gate access to the secret, which lives where the agent
/// cannot read it.
pub fn derive_totp_key(
    totp_secret: &[u8],
    salt: &[u8],
) -> Result<Zeroizing<[u8; KEY_LEN]>, TotpEnvelopeError> {
    if totp_secret.len() < MIN_SECRET_BYTES {
        return Err(TotpError::SecretTooShort.into());
    }

    let hkdf = Hkdf::<Sha256>::new(Some(salt), totp_secret);
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    hkdf.expand(TOTP_HKDF_INFO, out.as_mut())
        .map_err(|_| TotpEnvelopeError::HkdfFailed)?;
    Ok(out)
}

/// Wrap `vault_key` in a TOTP envelope.
pub fn create_totp_envelope(
    vault_key: &[u8; KEY_LEN],
    totp_secret: &[u8],
    salt: [u8; 32],
) -> Result<Envelope, TotpEnvelopeError> {
    let wrap_key = derive_totp_key(totp_secret, &salt)?;
    let packed = aead::encrypt_packed(&wrap_key, TOTP_ENVELOPE_AAD, vault_key.as_ref())?;
    Ok(Envelope::Totp {
        totp_salt: b64_encode(&salt),
        wrapped_key: b64_encode(&packed),
    })
}

/// Unwrap a TOTP envelope and return the vault key.
///
/// The caller is responsible for having verified a code *first*
/// (and for rejecting a replayed step). This function only proves
/// possession of the shared secret — which the daemon has for the
/// whole session, so on its own it authorises nothing.
pub fn unwrap_totp(
    envelope: &Envelope,
    totp_secret: &[u8],
) -> Result<Zeroizing<[u8; KEY_LEN]>, TotpEnvelopeError> {
    let (totp_salt, wrapped_key) = match envelope {
        Envelope::Totp {
            totp_salt,
            wrapped_key,
        } => (totp_salt, wrapped_key),
        Envelope::Passphrase { .. } => {
            return Err(TotpEnvelopeError::WrongKind { kind: "passphrase" });
        }
        Envelope::Recovery { .. } => {
            return Err(TotpEnvelopeError::WrongKind { kind: "recovery" });
        }
        Envelope::Keyfile { .. } => {
            return Err(TotpEnvelopeError::WrongKind { kind: "keyfile" });
        }
    };

    let salt_bytes = b64_decode(totp_salt)?;
    if salt_bytes.len() != 32 {
        return Err(TotpEnvelopeError::SaltLength {
            got: salt_bytes.len(),
        });
    }

    let wrap_key = derive_totp_key(totp_secret, &salt_bytes)?;
    let packed = b64_decode(wrapped_key)?;
    let plaintext = aead::decrypt_packed(&wrap_key, TOTP_ENVELOPE_AAD, &packed)?;

    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    if plaintext.len() != KEY_LEN {
        return Err(AeadError::AeadFailed.into());
    }
    out.copy_from_slice(&plaintext);
    Ok(out)
}

/// Percent-encode the characters that would break an
/// `otpauth://` label or query value.
fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 Appendix B publishes SHA-1 vectors for the ASCII
    /// secret "12345678901234567890" with 8 digits. This crate
    /// emits 6, so the expected values are the low six digits of
    /// the published ones — the truncation step is identical.
    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn matches_rfc_6238_appendix_b_vectors() {
        // (unix time, RFC 8-digit value) -> our 6-digit tail.
        let vectors = [
            (59u64, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
            (20_000_000_000, "65353130"),
        ];

        for (time, rfc_eight_digits) in vectors {
            let expected = &rfc_eight_digits[2..];
            let got = code_for_step(RFC_SECRET, step_at(time)).unwrap();
            assert_eq!(got, expected, "RFC vector at t={time}");
        }
    }

    #[test]
    fn codes_are_always_six_digits_including_leading_zeros() {
        // Scan enough steps to hit a value below 100_000, which is
        // where a naive formatter would emit five characters.
        let mut saw_leading_zero = false;
        for step in 0..5_000u64 {
            let code = code_for_step(RFC_SECRET, step).unwrap();
            assert_eq!(code.len(), 6, "step {step} produced `{code}`");
            assert!(code.bytes().all(|b| b.is_ascii_digit()));
            if code.starts_with('0') {
                saw_leading_zero = true;
            }
        }
        assert!(saw_leading_zero, "test did not exercise the padding path");
    }

    #[test]
    fn verify_accepts_the_current_step_and_reports_it() {
        let now = 1_234_567_890;
        let code = code_for_step(RFC_SECRET, step_at(now)).unwrap();

        let step = verify(RFC_SECRET, &code, now).unwrap();
        assert_eq!(step, VerifiedStep(step_at(now)));
    }

    /// ±1 step absorbs ordinary drift.
    #[test]
    fn verify_accepts_one_step_of_skew_in_both_directions() {
        let now = 1_234_567_890;

        let past = code_for_step(RFC_SECRET, step_at(now) - 1).unwrap();
        let future = code_for_step(RFC_SECRET, step_at(now) + 1).unwrap();

        assert_eq!(
            verify(RFC_SECRET, &past, now).unwrap(),
            VerifiedStep(step_at(now) - 1)
        );
        assert_eq!(
            verify(RFC_SECRET, &future, now).unwrap(),
            VerifiedStep(step_at(now) + 1)
        );
    }

    /// Two steps out must fail, or the window a replayed code
    /// lives in grows without bound.
    #[test]
    fn verify_rejects_two_steps_of_skew() {
        let now = 1_234_567_890;
        let stale = code_for_step(RFC_SECRET, step_at(now) - 2).unwrap();

        assert!(verify(RFC_SECRET, &stale, now).is_err());
    }

    #[test]
    fn verify_rejects_a_wrong_code() {
        assert!(verify(RFC_SECRET, "000000", 1_234_567_890).is_err());
    }

    #[test]
    fn verify_rejects_malformed_input_without_touching_the_secret() {
        for bad in ["12345", "1234567", "12345a", "", "  1234"] {
            assert_eq!(
                verify(RFC_SECRET, bad, 59),
                Err(TotpError::MalformedCode),
                "accepted malformed code `{bad}`"
            );
        }
    }

    /// A short secret is the one parameter that genuinely weakens
    /// the scheme, so it is refused rather than tolerated.
    #[test]
    fn secrets_below_the_rfc_minimum_are_refused() {
        assert_eq!(code_for_step(b"short", 0), Err(TotpError::SecretTooShort));
        assert_eq!(
            verify(b"short", "123456", 0),
            Err(TotpError::SecretTooShort)
        );
    }

    /// Different secrets must not produce the same code — the
    /// property that makes the secret, not the code, the thing
    /// worth protecting.
    #[test]
    fn a_different_secret_yields_a_different_code() {
        let a = code_for_step(b"aaaaaaaaaaaaaaaaaaaa", 42).unwrap();
        let b = code_for_step(b"bbbbbbbbbbbbbbbbbbbb", 42).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn step_boundaries_are_exact() {
        assert_eq!(step_at(0), 0);
        assert_eq!(step_at(29), 0);
        assert_eq!(step_at(30), 1);
        assert_eq!(step_at(59), 1);
        assert_eq!(step_at(60), 2);
    }

    #[test]
    fn provisioning_uri_is_authenticator_compatible() {
        let uri = provisioning_uri(RFC_SECRET, "DevBoy", "vault@example.com");

        assert!(uri.starts_with("otpauth://totp/"), "{uri}");
        assert!(uri.contains("algorithm=SHA1"), "{uri}");
        assert!(uri.contains("digits=6"), "{uri}");
        assert!(uri.contains("period=30"), "{uri}");

        // Base32, unpadded — several authenticators reject `=`
        // inside the secret. Check the secret value itself, not
        // the whole URI, where `=` is the legitimate query
        // separator.
        let secret_param = uri
            .split("secret=")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .expect("uri carries a secret");
        assert!(
            !secret_param.contains('='),
            "base32 secret must be unpadded: {secret_param}"
        );
        // The `@` in the account must be escaped, or the label
        // parses wrongly.
        assert!(uri.contains("%40"), "{uri}");
    }

    // -- Envelope ---------------------------------------------------

    const VAULT_KEY: [u8; KEY_LEN] = [0x42; KEY_LEN];
    const TOTP_SECRET: &[u8] = &[0x11; 32];

    #[test]
    fn envelope_round_trips_with_the_same_secret() {
        let env = create_totp_envelope(&VAULT_KEY, TOTP_SECRET, [0x33; 32]).unwrap();
        let recovered = unwrap_totp(&env, TOTP_SECRET).unwrap();

        assert_eq!(recovered.as_ref(), &VAULT_KEY);
    }

    /// The wrap is backed by the 32-byte secret, so a different
    /// secret must not open it — this is the property that makes
    /// the secret, not the six-digit code, the thing worth
    /// protecting.
    #[test]
    fn a_different_secret_cannot_unwrap() {
        let env = create_totp_envelope(&VAULT_KEY, TOTP_SECRET, [0x33; 32]).unwrap();

        assert!(unwrap_totp(&env, &[0x22u8; 32]).is_err());
    }

    /// The AAD is kind-bound, so a `wrapped_key` lifted from
    /// another envelope kind cannot be unwrapped here.
    #[test]
    fn envelope_kinds_are_not_interchangeable() {
        let totp = create_totp_envelope(&VAULT_KEY, TOTP_SECRET, [0x33; 32]).unwrap();
        let Envelope::Totp { wrapped_key, .. } = &totp else {
            unreachable!()
        };

        // Same ciphertext, presented as a recovery envelope.
        let disguised = Envelope::Recovery {
            bip39_salt: b64_encode(&[0x33; 32]),
            wrapped_key: wrapped_key.clone(),
        };

        assert!(matches!(
            unwrap_totp(&disguised, TOTP_SECRET),
            Err(TotpEnvelopeError::WrongKind { kind: "recovery" })
        ));
    }

    #[test]
    fn envelope_rejects_a_passphrase_envelope() {
        let env = Envelope::Passphrase {
            argon2_salt: b64_encode(&[0; 32]),
            argon2_params: crate::format::EnvelopeKdfParams { m: 8, t: 1, p: 1 },
            wrapped_key: b64_encode(&[0; 64]),
        };
        assert!(matches!(
            unwrap_totp(&env, TOTP_SECRET),
            Err(TotpEnvelopeError::WrongKind { kind: "passphrase" })
        ));
    }

    #[test]
    fn envelope_rejects_a_salt_of_the_wrong_length() {
        let env = Envelope::Totp {
            totp_salt: b64_encode(&[0x33; 16]),
            wrapped_key: b64_encode(&[0; 64]),
        };
        assert!(matches!(
            unwrap_totp(&env, TOTP_SECRET),
            Err(TotpEnvelopeError::SaltLength { got: 16 })
        ));
    }

    #[test]
    fn envelope_refuses_a_secret_below_the_rfc_minimum() {
        assert!(create_totp_envelope(&VAULT_KEY, b"short", [0x33; 32]).is_err());
    }

    /// A tampered ciphertext must fail closed rather than yield a
    /// wrong key.
    #[test]
    fn envelope_detects_tampering() {
        let env = create_totp_envelope(&VAULT_KEY, TOTP_SECRET, [0x33; 32]).unwrap();
        let Envelope::Totp {
            totp_salt,
            wrapped_key,
        } = env
        else {
            unreachable!()
        };

        let mut bytes = b64_decode(&wrapped_key).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;

        let tampered = Envelope::Totp {
            totp_salt,
            wrapped_key: b64_encode(&bytes),
        };
        assert!(unwrap_totp(&tampered, TOTP_SECRET).is_err());
    }

    /// Distinct salts must produce distinct wrapped keys even for
    /// the same secret and vault key.
    #[test]
    fn salt_is_actually_mixed_into_the_derivation() {
        let a = create_totp_envelope(&VAULT_KEY, TOTP_SECRET, [0x01; 32]).unwrap();
        let b = create_totp_envelope(&VAULT_KEY, TOTP_SECRET, [0x02; 32]).unwrap();
        assert_ne!(a, b);

        let key_a = derive_totp_key(TOTP_SECRET, &[0x01; 32]).unwrap();
        let key_b = derive_totp_key(TOTP_SECRET, &[0x02; 32]).unwrap();
        assert_ne!(key_a.as_ref(), key_b.as_ref());
    }

    #[test]
    /// The round trip the daemon depends on: what enrolment stores
    /// decodes back to the key the authenticator uses.
    #[test]
    fn a_stored_secret_decodes_to_the_key_the_app_hmacs() {
        let secret = [7u8; 32];
        let stored = data_encoding::BASE32_NOPAD.encode(&secret);

        assert_eq!(decode_secret(&stored).expect("decode"), secret.to_vec());
    }

    /// The failure that must not be silent: using the base32 text as
    /// the key produces different codes from using the decoded bytes.
    /// This is the bug that shipped.
    #[test]
    fn hmacing_the_base32_text_gives_a_different_code_than_the_key() {
        let secret = [7u8; 32];
        let stored = data_encoding::BASE32_NOPAD.encode(&secret);

        let from_key = code_for_step(&secret, 42).expect("code");
        let from_text = code_for_step(stored.as_bytes(), 42).expect("code");

        assert_ne!(
            from_key, from_text,
            "if these ever matched, the mistake this guards against would be undetectable"
        );
    }

    #[test]
    fn a_secret_that_is_not_base32_is_refused_rather_than_used_raw() {
        assert!(matches!(
            decode_secret("this is not base32!"),
            Err(TotpError::MalformedStoredSecret)
        ));
    }

    #[test]
    fn provisioning_uri_secret_round_trips_through_base32() {
        let uri = provisioning_uri(RFC_SECRET, "DevBoy", "acct");
        let encoded = uri
            .split("secret=")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .expect("uri carries a secret");

        let decoded = data_encoding::BASE32_NOPAD
            .decode(encoded.as_bytes())
            .unwrap();
        assert_eq!(decoded, RFC_SECRET);
    }
}
