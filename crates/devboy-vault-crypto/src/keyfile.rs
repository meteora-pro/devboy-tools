//! Keyfile envelope for unattended cold start (ADR-024 §6).
//!
//! # What problem this solves
//!
//! After the OS keychain left the default chain, nothing could
//! open a vault without a human typing a passphrase. That is fine
//! on a laptop and useless on a server, in a container, or for an
//! integration harness — the environments where a daemon has to
//! come back on its own after a reboot.
//!
//! A keyfile restores that: 32 bytes on disk whose HKDF output
//! wraps the vault key.
//!
//! # What it does and does not protect against
//!
//! Stated plainly, because the honest scope is narrow:
//!
//! - **Protects against a file-level leak.** A backup, a cloud
//!   sync, a stray `git add`, or a shared directory that captures
//!   the vault will not capture the key, provided the two live in
//!   different trees — which is why the default path is outside
//!   the config directory and why the envelope deliberately does
//!   **not** record where the keyfile is.
//! - **Does not protect against a process running as the same
//!   user.** Anything that can read the vault can read the keyfile
//!   too. Neither did the OS keychain on Linux, where the Secret
//!   Service hands stored secrets to any process in the session,
//!   or on Windows, where DPAPI is scoped to the user.
//!
//! So this is a lateral move in security terms and a real gain in
//! portability: it works identically everywhere, with no D-Bus, no
//! daemon, and no prompt.
//!
//! # Permission enforcement
//!
//! A keyfile readable by other users defeats the one guarantee it
//! offers, so [`load_keyfile`] refuses one outright rather than
//! warning. A warning here would be routinely ignored, and the
//! failure it precedes is silent.

use std::path::Path;

use hkdf::Hkdf;
use sha2::Sha256;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::aead::{self, AeadError, KEY_LEN};
use crate::format::{Envelope, b64_decode, b64_encode};

/// AAD bound into the keyfile envelope's AEAD wrap.
pub const KEYFILE_ENVELOPE_AAD: &str = "devboy-vault-envelope:keyfile:v1";

/// HKDF `info` label for the keyfile wrap key.
pub const KEYFILE_HKDF_INFO: &[u8] = b"devboy-vault-keyfile-key-v1";

/// Size of a generated keyfile, in bytes.
pub const KEYFILE_LEN: usize = 32;

/// Minimum accepted keyfile length.
///
/// Matches [`KEYFILE_LEN`]: there is no reason to accept a shorter
/// one, and a truncated file is a likelier explanation than a
/// deliberate choice.
pub const MIN_KEYFILE_LEN: usize = 32;

