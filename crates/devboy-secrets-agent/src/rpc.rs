//! JSON-RPC 2.0 wire protocol for the agent daemon.
//!
//! Request/response framing is **newline-delimited JSON** (one JSON
//! object per line, terminated by `\n`). Same shape MCP uses, so the
//! existing tooling (logging, tracing, fault injection in tests)
//! applies unchanged.
//!
//! # Error code allocation
//!
//! Standard JSON-RPC 2.0 codes (-32700 … -32603) are reserved by the
//! spec; we use them verbatim for transport / parse / dispatch
//! errors. Application errors live in the implementation-defined
//! range -32099 … -32000 and are namespaced via the constants
//! [`VAULT_LOCKED`], [`ENTRY_NOT_FOUND`], [`BAD_UNLOCK`], etc.
//!
//! See [ADR-023] §3.3 for the wire-protocol decision.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// JSON-RPC 2.0 protocol version string.
pub const JSONRPC_VERSION: &str = "2.0";

// =============================================================================
// JSON-RPC 2.0 standard error codes
// =============================================================================

/// JSON received was malformed.
pub const PARSE_ERROR: i32 = -32700;
/// JSON received was not a valid Request.
pub const INVALID_REQUEST: i32 = -32600;
/// Method does not exist or is not available.
pub const METHOD_NOT_FOUND: i32 = -32601;
/// Invalid method parameters.
pub const INVALID_PARAMS: i32 = -32602;
/// Internal JSON-RPC error.
pub const INTERNAL_ERROR: i32 = -32603;

// =============================================================================
// Application error codes (-32099 … -32000)
// =============================================================================

/// Daemon vault is currently locked; caller must call `vault.unlock`
/// first.
pub const VAULT_LOCKED: i32 = -32000;
/// Requested entry does not exist in the vault.
pub const ENTRY_NOT_FOUND: i32 = -32001;
/// `fresh_unlock` parameter on a write operation did not validate.
pub const BAD_UNLOCK: i32 = -32002;
/// `vault.unlock` could not find an envelope of the requested kind.
pub const NO_MATCHING_ENVELOPE: i32 = -32003;
/// Underlying I/O error talking to the vault file.
pub const IO_ERROR: i32 = -32004;
/// ADR-024 §7 check A: the caller is an ancestor of this daemon
/// and could `ptrace` it, so the connection is refused.
///
/// Sent *before* the socket closes so the caller receives an
/// instruction rather than a dropped connection.
pub const DAEMON_UNTRUSTED: i32 = -32005;

/// ADR-024 §7: the daemon was asked to collect a passphrase itself
/// but has nowhere to ask.
///
/// Distinct from a failed unlock because the fix is completely
/// different — nothing about the passphrase is wrong, the daemon
/// simply has no channel to the human. A properly daemonised
/// process has no controlling terminal by construction, so this is
/// the *expected* answer until a prompt channel that does not
/// depend on one is chosen.
pub const NO_PROMPT_SURFACE: i32 = -32006;

// =============================================================================
// Wire types
// =============================================================================

/// Inbound JSON-RPC 2.0 request.
///
/// `id` is `serde_json::Value` so the daemon can echo back whatever
/// shape the client picked (per the spec it can be a number, a string,
/// or `null`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct JsonRpcRequest {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Caller-supplied id; echoed in the response.
    pub id: serde_json::Value,
    /// Dotted method name, e.g. `"vault.unlock"`.
    pub method: String,
    /// Positional or named parameters; defaults to `null` if absent.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Outbound JSON-RPC 2.0 response. Either `result` or `error` is set,
/// never both — enforced by the [`JsonRpcResponse::ok`] /
/// [`JsonRpcResponse::err`] constructors.
///
/// `Deserialize` is implemented so client-side code (and the
/// in-memory tests) can parse responses off the wire; the daemon
/// itself only ever serializes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcResponse {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Echoed from the request.
    pub id: serde_json::Value,
    /// Method-specific result payload. Mutually exclusive with
    /// `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error envelope. Mutually exclusive with `result`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Build a success response.
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Build an error response.
    pub fn err(id: serde_json::Value, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// JSON-RPC 2.0 error envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcError {
    /// Error code — one of the constants in this module or in the
    /// application range.
    pub code: i32,
    /// Short human-readable message.
    pub message: String,
    /// Optional structured payload for richer reporting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcError {
    /// Convenience constructor without a `data` payload.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Convenience constructor with a `data` payload.
    pub fn with_data(code: i32, message: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }
}

