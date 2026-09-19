//! `tracedecay_dashboard` over the production MCP `tools/call` path.
//!
//! The call is the daemon's own dispatcher. The listening URL is then read
//! back with HTTP, so a handler that only invents a URL cannot satisfy the
//! assertions. Ports are ephemeral; every other field is a literal the caller
//! observes.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};
use tracedecay_mcp::jsonrpc::JsonRpcResponse;

use crate::common::{get_json, http_agent};
use crate::support::{ProductionCompositionFixture, production_composition_fixture};

const DASHBOARD_CLI_FALLBACK: &str = "This tool is also available from the shell: `tracedecay tool dashboard ...` (`tracedecay tool dashboard --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly.";

async fn call_dashboard(
    fixture: &ProductionCompositionFixture,
    arguments: Value,
) -> JsonRpcResponse {
    fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_dashboard", arguments)
        .await
        .unwrap_or_else(|error| {
            panic!("tracedecay_dashboard production invocation failed: {error}")
        })
}

fn tool_text(result: &Value) -> &str {
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("dashboard tool result has no content: {result}"));
    assert_eq!(
        content.len(),
        1,
        "dashboard tool result must be one text block: {result}"
    );
    assert_eq!(content[0]["type"], "text", "{result}");
    content[0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("dashboard tool text missing: {result}"))
}

fn expect_payload(response: &JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "tracedecay_dashboard returned a production MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    let result = response
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("tracedecay_dashboard returned no production MCP result"));
    serde_json::from_str(tool_text(result))
        .unwrap_or_else(|error| panic!("dashboard payload was not JSON: {error}"))
}

fn expect_text(response: &JsonRpcResponse) -> &str {
    assert!(
        response.error.is_none(),
        "tracedecay_dashboard returned a production MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    let result = response
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("tracedecay_dashboard returned no production MCP result"));
    tool_text(result)
}

fn expect_execution_failure(response: &JsonRpcResponse, message: &str) {
    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, json!(1));
    assert!(response.result.is_none());
    let error = response
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("expected a JSON-RPC error, got {response:?}"));
    assert_eq!(error.code, -32603);
    assert_eq!(error.message, message);
    assert_eq!(
        error.data,
        Some(json!({
            "tool": "tracedecay_dashboard",
            "cli_fallback": DASHBOARD_CLI_FALLBACK,
        }))
    );
}

fn bound_port(url: &str) -> u64 {
    url.trim_end_matches('/')
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("dashboard url has no port: {url}"))
}

// Multi-thread runtime: the blocking HTTP probe must not starve the spawned
// dashboard task.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_starts_serves_capabilities_stops_and_refuses_non_loopback() {
    let fixture = production_composition_fixture().await;
    let project_root = fixture
        .project_root
        .canonicalize()
        .expect("production project root");

    let refused = call_dashboard(
        &fixture,
        json!({ "host": "0.0.0.0", "port": 0, "format": "json" }),
    )
    .await;
    expect_execution_failure(
        &refused,
        "tool execution failed: config error: dashboard host is loopback-only; use 127.0.0.1, localhost, or ::1 (got \"0.0.0.0\")",
    );

    let idle = call_dashboard(&fixture, json!({ "action": "stop", "format": "json" })).await;
    assert_eq!(expect_payload(&idle), json!({ "status": "not_running" }));

    let unknown = call_dashboard(&fixture, json!({ "action": "pause" })).await;
    expect_execution_failure(
        &unknown,
        "tool execution failed: config error: unknown action for tracedecay_dashboard: pause (use 'start' or 'stop')",
    );

    let started = call_dashboard(
        &fixture,
        json!({ "host": "127.0.0.1", "port": 0, "format": "json" }),
    )
    .await;
    let started_payload = expect_payload(&started);
    let url = started_payload["url"]
        .as_str()
        .unwrap_or_else(|| panic!("started dashboard omitted url: {started_payload}"))
        .to_owned();
    let port = bound_port(&url);
    assert_eq!(
        started_payload,
        json!({
            "host": "127.0.0.1",
            "port": port,
            "status": "started",
            "url": format!("http://127.0.0.1:{port}/"),
        })
    );

    let agent = http_agent();
    let (status, capabilities) = get_json(&agent, &format!("{url}api/capabilities"));
    assert_eq!(status, 200, "GET capabilities failed: {capabilities}");
    assert_eq!(capabilities["name"], "tracedecay-dashboard");
    assert_eq!(capabilities["mode"], "standalone");
    assert_eq!(capabilities["dashboards"], json!(["tracedecay"]));
    assert_eq!(
        capabilities["project_root"],
        project_root.display().to_string()
    );
    assert_eq!(capabilities["storage_mode"], "profile_sharded");
    assert_eq!(capabilities["lcm_scope"], "profile_sharded");
    assert_eq!(
        capabilities["features"],
        json!({
            "analytics": true,
            "automation": true,
            "code_diagnostics": true,
            "curation": true,
            "feedback": true,
            "graph": true,
            "lcm": true,
            "llm_curation": true,
            "managed_skills": true,
            "memory": true,
            "multi_root": true,
            "savings": true,
            "settings": true,
        })
    );
    assert_eq!(
        capabilities["multi_root"],
        json!({
            "status": "unavailable",
            "reason": "no default multi-root collection is configured; name an explicit collection",
        })
    );
    assert_eq!(capabilities["automation"]["available"], true);
    assert_eq!(capabilities["automation"]["enabled"], true);
    assert_eq!(capabilities["automation"]["mode"], "standalone_backend");
    assert_eq!(capabilities["automation"]["backend"], "codex_app_server");
    assert_eq!(capabilities["automation"]["host_mode"], "standalone");
    assert_eq!(
        capabilities["automation"]["availability"]["backend"],
        "codex_app_server"
    );

    let requested_port = if port == 1 { 2 } else { 1 };
    let repeated = call_dashboard(
        &fixture,
        json!({ "host": "localhost", "port": requested_port }),
    )
    .await;
    assert_eq!(
        expect_text(&repeated),
        format!(
            "**host:** 127.0.0.1\n**port:** {port}\n**requested_host:** localhost\n**requested_port:** {requested_port}\n**requested_port_honored:** false\n**status:** already_running\n**url:** http://127.0.0.1:{port}/\n"
        )
    );

    let repeated_json = call_dashboard(
        &fixture,
        json!({
            "action": "start",
            "host": "localhost",
            "port": requested_port,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(
        expect_payload(&repeated_json),
        json!({
            "host": "127.0.0.1",
            "port": port,
            "requested_host": "localhost",
            "requested_port": requested_port,
            "requested_port_honored": false,
            "status": "already_running",
            "url": format!("http://127.0.0.1:{port}/"),
        })
    );

    let stopped = call_dashboard(&fixture, json!({ "action": "stop", "format": "json" })).await;
    assert_eq!(
        expect_payload(&stopped),
        json!({
            "previous_url": format!("http://127.0.0.1:{port}/"),
            "status": "stopped",
        })
    );

    let stopped_again = call_dashboard(&fixture, json!({ "action": "stop" })).await;
    assert_eq!(expect_text(&stopped_again), "**status:** not_running\n");

    fixture.harness.shutdown().await;
}