/// Failure modes of the keyfile envelope.
#[derive(Debug, Error)]
pub enum KeyfileError {
    /// The keyfile could not be read.
    #[error("could not read keyfile {path}: {source}")]
    Read {
        /// Path that failed to open.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The keyfile is group- or world-accessible.
    ///
    /// Refused rather than warned about: a keyfile others can read
    /// provides none of the protection it exists for.
    #[error(
        "keyfile {path} has mode {mode:04o}; it must be readable only by its owner (0600). \
         Fix it with `chmod 600 {path}`"
    )]
    Permissions {
        /// The offending path.
        path: String,
        /// Permission bits as found on disk.
        mode: u32,
    },

    /// The keyfile is shorter than [`MIN_KEYFILE_LEN`].
    #[error("keyfile {path} is {got} bytes; at least {MIN_KEYFILE_LEN} are required")]
    TooShort {
        /// The offending path.
        path: String,
        /// Length actually read.
        got: usize,
    },

    /// AEAD wrap or unwrap failed.
    #[error(transparent)]
    Aead(#[from] AeadError),

    /// The envelope handed in is not a keyfile envelope.
    #[error("expected a keyfile envelope, got a {kind} envelope")]
    WrongKind {
        /// The kind actually supplied.
        kind: &'static str,
    },

    /// Stored salt is not the expected length.
    #[error("keyfile envelope salt must be 32 bytes, got {got}")]
    SaltLength {
        /// Length actually decoded.
        got: usize,
    },

    /// A base64 field failed to decode.
    #[error("keyfile envelope field is not valid base64: {0}")]
    Base64(#[from] base64::DecodeError),

    /// HKDF refused to expand.
    #[error("HKDF expansion failed")]
    HkdfFailed,
}

/// Read a keyfile, enforcing its permissions and length.
///
/// The contents are wrapped in [`Zeroizing`] so the bytes leave
/// RAM when the caller drops them.
pub fn load_keyfile(path: &Path) -> Result<Zeroizing<Vec<u8>>, KeyfileError> {
    let display = path.display().to_string();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let meta = std::fs::metadata(path).map_err(|source| KeyfileError::Read {
            path: display.clone(),
            source,
        })?;
        let mode = meta.permissions().mode() & 0o777;
        // Anything readable, writable or executable by group or
        // other defeats the point.
        if mode & 0o077 != 0 {
            return Err(KeyfileError::Permissions {
                path: display,
                mode,
            });
        }
    }

    let bytes = std::fs::read(path).map_err(|source| KeyfileError::Read {
        path: display.clone(),
        source,
    })?;

    if bytes.len() < MIN_KEYFILE_LEN {
        return Err(KeyfileError::TooShort {
            path: display,
            got: bytes.len(),
        });
    }

    Ok(Zeroizing::new(bytes))
}

/// Write a freshly generated keyfile at `path` with mode 0600.
///
/// Refuses to overwrite an existing file: silently replacing a
/// keyfile would orphan every envelope wrapped under the old one,
/// and the vault would become unopenable by that method with no
/// warning.
pub fn create_keyfile(path: &Path) -> Result<Zeroizing<Vec<u8>>, KeyfileError> {
    let display = path.display().to_string();

    let mut bytes = Zeroizing::new(vec![0u8; KEYFILE_LEN]);
    getrandom::getrandom(bytes.as_mut()).map_err(|e| KeyfileError::Read {
        path: display.clone(),
        source: std::io::Error::other(e),
    })?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| KeyfileError::Read {
            path: display.clone(),
            source,
        })?;
    }

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Created 0600 from the start rather than chmod'ed after:
        // otherwise the bytes exist world-readable for a moment.
        opts.mode(0o600);
    }

    use std::io::Write;
    let mut file = opts.open(path).map_err(|source| KeyfileError::Read {
        path: display.clone(),
        source,
    })?;
    file.write_all(&bytes)
        .map_err(|source| KeyfileError::Read {
            path: display.clone(),
            source,
        })?;
    file.sync_all().map_err(|source| KeyfileError::Read {
        path: display,
        source,
    })?;

    Ok(bytes)
}

/// Derive the envelope wrap key from keyfile bytes.
pub fn derive_keyfile_key(
    keyfile: &[u8],
    salt: &[u8],
) -> Result<Zeroizing<[u8; KEY_LEN]>, KeyfileError> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), keyfile);
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    hkdf.expand(KEYFILE_HKDF_INFO, out.as_mut())
        .map_err(|_| KeyfileError::HkdfFailed)?;
    Ok(out)
}

/// Wrap `vault_key` in a keyfile envelope.
pub fn create_keyfile_envelope(
    vault_key: &[u8; KEY_LEN],
    keyfile: &[u8],
    salt: [u8; 32],
) -> Result<Envelope, KeyfileError> {
    let wrap_key = derive_keyfile_key(keyfile, &salt)?;
    let packed = aead::encrypt_packed(&wrap_key, KEYFILE_ENVELOPE_AAD, vault_key.as_ref())?;
    Ok(Envelope::Keyfile {
        keyfile_salt: b64_encode(&salt),
        wrapped_key: b64_encode(&packed),
    })
}

