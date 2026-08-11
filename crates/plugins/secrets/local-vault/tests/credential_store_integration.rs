//! End-to-end proof that the vault bridge closes the ADR-024 §6
//! gap (Ф1a).
//!
//! Everything here runs against a **real daemon on a real socket**
//! with a **real encrypted vault**. The bridge exists to join two
//! stacks that were never connected, and the failure mode it must
//! rule out — a secret sitting in the vault that the credential
//! chain cannot see — is invisible to a mock: a fake socket would
//! answer whatever the test told it to.
//!
//! The daemon runs in-process on a tokio task while the bridge
//! talks to it with blocking I/O from a separate thread, which is
//! also a live check that the sync client does not deadlock against
//! an async server.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use devboy_secret_local_vault::VaultStore;
use devboy_secrets_agent::VaultServer;
use devboy_storage::{ChainStore, CredentialStore};
use devboy_vault_crypto::format::EnvelopeKdfParams;
use devboy_vault_crypto::vault::{EntryMetadata, InitialUnlock, UnlockMethod, Vault};
use secrecy::{ExposeSecret, SecretString};
use serde_json::json;
use tempfile::TempDir;
use tokio::net::UnixListener;

const PASSPHRASE: &str = "correct horse battery staple";

/// Argon2 parameters low enough that a vault create and each
/// `fresh_unlock` cost milliseconds. Security is not what this test
/// is measuring.
fn fast_init() -> InitialUnlock {
    InitialUnlock {
        passphrase: SecretString::from(PASSPHRASE.to_owned()),
        passphrase_params: Some(EnvelopeKdfParams { m: 8, t: 1, p: 1 }),
        with_recovery: false,
        with_totp_secret: None,
    }
}

/// A daemon serving one vault on a socket, plus the temp dir that
/// owns both.
struct Daemon {
    _dir: TempDir,
    socket_path: PathBuf,
    vault_path: PathBuf,
}

impl Daemon {
    /// Create a vault, seed it, and start serving.
    ///
    /// `unlocked` decides whether the daemon holds an open vault,
    /// which is the difference between a readable secret and a
    /// `VAULT_LOCKED` answer — both of which the bridge must handle
    /// distinctly.
    async fn start(seed: &[(&str, &str)], unlocked: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault_path = dir.path().join("vault.dvb");
        let socket_path = dir.path().join("agent.sock");

        Vault::create(&vault_path, fast_init()).expect("create vault");
        {
            let mut vault = Vault::open(
                &vault_path,
                UnlockMethod::Passphrase(SecretString::from(PASSPHRASE.to_owned())),
            )
            .expect("open vault");
            for (path, value) in seed {
                vault
                    .put(
                        path,
                        &SecretString::from((*value).to_owned()),
                        EntryMetadata::default(),
                    )
                    .expect("seed entry");
            }
        }

        let listener = UnixListener::bind(&socket_path).expect("bind socket");
        let mut server = VaultServer::new(vault_path.clone());
        if unlocked {
            let response = server
                .handle_request(
                    serde_json::from_value(json!({
                        "jsonrpc": "2.0",
                        "id": 0,
                        "method": "vault.unlock",
                        "params": {"kind": "passphrase", "secret": PASSPHRASE},
                    }))
                    .expect("unlock request"),
                )
                .await;
            assert!(
                response.error.is_none(),
                "daemon failed to unlock: {response:?}"
            );
        }

        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                // One connection at a time matches the daemon's own
                // short-lived-connection model.
                let _ = server.serve_connection(stream).await;
            }
        });

        Self {
            _dir: dir,
            socket_path,
            vault_path,
        }
    }

    fn store(&self) -> VaultStore {
        VaultStore::with_socket(&self.socket_path)
    }

    fn vault_path(&self) -> &Path {
        &self.vault_path
    }
}

/// Run blocking bridge calls off the runtime thread, as the real
/// sync callers do.
async fn blocking<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.expect("blocking task")
}

