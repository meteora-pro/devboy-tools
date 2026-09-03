//! Entry point for the `devboy-secrets-agent` daemon binary.
//!
//! Lifecycle (per [ADR-023] §3.3):
//!
//! 1. Detach from any controlling terminal so a `Ctrl-C` to the
//!    spawning shell does not propagate here. We do this on the
//!    *agent* side (via `nix::unistd::setsid()`) rather than on the
//!    CLI side because the CLI crate has `unsafe_code = "forbid"`
//!    and `Command::pre_exec` requires `unsafe`.
//! 2. Resolve the vault path (`DEVBOY_VAULT_PATH` env, or
//!    `<config_dir>/devboy-tools/secrets/vault.dvb`).
//! 3. Bind the agent socket via [`AgentListener::bind`] — its
//!    permissions and peer-credential check are P4.1 territory.
//! 4. Build a single shared [`VaultServer`] (one daemon = one
//!    unlocked vault state) and accept connections; each connection
//!    is dispatched in its own task with the shared server held
//!    behind a `tokio::sync::Mutex`. Per-request locking keeps
//!    multiple clients from blocking each other for the full
//!    connection lifetime.
//! 5. Install the SIGTERM handler from
//!    [`install_sigterm_handler`]; on signal it drops the cached
//!    `Vault` (zeroizing the wrap key inside `secrecy::SecretBox`)
//!    and signals the accept loop to stop.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

/// Everything in this file below the non-UNIX `main` is part of
/// the daemon proper and needs `#[cfg(unix)]`. The daemon speaks
/// over a UNIX domain socket and identifies its callers through
/// `SO_PEERCRED`, neither of which exists elsewhere. An item that
/// forgets the attribute does not fail here — it fails on a
/// Windows runner, which is why `cargo check --target
/// x86_64-pc-windows-gnu` is worth running before pushing.
#[cfg(not(unix))]
fn main() {
    eprintln!(
        "devboy-secrets-agent runs on UNIX-like targets only (the daemon \
         protocol uses UNIX domain sockets). On Windows, use the OS \
         credential manager via the keychain source instead."
    );
    std::process::exit(1);
}

#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use devboy_secrets_agent::rpc::{DAEMON_UNTRUSTED, FramingError, JsonRpcError, PARSE_ERROR};
#[cfg(unix)]
use devboy_secrets_agent::{
    AgentListener, JsonRpcResponse, VaultServer, default_socket_path, idle::UnlockWindow,
    install_sigterm_handler, read_request, write_response,
};
#[cfg(unix)]
use serde_json::Value;
#[cfg(unix)]
use tokio::io::BufReader;
#[cfg(unix)]
use tokio::sync::{Mutex, Notify};

#[cfg(unix)]
#[cfg(unix)]
const VAULT_PATH_ENV: &str = "DEVBOY_VAULT_PATH";

#[cfg(unix)]
fn resolve_vault_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(s) = std::env::var(VAULT_PATH_ENV)
        && !s.is_empty()
    {
        return Ok(PathBuf::from(s));
    }
    let dir = dirs::config_dir().ok_or("could not resolve the user's config_dir")?;
    Ok(dir.join("devboy-tools").join("secrets").join("vault.dvb"))
}

