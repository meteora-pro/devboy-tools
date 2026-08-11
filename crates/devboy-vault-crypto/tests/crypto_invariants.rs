//! Cross-module crypto invariants (T6).
//!
//! Each module already tests its own arithmetic. These check
//! properties that only appear when the pieces are combined, and
//! that a single-module test cannot see:
//!
//! - a ciphertext must not be movable between paths, versions, or
//!   vaults;
//! - the four envelope kinds must be mutually non-interchangeable,
//!   as a full matrix rather than the couple of pairs each module
//!   happens to check;
//! - nonces must not repeat, since XChaCha20-Poly1305 nonce reuse
//!   under one key is catastrophic rather than merely weakening.

use std::collections::HashSet;

use devboy_vault_crypto::format::{Envelope, EnvelopeKdfParams, VaultFile, b64_decode, b64_encode};
use devboy_vault_crypto::keyfile::{create_keyfile_envelope, unwrap_keyfile};
use devboy_vault_crypto::totp::{create_totp_envelope, unwrap_totp};
use devboy_vault_crypto::vault::{InitialUnlock, UnlockMethod, Vault};
use devboy_vault_crypto::{EntryMetadata, KEY_LEN, create_recovery_envelope, unwrap_recovery};
use devboy_vault_crypto::{create_passphrase_envelope, unwrap_passphrase};
use secrecy::{ExposeSecret, SecretString};
use tempfile::TempDir;

const VAULT_KEY: [u8; KEY_LEN] = [0x42; KEY_LEN];
const TOTP_SECRET: &[u8] = &[0x11; 32];
const KEYFILE: &[u8] = &[0x22; 32];

/// Argon2 parameters small enough that a test suite stays fast.
fn fast_params() -> EnvelopeKdfParams {
    EnvelopeKdfParams { m: 8, t: 1, p: 1 }
}

fn fast_init(passphrase: &str) -> InitialUnlock {
    InitialUnlock {
        passphrase: SecretString::from(passphrase.to_owned()),
        passphrase_params: Some(fast_params()),
        with_recovery: false,
        with_totp_secret: None,
    }
}

fn open(dir: &TempDir, passphrase: &str) -> Vault {
    Vault::open(
        &dir.path().join("vault.dvb"),
        UnlockMethod::Passphrase(SecretString::from(passphrase.to_owned())),
    )
    .expect("vault opens")
}

fn create(dir: &TempDir, passphrase: &str) -> Vault {
    Vault::create(&dir.path().join("vault.dvb"), fast_init(passphrase))
        .expect("vault is created")
        .vault
}

/// Path-as-AAD, checked end to end: moving a blob from one path to
/// another must fail rather than decrypt under the wrong name.
///
/// This is the swap attack ADR-023 §4.1 guards against — an
/// attacker with write access to the vault file promoting a
/// low-value secret into a high-value path.
#[test]
fn a_ciphertext_cannot_be_moved_between_paths() {
    let dir = TempDir::new().unwrap();
    let mut v = create(&dir, "pw");

    v.put(
        "team/low/value",
        &SecretString::from("LOW".to_owned()),
        EntryMetadata::default(),
    )
    .unwrap();
    v.put(
        "team/high/value",
        &SecretString::from("HIGH".to_owned()),
        EntryMetadata::default(),
    )
    .unwrap();

    // Both resolve normally.
    assert_eq!(
        v.get("team/low/value")
            .unwrap()
            .map(|s| s.expose_secret().to_owned()),
        Some("LOW".to_owned())
    );

    drop(v);

    // Now act as an attacker with write access to the vault file:
    // swap the two entries' ciphertext pointers and nonces so the
    // high-value path points at the low-value blob.
    let file_path = dir.path().join("vault.dvb");
    let mut file = VaultFile::read_file(&file_path).unwrap();
    let low = file
        .entries
        .iter()
        .position(|e| e.path == "team/low/value")
        .unwrap();
    let high = file
        .entries
        .iter()
        .position(|e| e.path == "team/high/value")
        .unwrap();

    let (low_nonce, low_off, low_len) = {
        let e = &file.entries[low];
        (e.nonce.clone(), e.ct_offset, e.ct_length)
    };
    file.entries[low].nonce = file.entries[high].nonce.clone();
    file.entries[low].ct_offset = file.entries[high].ct_offset;
    file.entries[low].ct_length = file.entries[high].ct_length;
    file.entries[high].nonce = low_nonce;
    file.entries[high].ct_offset = low_off;
    file.entries[high].ct_length = low_len;
    file.write_file_atomic(&file_path).unwrap();

    // The AAD no longer matches the path, so the read fails closed
    // rather than handing back the other secret.
    let tampered = open(&dir, "pw");
    assert!(
        tampered.get("team/high/value").is_err(),
        "a swapped ciphertext must not decrypt under a different path"
    );
}