// =============================================================================
// Framing — newline-delimited JSON
// =============================================================================

/// Failure modes for [`read_request`] / [`write_response`].
#[derive(Debug, Error)]
pub enum FramingError {
    /// Underlying I/O error (EOF, broken pipe, ...).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The bytes read from the wire were not valid JSON (or did not
    /// match the [`JsonRpcRequest`] shape). Surfaced separately from
    /// `Io` so the server can return `PARSE_ERROR` / `INVALID_REQUEST`
    /// to the client without tearing down the connection.
    #[error("malformed request: {0}")]
    Parse(#[from] serde_json::Error),

    /// EOF reached cleanly — peer closed the connection between
    /// requests. Not really an error; the server's read loop should
    /// observe this and exit gracefully.
    #[error("peer closed the connection")]
    Eof,
}

/// Read one newline-delimited JSON-RPC request from `reader`.
///
/// Returns [`FramingError::Eof`] when the peer closed the connection
/// cleanly (zero-byte read).
pub async fn read_request<R>(reader: &mut BufReader<R>) -> Result<JsonRpcRequest, FramingError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(FramingError::Eof);
    }
    let req: JsonRpcRequest = serde_json::from_str(line.trim_end_matches('\n'))?;
    Ok(req)
}

/// Write one JSON-RPC response to `writer`, terminated by `\n`.
pub async fn write_response<W>(
    writer: &mut W,
    response: &JsonRpcResponse,
) -> Result<(), FramingError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Write one JSON-RPC request to `writer`, terminated by `\n`.
///
/// Symmetric to [`write_response`]; used by client-side code (the
/// CLI's local-vault source plugin, future MCP proxy) that talks to
/// the agent over the same newline-delimited framing.
pub async fn write_request<W>(writer: &mut W, request: &JsonRpcRequest) -> Result<(), FramingError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one newline-delimited JSON-RPC response from `reader`.
///
/// Symmetric to [`read_request`]. Returns [`FramingError::Eof`] when
/// the peer closed the connection cleanly before sending a response
/// (treated as a protocol error by clients — the daemon should
/// always respond, even on error).
pub async fn read_response<R>(reader: &mut BufReader<R>) -> Result<JsonRpcResponse, FramingError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(FramingError::Eof);
    }
    let resp: JsonRpcResponse = serde_json::from_str(line.trim_end_matches('\n'))?;
    Ok(resp)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, BufReader};

    fn id() -> serde_json::Value {
        serde_json::json!(7)
    }

    // -- Wire types --------------------------------------------------------

    #[test]
    fn response_ok_sets_result_only() {
        let r = JsonRpcResponse::ok(id(), serde_json::json!({"x": 1}));
        assert_eq!(r.jsonrpc, "2.0");
        assert!(r.result.is_some());
        assert!(r.error.is_none());
    }

    #[test]
    fn response_err_sets_error_only() {
        let r = JsonRpcResponse::err(id(), JsonRpcError::new(VAULT_LOCKED, "locked"));
        assert!(r.result.is_none());
        let e = r.error.unwrap();
        assert_eq!(e.code, VAULT_LOCKED);
        assert_eq!(e.message, "locked");
        assert!(e.data.is_none());
    }

    #[test]
    fn response_with_data_carries_payload() {
        let e = JsonRpcError::with_data(
            BAD_UNLOCK,
            "wrong passphrase",
            serde_json::json!({"hint": "try the recovery phrase"}),
        );
        assert_eq!(e.code, BAD_UNLOCK);
        assert_eq!(e.data.unwrap()["hint"], "try the recovery phrase");
    }

    // -- Round-trip serialization ------------------------------------------

    #[test]
    fn request_round_trips_through_serde() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: serde_json::json!(42),
            method: "vault.unlock".to_owned(),
            params: serde_json::json!({"kind": "passphrase", "secret": "p"}),
        };
        let bytes = serde_json::to_vec(&req).unwrap();
        let back: JsonRpcRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn response_skips_none_fields_in_json() {
        let ok = JsonRpcResponse::ok(id(), serde_json::json!(null));
        let s = serde_json::to_string(&ok).unwrap();
        assert!(
            !s.contains("\"error\""),
            "ok response must not emit error field"
        );

        let err = JsonRpcResponse::err(id(), JsonRpcError::new(INTERNAL_ERROR, "x"));
        let s = serde_json::to_string(&err).unwrap();
        assert!(
            !s.contains("\"result\""),
            "err response must not emit result field"
        );
    }

    #[test]
    fn request_with_default_params_deserializes() {
        // A request that omits `params` entirely should still parse.
        let s = r#"{"jsonrpc":"2.0","id":1,"method":"vault.lock"}"#;
        let req: JsonRpcRequest = serde_json::from_str(s).unwrap();
        assert_eq!(req.method, "vault.lock");
        assert!(req.params.is_null());
    }

    // -- Framing -----------------------------------------------------------

    #[tokio::test]
    async fn read_then_write_round_trip_in_memory() {
        // Build a duplex pipe; spawn a "client" that writes one
        // request and reads one response; "server" reads, builds
        // a response, writes back.
        let (client, server) = tokio::io::duplex(1024);
        let (server_r, mut server_w) = tokio::io::split(server);
        let (client_r, mut client_w) = tokio::io::split(client);

        let server_task = tokio::spawn(async move {
            let mut reader = BufReader::new(server_r);
            let req = read_request(&mut reader).await.unwrap();
            assert_eq!(req.method, "vault.status");
            let resp = JsonRpcResponse::ok(req.id, serde_json::json!({"state": "locked"}));
            write_response(&mut server_w, &resp).await.unwrap();
        });

        // Client writes request.
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: serde_json::json!(1),
            method: "vault.status".to_owned(),
            params: serde_json::json!(null),
        };
        let mut bytes = serde_json::to_vec(&req).unwrap();
        bytes.push(b'\n');
        client_w.write_all(&bytes).await.unwrap();

        // Client reads response.
        let mut reader = BufReader::new(client_r);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let resp: JsonRpcResponse = serde_json::from_str(line.trim_end_matches('\n')).unwrap();
        assert_eq!(resp.id, serde_json::json!(1));
        assert_eq!(resp.result.unwrap()["state"], "locked");

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn read_request_returns_eof_on_clean_close() {
        let (client, server) = tokio::io::duplex(64);
        // Drop the client end immediately so the server side sees EOF
        // with zero bytes read.
        drop(client);
        let (server_r, _server_w) = tokio::io::split(server);
        let mut reader = BufReader::new(server_r);
        let err = read_request(&mut reader).await.unwrap_err();
        assert!(matches!(err, FramingError::Eof));
    }

    #[tokio::test]
    async fn read_request_returns_parse_error_on_garbage() {
        let (mut client, server) = tokio::io::duplex(64);
        client.write_all(b"not json at all\n").await.unwrap();
        client.shutdown().await.unwrap();
        let (server_r, _server_w) = tokio::io::split(server);
        let mut reader = BufReader::new(server_r);
        let err = read_request(&mut reader).await.unwrap_err();
        assert!(matches!(err, FramingError::Parse(_)));
    }

    #[tokio::test]
    async fn write_response_terminates_with_newline() {
        // Don't split `server` here — keeping the read half alive
        // (even unused) prevents the EOF signal from reaching
        // `client.read_to_end`, because tokio's split halves share
        // ownership of the underlying duplex and EOF only propagates
        // when both halves are dropped. Drop the whole `server`
        // after writing instead.
        let (mut client, mut server) = tokio::io::duplex(64);
        let resp = JsonRpcResponse::ok(serde_json::json!(1), serde_json::json!("ok"));
        write_response(&mut server, &resp).await.unwrap();
        drop(server);
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut client, &mut buf)
            .await
            .unwrap();
        assert!(buf.ends_with(b"\n"));
        let parsed: JsonRpcResponse = serde_json::from_slice(&buf[..buf.len() - 1]).unwrap();
        assert_eq!(parsed, resp);
    }

    // -- Error code constants ----------------------------------------------

    #[test]
    fn standard_json_rpc_codes_match_spec() {
        assert_eq!(PARSE_ERROR, -32700);
        assert_eq!(INVALID_REQUEST, -32600);
        assert_eq!(METHOD_NOT_FOUND, -32601);
        assert_eq!(INVALID_PARAMS, -32602);
        assert_eq!(INTERNAL_ERROR, -32603);
    }

    #[test]
    fn application_codes_are_in_implementation_range() {
        // -32099 … -32000 is the JSON-RPC 2.0 implementation-defined
        // range. Every application-specific code in this module
        // must sit inside it.
        for code in [
            VAULT_LOCKED,
            ENTRY_NOT_FOUND,
            BAD_UNLOCK,
            NO_MATCHING_ENVELOPE,
            IO_ERROR,
        ] {
            assert!(
                (-32099..=-32000).contains(&code),
                "code {code} is outside the implementation-defined range"
            );
        }
    }
}
