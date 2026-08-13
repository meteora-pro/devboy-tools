//! `devboy secrets keyfile` end to end, against the real binary
//! (ADR-024 §6, Ф7-2 + Ф16).
//!
//! # Why the binary and not the functions
//!
//! The defect this command fixes was not a wrong function. Every
//! piece existed — `add_keyfile_envelope`, the config field, the
//! daemon's unlock path — and none of them were connected to
//! anything a user could run. The daemon's error text even named a
//! command that had never been written.
//!
//! A unit test of the enrolment logic would have passed happily
//! throughout. So these tests spawn the actual `devboy` binary and
//! then check the vault file it left behind.
//!
//! # Why UNIX only
//!
//! `keyfile add` writes `secrets.keyfile_path` into the global
//! config, and the global config's location comes from
//! `dirs::config_dir()`. On Windows that resolves through the Known
//! Folder API, which no environment variable can redirect — so on
//! Windows these tests would not be hermetic: they would rewrite the
//! real config of whoever ran `cargo test`. Refusing to run is better
//! than running and modifying a developer's machine.
//!
//! The command itself is not UNIX-only; only this harness is. Making
//! it testable on Windows needs a config-directory override, which is
//! a change to `devboy-core` rather than to a test.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

use devboy_vault_crypto::format::{Envelope, VaultFile};
use devboy_vault_crypto::keyfile::load_keyfile;
use devboy_vault_crypto::vault::{InitialUnlock, UnlockMethod, Vault};
use secrecy::SecretString;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";

fn devboy_bin() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // test binary name
    path.pop(); // deps/
    path.push(format!("devboy{}", std::env::consts::EXE_SUFFIX));
    path
}

/// A hermetic home with a real vault already created.
struct Env {
    home: TempDir,
    vault: PathBuf,
    /// Where the keyfile goes.
    ///
    /// Passed explicitly rather than left to `default_keyfile_path()`
    /// because `dirs` does not honour `HOME` on Windows and does not
    /// honour `XDG_STATE_HOME` on macOS. Left to the default, every
    /// test in this file would write to the *real* user directory and
    /// race the others there — which is exactly how they failed on
    /// both platforms. The default path has its own unit tests.
    keyfile: PathBuf,
}

impl Env {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let vault = home.path().join("vault.dvb");

        // Cheapest Argon2 parameters the format allows: this test is
        // about wiring, and a realistic KDF would add seconds per run
        // for nothing.
        let mut init = InitialUnlock::with_passphrase(SecretString::from(PASSPHRASE.to_owned()));
        init.passphrase_params =
            Some(devboy_vault_crypto::format::EnvelopeKdfParams { m: 8, t: 1, p: 1 });
        Vault::create(&vault, init).expect("create vault");

        let keyfile = home.path().join("vault.key");
        Self {
            home,
            vault,
            keyfile,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(devboy_bin())
            .args(args)
            .args(self.path_args(args))
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_STATE_HOME", self.home.path().join("state"))
            .env("DEVBOY_VAULT_PATH", &self.vault)
            .env("DEVBOY_VAULT_PASSPHRASE", PASSPHRASE)
            .output()
            .expect("run devboy")
    }

    /// `--path` for the subcommands that accept it.
    fn path_args(&self, args: &[&str]) -> Vec<String> {
        if args.contains(&"add") {
            vec!["--path".to_string(), self.keyfile.display().to_string()]
        } else {
            Vec::new()
        }
    }

    fn keyfile_envelope(&self) -> Option<Option<String>> {
        let file = VaultFile::read_file(&self.vault).expect("read vault");
        file.envelopes.iter().find_map(|e| match e {
            Envelope::Keyfile {
                machine_binding, ..
            } => Some(machine_binding.clone()),
            _ => None,
        })
    }
}

/// Locate the `config.toml` the binary wrote, wherever the platform
/// put it.
///
/// Found rather than assumed: `dirs::config_dir()` honours
/// `XDG_CONFIG_HOME` on Linux and `$HOME/Library/Application Support`
/// on macOS, so a hardcoded path passes on one and fails on the other.
fn find_config(root: &std::path::Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == "config.toml") {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_config(&path)
        {
            return Some(found);
        }
    }
    None
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The whole point: after running the command the daemon's error
/// message names, an unattended start is actually possible.
#[test]
fn enrolling_a_keyfile_makes_the_vault_openable_without_a_passphrase() {
    let env = Env::new();
    assert!(
        env.keyfile_envelope().is_none(),
        "a fresh vault must not already have a keyfile envelope"
    );

    let out = env.run(&["secrets", "keyfile", "add"]);
    assert!(
        out.status.success(),
        "add failed: {}\n{}",
        stdout(&out),
        String::from_utf8_lossy(&out.stderr)
    );

    let binding = env
        .keyfile_envelope()
        .expect("the envelope must be in the vault file after `keyfile add`");

    // And the file it wrote actually opens the vault — the property
    // the envelope exists for, checked rather than assumed.
    assert!(
        env.keyfile.exists(),
        "expected the keyfile at {}",
        env.keyfile.display()
    );

    let bytes = load_keyfile(&env.keyfile).expect("load keyfile");
    let vault = Vault::open(&env.vault, UnlockMethod::Keyfile { keyfile: bytes })
        .expect("the enrolled keyfile must open the vault");
    drop(vault);

    // On any machine with a stable identifier — which includes every
    // CI runner this suite targets — the envelope should be bound.
    // Where none exists the envelope is unbound by design, so the
    // assertion is on consistency with what the command reported.
    let reported_bound = stdout(&out).contains("bound:  yes");
    assert_eq!(
        reported_bound,
        binding.is_some(),
        "the message and the envelope disagree about machine binding: {}",
        stdout(&out)
    );
}

