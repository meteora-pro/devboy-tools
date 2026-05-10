//! macOS Keychain envelope per [ADR-023] §3.2.
//!
//! Wraps the 32-byte vault key under a fresh random 32-byte key
//! stored in the macOS Keychain. The Keychain entry is the *unlock
//! factor*; reading it (with biometric protection enabled — see
//! "Biometric protection" below) returns the wrap key, which then
//! unwraps the AEAD-encrypted vault key.
//!
//! # Why this is one of three envelope variants
//!
//! Per ADR-023 §3.2 a vault accepts up to three independent unlock
//! methods so adding a Touch-ID unlock does not invalidate the
//! existing passphrase / recovery wraps. Each envelope is a separate
//! AEAD wrap of the same `vault_key` under a different derived key:
//!
//! - [`crate::passphrase`] (mandatory) — Argon2id over the user's
//!   passphrase.
//! - [`crate::recovery`] (optional) — HKDF-SHA256 over a BIP39 seed.
//! - [`keychain`](self) (optional, **macOS only**) — random wrap
//!   key in the macOS Keychain, ideally protected by `kSecAccessControl`
//!   with `kSecAccessControlBiometryAny | kSecAccessControlUserPresence`
//!   so reading triggers Touch ID.
//!
//! # Biometric protection — current limitation
//!
//! The first cut uses [`keyring`] for storage, which writes a
//! generic-password Keychain entry without an attached
//! `SecAccessControl`. That means the wrap key is protected by the
//! standard "user is logged in and the keychain is unlocked" ACL,
//! **not** by Touch ID. Wiring up `SecAccessControlCreateWithFlags`
//! with `kSecAccessControlBiometryAny | kSecAccessControlUserPresence`
//! requires lower-level `Security.framework` calls than `keyring`
//! exposes; that integration is the subject of a focused follow-up
//! once the rest of the stack is in place. The API surface here is
//! the one the future biometric path will consume — only the storage
//! step changes.
//!
//! When the upgrade lands the API does **not** change for callers;
//! `Envelope::Keychain { keychain_account, wrapped_key }` already
//! carries everything the unwrap step needs.
//!
//! # Non-macOS targets
//!
//! Every public function returns [`KeychainError::Unsupported`] on
//! non-macOS targets. This keeps cross-platform callers compiling
//! without `cfg!` and surfaces the limitation as a real error rather
//! than a silent compile failure or a panic.
//!
//! # Manual test instructions (macOS)
//!
//! Integration tests for the real Keychain are gated `#[ignore]`
//! because they (a) require an unlocked Keychain and (b) leave
//! detectable side effects on success. To exercise the round trip:
//!
//! ```text
//! cargo test -p devboy-vault-crypto --test '*' -- --ignored
//! # or, for a unit-test-style invocation:
//! cargo test -p devboy-vault-crypto keychain::tests -- --ignored
//! ```
//!
//! Each ignored test uses a unique account name under the
//! [`KEYCHAIN_ACCOUNT_PREFIX`] namespace and cleans up via
//! [`delete_keychain_entry`] in a `Drop` guard.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

// On non-macOS targets the keychain envelope only ships stub
// `Unsupported` errors, so the helpers below + a couple of
// imports are dead code (used exclusively by the
// `#[cfg(target_os = "macos")]` block and the `#[cfg(test)]`
// module). Suppressing here keeps the helpers visible to tests
// on any platform without the cross-compile rustdoc / clippy
// job tripping on the workspace's `-D warnings`.
#![cfg_attr(not(any(target_os = "macos", test)), allow(dead_code, unused_imports))]

use thiserror::Error;
use zeroize::Zeroizing;

use crate::aead::{self, AeadError, KEY_LEN};
use crate::format::{Envelope, b64_decode, b64_encode};

