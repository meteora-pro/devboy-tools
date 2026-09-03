//! Binding a keyfile envelope to the machine that made it
//! (ADR-024 §6, Ф16).
//!
//! # What this is for
//!
//! The keyfile envelope's protection is that the vault and the key
//! live in different trees, so a backup or a cloud sync captures one
//! and not the other. That holds right up until someone syncs a whole
//! home directory, or copies a container image, or restores a machine
//! wholesale — at which point both halves travel together and the
//! vault opens anywhere.
//!
//! Mixing a machine identifier into the derivation closes that: the
//! same two files on a different machine derive a different wrap key
//! and the vault does not open. Non-portability is the feature, not a
//! side effect.
//!
//! # What it is honestly worth
//!
//! Not much against an attacker who is *trying*. Every input here is
//! readable by any process on the box, so anyone who can copy your
//! vault can copy your machine id too, and a targeted attacker will.
//!
//! It is worth a great deal against the two things actually in this
//! threat model: **accidental disclosure** — a synced directory, a
//! shared backup, an image pushed to a registry — and **generic
//! credential harvesters**, which grab files by shape and never
//! reconstruct a per-host derivation. Neither of those adapts.
//!
//! Calling this "encryption at rest that also resists copying" would
//! be a lie. Calling it "the copied file does not just work" is
//! accurate, and that is the whole claim.
//!
//! # Why absence is not an error
//!
//! Not every environment has a stable machine identifier — a
//! scratch container, a stripped image, a platform this code does not
//! know. Refusing to create an envelope there would break unattended
//! start for the users who need it most.
//!
//! So the binding is recorded *in the envelope*. An envelope created
//! without one unwraps without one, forever. An envelope created with
//! one requires it, and if the machine has changed it says so in
//! those words rather than surfacing "AEAD failed" — the mismatch is
//! recoverable (re-run `devboy init`) but only if the user is told
//! which thing went wrong.

use sha2::{Digest, Sha256};

/// Label recorded in an envelope bound by this scheme.
///
/// Versioned so a future change to the input set can be told apart
/// from this one rather than silently failing to unwrap.
pub const BINDING_V1: &str = "machine-v1";

/// Environment variable that overrides the machine identifier.
///
/// Exists so the behaviour can be tested — deriving a key from the
/// real machine id makes a test that either cannot run twice or
/// cannot run at all in CI.
///
/// It is **not** a security boundary. Anyone who can set this
/// variable in the daemon's environment is already running as the
/// user whose vault it is, and could read the keyfile directly.
pub const MACHINE_ID_OVERRIDE_ENV: &str = "DEVBOY_MACHINE_ID";

/// A machine identifier, reduced to a digest.
///
/// The raw identifier is never carried around: it is mildly
/// identifying (a stable per-host UUID) and nothing downstream needs
/// it, so the digest is taken at the point of collection.
#[derive(Clone, PartialEq, Eq)]
pub struct MachineFingerprint {
    digest: [u8; 32],
    /// Where the identifier came from, for diagnostics.
    source: &'static str,
}

impl std::fmt::Debug for MachineFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The digest is not secret, but printing it invites it into
        // logs and issue reports where it becomes a host identifier.
        f.debug_struct("MachineFingerprint")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl MachineFingerprint {
    /// Bytes to mix into a key derivation.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.digest
    }

    /// Which source supplied the identifier.
    pub fn source(&self) -> &'static str {
        self.source
    }
}

/// Collect this machine's identifier, if one is available.
///
/// Returns `None` when no stable identifier could be found, which is
/// a supported outcome — see the module docs.
pub fn machine_fingerprint() -> Option<MachineFingerprint> {
    let (raw, source) = raw_machine_id()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(MachineFingerprint {
        digest: digest_of(trimmed),
        source,
    })
}

/// Build a fingerprint from an identifier supplied by the caller.
///
/// For callers that already know which machine they mean — a test
/// exercising the cross-machine behaviour, or a future "re-bind this
/// vault to another host" flow — rather than asking the platform.
pub fn fingerprint_of(raw: &str) -> MachineFingerprint {
    MachineFingerprint {
        digest: digest_of(raw),
        source: "explicit",
    }
}

/// Hash an identifier into the fixed-width form used downstream.
fn digest_of(raw: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    // Domain-separated, so this digest cannot collide with another
    // use of SHA-256 over the same string elsewhere.
    hasher.update(b"devboy-machine-fingerprint-v1\0");
    hasher.update(raw.as_bytes());
    hasher.finalize().into()
}

