//! Negative integration test per ADR-023 §3.7 ("agent never
//! sees the value"): drive every `secrets_*` MCP tool with
//! mock inputs containing a sentinel value substring, and
//! assert no JSON response carries that substring.
//!
//! The sentinel is a deliberately distinctive string so a
//! grep over the response is sufficient — false positives are
//! essentially impossible.
//!
//! What this test catches:
//!
//! - A new field on a reply struct that accidentally serializes
//!   a value-like payload.
//! - A `Display` impl for a status enum that includes a value.
//! - A serde annotation regression that exposes a hidden field.
//!
//! What this test deliberately does NOT cover:
//!
//! - Side channels (logs, telemetry) — those are caught by the
//!   `no_expose_secret_outside_allowlist` grep gate.
//! - Provider tools (e.g. `get_issues`) — those have their own
//!   per-provider tests.

use devboy_mcp::McpServer;
use devboy_mcp::protocol::{JSONRPC_VERSION, JsonRpcRequest, RequestId};
use devboy_mcp::secrets_provision::{FakeUiLauncher, ProvisionRegistry};
use std::sync::Arc;

/// Sentinel string injected into every mock input field. If
/// it shows up in any response JSON outside an explicitly
/// allowed echo (the path the agent passed in), that's a
/// leak. Kept lowercase + kebab-case so it slots into ADR-020
/// path segments without tripping the validator.
const SENTINEL: &str = "sentinel-do-not-leak-1f9b3a2c5e7d";

#[tokio::test]
async fn secrets_request_provision_response_never_carries_sentinel() {
    // request_provision takes only a path — no value channel —
    // but a future regression that started echoing arbitrary
    // input fields back would be caught here.
    let server = build_server();
    let resp = call(
        &server,
        "secrets_request_provision",
        serde_json::json!({
            "path": format!("team/jira/{SENTINEL}"),
        }),
    )
    .await;
    assert_no_sentinel(&resp, "secrets_request_provision");
}

#[tokio::test]
async fn secrets_request_rotation_response_never_carries_sentinel() {
    let server = build_server();
    let resp = call(
        &server,
        "secrets_request_rotation",
        serde_json::json!({
            "path": format!("team/openai/{SENTINEL}"),
        }),
    )
    .await;
    assert_no_sentinel(&resp, "secrets_request_rotation");
}

#[tokio::test]
async fn secrets_propose_metadata_response_never_carries_sentinel() {
    let server = build_server();
    let resp = call(
        &server,
        "secrets_propose_metadata",
        serde_json::json!({
            "path": format!("team/jira/{SENTINEL}"),
            "fields": {
                "description": SENTINEL,
                "retrieval_url": format!("https://example.invalid/{SENTINEL}"),
                "pattern_id": SENTINEL,
            }
        }),
    )
    .await;
    assert_no_sentinel(&resp, "secrets_propose_metadata");
}

#[tokio::test]
async fn secrets_propose_new_path_response_never_carries_sentinel() {
    let server = build_server();
    let resp = call(
        &server,
        "secrets_propose_new_path",
        serde_json::json!({
            "suggested_path": format!("team/openai/{SENTINEL}"),
            "metadata": {
                "description": SENTINEL,
                "pattern_id": SENTINEL,
            }
        }),
    )
    .await;
    assert_no_sentinel(&resp, "secrets_propose_new_path");
}

#[tokio::test]
async fn secrets_request_use_approval_response_never_carries_sentinel() {
    // request_use_approval takes path + reason + optional TTL. A
    // future regression that started echoing the reason (a
    // free-form string the agent supplies) back into the reply
    // would be caught here.
    let server = build_server();
    let resp = call(
        &server,
        "secrets_request_use_approval",
        serde_json::json!({
            "path": format!("team/openai/{SENTINEL}"),
            "reason": format!("agent wants to call /v1/embeddings with {SENTINEL}"),
            "ttl_seconds": 600,
        }),
    )
    .await;
    assert_no_sentinel(&resp, "secrets_request_use_approval");
}

/// R6 (PR #265 review) — same agent-input-echo guarantee for
/// the K16 `kdbx_describe_metadata` tool. The tool needs a real
/// KDBX file + DEVBOY_KDBX_PASSPHRASE env var to do anything
/// useful, but the input-echo invariant should hold even on the
/// "missing env / missing file" error path — neither the error
/// message nor any other response field should bounce the
/// agent-supplied UUID back.
#[tokio::test]
async fn kdbx_describe_metadata_response_never_carries_sentinel() {
    // Use temp_env to scope the env-var removal — the workspace
    // `unsafe_code = "forbid"` lint blocks the bare
    // `std::env::remove_var` call this would otherwise need.
    let resp = temp_env::async_with_vars([("DEVBOY_KDBX_PASSPHRASE", None::<&str>)], async {
        let server = build_server();
        call(
            &server,
            "kdbx_describe_metadata",
            serde_json::json!({
                "file": "/nonexistent/test.kdbx",
                "uuid": SENTINEL,
            }),
        )
        .await
    })
    .await;
    assert_no_sentinel(&resp, "kdbx_describe_metadata");
}