/// AAD bound into the keychain envelope's AEAD wrap. Picked so that a
/// wrapped key from a Passphrase or Recovery envelope cannot decrypt
/// under a Keychain envelope's slot.
pub const KEYCHAIN_ENVELOPE_AAD: &str = "devboy-vault-envelope:keychain:v1";

/// `kSecAttrService` value used for every Keychain entry written by
/// this crate. Matches the value `devboy-storage` uses for ADR-005
/// credential entries so there is exactly one service identifier
/// across the workspace.
pub const KEYCHAIN_SERVICE: &str = "devboy-tools";

/// Prefix prepended to every account name a caller supplies to
/// [`create_keychain_envelope`]. Namespacing keeps vault wrap keys
/// out of collision with the per-provider credential entries that
/// `devboy-storage` writes under the same service.
pub const KEYCHAIN_ACCOUNT_PREFIX: &str = "vault-wrap:";

// =============================================================================
// Errors
// =============================================================================

/// Failure modes for keychain-envelope operations.
#[derive(Debug, Error)]
pub enum KeychainError {
    /// The current target does not support the macOS Keychain. Every
    /// public function in this module returns this error on non-macOS
    /// targets.
    #[error("the keychain envelope is only supported on macOS")]
    Unsupported,

    /// Underlying [`keyring`] crate error (Keychain returned an
    /// unexpected status code, the entry could not be created, etc.).
    #[error("keychain operation failed: {0}")]
    #[cfg(target_os = "macos")]
    Keyring(String),