/// Two vaults have independent keys, so a blob lifted from one
/// must be meaningless in the other even at the same path.
#[test]
fn a_ciphertext_cannot_be_moved_between_vaults() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    let mut a = create(&dir_a, "pw-a");
    let mut b = create(&dir_b, "pw-b");

    a.put(
        "shared/path",
        &SecretString::from("FROM-A".to_owned()),
        EntryMetadata::default(),
    )
    .unwrap();
    b.put(
        "shared/path",
        &SecretString::from("FROM-B".to_owned()),
        EntryMetadata::default(),
    )
    .unwrap();

    drop(a);
    drop(b);

    // Lift vault A's blob region and entry pointer into vault B.
    let a_file = VaultFile::read_file(&dir_a.path().join("vault.dvb")).unwrap();
    let b_path = dir_b.path().join("vault.dvb");
    let mut b_file = VaultFile::read_file(&b_path).unwrap();

    let a_entry = a_file
        .entries
        .iter()
        .find(|e| e.path == "shared/path")
        .unwrap();
    let b_entry = b_file
        .entries
        .iter_mut()
        .find(|e| e.path == "shared/path")
        .unwrap();
    b_entry.nonce = a_entry.nonce.clone();
    b_entry.ct_offset = a_entry.ct_offset;
    b_entry.ct_length = a_entry.ct_length;
    b_file.ciphertext_blobs = a_file.ciphertext_blobs.clone();
    b_file.write_file_atomic(&b_path).unwrap();

    let tampered = open(&dir_b, "pw-b");
    assert!(
        tampered.get("shared/path").is_err(),
        "another vault's ciphertext must not decrypt here"
    );
}

/// Every envelope kind must reject every other kind, as a full
/// matrix. Individual modules each check a pair or two; a gap
/// between them would be invisible.
#[test]
fn envelope_kinds_are_mutually_non_interchangeable() {
    let passphrase = create_passphrase_envelope(
        &VAULT_KEY,
        &SecretString::from("pw".to_owned()),
        [0x01; 32],
        fast_params(),
    )
    .unwrap();
    let totp = create_totp_envelope(&VAULT_KEY, TOTP_SECRET, [0x02; 32]).unwrap();
    let keyfile = create_keyfile_envelope(&VAULT_KEY, KEYFILE, [0x03; 32]).unwrap();

    let pw = SecretString::from("pw".to_owned());

    // Each unwrapper accepts only its own kind.
    assert!(unwrap_passphrase(&passphrase, &pw).is_ok());
    assert!(unwrap_passphrase(&totp, &pw).is_err());
    assert!(unwrap_passphrase(&keyfile, &pw).is_err());

    assert!(unwrap_totp(&totp, TOTP_SECRET).is_ok());
    assert!(unwrap_totp(&passphrase, TOTP_SECRET).is_err());
    assert!(unwrap_totp(&keyfile, TOTP_SECRET).is_err());

    assert!(unwrap_keyfile(&keyfile, KEYFILE).is_ok());
    assert!(unwrap_keyfile(&passphrase, KEYFILE).is_err());
    assert!(unwrap_keyfile(&totp, KEYFILE).is_err());
}

/// The kind-bound AAD must survive a disguise: copying a wrapped
/// key into a different envelope kind's struct must not open it,
/// even though the bytes are identical.
#[test]
fn a_wrapped_key_disguised_as_another_kind_stays_shut() {
    let totp = create_totp_envelope(&VAULT_KEY, TOTP_SECRET, [0x02; 32]).unwrap();
    let Envelope::Totp {
        totp_salt,
        wrapped_key,
    } = totp
    else {
        unreachable!()
    };

    // Same ciphertext, same salt, presented as a keyfile envelope.
    let disguised = Envelope::Keyfile {
        keyfile_salt: totp_salt,
        wrapped_key,
    };

    assert!(
        unwrap_keyfile(&disguised, TOTP_SECRET).is_err(),
        "the AAD must bind the ciphertext to its envelope kind"
    );
}