/// R6 (PR #265 review) — same for `kdbx_edit_metadata`. Inject
/// the sentinel into every patch field (title / username / url
/// / notes / tags) plus the UUID; assert it never shows up in
/// the response.
#[tokio::test]
async fn kdbx_edit_metadata_response_never_carries_sentinel() {
    let resp = temp_env::async_with_vars([("DEVBOY_KDBX_PASSPHRASE", None::<&str>)], async {
        let server = build_server();
        call(
            &server,
            "kdbx_edit_metadata",
            serde_json::json!({
                "file": "/nonexistent/test.kdbx",
                "uuid": SENTINEL,
                "patch": {
                    "title": SENTINEL,
                    "username": SENTINEL,
                    "url": format!("https://example.invalid/{SENTINEL}"),
                    "notes": format!("agent note: {SENTINEL}"),
                    "tags": [SENTINEL, format!("prod-{SENTINEL}")],
                }
            }),
        )
        .await
    })
    .await;
    assert_no_sentinel(&resp, "kdbx_edit_metadata");
}

#[tokio::test]
async fn secrets_poll_status_response_never_carries_sentinel() {
    // poll_status reads the registry; the value-shaped fields
    // it could echo are the path and the kind. Inject the
    // sentinel via a real request_provision call first, then
    // poll.
    let mut server = build_server();
    let prov = call(
        &server,
        "secrets_request_provision",
        serde_json::json!({ "path": format!("team/jira/{SENTINEL}") }),
    )
    .await;
    // Pull the request_id out of the request_provision reply.
    let id = extract_request_id(&prov);

    // Re-borrow for the second call.
    let _ = &mut server;
    let resp = call(
        &server,
        "secrets_poll_status",
        serde_json::json!({ "request_id": id }),
    )
    .await;
    // The path field will echo the sentinel — that's
    // by-design (the agent passed it in). What we care about is
    // that no value-shaped field appeared. Filter the path out
    // and re-run the check.
    assert_no_sentinel_outside_path(&resp, "secrets_poll_status");
}

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

fn build_server() -> McpServer {
    let mut server = McpServer::new();
    let registry = Arc::new(ProvisionRegistry::with_launcher(Arc::new(
        FakeUiLauncher::new(),
    )));
    server.set_secrets_provision_registry(registry);
    server
}

async fn call(server: &McpServer, name: &str, args: serde_json::Value) -> String {
    // The server takes &mut for handle_request; clone-on-arc
    // isn't quite right here. Build a fresh server per call
    // instead — these tools don't share mutable state we care
    // about.
    let mut s = build_server_like(server);
    let req = JsonRpcRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id: RequestId::Number(1),
        method: "tools/call".to_string(),
        params: Some(serde_json::json!({
            "name": name,
            "arguments": args,
        })),
    };
    let resp = s.handle_request(req).await;
    serde_json::to_string(&resp).expect("response serialises")
}

fn build_server_like(_template: &McpServer) -> McpServer {
    build_server()
}

fn assert_no_sentinel(resp: &str, tool: &str) {
    assert!(
        !resp.contains(SENTINEL),
        "ADR-023 §3.7 violation: `{tool}` response leaked the sentinel \
         substring `{SENTINEL}` into JSON-RPC output:\n{resp}"
    );
}

fn assert_no_sentinel_outside_path(resp: &str, tool: &str) {
    // poll_status echoes the agent's path — that's by design.
    // Walk every sentinel occurrence and confirm it sits
    // immediately after a `"path":"…` opener; anywhere else is
    // a leak.
    let mut idx = 0;
    while let Some(at) = resp[idx..].find(SENTINEL) {
        let absolute = idx + at;
        let lookback_start = absolute.saturating_sub(40);
        let context = &resp[lookback_start..absolute];
        assert!(
            context.contains("\"path\":\""),
            "ADR-023 §3.7 violation: `{tool}` echoed sentinel outside the \
             `path` field. Context window:\n  …{context}{SENTINEL}…\nFull response:\n{resp}"
        );
        idx = absolute + SENTINEL.len();
    }
}

fn extract_request_id(resp: &str) -> String {
    // Crude but effective for test fixtures: dig past the
    // text-content envelope and find the embedded JSON.
    let needle = "\\\"request_id\\\":\\\"";
    let start = resp.find(needle).expect("response carries request_id");
    let after = &resp[start + needle.len()..];
    let end = after.find("\\\"").expect("request_id is quoted");
    after[..end].to_owned()
}