    /// `wrapped_key` is not valid base64.
    #[error("envelope payload is not valid base64: {0}")]
    Base64Decode(#[from] base64::DecodeError),

    /// The envelope was decrypted successfully but yielded a buffer
    /// whose length is not 32 bytes.
    #[error("envelope yielded a vault key of wrong length: expected 32 bytes, got {got}")]
    VaultKeyLength {
        /// Length of the decrypted payload.
        got: usize,
    },

    /// The wrap key fetched from the Keychain is not 32 bytes.
    /// Indicates a Keychain entry written by an older version of the
    /// framework (or by another tool reusing the same account name).
    #[error("Keychain wrap key has wrong length: expected 32 bytes, got {got}")]
    WrapKeyLength {
        /// Length of the decoded Keychain entry payload.
        got: usize,
    },

    /// The supplied `Envelope` is not [`Envelope::Keychain`].
    #[error("envelope kind '{kind}' is not 'keychain'")]
    WrongKind {
        /// The variant the caller actually supplied.
        kind: &'static str,
    },

    /// AEAD wrap or unwrap operation failed. For [`unwrap_keychain`]
    /// this is the expected signal of a tampered envelope or a
    /// Keychain entry that has been replaced. The variant is
    /// intentionally opaque (matches the [`AeadError::AeadFailed`]
    /// policy in [`crate::aead`]).
    #[error("AEAD operation failed (tampered envelope or replaced Keychain entry)")]
    AeadFailed,

    /// The OS-level CSPRNG ([`getrandom`]) refused to produce bytes
    /// during wrap-key generation or AEAD nonce generation.
    #[error("CSPRNG unavailable: {0}")]
    RngUnavailable(getrandom::Error),
}

impl From<AeadError> for KeychainError {
    fn from(e: AeadError) -> Self {
        match e {
            AeadError::AeadFailed => Self::AeadFailed,
            AeadError::RngUnavailable(rng_err) => Self::RngUnavailable(rng_err),
        }
    }
}

#[cfg(target_os = "macos")]
impl From<keyring::Error> for KeychainError {
    fn from(e: keyring::Error) -> Self {
        Self::Keyring(e.to_string())
    }
}

// =============================================================================
// Public API (cfg-dispatched)
// =============================================================================

/// Wrap `vault_key` in a Keychain envelope.
///
/// Generates a fresh random 32-byte wrap key, stores it in the macOS
/// Keychain under `KEYCHAIN_SERVICE` / `<KEYCHAIN_ACCOUNT_PREFIX><account>`,
/// then AEAD-wraps `vault_key` under that key. The returned envelope
/// carries the *prefixed* account name so [`unwrap_keychain`] can
/// find the entry without the caller passing it again.
///
/// On non-macOS targets, returns [`KeychainError::Unsupported`].
pub fn create_keychain_envelope(
    vault_key: &[u8; KEY_LEN],
    account: &str,
) -> Result<Envelope, KeychainError> {
    imp::create_keychain_envelope(vault_key, account)
}

/// Unwrap a Keychain envelope and return the vault key.
///
/// Reads the wrap key from the macOS Keychain (will trigger Touch ID
/// once biometric protection is wired in — see the module-level
/// "Biometric protection — current limitation" note), then
/// AEAD-decrypts the wrapped vault key.
///
/// On non-macOS targets, returns [`KeychainError::Unsupported`].
pub fn unwrap_keychain(envelope: &Envelope) -> Result<Zeroizing<[u8; KEY_LEN]>, KeychainError> {
    imp::unwrap_keychain(envelope)
}

/// Delete the Keychain entry that backs a previously-created
/// envelope. Returns `Ok(())` even when the entry does not exist —
/// the operation is idempotent so callers can use it as a cleanup
/// step in `Drop` guards and migration tooling without first
/// checking existence.
///
/// On non-macOS targets, returns [`KeychainError::Unsupported`].
pub fn delete_keychain_entry(account: &str) -> Result<(), KeychainError> {
    imp::delete_keychain_entry(account)
}

// =============================================================================
// Cross-platform helpers (used by both the macOS impl and the tests)
// =============================================================================

/// Resolve the supplied caller account into the actual Keychain
/// account name (with [`KEYCHAIN_ACCOUNT_PREFIX`]).
fn keychain_account_name(account: &str) -> String {
    format!("{KEYCHAIN_ACCOUNT_PREFIX}{account}")
}

fn parse_envelope_account(envelope: &Envelope) -> Result<&str, KeychainError> {
    match envelope {
        Envelope::Keychain {
            keychain_account, ..
        } => Ok(keychain_account.as_str()),
        Envelope::Passphrase { .. } => Err(KeychainError::WrongKind { kind: "passphrase" }),
        Envelope::Recovery { .. } => Err(KeychainError::WrongKind { kind: "recovery" }),
    }
}

fn parse_envelope_payload(envelope: &Envelope) -> Result<Vec<u8>, KeychainError> {
    let wrapped_key = match envelope {
        Envelope::Keychain { wrapped_key, .. } => wrapped_key,
        Envelope::Passphrase { .. } => return Err(KeychainError::WrongKind { kind: "passphrase" }),
        Envelope::Recovery { .. } => return Err(KeychainError::WrongKind { kind: "recovery" }),
    };
    Ok(b64_decode(wrapped_key)?)
}

// =============================================================================
// macOS implementation
// =============================================================================

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use keyring::Entry;

    pub fn create_keychain_envelope(
        vault_key: &[u8; KEY_LEN],
        account: &str,
    ) -> Result<Envelope, KeychainError> {
        let entry_account = keychain_account_name(account);

        // 1. Generate a fresh 32-byte wrap key from the OS CSPRNG.
        let mut wrap_key = Zeroizing::new([0u8; KEY_LEN]);
        getrandom::getrandom(wrap_key.as_mut()).map_err(KeychainError::RngUnavailable)?;

        // 2. Store the wrap key in the macOS Keychain under the
        //    namespaced account name. NOTE: biometric protection is
        //    deferred — see the module-level doc.
        //
        //    The wrap key bytes are stored base64-encoded because
        //    `keyring` works with passwords (UTF-8 strings); the
        //    base64 alphabet survives the round-trip safely.
        let entry = Entry::new(KEYCHAIN_SERVICE, &entry_account)?;
        entry.set_password(&b64_encode(wrap_key.as_ref()))?;

        // 3. AEAD-wrap `vault_key` under the wrap key.
        let packed = aead::encrypt_packed(&wrap_key, KEYCHAIN_ENVELOPE_AAD, vault_key.as_ref())?;

        Ok(Envelope::Keychain {
            keychain_account: entry_account,
            wrapped_key: b64_encode(&packed),
        })
    }

