//! Synchronous client for the daemon's JSON-RPC protocol.
//!
//! # Why it lives here
//!
//! Two crates need to talk to this daemon from synchronous code —
//! the credential-chain bridge in `devboy-secret-local-vault` and
//! the MCP server's unlock tools. A protocol with two client
//! implementations grows two sets of bugs, so the crate that
//! defines the protocol ships the client for it.
//!
//! # Why synchronous
//!
//! Both callers are reached from inside a running tokio runtime,
//! where blocking on a nested runtime panics. The wire format is
//! line-delimited JSON over a UNIX socket, so a blocking client is
//! short and has nothing to conflict with.
//!
//! # Why it compiles on Windows at all
//!
//! The daemon protocol is UNIX-socket-only by design, but consumers
//! import this type unconditionally. Rather than push `#[cfg]` onto
//! every call site, the type exists everywhere and every call
//! short-circuits to [`ClientError::Protocol`] off UNIX — the same
//! shape `AgentError` already uses in `lib.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};

use serde_json::{Value, json};

use crate::rpc::JsonRpcError;

/// How long to wait on the daemon before giving up.
///
/// Short on purpose: a wedged daemon must not stall a caller that
/// has something else it could do.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a call did not produce a result.
///
/// The three cases are kept apart because callers treat them
/// differently: an unreachable daemon usually means "carry on
/// without it", while a daemon error is an answer that deserves
/// forwarding.
#[derive(Debug)]
pub enum ClientError {
    /// Could not reach the daemon at all.
    Unreachable(std::io::Error),
    /// Reached it, but the exchange did not make sense.
    Protocol(String),
    /// The daemon answered with a JSON-RPC error.
    Daemon(JsonRpcError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "daemon unreachable: {e}"),
            Self::Protocol(m) => write!(f, "protocol error: {m}"),
            Self::Daemon(e) => write!(f, "daemon error {}: {}", e.code, e.message),
        }
    }
}

impl std::error::Error for ClientError {}

impl ClientError {
    /// The daemon's error code, when the daemon answered.
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Daemon(e) => Some(e.code),
            _ => None,
        }
    }
}

/// A blocking client bound to one socket path.
#[derive(Debug, Clone)]
pub struct AgentClient {
    socket_path: PathBuf,
}

impl AgentClient {
    /// Build a client against the canonical socket, or `None` when
    /// no path can be derived.
    ///
    /// Off UNIX this is always `None`: there is no socket path to
    /// derive, which is the honest answer rather than handing back
    /// a client that can only fail.
    pub fn new() -> Option<Self> {
        crate::default_socket_path()
            .ok()
            .map(|socket_path| Self { socket_path })
    }

    /// Build a client against an explicit socket path.
    pub fn with_socket(path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: path.into(),
        }
    }

    /// The socket this client talks to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Whether the daemon socket exists.
    ///
    /// Worth checking first: it costs a `stat`, where connecting to
    /// an absent socket costs a timeout on every call.
    pub fn is_running(&self) -> bool {
        self.socket_path.exists()
    }

    /// Send one request and read one response.
    #[cfg(unix)]
    pub fn call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        use std::os::unix::net::UnixStream;

        let stream = UnixStream::connect(&self.socket_path).map_err(ClientError::Unreachable)?;
        stream.set_read_timeout(Some(RPC_TIMEOUT)).ok();
        stream.set_write_timeout(Some(RPC_TIMEOUT)).ok();

        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let mut writer = stream.try_clone().map_err(ClientError::Unreachable)?;
        writeln!(writer, "{request}").map_err(ClientError::Unreachable)?;
        writer.flush().map_err(ClientError::Unreachable)?;
        // Half-close so the daemon's read loop sees EOF and answers
        // rather than blocking for more bytes.
        drop(writer);

        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(ClientError::Unreachable)?;

        let response: Value = serde_json::from_str(&line)
            .map_err(|e| ClientError::Protocol(format!("malformed reply: {e}")))?;

        if let Some(error) = response.get("error")
            && !error.is_null()
        {
            return Err(ClientError::Daemon(JsonRpcError {
                code: error.get("code").and_then(Value::as_i64).unwrap_or(0) as i32,
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown daemon error")
                    .to_owned(),
                data: error.get("data").cloned(),
            }));
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Send one request and read one response.
    ///
    /// The daemon speaks only over UNIX domain sockets, so off UNIX
    /// there is nothing to reach.
    #[cfg(not(unix))]
    pub fn call(&self, _method: &str, _params: Value) -> Result<Value, ClientError> {
        Err(ClientError::Protocol(
            "the secret daemon is only reachable over UNIX domain sockets".to_owned(),
        ))
    }

    /// `vault.status`.
    pub fn status(&self) -> Result<Value, ClientError> {
        self.call("vault.status", Value::Null)
    }

    /// `secret.get` for one path.
    pub fn secret_get(&self, path: &str) -> Result<Value, ClientError> {
        self.call("secret.get", json!({ "path": path }))
    }

    /// `totp.unlock` with a six-digit code.
    pub fn totp_unlock(
        &self,
        code: &str,
        duration_seconds: Option<u64>,
    ) -> Result<Value, ClientError> {
        self.call(
            "totp.unlock",
            json!({ "code": code, "duration_seconds": duration_seconds }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_socket_reports_not_running_without_connecting() {
        let client = AgentClient::with_socket("/nonexistent/devboy-client-test.sock");
        assert!(!client.is_running());
    }

    /// An unreachable daemon must be distinguishable from one that
    /// answered with an error: the first means "carry on without
    /// it", the second is an answer worth forwarding.
    #[test]
    fn an_unreachable_daemon_is_its_own_error() {
        let client = AgentClient::with_socket("/nonexistent/devboy-client-test.sock");
        let err = client.status().expect_err("no daemon there");

        assert!(matches!(err, ClientError::Unreachable(_)));
        assert!(
            err.code().is_none(),
            "an unreachable daemon has no error code to report"
        );
        assert!(err.to_string().contains("unreachable"));
    }
}