/// Detach from the controlling terminal. Best-effort: failures other
/// than `EPERM` (already a session leader, e.g. launchd/systemd) are
/// logged but non-fatal — the daemon is useful even if it stays
/// attached.
#[cfg(unix)]
fn detach_from_controlling_terminal() {
    match nix::unistd::setsid() {
        Ok(_) => {}
        Err(nix::errno::Errno::EPERM) => {
            // Already a session leader — fine, that's the goal.
        }
        Err(e) => {
            eprintln!("devboy-secrets-agent: setsid warning: {e}");
        }
    }
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    detach_from_controlling_terminal();

    // ADR-024 §7 checks B and C, before anything else happens.
    //
    // Fail closed: a degraded-but-running daemon preserves the
    // appearance of a guarantee that no longer holds, and a user
    // who does not read status output never learns the difference.
    let provenance = devboy_secrets_agent::provenance::startup_provenance();
    if provenance.is_fatal() {
        eprintln!(
            "devboy-secrets-agent: refusing to start.\n\n{}",
            provenance.refusal_message()
        );
        std::process::exit(1);
    }
    // Warn on *every* launch rather than once — a single line
    // scrolls away, and the override must never become invisible.
    for warning in provenance.warnings() {
        eprintln!("devboy-secrets-agent: WARNING: {warning}");
    }
    let trust_level = provenance.trust_level();
    eprintln!(
        "devboy-secrets-agent: trust_level={trust_level} totp_available={}",
        trust_level.allows_totp()
    );

    let vault_path = resolve_vault_path()?;
    let socket_path = default_socket_path()?;
    eprintln!(
        "devboy-secrets-agent: vault={} socket={}",
        vault_path.display(),
        socket_path.display()
    );

    // Read the user's unlock policy. Until this existed the daemon
    // ran on the default window regardless of what was configured,
    // so `secrets.profile = strict` silently changed nothing.
    //
    // A missing or unreadable config is not fatal — the defaults are
    // the `convenient` profile, which is what a user with no config
    // expects anyway.
    let config = devboy_core::config::Config::load().unwrap_or_default();
    let window = UnlockWindow::from_config(&config);
    eprintln!(
        "devboy-secrets-agent: unlock window {}s (ceiling {}s, idle re-lock {})",
        window.unlock_ttl.as_secs(),
        window.max_unlock_ttl.as_secs(),
        window
            .idle_relock
            .map(|d| format!("{}s", d.as_secs()))
            .unwrap_or_else(|| "off".to_owned()),
    );

    let listener = AgentListener::bind(&socket_path).await?;
    let server = Arc::new(Mutex::new(VaultServer::with_window(vault_path, window)));

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    let server_for_shutdown = server.clone();
    let window_for_shutdown = window;
    tokio::spawn(async move {
        install_sigterm_handler(move || async move {
            // Drop the cached vault (zeroizes the wrap key inside
            // `secrecy::SecretBox`) by replacing the server with a
            // fresh, locked instance pointing at the same path.
            let mut s = server_for_shutdown.lock().await;
            let path = s.vault_path().to_path_buf();
            // Rebuild with the same window: a re-lock must not
            // quietly widen the user's policy back to the default.
            *s = VaultServer::with_window(path, window_for_shutdown);
            shutdown_signal.notify_waiters();
        })
        .await;
    });

    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                eprintln!("devboy-secrets-agent: shutdown signal received, exiting accept loop");
                break;
            }
            res = listener.accept_authenticated() => {
                let stream = match res {
                    Ok(s) => s,
                    // Check A tripped. The stream comes back still
                    // open specifically so the client learns *why*
                    // rather than seeing a dropped socket and
                    // improvising.
                    Err(devboy_secrets_agent::socket::AgentError::CallerIsAncestor {
                        peer_pid,
                        stream,
                    }) => {
                        eprintln!(
                            "devboy-secrets-agent: refusing connection from pid {peer_pid}: it is \
                             an ancestor of this daemon and could read its memory"
                        );
                        tokio::spawn(async move {
                            let _ = refuse_untrusted_caller(stream).await;
                        });
                        continue;
                    }
                    Err(e) => {
                        eprintln!("devboy-secrets-agent: accept failed: {e}");
                        continue;
                    }
                };
                let server = server.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_one_connection(server, stream).await {
                        eprintln!("devboy-secrets-agent: connection ended with error: {e}");
                    }
                });
            }
        }
    }

    Ok(())
}

#[cfg(unix)]
/// Tell a refused caller *why* before closing (ADR-024 §7/§8).
///
/// The connection is doomed either way, but a client that receives
/// `DaemonUntrusted` with its remediation knows to stop, relay a
/// specific command to the user, and wait — where a client handed
/// a dropped socket learns only that something broke and will
/// retry or improvise. A hard failure should still arrive as an
/// instruction.
async fn refuse_untrusted_caller(stream: tokio::net::UnixStream) -> Result<(), FramingError> {
    let (_read, mut write) = tokio::io::split(stream);

    let error = JsonRpcError::new(
        DAEMON_UNTRUSTED,
        format!(
            "The secret daemon was started by this session and cannot protect its own memory \
             from it. Stop it and start it with `{}`, then retry.",
            devboy_secrets_agent::provenance::platform_start_command()
        ),
    );

    let resp = JsonRpcResponse::err(Value::Null, error);
    write_response(&mut write, &resp).await
}

#[cfg(unix)]
async fn handle_one_connection(
    server: Arc<Mutex<VaultServer>>,
    stream: tokio::net::UnixStream,
) -> Result<(), FramingError> {
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    loop {
        let req = match read_request(&mut reader).await {
            Ok(r) => r,
            Err(FramingError::Eof) => return Ok(()),
            Err(FramingError::Parse(e)) => {
                let resp = JsonRpcResponse::err(
                    Value::Null,
                    JsonRpcError::new(PARSE_ERROR, format!("malformed JSON: {e}")),
                );
                write_response(&mut write, &resp).await?;
                continue;
            }
            Err(other) => return Err(other),
        };
        let resp = {
            let mut s = server.lock().await;
            s.handle_request(req).await
        };
        write_response(&mut write, &resp).await?;
    }
}