    pub fn unwrap_keychain(envelope: &Envelope) -> Result<Zeroizing<[u8; KEY_LEN]>, KeychainError> {
        let entry_account = parse_envelope_account(envelope)?.to_owned();
        let packed = parse_envelope_payload(envelope)?;

        let entry = Entry::new(KEYCHAIN_SERVICE, &entry_account)?;
        let wrap_key_b64 = entry.get_password()?;
        let wrap_key_bytes = b64_decode(&wrap_key_b64)?;
        if wrap_key_bytes.len() != KEY_LEN {
            return Err(KeychainError::WrapKeyLength {
                got: wrap_key_bytes.len(),
            });
        }
        let mut wrap_key = Zeroizing::new([0u8; KEY_LEN]);
        wrap_key.copy_from_slice(&wrap_key_bytes);

        let plaintext = aead::decrypt_packed(&wrap_key, KEYCHAIN_ENVELOPE_AAD, &packed)?;
        if plaintext.len() != KEY_LEN {
            return Err(KeychainError::VaultKeyLength {
                got: plaintext.len(),
            });
        }
        let mut out = Zeroizing::new([0u8; KEY_LEN]);
        out.copy_from_slice(&plaintext);
        Ok(out)
    }

    pub fn delete_keychain_entry(account: &str) -> Result<(), KeychainError> {
        let entry_account = keychain_account_name(account);
        let entry = Entry::new(KEYCHAIN_SERVICE, &entry_account)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(KeychainError::from(e)),
        }
    }
}