/// Unwrap a keyfile envelope and return the vault key.
pub fn unwrap_keyfile(
    envelope: &Envelope,
    keyfile: &[u8],
) -> Result<Zeroizing<[u8; KEY_LEN]>, KeyfileError> {
    let (keyfile_salt, wrapped_key) = match envelope {
        Envelope::Keyfile {
            keyfile_salt,
            wrapped_key,
        } => (keyfile_salt, wrapped_key),
        Envelope::Passphrase { .. } => {
            return Err(KeyfileError::WrongKind { kind: "passphrase" });
        }
        Envelope::Recovery { .. } => return Err(KeyfileError::WrongKind { kind: "recovery" }),
        Envelope::Totp { .. } => return Err(KeyfileError::WrongKind { kind: "totp" }),
    };

    let salt_bytes = b64_decode(keyfile_salt)?;
    if salt_bytes.len() != 32 {
        return Err(KeyfileError::SaltLength {
            got: salt_bytes.len(),
        });
    }

    let wrap_key = derive_keyfile_key(keyfile, &salt_bytes)?;
    let packed = b64_decode(wrapped_key)?;
    let plaintext = aead::decrypt_packed(&wrap_key, KEYFILE_ENVELOPE_AAD, &packed)?;

    if plaintext.len() != KEY_LEN {
        return Err(AeadError::AeadFailed.into());
    }
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    out.copy_from_slice(&plaintext);
    Ok(out)
}

/// Default keyfile location, deliberately **outside** the config
/// directory that holds the vault.
///
/// `~/.local/state/devboy-tools/vault.key` on Linux (or the
/// platform's state dir), while the vault lives under the config
/// dir. Keeping them in separate trees is what makes a keyfile
/// worth having: a backup or sync of one does not carry the other.
///
/// # Why the separation is checked rather than assumed
///
/// The platform's "state" and "config" directories are distinct
/// on Linux and Windows, and on macOS they are *the same
/// directory* — `dirs` maps state, data and config all to
/// `~/Library/Application Support`. Deriving the path and hoping
/// silently produced a keyfile sitting next to the vault it is
/// supposed to be separable from, which defeats the point while
/// looking correct.
///
/// So the derived path is checked against the config dir, and a
/// collision falls back to `home_state_keyfile` —
/// `~/.local/state/devboy-tools/vault.key` spelled out. Any
/// future platform where `dirs` collapses two of these gets the
/// same treatment without anybody having to notice.
pub fn default_keyfile_path() -> Option<std::path::PathBuf> {
    let derived = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|d| d.join("devboy-tools").join("vault.key"));

    keyfile_path_from(derived, dirs::config_dir().as_deref(), home_state_keyfile())
}

/// The choice itself, with the environment passed in.
///
/// Split out because the interesting branch only fires on
/// platforms where `dirs` collapses two directories into one:
/// tested through [`default_keyfile_path`] it would be exercised
/// on macOS and nowhere else, which is the wrong way round for a
/// fix whose whole job is to hold on the platform the developer
/// is least likely to be running.
fn keyfile_path_from(
    derived: Option<std::path::PathBuf>,
    config: Option<&std::path::Path>,
    fallback: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    match (derived, config) {
        (Some(path), Some(config)) if path.starts_with(config) => fallback,
        (Some(path), _) => Some(path),
        (None, _) => fallback,
    }
}

