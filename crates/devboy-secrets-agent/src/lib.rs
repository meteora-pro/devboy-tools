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

pub mod socket;

pub use socket::{
    AgentError, AgentListener, SECRETS_SUBDIR, SOCKET_FILENAME, SOCKET_MODE,
    SOCKET_PARENT_DIR_MODE, SOCKET_PATH_ENV, default_socket_path,
};