// =============================================================================
// Non-macOS stub
// =============================================================================

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn create_keychain_envelope(
        _vault_key: &[u8; KEY_LEN],
        _account: &str,
    ) -> Result<Envelope, KeychainError> {
        Err(KeychainError::Unsupported)
    }

    pub fn unwrap_keychain(envelope: &Envelope) -> Result<Zeroizing<[u8; KEY_LEN]>, KeychainError> {
        // Wrong-kind detection still works on non-macOS so callers
        // get a precise error when they pass a Passphrase or Recovery
        // envelope by mistake. Only the actual macOS-required path
        // (correct kind → fetch from keychain) returns Unsupported.
        let _ = parse_envelope_account(envelope)?;
        Err(KeychainError::Unsupported)
    }

    pub fn delete_keychain_entry(_account: &str) -> Result<(), KeychainError> {
        Err(KeychainError::Unsupported)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::EnvelopeKdfParams;

    fn fixed_vault_key() -> [u8; KEY_LEN] {
        let mut k = [0u8; KEY_LEN];
        for (i, b) in k.iter_mut().enumerate() {
            *b = (i + 0x30) as u8;
        }
        k
    }

    fn passphrase_envelope_for_kind_test() -> Envelope {
        Envelope::Passphrase {
            argon2_salt: b64_encode(&[0; 32]),
            argon2_params: EnvelopeKdfParams { m: 8, t: 1, p: 1 },
            wrapped_key: b64_encode(&[0; 64]),
        }
    }

    fn recovery_envelope_for_kind_test() -> Envelope {
        Envelope::Recovery {
            bip39_salt: b64_encode(&[0; 32]),
            wrapped_key: b64_encode(&[0; 64]),
        }
    }

    // -- Cross-platform behaviour -------------------------------------------

    #[test]
    fn unwrap_rejects_passphrase_envelope_on_every_target() {
        let env = passphrase_envelope_for_kind_test();
        match unwrap_keychain(&env).unwrap_err() {
            KeychainError::WrongKind { kind } => assert_eq!(kind, "passphrase"),
            other => panic!("expected WrongKind, got {other:?}"),
        }
    }

    #[test]
    fn unwrap_rejects_recovery_envelope_on_every_target() {
        let env = recovery_envelope_for_kind_test();
        match unwrap_keychain(&env).unwrap_err() {
            KeychainError::WrongKind { kind } => assert_eq!(kind, "recovery"),
            other => panic!("expected WrongKind, got {other:?}"),
        }
    }

    #[test]
    fn keychain_account_name_uses_prefix() {
        let n = keychain_account_name("test-account");
        assert!(n.starts_with(KEYCHAIN_ACCOUNT_PREFIX));
        assert!(n.ends_with("test-account"));
    }

    #[test]
    fn parse_envelope_account_reads_keychain_variant() {
        let env = Envelope::Keychain {
            keychain_account: "vault-wrap:x".to_owned(),
            wrapped_key: b64_encode(&[0; 64]),
        };
        let acct = parse_envelope_account(&env).unwrap();
        assert_eq!(acct, "vault-wrap:x");
    }

    // -- Non-macOS stub behaviour --------------------------------------------

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn create_returns_unsupported_on_non_macos() {
        let err = create_keychain_envelope(&fixed_vault_key(), "test").unwrap_err();
        assert!(matches!(err, KeychainError::Unsupported));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unwrap_returns_unsupported_for_correct_kind_on_non_macos() {
        // With the correct envelope kind we still hit `Unsupported`
        // on non-macOS because reaching the Keychain isn't possible.
        let env = Envelope::Keychain {
            keychain_account: "vault-wrap:test".to_owned(),
            wrapped_key: b64_encode(&[0; 64]),
        };
        assert!(matches!(
            unwrap_keychain(&env).unwrap_err(),
            KeychainError::Unsupported
        ));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn delete_returns_unsupported_on_non_macos() {
        assert!(matches!(
            delete_keychain_entry("test").unwrap_err(),
            KeychainError::Unsupported
        ));
    }

    // -- macOS round trip (manual / ignored) --------------------------------

    /// Cleanup helper that deletes the Keychain entry on drop, even
    /// if the test panics. Tests use unique account names so multiple
    /// concurrent runs don't collide.
    #[cfg(target_os = "macos")]
    struct KeychainTestEntry {
        account: String,
    }

    #[cfg(target_os = "macos")]
    impl Drop for KeychainTestEntry {
        fn drop(&mut self) {
            let _ = delete_keychain_entry(&self.account);
        }
    }

    /// Round-trip a real Keychain envelope. Marked `#[ignore]` so it
    /// is excluded from `cargo test` by default — it requires an
    /// unlocked macOS keychain and writes a real entry. Run with
    /// `cargo test -p devboy-vault-crypto keychain::tests -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "writes to the macOS Keychain — run manually with --ignored"]
    fn macos_create_then_unwrap_recovers_vault_key() {
        // Unique-per-test account name so concurrent runs cannot
        // collide on the Keychain entry.
        let account = format!(
            "test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let _cleanup = KeychainTestEntry {
            account: account.clone(),
        };

        let vk = fixed_vault_key();
        let env = create_keychain_envelope(&vk, &account).expect("create on macOS must succeed");
        let unwrapped = unwrap_keychain(&env).expect("unwrap on macOS must succeed");
        assert_eq!(unwrapped.as_ref(), &vk);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "writes to the macOS Keychain — run manually with --ignored"]
    fn macos_delete_is_idempotent() {
        let account = format!(
            "test-delete-idempotent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        // Delete before any create — should still be Ok.
        delete_keychain_entry(&account).expect("delete of non-existent entry must succeed");
        // Create.
        let _ = create_keychain_envelope(&fixed_vault_key(), &account).unwrap();
        // First delete removes the entry.
        delete_keychain_entry(&account).unwrap();
        // Second delete still Ok (idempotent).
        delete_keychain_entry(&account).unwrap();
    }
}
