//! `devboy-secrets-agent` — long-running daemon that owns the unlocked
//! local vault and exposes it through a JSON-RPC 2.0 wire over a UNIX
//! domain socket.
//!
//! See [ADR-023] §3.3 for the lifecycle, wire protocol, and security
//! boundary.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md
//!
//! Status: under construction. Phase P4.1 ships the socket layer
//! (this commit); P4.2 layers JSON-RPC 2.0 on top; P4.3 adds the
//! idle-timeout + zeroize policy; P4.4 wires CLI on-demand spawn;
//! P4.5 generates launchd / systemd service files.

#![forbid(unsafe_code)]

pub mod audit_writer;
pub mod client;
#[cfg(unix)]
pub mod client_terminal;
pub mod idle;
#[cfg(unix)]
pub mod prompt;
pub mod provenance;
pub mod rpc;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
pub mod socket;
pub mod totp_session;

pub use audit_writer::{AuditWriter, ScrubbedDetail};
pub use client::{AgentClient, ClientError};
#[cfg(unix)]
pub use idle::{
    DEFAULT_IDLE_TIMEOUT, IdleClock, IdleTracker, ManualClock, SIGTERM_GRACE, SystemClock,
    install_sigterm_handler,
};
#[cfg(unix)]
pub use prompt::{TtyPrompt, echo_enabled};
pub use rpc::{
    BAD_UNLOCK, ENTRY_NOT_FOUND, FramingError, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST,
    IO_ERROR, JSONRPC_VERSION, JsonRpcError, JsonRpcRequest, JsonRpcResponse, METHOD_NOT_FOUND,
    NO_MATCHING_ENVELOPE, PARSE_ERROR, VAULT_LOCKED, read_request, read_response, write_request,
    write_response,
};
#[cfg(unix)]
pub use server::VaultServer;
#[cfg(unix)]
pub use socket::{
    AgentError, AgentListener, SECRETS_SUBDIR, SOCKET_FILENAME, SOCKET_MODE,
    SOCKET_PARENT_DIR_MODE, SOCKET_PATH_ENV, default_socket_path,
};
pub use totp_session::{TOTP_SECRET_PATH, TotpDenial, TotpSession, is_reserved};

// Cross-platform stubs for the items downstream crates
// (`devboy-secret-local-vault`) need to import unconditionally.
// On Windows the daemon protocol is unavailable, so the stubs
// surface that as `AgentError::Unsupported` / `default_socket_path`
// returning the same error.
#[cfg(not(unix))]
mod windows_stub {
    use std::path::PathBuf;

    use thiserror::Error;

    #[derive(Debug, Error)]
    pub enum AgentError {
        #[error("agent socket I/O error: {0}")]
        Io(#[from] std::io::Error),
        #[error(
            "the agent daemon is unavailable on this platform — \
             UNIX domain sockets are required"
        )]
        Unsupported,
    }

    pub fn default_socket_path() -> Result<PathBuf, AgentError> {
        Err(AgentError::Unsupported)
    }
}

#[cfg(not(unix))]
pub use windows_stub::{AgentError, default_socket_path};