/// The gap this whole module exists to close: a secret stored in
/// the vault, read back through the ADR-005 credential interface
/// under its legacy dotted key.
#[tokio::test]
async fn a_vault_secret_resolves_through_the_credential_interface() {
    let daemon = Daemon::start(&[("personal/github/token", "ghp-from-the-vault")], true).await;
    let store = daemon.store();

    let found = blocking(move || store.get("github.token")).await.unwrap();

    let found = found.expect("the vault entry should be reachable as `github.token`");
    assert_eq!(found.expose_secret(), "ghp-from-the-vault");
}

/// A path-shaped key must reach the same entry without being
/// rewritten, so a migrated caller and a legacy one agree.
#[tokio::test]
async fn an_adr_020_path_reaches_the_same_entry() {
    let daemon = Daemon::start(&[("team/gitlab/token", "glpat-team")], true).await;
    let store = daemon.store();

    let found = blocking(move || store.get("team/gitlab/token"))
        .await
        .unwrap();

    assert_eq!(
        found.expect("entry is reachable").expose_secret(),
        "glpat-team"
    );
}

/// An absent entry is not an error — the chain has other stores to
/// ask.
#[tokio::test]
async fn an_unknown_key_falls_through_quietly() {
    let daemon = Daemon::start(&[("personal/github/token", "v")], true).await;
    let store = daemon.store();

    let found = blocking(move || store.get("nothing.here")).await.unwrap();
    assert!(found.is_none());
}

/// The distinction that matters most in practice.
///
/// A locked vault reported as "not found" sends the user down the
/// "export this environment variable" path when the real fix is to
/// unlock. It has to surface as an error naming the unlock.
#[tokio::test]
async fn a_locked_vault_is_an_error_not_an_absent_secret() {
    let daemon = Daemon::start(&[("personal/github/token", "ghp-hidden")], false).await;
    let store = daemon.store();

    let result = blocking(move || store.get("github.token")).await;

    let err = result.expect_err("a locked vault must not look like a missing secret");
    let message = err.to_string();
    assert!(
        message.contains("locked"),
        "the error should say the vault is locked: {message}"
    );
    assert!(
        message.contains("unlock"),
        "the error should name the way out: {message}"
    );
}

/// Reads must not extend the unlock window in a way that keeps the
/// vault open forever, and more basically: repeated reads must keep
/// working over fresh connections.
#[tokio::test]
async fn repeated_reads_work_over_separate_connections() {
    let daemon = Daemon::start(
        &[
            ("personal/github/token", "ghp-1"),
            ("personal/gitlab/token", "glpat-2"),
        ],
        true,
    )
    .await;

    for (key, expected) in [("github.token", "ghp-1"), ("gitlab.token", "glpat-2")] {
        let store = daemon.store();
        let key = key.to_owned();
        let found = blocking(move || store.get(&key)).await.unwrap();
        assert_eq!(found.expect("entry is reachable").expose_secret(), expected);
    }
}

/// A write must not silently vanish, and — the part worth
/// asserting — it must not corrupt the vault on the way out.
#[tokio::test]
async fn a_refused_write_leaves_the_vault_untouched() {
    let daemon = Daemon::start(&[("personal/github/token", "ghp-original")], true).await;
    let vault_path = daemon.vault_path().to_path_buf();
    let store = daemon.store();

    let result = blocking(move || {
        store.store(
            "github.token",
            &SecretString::from("ghp-replacement".to_owned()),
        )
    })
    .await;
    assert!(result.is_err(), "the write must be refused, not swallowed");

    // Re-open from disk: the original value has to be intact.
    let vault = Vault::open(
        &vault_path,
        UnlockMethod::Passphrase(SecretString::from(PASSPHRASE.to_owned())),
    )
    .expect("vault still opens");
    assert_eq!(
        vault
            .get("personal/github/token")
            .unwrap()
            .expect("entry survives")
            .expose_secret(),
        "ghp-original"
    );
}

