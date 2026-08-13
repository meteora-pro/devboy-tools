//! A proxied upstream must not be able to put a secret into the
//! agent's session transcript (ADR-024 §4, Ф15).
//!
//! # Why this is an integration test and not a unit test
//!
//! `response_scrub`'s unit tests prove the redaction works when it is
//! called. They prove nothing about whether the proxy calls it, and
//! that gap is the shape of nearly every defect found in this epic:
//! a correct mechanism with the wire never connected.
//!
//! So these tests drive a real [`McpProxyClient`] against a real HTTP
//! server that echoes a credential back, and assert on the value
//! `call_tool` actually returns — the exact bytes an agent would
//! write to its transcript.

use devboy_mcp::protocol::{ToolCallResult, ToolResultContent};
use devboy_mcp::proxy::{McpProxyClient, ProxyTransport};
use httpmock::prelude::*;
use secrecy::SecretString;
use serde_json::json;

/// A GitLab-shaped token, so both redaction passes have something to
/// find: the known-value pass because the proxy connects with it, and
/// the shape pass because it matches a catalogue pattern.
const TOKEN: &str = "glpat-ABCDEFGHIJKLMNOPQRSTU";

/// A credential the proxy never sends — only the shape pass can catch
/// this one.
const FOREIGN_TOKEN: &str = "AKIAIOSFODNN7EXAMPLE";

fn stub_handshake(server: &MockServer) {
    server.mock(|when, then| {
        when.method(POST)
            .path("/mcp")
            .body_includes(r#""method":"initialize""#);
        then.status(200)
            .header("mcp-session-id", "leak-test")
            .json_body(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "mock-upstream", "version": "1.0"}
                }
            }));
    });
}

/// Make `tools/call` answer with `body`.
fn stub_tool_call(server: &MockServer, body: serde_json::Value) {
    server.mock(|when, then| {
        when.method(POST)
            .path("/mcp")
            .body_includes(r#""method":"tools/call""#);
        then.status(200).json_body(body);
    });
}

async fn connect(url: &str, token: Option<&str>) -> McpProxyClient {
    let secret = token.map(|t| SecretString::from(t.to_owned()));
    McpProxyClient::connect(
        "cloud",
        url,
        Some("cloud"),
        secret.as_ref(),
        "bearer",
        ProxyTransport::StreamableHttp,
        None,
    )
    .await
    .expect("connect")
}

fn text_of(result: &ToolCallResult) -> String {
    result
        .content
        .iter()
        .map(|c| match c {
            ToolResultContent::Text { text } => text.clone(),
        })
        .collect()
}

/// The scenario named at the OSS sync as the main practical threat:
/// a 401 body quoting the token, written straight into the agent's
/// JSONL where any process running as the user can read it.
#[tokio::test]
async fn an_upstream_quoting_our_token_cannot_reach_the_agent() {
    let upstream = MockServer::start();
    stub_handshake(&upstream);
    stub_tool_call(
        &upstream,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "content": [{
                    "type": "text",
                    "text": format!("401 Unauthorized: token {TOKEN} is expired")
                }]
            }
        }),
    );

    let url = format!("{}/mcp", upstream.base_url());
    let client = connect(&url, Some(TOKEN)).await;

    let result = client.call_tool("get_issues", None).await.expect("call");
    let text = text_of(&result);

    assert!(
        !text.contains(TOKEN),
        "the token reached the agent verbatim: {text}"
    );
    assert!(
        text.contains("@secret:proxy/cloud/token"),
        "the redaction should name which credential leaked: {text}"
    );
    assert!(
        text.contains("401 Unauthorized"),
        "the diagnostic itself must survive, or the agent cannot act on it: {text}"
    );
}