/// The XDG state path spelled out relative to `$HOME`.
///
/// Used where the platform's own state directory is unusable —
/// either absent, or the same directory as the config dir. Not a
/// native macOS convention, but it is stable, is not purged the
/// way `~/Library/Caches` is, and reads the same in the docs on
/// every UNIX.
fn home_state_keyfile() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| {
        home.join(".local")
            .join("state")
            .join("devboy-tools")
            .join("vault.key")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const VAULT_KEY: [u8; KEY_LEN] = [0x42; KEY_LEN];

    fn keyfile_bytes() -> Vec<u8> {
        vec![0x11; KEYFILE_LEN]
    }

    #[test]
    fn envelope_round_trips() {
        let env = create_keyfile_envelope(&VAULT_KEY, &keyfile_bytes(), [0x33; 32]).unwrap();
        let recovered = unwrap_keyfile(&env, &keyfile_bytes()).unwrap();
        assert_eq!(recovered.as_ref(), &VAULT_KEY);
    }

    #[test]
    fn a_different_keyfile_cannot_unwrap() {
        let env = create_keyfile_envelope(&VAULT_KEY, &keyfile_bytes(), [0x33; 32]).unwrap();
        assert!(unwrap_keyfile(&env, &[0x22; KEYFILE_LEN]).is_err());
    }

    /// Kind-bound AAD: a wrapped key from another envelope kind
    /// must not open here even if the ciphertext is copied across.
    #[test]
    fn envelope_kinds_are_not_interchangeable() {
        let env = create_keyfile_envelope(&VAULT_KEY, &keyfile_bytes(), [0x33; 32]).unwrap();
        let Envelope::Keyfile { wrapped_key, .. } = &env else {
            unreachable!()
        };

        let disguised = Envelope::Totp {
            totp_salt: b64_encode(&[0x33; 32]),
            wrapped_key: wrapped_key.clone(),
        };
        assert!(matches!(
            unwrap_keyfile(&disguised, &keyfile_bytes()),
            Err(KeyfileError::WrongKind { kind: "totp" })
        ));
    }

    #[test]
    fn tampering_is_detected() {
        let env = create_keyfile_envelope(&VAULT_KEY, &keyfile_bytes(), [0x33; 32]).unwrap();
        let Envelope::Keyfile {
            keyfile_salt,
            wrapped_key,
        } = env
        else {
            unreachable!()
        };

        let mut bytes = b64_decode(&wrapped_key).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;

        let tampered = Envelope::Keyfile {
            keyfile_salt,
            wrapped_key: b64_encode(&bytes),
        };
        assert!(unwrap_keyfile(&tampered, &keyfile_bytes()).is_err());
    }

    #[test]
    fn create_writes_a_private_keyfile_of_the_right_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.key");

        let bytes = create_keyfile(&path).unwrap();
        assert_eq!(bytes.len(), KEYFILE_LEN);

        let on_disk = fs::read(&path).unwrap();
        assert_eq!(on_disk, *bytes);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "keyfile must be created private, got {mode:04o}"
            );
        }
    }

    /// Silently replacing a keyfile would orphan every envelope
    /// wrapped under the old one, leaving the vault unopenable by
    /// that method with no warning.
    #[test]
    fn create_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.key");

        create_keyfile(&path).unwrap();
        assert!(
            create_keyfile(&path).is_err(),
            "must not clobber an existing keyfile"
        );
    }

    #[test]
    fn load_round_trips_what_create_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.key");

        let written = create_keyfile(&path).unwrap();
        let loaded = load_keyfile(&path).unwrap();
        assert_eq!(*loaded, *written);
    }

    /// A keyfile others can read provides none of the protection
    /// it exists for, so this is refused rather than warned about.
    #[cfg(unix)]
    #[test]
    fn load_refuses_a_group_or_world_readable_keyfile() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.key");
        create_keyfile(&path).unwrap();

        for bad_mode in [0o640, 0o644, 0o604, 0o660] {
            fs::set_permissions(&path, fs::Permissions::from_mode(bad_mode)).unwrap();

            match load_keyfile(&path) {
                Err(KeyfileError::Permissions { mode, .. }) => {
                    assert_eq!(mode, bad_mode);
                }
                other => panic!("mode {bad_mode:04o} should have been refused, got {other:?}"),
            }
        }

        // And 0600 is accepted again.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_keyfile(&path).is_ok());
    }

    #[test]
    fn load_refuses_a_truncated_keyfile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.key");
        fs::write(&path, [0u8; 8]).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        assert!(matches!(
            load_keyfile(&path),
            Err(KeyfileError::TooShort { got: 8, .. })
        ));
    }

    #[test]
    fn load_reports_a_missing_file_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_keyfile(&dir.path().join("nope.key")).unwrap_err();
        assert!(matches!(err, KeyfileError::Read { .. }), "{err:?}");
    }

    /// The whole point of the default location: vault and keyfile
    /// must not share a directory, or a single backup carries both.
    ///
    /// This one caught the macOS case, where `dirs` returns the
    /// same directory for state, data and config — so keep it
    /// honest about skipping. A `return` on a missing home dir
    /// reads like a pass and would hide a real regression on any
    /// runner configured that way.
    #[test]
    fn default_path_is_outside_the_config_tree() {
        let (Some(keyfile), Some(config)) = (default_keyfile_path(), dirs::config_dir()) else {
            panic!("expected both a keyfile default and a config dir in a test environment");
        };

        assert!(
            !keyfile.starts_with(&config),
            "keyfile default {} must not live under the config dir {}",
            keyfile.display(),
            config.display()
        );
    }

    /// macOS in a nutshell: `dirs` hands back the same directory
    /// for state and for config, so the derived path lands next
    /// to the vault it is supposed to be separable from. Written
    /// against the pure function so every platform runs it.
    #[test]
    fn a_state_dir_that_is_the_config_dir_falls_back() {
        let shared = std::path::Path::new("/somewhere/Application Support");
        let derived = shared.join("devboy-tools").join("vault.key");
        let fallback = std::path::PathBuf::from("/home/u/.local/state/devboy-tools/vault.key");

        let chosen = keyfile_path_from(Some(derived), Some(shared), Some(fallback.clone()));

        assert_eq!(chosen, Some(fallback), "a collision must not be kept");
    }

    #[test]
    fn a_state_dir_distinct_from_the_config_dir_is_kept() {
        let derived = std::path::PathBuf::from("/home/u/.local/state/devboy-tools/vault.key");
        let config = std::path::Path::new("/home/u/.config");
        let fallback = std::path::PathBuf::from("/unused");

        let chosen = keyfile_path_from(Some(derived.clone()), Some(config), Some(fallback));

        assert_eq!(chosen, Some(derived), "no collision, nothing to fix");
    }

    /// A path that merely shares a *prefix string* with the config
    /// dir is not inside it. `starts_with` on `Path` compares
    /// whole components, and this pins that: `/home/u/.config-old`
    /// is a sibling, not a child.
    #[test]
    fn a_sibling_directory_is_not_mistaken_for_a_collision() {
        let derived = std::path::PathBuf::from("/home/u/.config-old/devboy-tools/vault.key");
        let config = std::path::Path::new("/home/u/.config");
        let fallback = std::path::PathBuf::from("/fallback");

        let chosen = keyfile_path_from(Some(derived.clone()), Some(config), Some(fallback));

        assert_eq!(chosen, Some(derived));
    }

    #[test]
    fn no_derivable_state_dir_falls_back() {
        let fallback = std::path::PathBuf::from("/home/u/.local/state/devboy-tools/vault.key");
        let chosen = keyfile_path_from(
            None,
            Some(std::path::Path::new("/c")),
            Some(fallback.clone()),
        );
        assert_eq!(chosen, Some(fallback));
    }

    /// The fallback is only reachable on platforms that collapse
    /// the directories, so exercise it directly — otherwise the
    /// branch that fixes macOS is untested everywhere else.
    #[test]
    fn the_home_relative_fallback_is_also_outside_the_config_tree() {
        let Some(fallback) = home_state_keyfile() else {
            panic!("expected a home directory in a test environment");
        };
        let Some(config) = dirs::config_dir() else {
            panic!("expected a config dir in a test environment");
        };

        assert!(
            !fallback.starts_with(&config),
            "the fallback {} defeats its own purpose under {}",
            fallback.display(),
            config.display()
        );
        assert!(fallback.ends_with("devboy-tools/vault.key"));
    }
}
