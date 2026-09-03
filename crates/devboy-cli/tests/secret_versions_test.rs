//! Undoing a write, end to end against the real binary
//! (ADR-024 §5, Ф8-2).
//!
//! # Why this file exists
//!
//! The vault kept every version and `Vault::restore` worked, with
//! tests to prove it. Nothing called either one: no command, no RPC
//! method, no tool. So §5's promise — "an agent that stores the wrong
//! token is always recoverable" — was true of the file format and
//! false of the product, and every unit test in the crypto crate
//! stayed green throughout.
//!
//! These drive `devboy` itself and then read the vault back, because
//! that is the only kind of test that would have noticed.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

use devboy_vault_crypto::vault::{InitialUnlock, UnlockMethod, Vault};
use secrecy::{ExposeSecret, SecretString};
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";
const PATH: &str = "team/gitlab/token";

fn devboy_bin() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    p.pop();
    p.push(format!("devboy{}", std::env::consts::EXE_SUFFIX));
    p
}

struct Env {
    home: TempDir,
    vault: PathBuf,
}

impl Env {
    /// A vault where the good value was written first and then
    /// clobbered — the situation a user is in when they reach for
    /// this command.
    fn with_a_clobbered_secret() -> Self {
        let home = TempDir::new().unwrap();
        let vault_path = home.path().join("vault.dvb");

        let mut init = InitialUnlock::with_passphrase(SecretString::from(PASSPHRASE.to_owned()));
        init.passphrase_params =
            Some(devboy_vault_crypto::format::EnvelopeKdfParams { m: 8, t: 1, p: 1 });
        let outcome = Vault::create(&vault_path, init).expect("create vault");
        let mut vault = outcome.vault;

        vault
            .put(
                PATH,
                &SecretString::from("the-good-token".to_owned()),
                Default::default(),
            )
            .expect("write v1");
        vault
            .put(
                PATH,
                &SecretString::from("oops-wrong-token".to_owned()),
                Default::default(),
            )
            .expect("write v2");

        Self {
            home,
            vault: vault_path,
        }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(devboy_bin())
            .args(args)
            .env("HOME", self.home.path())
            .env("DEVBOY_CONFIG_DIR", self.home.path().join("config"))
            .env("DEVBOY_VAULT_PATH", &self.vault)
            .env("DEVBOY_VAULT_PASSPHRASE", PASSPHRASE)
            .output()
            .expect("run devboy")
    }

    fn current_value(&self) -> String {
        let vault = Vault::open(
            &self.vault,
            UnlockMethod::Passphrase(SecretString::from(PASSPHRASE.to_owned())),
        )
        .expect("reopen");
        vault
            .get(PATH)
            .expect("get")
            .expect("present")
            .expose_secret()
            .to_owned()
    }
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The whole point: a wrong value written over a good one can be
/// taken back.
#[test]
fn restore_brings_back_the_value_that_was_overwritten() {
    let env = Env::with_a_clobbered_secret();
    assert_eq!(env.current_value(), "oops-wrong-token");

    let out = env.run(&["secrets", "restore", PATH]);
    assert!(
        out.status.success(),
        "{}{}",
        stdout(&out),
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        env.current_value(),
        "the-good-token",
        "the earlier value must be live again"
    );
}

/// Restoring appends rather than rewriting, so the mistake is still
/// there to be restored in turn. Without that, "undo" would be a way
/// to destroy history.
#[test]
fn restoring_does_not_destroy_what_it_replaced() {
    let env = Env::with_a_clobbered_secret();
    assert!(env.run(&["secrets", "restore", PATH]).status.success());

    let out = env.run(&["secrets", "versions", PATH]);
    let text = stdout(&out);

    assert!(
        text.contains("v3"),
        "the restore itself must be a version: {text}"
    );
    assert!(
        text.contains("v2"),
        "the mistake must still be listed: {text}"
    );
}

/// An explicit version wins over "the one before the newest".
#[test]
fn an_explicit_version_can_be_named() {
    let env = Env::with_a_clobbered_secret();

    let out = env.run(&["secrets", "restore", PATH, "--version", "1"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(env.current_value(), "the-good-token");
}

/// The listing is how a user decides what to restore, so it has to
/// show the history — and must not become a way to read values.
#[test]
fn versions_lists_the_history_without_printing_values() {
    let env = Env::with_a_clobbered_secret();
    let out = env.run(&["secrets", "versions", PATH]);

    assert!(out.status.success(), "{}", stdout(&out));
    let text = stdout(&out);

    assert!(text.contains(PATH), "{text}");
    assert!(text.contains("v1") && text.contains("v2"), "{text}");
    assert!(
        !text.contains("the-good-token") && !text.contains("oops-wrong-token"),
        "a version listing must never print values: {text}"
    );
}

/// A path with no history is a typo more often than a bug, so the
/// error should point at the way to check.
#[test]
fn an_unknown_path_says_how_to_find_the_right_one() {
    let env = Env::with_a_clobbered_secret();
    let out = env.run(&["secrets", "versions", "team/nope/missing"]);

    assert!(!out.status.success());
    let text = format!("{}{}", stdout(&out), String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("devboy secrets list"), "{text}");
}

/// Purge is the one operation the version history does not protect
/// against, so it must actually destroy — and must refuse to do so
/// without explicit agreement.
#[test]
fn purge_destroys_only_after_explicit_agreement() {
    let env = Env::with_a_clobbered_secret();

    // Without --yes and without a terminal, it must refuse.
    let refused = env.run(&["secrets", "purge", PATH]);
    assert!(!refused.status.success(), "{}", stdout(&refused));
    let text = format!(
        "{}{}",
        stdout(&refused),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(text.contains("--yes"), "the refusal must say how: {text}");
    assert_eq!(
        env.current_value(),
        "oops-wrong-token",
        "a refused purge must not have destroyed anything"
    );

    // With --yes it goes through.
    let done = env.run(&["secrets", "purge", PATH, "--yes"]);
    assert!(done.status.success(), "{}", stdout(&done));

    let vault = Vault::open(
        &env.vault,
        UnlockMethod::Passphrase(SecretString::from(PASSPHRASE.to_owned())),
    )
    .expect("reopen");
    assert!(
        vault.versions(PATH).is_empty(),
        "every version should be gone"
    );
}

/// One version can be purged while the rest survive.
#[test]
fn a_single_version_can_be_purged_by_the_inline_form() {
    let env = Env::with_a_clobbered_secret();

    let out = env.run(&["secrets", "purge", &format!("{PATH}@1"), "--yes"]);
    assert!(out.status.success(), "{}", stdout(&out));

    let listing = stdout(&env.run(&["secrets", "versions", PATH]));
    assert!(!listing.contains("v1 "), "v1 should be gone: {listing}");
    assert!(listing.contains("v2"), "v2 should remain: {listing}");
}