/// Enrolment is only half-done until the daemon can find the file,
/// and the daemon reads the path from configuration alone.
#[test]
fn enrolling_records_the_path_in_the_config() {
    let env = Env::new();
    let out = env.run(&["secrets", "keyfile", "add"]);
    assert!(out.status.success(), "{}", stdout(&out));

    // Found rather than assumed: `dirs::config_dir()` honours
    // `XDG_CONFIG_HOME` on Linux and `$HOME/Library/Application
    // Support` on macOS, so a hardcoded path passes on one and fails
    // on the other.
    let config = find_config(env.home.path())
        .unwrap_or_else(|| panic!("no config.toml under {}", env.home.path().display()));
    let text = std::fs::read_to_string(&config).expect("read config");

    assert!(
        text.contains("keyfile_path"),
        "the config must record where the keyfile is, or the daemon cannot use it:\n{text}"
    );
}

/// Overwriting silently would strand whatever the old file still
/// opens, and the user would find out at the next unattended start.
#[test]
fn a_second_add_refuses_to_clobber_the_existing_keyfile() {
    let env = Env::new();
    assert!(env.run(&["secrets", "keyfile", "add"]).status.success());

    let out = env.run(&["secrets", "keyfile", "add"]);
    assert!(!out.status.success(), "the second add should have failed");

    let combined = format!("{}{}", stdout(&out), String::from_utf8_lossy(&out.stderr));
    assert!(
        combined.contains("--use-existing"),
        "the refusal must say how to proceed: {combined}"
    );
}

/// `status` is how a user finds out whether an unattended start will
/// work, so it has to change once enrolment happens.
#[test]
fn status_reflects_enrolment() {
    let env = Env::new();

    let before = env.run(&["secrets", "keyfile", "status"]);
    assert!(
        stdout(&before).contains("not enrolled"),
        "{}",
        stdout(&before)
    );

    assert!(env.run(&["secrets", "keyfile", "add"]).status.success());

    let after = env.run(&["secrets", "keyfile", "status"]);
    let text = stdout(&after);
    assert!(text.contains("enrolled"), "{text}");
    assert!(!text.contains("not enrolled"), "{text}");
    assert!(text.contains("(present)"), "{text}");
}

/// Un-enrolling has to actually remove the envelope, or the old file
/// keeps opening the vault after the user believes it does not.
#[test]
fn removing_un_enrols_the_keyfile() {
    let env = Env::new();
    assert!(env.run(&["secrets", "keyfile", "add"]).status.success());
    assert!(env.keyfile_envelope().is_some());

    let out = env.run(&["secrets", "keyfile", "remove"]);
    assert!(out.status.success(), "{}", stdout(&out));

    assert!(
        env.keyfile_envelope().is_none(),
        "the envelope survived removal"
    );

    let bytes = load_keyfile(&env.keyfile).expect("the file itself is left in place");
    assert!(
        Vault::open(&env.vault, UnlockMethod::Keyfile { keyfile: bytes }).is_err(),
        "the un-enrolled keyfile still opens the vault"
    );
}

/// A binding that follows `DEVBOY_MACHINE_ID` is not a machine
/// binding: the daemon, started by systemd without that variable,
/// will refuse to unlock and report that the machine changed. The
/// one moment anyone can act on that is here.
#[test]
fn enrolling_under_a_machine_id_override_warns() {
    let env = Env::new();
    let out = Command::new(devboy_bin())
        .args(["secrets", "keyfile", "add", "--path"])
        .arg(&env.keyfile)
        .env("HOME", env.home.path())
        .env("XDG_CONFIG_HOME", env.home.path().join("config"))
        .env("XDG_STATE_HOME", env.home.path().join("state"))
        .env("DEVBOY_VAULT_PATH", &env.vault)
        .env("DEVBOY_VAULT_PASSPHRASE", PASSPHRASE)
        .env("DEVBOY_MACHINE_ID", "a-value-from-this-shell-only")
        .output()
        .expect("run devboy");

    assert!(out.status.success(), "{}", stdout(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("DEVBOY_MACHINE_ID"),
        "the trap must be named where someone can act on it: {stderr}"
    );
}
