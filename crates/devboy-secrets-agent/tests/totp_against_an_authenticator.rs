//! A code from the user's phone must actually verify (ADR-024 §1).
//!
//! # The bug this exists to prevent recurring
//!
//! Enrolment writes the shared secret into a vault slot, and that
//! slot is a `SecretString` — so what is stored is the *base32 text*
//! of the key. The daemon read that text and used its ASCII bytes as
//! the HMAC key. The authenticator app, given the same text in the
//! `otpauth://` URI, decodes it and HMACs the raw bytes.
//!
//! Two different keys. Every code the user typed was rejected, and
//! after five tries they were rate-limited for their trouble.
//!
//! It survived a full test suite because every existing test seeded
//! the slot with an arbitrary ASCII string and then computed the
//! expected code from that same string. Both halves of the test
//! agreed with each other and both disagreed with reality.
//!
//! So this test refuses to compute the code from anything the daemon
//! touched. It takes the `otpauth://` URI — the only thing the user's
//! phone ever sees — parses the secret out of it, decodes the base32
//! exactly as an authenticator does, and derives the code from that.
//! If the two sides ever disagree again, this fails.

#![cfg(unix)]

use devboy_vault_crypto::totp;

/// Pull `secret=` out of an `otpauth://` URI, the way an
/// authenticator app does when it scans the QR code.
fn secret_from_uri(uri: &str) -> String {
    uri.split(['?', '&'])
        .find_map(|part| part.strip_prefix("secret="))
        .expect("the URI must carry a secret parameter")
        .to_owned()
}

/// What the phone computes: decode the base32 from the URI, HMAC the
/// raw bytes. Deliberately does not call anything the daemon uses to
/// *store* the secret.
fn code_an_authenticator_would_show(uri: &str, step: u64) -> String {
    let base32 = secret_from_uri(uri);
    let key = data_encoding::BASE32_NOPAD
        .decode(base32.as_bytes())
        .expect("an authenticator decodes the base32 from the URI");
    totp::code_for_step(&key, step).expect("code")
}

/// The property: what the phone shows is what the daemon accepts.
#[test]
fn a_code_computed_the_way_a_phone_does_it_verifies() {
    let key = [0x2au8; 32];
    let uri = totp::provisioning_uri(&key, "devboy", "alice");

    // What enrolment stores in the vault slot.
    let stored = data_encoding::BASE32_NOPAD.encode(&key);
    // What the daemon must end up using as the key.
    let adopted = totp::decode_secret(&stored).expect("the daemon decodes the stored secret");

    let step = 1_700_000_000 / 30;
    let from_phone = code_an_authenticator_would_show(&uri, step);
    let from_daemon = totp::code_for_step(&adopted, step).expect("code");

    assert_eq!(
        from_phone, from_daemon,
        "the code on the user's phone must be the code the daemon computes"
    );
}

/// The specific mistake, pinned: using the stored text as the key
/// gives a different answer. If this ever stops being true, the test
/// above stops proving anything.
#[test]
fn using_the_stored_text_as_the_key_does_not_match_the_phone() {
    let key = [0x2au8; 32];
    let uri = totp::provisioning_uri(&key, "devboy", "alice");
    let stored = data_encoding::BASE32_NOPAD.encode(&key);

    let step = 1_700_000_000 / 30;
    let from_phone = code_an_authenticator_would_show(&uri, step);
    let the_old_bug = totp::code_for_step(stored.as_bytes(), step).expect("code");

    assert_ne!(
        from_phone, the_old_bug,
        "this is the defect that shipped; if these matched it would be invisible"
    );
}

/// A verify against a code derived independently — the end the user
/// actually experiences.
#[test]
fn the_daemons_verify_accepts_a_phone_code() {
    let key = [0x11u8; 32];
    let uri = totp::provisioning_uri(&key, "devboy", "alice");
    let stored = data_encoding::BASE32_NOPAD.encode(&key);
    let adopted = totp::decode_secret(&stored).expect("decode");

    let now = 1_700_000_000u64;
    let code = code_an_authenticator_would_show(&uri, now / 30);

    totp::verify(&adopted, &code, now).expect("a code from the phone must verify");
}
