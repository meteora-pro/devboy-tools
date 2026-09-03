//! A keyfile envelope must not open on a machine other than the one
//! that created it (ADR-024 §6, Ф16).
//!
//! # Why these live in their own binary
//!
//! They drive the real `create_keyfile_envelope` / `unwrap_keyfile`
//! pair, which read the machine identifier themselves — that is the
//! point, since an API that took the fingerprint as an argument could
//! be called with `None` and silently lose the binding. Controlling
//! what they read means setting an environment variable, and a
//! process-wide variable would leak into the crate's other tests,
//! several of which round-trip the same envelope. A separate
//! integration binary keeps that blast radius at zero.

use devboy_vault_crypto::aead::KEY_LEN;
use devboy_vault_crypto::format::Envelope;
use devboy_vault_crypto::keyfile::{KeyfileError, create_keyfile_envelope, unwrap_keyfile};

const VAULT_KEY: [u8; KEY_LEN] = [0x42; KEY_LEN];
const KEYFILE: [u8; 32] = [0x11; 32];
const SALT: [u8; 32] = [0x33; 32];

const ENV: &str = "DEVBOY_MACHINE_ID";

/// The property the feature exists for.
///
/// Both halves travel together — a synced home directory, a container
/// image, a restored backup — and the vault still does not open.
#[test]
fn an_envelope_does_not_open_on_a_different_machine() {
    let envelope = temp_env::with_var(ENV, Some("machine-alpha"), || {
        create_keyfile_envelope(&VAULT_KEY, &KEYFILE, SALT).expect("create")
    });

    let result = temp_env::with_var(ENV, Some("machine-beta"), || {
        unwrap_keyfile(&envelope, &KEYFILE)
    });

    assert!(
        result.is_err(),
        "the same vault and the same keyfile opened on a different machine"
    );
}

/// The other half of the same property: binding must not break the
/// ordinary case, or nobody will keep it switched on.
#[test]
fn an_envelope_opens_on_the_machine_that_made_it() {
    temp_env::with_var(ENV, Some("machine-alpha"), || {
        let envelope = create_keyfile_envelope(&VAULT_KEY, &KEYFILE, SALT).expect("create");
        let recovered = unwrap_keyfile(&envelope, &KEYFILE).expect("unwrap on the same machine");

        assert_eq!(recovered.as_ref(), &VAULT_KEY);
    });
}

/// A binding is recorded, so it is visible rather than inferred.
#[test]
fn a_bound_envelope_records_the_scheme() {
    let envelope = temp_env::with_var(ENV, Some("machine-alpha"), || {
        create_keyfile_envelope(&VAULT_KEY, &KEYFILE, SALT).expect("create")
    });

    let Envelope::Keyfile {
        machine_binding, ..
    } = &envelope
    else {
        unreachable!()
    };

    assert_eq!(machine_binding.as_deref(), Some("machine-v1"));
}

/// Envelopes written before this field existed have no
/// `machine_binding` key at all. They must keep opening — an upgrade
/// that locks people out of their own vault is not a security
/// improvement.
#[test]
fn an_envelope_written_before_binding_existed_still_opens() {
    // Built by deserialising the legacy shape rather than by
    // constructing the struct, so the test would catch a serde
    // default going missing as well as the logic.
    let legacy_json = temp_env::with_var(ENV, Some("machine-alpha"), || {
        let envelope = create_keyfile_envelope(&VAULT_KEY, &KEYFILE, SALT).expect("create");
        let mut value = serde_json::to_value(&envelope).expect("serialise");
        value
            .as_object_mut()
            .expect("object")
            .remove("machine_binding");
        value
    });

    // Legacy envelopes were derived without a fingerprint, so the
    // one above — created *with* one — must not unwrap after the
    // field is stripped. That is the honest check that the field is
    // load-bearing rather than decorative.
    let stripped: Envelope = serde_json::from_value(legacy_json).expect("deserialise");
    let result = temp_env::with_var(ENV, Some("machine-alpha"), || {
        unwrap_keyfile(&stripped, &KEYFILE)
    });
    assert!(
        result.is_err(),
        "stripping the binding must change the derivation, or the binding does nothing"
    );

    // And a genuinely unbound envelope round-trips regardless of what
    // machine it is opened on.
    let unbound = temp_env::with_var(ENV, Some("machine-alpha"), || {
        let e = create_keyfile_envelope(&VAULT_KEY, &KEYFILE, SALT).expect("create");
        let Envelope::Keyfile {
            keyfile_salt,
            wrapped_key,
            ..
        } = e
        else {
            unreachable!()
        };
        // Re-derive without a binding, the way an old devboy did.
        let key = devboy_vault_crypto::keyfile::derive_keyfile_key(&KEYFILE, &SALT, None)
            .expect("derive");
        let packed = devboy_vault_crypto::aead::encrypt_packed(
            &key,
            devboy_vault_crypto::keyfile::KEYFILE_ENVELOPE_AAD,
            VAULT_KEY.as_ref(),
        )
        .expect("wrap");
        let _ = wrapped_key;
        Envelope::Keyfile {
            keyfile_salt,
            wrapped_key: devboy_vault_crypto::format::b64_encode(&packed),
            machine_binding: None,
        }
    });

    let recovered = temp_env::with_var(ENV, Some("machine-gamma"), || {
        unwrap_keyfile(&unbound, &KEYFILE).expect("an unbound envelope opens anywhere")
    });
    assert_eq!(recovered.as_ref(), &VAULT_KEY);
}

/// An envelope from a newer devboy must say so, rather than failing
/// as a corrupt or wrong-key error and sending the user hunting.
#[test]
fn an_unknown_binding_scheme_is_named_in_the_error() {
    let envelope = temp_env::with_var(ENV, Some("machine-alpha"), || {
        let e = create_keyfile_envelope(&VAULT_KEY, &KEYFILE, SALT).expect("create");
        let Envelope::Keyfile {
            keyfile_salt,
            wrapped_key,
            ..
        } = e
        else {
            unreachable!()
        };
        Envelope::Keyfile {
            keyfile_salt,
            wrapped_key,
            machine_binding: Some("machine-v99".to_owned()),
        }
    });

    let err = temp_env::with_var(ENV, Some("machine-alpha"), || {
        unwrap_keyfile(&envelope, &KEYFILE).expect_err("unknown scheme")
    });

    assert!(
        matches!(&err, KeyfileError::UnknownBinding { scheme } if scheme == "machine-v99"),
        "got {err:?}"
    );
    assert!(
        err.to_string().contains("Upgrade devboy"),
        "the error must say what to do: {err}"
    );
}