/// Read the platform's stable machine identifier.
fn raw_machine_id() -> Option<(String, &'static str)> {
    if let Ok(overridden) = std::env::var(MACHINE_ID_OVERRIDE_ENV)
        && !overridden.trim().is_empty()
    {
        return Some((overridden, "environment override"));
    }
    platform_machine_id()
}

/// Linux and other systemd-adjacent systems: `/etc/machine-id`, with
/// the older D-Bus location as a fallback.
///
/// Both are plain files, so no subprocess is involved on the platform
/// where a daemon is most likely to be running unattended.
#[cfg(all(unix, not(target_os = "macos")))]
fn platform_machine_id() -> Option<(String, &'static str)> {
    for (path, label) in [
        ("/etc/machine-id", "/etc/machine-id"),
        ("/var/lib/dbus/machine-id", "/var/lib/dbus/machine-id"),
    ] {
        if let Ok(contents) = std::fs::read_to_string(path)
            && !contents.trim().is_empty()
        {
            return Some((contents, label));
        }
    }
    None
}

/// macOS: the hardware UUID from the IO registry.
///
/// Reached through `ioreg` rather than IOKit FFI: this runs once per
/// unlock, never on a hot path, and a subprocess needs no `unsafe`
/// and no extra dependency in a crate that does key derivation.
#[cfg(target_os = "macos")]
fn platform_machine_id() -> Option<(String, &'static str)> {
    let output = std::process::Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let uuid = extract_quoted_after(&text, "IOPlatformUUID")?;
    Some((uuid, "IOPlatformUUID"))
}

/// Windows: `MachineGuid`, written at install time and stable for the
/// life of the installation.
///
/// Read through `reg query` for the same reason `ioreg` is used on
/// macOS — no registry crate, no `unsafe`, and it runs once.
#[cfg(windows)]
fn platform_machine_id() -> Option<(String, &'static str)> {
    let output = std::process::Command::new("reg")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Cryptography",
            "/v",
            "MachineGuid",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let guid = text
        .lines()
        .find(|l| l.contains("MachineGuid"))
        .and_then(|l| l.split_whitespace().next_back())
        .map(str::to_owned)?;
    Some((guid, "MachineGuid"))
}

/// Any platform without a known identifier. Binding is simply
/// unavailable there, which the envelope records.
#[cfg(not(any(unix, windows)))]
fn platform_machine_id() -> Option<(String, &'static str)> {
    None
}

/// Pull `"<value>"` out of a line containing `key`.
///
/// Shared by the macOS reader and its test; kept separate so the
/// parsing can be tested on every platform rather than only where
/// `ioreg` exists.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn extract_quoted_after(text: &str, key: &str) -> Option<String> {
    let line = text.lines().find(|l| l.contains(key))?;
    let after = line.split_once('=')?.1.trim();
    let inner = after.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.is_empty()).then(|| inner.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest has to actually depend on the identifier, or the
    /// binding is decorative.
    #[test]
    fn different_machines_produce_different_digests() {
        assert_ne!(digest_of("machine-a"), digest_of("machine-b"));
    }

    #[test]
    fn the_same_machine_produces_the_same_digest() {
        assert_eq!(digest_of("machine-a"), digest_of("machine-a"));
    }

    /// Debug output ends up in logs and issue reports. A stable
    /// per-host digest there is a host identifier, so it stays out.
    #[test]
    fn debug_does_not_render_the_digest() {
        let fp = MachineFingerprint {
            digest: [7u8; 32],
            source: "test",
        };
        let rendered = format!("{fp:?}");

        assert!(rendered.contains("test"), "{rendered}");
        assert!(!rendered.contains('7'), "{rendered}");
    }

    #[test]
    fn the_ioreg_line_format_is_parsed() {
        let sample =
            "  | {\n    \"IOPlatformUUID\" = \"AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE\"\n  }";

        assert_eq!(
            extract_quoted_after(sample, "IOPlatformUUID").as_deref(),
            Some("AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE")
        );
    }

    #[test]
    fn a_missing_key_parses_to_none() {
        assert_eq!(extract_quoted_after("nothing here", "IOPlatformUUID"), None);
    }

    /// An empty value is worse than no value: it would bind every
    /// machine that also fails to read its id to the same digest.
    #[test]
    fn an_empty_quoted_value_parses_to_none() {
        assert_eq!(
            extract_quoted_after("\"IOPlatformUUID\" = \"\"", "IOPlatformUUID"),
            None
        );
    }
}
