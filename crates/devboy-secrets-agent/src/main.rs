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
use devboy_secrets_agent::rpc::{FramingError, JsonRpcError, PARSE_ERROR};
#[cfg(unix)]
use devboy_secrets_agent::{
    AgentListener, JsonRpcResponse, VaultServer, default_socket_path, install_sigterm_handler,
    read_request, write_response,
};
#[cfg(unix)]
use serde_json::Value;
#[cfg(unix)]
use tokio::io::BufReader;
#[cfg(unix)]
use tokio::sync::{Mutex, Notify};

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

    let vault_path = resolve_vault_path()?;
    let socket_path = default_socket_path()?;
    eprintln!(
        "devboy-secrets-agent: vault={} socket={}",
        vault_path.display(),
        socket_path.display()
    );

    let listener = AgentListener::bind(&socket_path).await?;
    let server = Arc::new(Mutex::new(VaultServer::new(vault_path)));

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = shutdown.clone();
    let server_for_shutdown = server.clone();
    tokio::spawn(async move {
        install_sigterm_handler(move || async move {
            // Drop the cached vault (zeroizes the wrap key inside
            // `secrecy::SecretBox`) by replacing the server with a
            // fresh, locked instance pointing at the same path.
            let mut s = server_for_shutdown.lock().await;
            let path = s.vault_path().to_path_buf();
            *s = VaultServer::new(path);
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