/// Through the chain, an environment variable still wins over the
/// vault. CI overrides and one-off `FOO_TOKEN=... devboy ...`
/// invocations depend on this precedence.
#[tokio::test]
async fn the_environment_still_takes_precedence_over_the_vault() {
    let daemon = Daemon::start(&[("personal/github/token", "ghp-from-the-vault")], true).await;
    let vault = daemon.store();

    let found = blocking(move || {
        temp_env::with_var("DEVBOY_GITHUB_TOKEN", Some("ghp-from-the-env"), || {
            let chain = ChainStore::new(vec![
                Box::new(devboy_storage::EnvVarStore::new()),
                Box::new(vault),
            ]);
            chain.get("github.token")
        })
    })
    .await
    .unwrap();

    assert_eq!(
        found.expect("resolved").expose_secret(),
        "ghp-from-the-env",
        "an explicit environment variable must override the vault"
    );
}

/// With no variable set, the same chain reaches the vault — the
/// end-to-end shape of the new default.
#[tokio::test]
async fn the_chain_reaches_the_vault_when_the_environment_is_empty() {
    let daemon = Daemon::start(&[("personal/github/token", "ghp-from-the-vault")], true).await;
    let vault = daemon.store();

    let found = blocking(move || {
        temp_env::with_var_unset("DEVBOY_GITHUB_TOKEN", || {
            temp_env::with_var_unset("GITHUB_TOKEN", || {
                let chain = ChainStore::new(vec![
                    Box::new(devboy_storage::EnvVarStore::new()),
                    Box::new(vault.clone()),
                ]);
                chain.get("github.token")
            })
        })
    })
    .await
    .unwrap();

    assert_eq!(
        found.expect("resolved from the vault").expose_secret(),
        "ghp-from-the-vault"
    );
}

/// The chain has no writable member once the keychain is out, and
/// the resulting error is what the user actually sees. It must name
/// a way forward rather than state a fact.
#[tokio::test]
async fn a_chain_with_nowhere_to_write_says_what_to_do_instead() {
    let daemon = Daemon::start(&[], true).await;
    let vault = daemon.store();

    let err = blocking(move || {
        let chain = ChainStore::new(vec![
            Box::new(devboy_storage::EnvVarStore::new()),
            Box::new(vault),
        ]);
        chain.store("github.token", &SecretString::from("v".to_owned()))
    })
    .await
    .expect_err("nothing in this chain accepts writes");

    let message = err.to_string();
    assert!(
        message.contains("github.token"),
        "the error should name the key: {message}"
    );
    assert!(
        message.contains("secrets ui")
            || message.contains("environment variable")
            || message.contains("keychain"),
        "the error should offer at least one way forward: {message}"
    );
}

/// Writability tracks what the daemon can actually do.
///
/// A write needs a fresh passphrase, the daemon collects it on its
/// own channel, and a daemon with no channel cannot. Since the
/// chain routes writes to the first writable store, a store that
/// claimed writability regardless would swallow writes that another
/// store could have handled.
#[tokio::test]
async fn writability_follows_the_daemon_prompt_channel() {
    let daemon = Daemon::start(&[], true).await;
    let store = daemon.store();

    let (writable, channel) = blocking(move || {
        let w = store.is_writable();
        (w, store.socket_path().to_path_buf())
    })
    .await;
    let _ = channel;

    // The test harness has no controlling terminal, so the daemon
    // reports no prompt channel and the store must not claim to
    // take writes.
    assert!(
        !writable,
        "with no prompt channel the store must not volunteer as the write target"
    );
}

/// And a write attempted anyway fails with the daemon's own
/// explanation rather than a vaguer wrapper.
#[tokio::test]
async fn a_write_without_a_prompt_channel_explains_itself() {
    let daemon = Daemon::start(&[], true).await;
    let store = daemon.store();

    let err =
        blocking(move || store.store("github.token", &SecretString::from("ghp-new".to_owned())))
            .await
            .expect_err("no prompt channel under the test harness");

    let message = err.to_string();
    assert!(
        message.contains("github.token"),
        "the error should name the key: {message}"
    );
    assert!(
        message.contains("secrets ui") || message.contains("terminal"),
        "the error should point somewhere: {message}"
    );
}