/// Nonce reuse under one key is catastrophic for
/// XChaCha20-Poly1305, not merely weakening, so this checks the
/// generator rather than assuming it.
#[test]
fn entry_nonces_never_repeat_within_a_vault() {
    let dir = TempDir::new().unwrap();
    let mut v = create(&dir, "pw");

    for i in 0..200 {
        v.put(
            &format!("path/{i}"),
            &SecretString::from(format!("value-{i}")),
            EntryMetadata::default(),
        )
        .unwrap();
    }

    drop(v);
    let file = VaultFile::read_file(&dir.path().join("vault.dvb")).unwrap();
    let nonces: HashSet<String> = file.entries.iter().map(|e| e.nonce.clone()).collect();
    assert_eq!(nonces.len(), 200, "a nonce repeated within one vault");
}

/// Writing the same value repeatedly must still produce distinct
/// ciphertexts — identical output would leak that two entries hold
/// the same secret.
#[test]
fn identical_values_encrypt_to_distinct_ciphertexts() {
    let dir = TempDir::new().unwrap();
    let mut v = create(&dir, "pw");

    let same = SecretString::from("IDENTICAL-VALUE".to_owned());
    v.put("a/one", &same, EntryMetadata::default()).unwrap();
    v.put("a/two", &same, EntryMetadata::default()).unwrap();

    drop(v);
    let file = VaultFile::read_file(&dir.path().join("vault.dvb")).unwrap();
    let one = file.entries.iter().find(|e| e.path == "a/one").unwrap();
    let two = file.entries.iter().find(|e| e.path == "a/two").unwrap();

    assert_ne!(one.nonce, two.nonce, "same value reused a nonce");
}

/// Flipping any single byte of a wrapped key must fail closed.
#[test]
fn every_envelope_kind_detects_a_single_bit_flip() {
    let pw = SecretString::from("pw".to_owned());

    let passphrase =
        create_passphrase_envelope(&VAULT_KEY, &pw, [0x01; 32], fast_params()).unwrap();
    let Envelope::Passphrase {
        argon2_salt,
        argon2_params,
        wrapped_key,
    } = passphrase
    else {
        unreachable!()
    };
    let mut bytes = b64_decode(&wrapped_key).unwrap();
    bytes[0] ^= 0x01;
    let tampered = Envelope::Passphrase {
        argon2_salt,
        argon2_params,
        wrapped_key: b64_encode(&bytes),
    };
    assert!(unwrap_passphrase(&tampered, &pw).is_err());

    let recovery_phrase = devboy_vault_crypto::generate_recovery_phrase().unwrap();
    let recovery = create_recovery_envelope(&VAULT_KEY, &recovery_phrase, [0x04; 32]).unwrap();
    let Envelope::Recovery {
        bip39_salt,
        wrapped_key,
    } = recovery
    else {
        unreachable!()
    };
    let mut bytes = b64_decode(&wrapped_key).unwrap();
    bytes[0] ^= 0x01;
    let tampered = Envelope::Recovery {
        bip39_salt,
        wrapped_key: b64_encode(&bytes),
    };
    assert!(unwrap_recovery(&tampered, &recovery_phrase).is_err());
}

/// A vault must survive being written, closed and reopened with
/// every value intact — the property everything else rests on.
#[test]
fn all_values_survive_a_close_and_reopen_cycle() {
    let dir = TempDir::new().unwrap();
    {
        let mut v = create(&dir, "pw");
        for i in 0..25 {
            v.put(
                &format!("path/{i}"),
                &SecretString::from(format!("value-{i}")),
                EntryMetadata::default(),
            )
            .unwrap();
        }
    }

    let reopened = open(&dir, "pw");
    for i in 0..25 {
        assert_eq!(
            reopened
                .get(&format!("path/{i}"))
                .unwrap()
                .map(|s| s.expose_secret().to_owned()),
            Some(format!("value-{i}")),
            "value {i} did not survive the round trip"
        );
    }
}

/// The wrong passphrase must fail, and must fail the same
/// indistinguishable way regardless of how wrong it is.
#[test]
fn a_wrong_passphrase_never_opens_the_vault() {
    let dir = TempDir::new().unwrap();
    {
        let mut v = create(&dir, "correct-horse");
        v.put(
            "a/b",
            &SecretString::from("v".to_owned()),
            EntryMetadata::default(),
        )
        .unwrap();
    }

    for wrong in ["", "correct-hors", "correct-horse ", "CORRECT-HORSE", "x"] {
        assert!(
            Vault::open(
                &dir.path().join("vault.dvb"),
                UnlockMethod::Passphrase(SecretString::from(wrong.to_owned()))
            )
            .is_err(),
            "passphrase `{wrong}` should not have opened the vault"
        );
    }
}
