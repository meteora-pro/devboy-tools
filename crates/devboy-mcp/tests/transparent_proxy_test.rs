//! End-to-end integration test for the transparent proxy stack.
//!
//! Exercises the Phase 1-6 pieces together:
//!   config → signature matching → routing engine → telemetry pipeline → observability.
//!
//! We stand up a mock upstream MCP server via `httpmock`, connect a `McpProxyClient` to
//! it, feed the raw upstream catalogue plus a hand-rolled local catalogue into
//! [`build_report`], run decisions through a [`RoutingEngine`] for every realistic case,
//! and capture the result set in a [`TelemetryPipeline`] with a mock telemetry endpoint.

use std::time::Duration;

use devboy_core::config::{
    ProxyConfig, ProxyRoutingConfig, ProxyRoutingOverride, ProxyTelemetryConfig, ProxyToolRule,
    RoutingStrategy,
};
use devboy_mcp::protocol::ToolDefinition;
use devboy_mcp::proxy::{McpProxyClient, ProxyManager, ProxyTransport};
use devboy_mcp::routing::{ProxyStatus, RoutingEngine, RoutingTarget};
use devboy_mcp::signature_match::{ToolCatalogue, build_report};
use devboy_mcp::telemetry::{TelemetryAuth, TelemetryEvent, TelemetryPipeline, TelemetryStatus};
use httpmock::prelude::*;
use serde_json::json;

fn local_tool(name: &str, required: &[&str]) -> ToolDefinition {
    let mut props = serde_json::Map::new();
    for r in required {
        props.insert(r.to_string(), json!({"type": "string"}));
    }
    ToolDefinition {
        name: name.to_string(),
        description: format!("local impl of {}", name),
        input_schema: json!({
            "type": "object",
            "properties": props,
            "required": required,
        }),
        category: None,
    }
}

fn upstream_stub_tools() -> Vec<serde_json::Value> {
    vec![
        json!({
            "name": "get_issues",
            "description": "Get issues (upstream)",
            "inputSchema": {"type": "object", "properties": {}, "required": []}
        }),
        // Same name but upstream requires workspace_id that local schema lacks.
        json!({
            "name": "get_issue",
            "description": "Get a single issue (upstream)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "workspace_id": {"type": "string"}
                },
                "required": ["key", "workspace_id"]
            }
        }),
        // Remote-only tool.
        json!({
            "name": "cloud_render_report",
            "description": "Render a dashboard report",
            "inputSchema": {"type": "object", "properties": {}, "required": []}
        }),
    ]
}

