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

use hmac::{Hmac, Mac};
use sha1::Sha1;
use subtle::ConstantTimeEq;

type HmacSha1 = Hmac<Sha1>;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TotpError {
    /// The shared secret is shorter than RFC 4226 permits.
    #[error("TOTP secret must be at least {MIN_SECRET_BYTES} bytes")]
    SecretTooShort,
    /// The submitted code is not `DIGITS` ASCII digits.
    #[error("TOTP code must be exactly {DIGITS} digits")]
    MalformedCode,
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
pub fn provisioning_uri(secret: &[u8], issuer: &str, account: &str) -> String {
    let encoded = data_encoding::BASE32_NOPAD.encode(secret);
    let issuer_enc = urlencode(issuer);
    let account_enc = urlencode(account);

    format!(
        "otpauth://totp/{issuer_enc}:{account_enc}?secret={encoded}&issuer={issuer_enc}\
         &algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}"
    )
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