/// The same leak arriving through the JSON-RPC error channel rather
/// than a result body. Upstreams use both, and an auth failure
/// arguably favours this one.
#[tokio::test]
async fn a_jsonrpc_error_quoting_our_token_is_scrubbed_too() {
    let upstream = MockServer::start();
    stub_handshake(&upstream);
    stub_tool_call(
        &upstream,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32000,
                "message": format!("auth rejected for {TOKEN}")
            }
        }),
    );

    let url = format!("{}/mcp", upstream.base_url());
    let client = connect(&url, Some(TOKEN)).await;

    let result = client.call_tool("get_issues", None).await.expect("call");
    let text = text_of(&result);

    assert!(!text.contains(TOKEN), "{text}");
    assert_eq!(
        result.is_error,
        Some(true),
        "an upstream error must still read as an error after scrubbing"
    );
}

/// An upstream leaking a credential of its own — one devboy has never
/// held and cannot recognise by value. Only the catalogue stops this,
/// and until the catalogue could scan embedded text it did not.
#[tokio::test]
async fn a_credential_devboy_never_sent_is_redacted_by_shape() {
    let upstream = MockServer::start();
    stub_handshake(&upstream);
    stub_tool_call(
        &upstream,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "content": [{
                    "type": "text",
                    "text": format!("env dump: AWS_ACCESS_KEY_ID={FOREIGN_TOKEN} AWS_REGION=eu-central-1")
                }]
            }
        }),
    );

    let url = format!("{}/mcp", upstream.base_url());
    let client = connect(&url, None).await;

    let result = client.call_tool("dump_env", None).await.expect("call");
    let text = text_of(&result);

    assert!(!text.contains(FOREIGN_TOKEN), "{text}");
    assert!(text.contains("[REDACTED:aws-access-key]"), "{text}");
    assert!(
        text.contains("eu-central-1"),
        "everything else must come through: {text}"
    );
}

/// The route that bypasses result-scrubbing entirely: a non-2xx
/// response never becomes a `ToolCallResult` inside the client at
/// all. It becomes a transport error carrying the body verbatim,
/// which the proxy manager then formats into `Proxy error: HTTP 401:
/// <body>` and hands to the agent.
///
/// A 401 body naming the token it rejected is the single likeliest
/// place for a credential to appear, so the error path matters at
/// least as much as the success path.
#[tokio::test]
async fn a_transport_error_carrying_the_body_is_scrubbed() {
    let upstream = MockServer::start();
    stub_handshake(&upstream);
    upstream.mock(|when, then| {
        when.method(POST)
            .path("/mcp")
            .body_includes(r#""method":"tools/call""#);
        then.status(403)
            .body(format!("{{\"error\":\"token {TOKEN} lacks scope\"}}"));
    });

    let url = format!("{}/mcp", upstream.base_url());
    let client = connect(&url, Some(TOKEN)).await;

    let err = client
        .call_tool("get_issues", None)
        .await
        .expect_err("a 403 must surface as an error");
    let rendered = err.to_string();

    assert!(
        !rendered.contains(TOKEN),
        "the token reached the agent through the error path: {rendered}"
    );
    assert!(
        rendered.contains("@secret:proxy/cloud/token"),
        "the redaction should name which credential leaked: {rendered}"
    );
    assert!(
        rendered.contains("403"),
        "the status must survive or the agent cannot tell what failed: {rendered}"
    );
}

/// The case that decides whether this feature survives contact with
/// users. If ordinary responses came back altered, the redaction
/// would be switched off and would then protect nothing.
#[tokio::test]
async fn an_ordinary_response_is_forwarded_unchanged() {
    let original = "Issue DEV-1234 moved to in progress by andrey; \
                    see commit 08a2981b047b0f8ffa464e80d5486e04ecaee460 \
                    in crates/devboy-storage/src/source.rs";

    let upstream = MockServer::start();
    stub_handshake(&upstream);
    stub_tool_call(
        &upstream,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {"content": [{"type": "text", "text": original}]}
        }),
    );

    let url = format!("{}/mcp", upstream.base_url());
    let client = connect(&url, Some(TOKEN)).await;

    let result = client.call_tool("get_issue", None).await.expect("call");

    assert_eq!(text_of(&result), original);
}