fn setup_upstream(server: &MockServer) {
    server.mock(|when, then| {
        when.method(POST)
            .path("/mcp")
            .body_includes(r#""method":"initialize""#);
        then.status(200)
            .header("mcp-session-id", "int-test-1")
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
    server.mock(|when, then| {
        when.method(POST)
            .path("/mcp")
            .body_includes(r#""method":"tools/list""#);
        then.status(200).json_body(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {"tools": upstream_stub_tools()}
        }));
    });
}

#[tokio::test]
async fn end_to_end_routing_over_matched_catalogues() {
    // 1. Stand up mock upstream.
    let upstream = MockServer::start();
    setup_upstream(&upstream);

    // 2. Connect proxy client and fetch upstream tools.
    let url = format!("{}/mcp", upstream.base_url());
    let mut client = McpProxyClient::connect(
        "cloud",
        &url,
        Some("cloud"),
        None,
        "none",
        ProxyTransport::StreamableHttp,
        None,
    )
    .await
    .unwrap();
    client.fetch_tools().await.unwrap();

    // 3. Wrap in ProxyManager (mirrors what the real server does).
    let mut manager = ProxyManager::new();
    manager.add_client(client);

    let catalogue = manager.raw_upstream_catalogue();
    assert_eq!(catalogue.len(), 1);
    let (prefix, upstream_tools) = &catalogue[0];
    assert_eq!(prefix, "cloud");
    assert_eq!(upstream_tools.len(), 3);

    // 4. Declare the local tool catalogue with schema mismatches and a local-only tool.
    let local = vec![
        local_tool("get_issues", &[]),
        local_tool("get_issue", &["key"]), // missing workspace_id → incompatible
        local_tool("list_contexts", &[]),  // local-only
    ];

    // 5. Build match report and spot-check classification.
    let report = build_report(ToolCatalogue {
        local: &local,
        upstream: catalogue.iter().map(|(p, t)| (p.clone(), &t[..])).collect(),
    });

    let m_get_issues = report.get("get_issues").unwrap();
    assert!(
        m_get_issues.is_routable_local(),
        "get_issues should be routable locally"
    );
    let m_get_issue = report.get("get_issue").unwrap();
    assert_eq!(m_get_issue.schema_compatible, Some(false));
    assert!(report.get("list_contexts").unwrap().local_present);
    assert!(report.get("cloud_render_report").unwrap().remote_present);

    // 6. Wire a routing engine with a `local-first` strategy + an override that forces
    //    every `get_*` tool local.
    let routing_config = ProxyRoutingConfig {
        strategy: RoutingStrategy::LocalFirst,
        fallback_on_error: true,
        tool_overrides: vec![ProxyToolRule {
            pattern: "get_*".to_string(),
            strategy: RoutingStrategy::Local,
        }],
    };
    let engine = RoutingEngine::new(routing_config, report);

    // Matched & schema-compatible tool with override → Local (no fallback because
    // RoutingStrategy::Local does not define one).
    let d = engine.decide_quiet("get_issues");
    assert_eq!(d.primary, RoutingTarget::Local);
    assert!(d.fallback.is_none());

    // Matched but schema-incompatible → forced Remote regardless of strategy/override.
    let d = engine.decide_quiet("get_issue");
    assert!(matches!(d.primary, RoutingTarget::Remote { .. }));

    // Local-only → Local.
    let d = engine.decide_quiet("list_contexts");
    assert_eq!(d.primary, RoutingTarget::Local);

    // Remote-only → Remote:cloud.
    let d = engine.decide_quiet("cloud_render_report");
    match d.primary {
        RoutingTarget::Remote {
            prefix,
            original_name,
        } => {
            assert_eq!(prefix, "cloud");
            assert_eq!(original_name, "cloud_render_report");
        }
        other => panic!("expected Remote, got {:?}", other),
    }

    // Unknown → Reject.
    let d = engine.decide_quiet("ghost");
    assert_eq!(d.primary, RoutingTarget::Reject);

    // Explicit prefix → Remote regardless of report.
    let d = engine.decide_quiet("cloud__list_contexts");
    match d.primary {
        RoutingTarget::Remote {
            prefix,
            original_name,
        } => {
            assert_eq!(prefix, "cloud");
            assert_eq!(original_name, "list_contexts");
        }
        other => panic!("expected Remote via explicit prefix, got {:?}", other),
    }

    // 7. Observability surface.
    let status = ProxyStatus::from_engine(&engine);
    assert!(status.routable_locally.contains(&"get_issues".to_string()));
    assert!(status.local_only.contains(&"list_contexts".to_string()));
    assert!(
        status
            .remote_only
            .contains(&"cloud_render_report".to_string())
    );
    assert_eq!(status.incompatible.len(), 1);
    assert_eq!(status.incompatible[0].tool, "get_issue");

    let report_text = status.to_text_report();
    assert!(report_text.contains("Routable locally"));
    assert!(report_text.contains("Schema incompatible"));

    // 8. Telemetry pipeline: emit one event per decision above and verify batch upload.
    let telemetry_server = MockServer::start_async().await;
    telemetry_server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/api/telemetry/tool-invocations")
                .body_includes(r#""tool":"get_issues""#);
            then.status(202).body("");
        })
        .await;

    let tele_cfg = ProxyTelemetryConfig {
        enabled: true,
        batch_size: 10,
        batch_interval_secs: 60,
        endpoint: Some(format!(
            "{}/api/telemetry/tool-invocations",
            telemetry_server.base_url()
        )),
        token_key: None,
        offline_queue_max: 100,
    };

    let mut pipeline = TelemetryPipeline::new(tele_cfg);
    pipeline
        .start(TelemetryAuth {
            bearer_token: Some("int-test".into()),
        })
        .unwrap();

    let buf = pipeline.buffer();
    for name in [
        "get_issues",
        "get_issue",
        "list_contexts",
        "cloud_render_report",
    ] {
        let decision = engine.decide_quiet(name);
        let mut ev = TelemetryEvent::now(&decision.resolved_name, decision.reason.as_label());
        ev.routing_detail = decision.reason.detail().map(String::from);
        ev.status = TelemetryStatus::Success;
        ev.latency_ms = 7;
        if let RoutingTarget::Remote { prefix, .. } = &decision.primary {
            ev.upstream = Some(prefix.clone());
        }
        buf.record(ev).await;
    }

    assert_eq!(buf.len().await, 4);
    let flushed = pipeline.flush().await.unwrap();
    assert_eq!(flushed, 4);
    assert_eq!(buf.len().await, 0);
    pipeline.shutdown().await;

    // 9. Default proxy config is a no-op regression guard for Phase 1 backward-compat.
    //    Full TOML parsing regression is covered in devboy-core unit tests.
    assert!(ProxyConfig::default().is_default());

    // 10. Quick sanity check on merged_with for per-server overrides.
    let global = ProxyRoutingConfig::default();
    let override_cfg = ProxyRoutingOverride {
        strategy: Some(RoutingStrategy::RemoteFirst),
        fallback_on_error: Some(false),
        tool_overrides: None,
    };
    let merged = global.merged_with(Some(&override_cfg));
    assert_eq!(merged.strategy, RoutingStrategy::RemoteFirst);
    assert!(!merged.fallback_on_error);

    // Avoid unused var warning on Duration import.
    let _ = Duration::from_secs(0);
}
