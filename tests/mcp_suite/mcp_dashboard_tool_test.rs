//! Tests for the new `tracedecay_dashboard` MCP tool (via direct handler dispatch).
//! Follows conventions from mcp_handler_test.rs: real TraceDecay + handle_tool_call,
//! plus live HTTP probe of /api/capabilities on the returned URL.

use std::fs;
#[cfg(feature = "test-transport")]
use std::time::Duration;

#[cfg(feature = "test-transport")]
use crate::common::http_agent;
use serde_json::{Value, json};
#[cfg(feature = "test-transport")]
use std::sync::Arc;
use tempfile::TempDir;
#[cfg(feature = "test-transport")]
use tracedecay::mcp::McpServer;
use tracedecay::mcp::handle_tool_call;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use crate::common::canonical_existing_path;
#[cfg(feature = "test-transport")]
use crate::support::{handle_real_server_tool_call, open_active_project_scoped_runtime};

/// The dashboard manager is process-global (one dashboard per MCP server
/// process), so these tests must not run concurrently: serialize them.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_minimal_project() -> (TraceDecay, TempDir, TempDir) {
    let home = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    let profile_root = canonical_existing_path(home.path()).join(".tracedecay");
    let open_options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/main.rs"),
        r#"
fn main() { println!("hi"); }
#[test] fn t() {}
"#,
    )
    .unwrap();
    let cg = crate::fixture::init_project_from_template_with_options(project, open_options)
        .await
        .unwrap();
    cg.index_all().await.unwrap();
    (cg, dir, home)
}

#[cfg(feature = "test-transport")]
async fn dashboard_test_server(cg: TraceDecay) -> Arc<McpServer> {
    let runtime = open_active_project_scoped_runtime(&cg).await;
    McpServer::new_with_host_admission_test_runtime_for_test(cg, None, runtime)
        .await
        .expect("registered test server")
}

// Multi-thread runtime: the blocking ureq probe must not starve the spawned
// axum server task (same reason dashboard_api_test.rs builds a 2-worker runtime).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracedecay_dashboard_tool_rejects_wildcard_host_without_starting() {
    let _guard = TEST_LOCK.lock().await;
    let (cg, _tmp, _home) = setup_minimal_project().await;

    let err = match handle_tool_call(
        &cg,
        "tracedecay_dashboard",
        json!({ "host": "0.0.0.0", "port": 0 }),
        None,
        None,
    )
    .await
    {
        Ok(_) => panic!("wildcard host should be rejected"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("loopback-only"),
        "unexpected error: {err}"
    );

    let stop_res = handle_tool_call(
        &cg,
        "tracedecay_dashboard",
        json!({ "action": "stop" }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(extract_text(&stop_res.value).contains("not_running"));
}

#[cfg(feature = "test-transport")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracedecay_dashboard_tool_starts_and_returns_url_and_serves_capabilities() {
    let _guard = TEST_LOCK.lock().await;
    let (cg, _tmp, _home) = setup_minimal_project().await;
    let server = dashboard_test_server(cg).await;

    // Start via the MCP dispatch (uses current cg's project)
    let res = handle_real_server_tool_call(
        &server,
        "tracedecay_dashboard",
        json!({ "host": "127.0.0.1", "port": 0, "format": "json" }),
    )
    .await;

    let content_text = res
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|t| t.get("text"))
        .and_then(|s| s.as_str())
        .expect("text result");

    let payload: Value = serde_json::from_str(content_text).expect("dashboard payload");
    assert!(
        matches!(
            payload["status"].as_str(),
            Some("started" | "already_running")
        ),
        "expected started or already: {payload}",
    );
    let url = payload["url"].as_str().expect("dashboard url");
    let url = if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{}/", url)
    };

    // Live probe: the returned URL must serve /api/capabilities
    let agent = http_agent();
    let cap_url = format!("{}api/capabilities", url);
    // Give the background server a moment to accept (rarely needed but robust)
    for _ in 0..40 {
        if let Ok(mut resp) = agent.get(&cap_url).call()
            && resp.status().as_u16() == 200
        {
            let raw = resp.body_mut().read_to_string().unwrap_or_default();
            let body: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            assert_eq!(body.get("name"), Some(&json!("tracedecay-dashboard")));
            assert!(body.get("features").is_some());
            // success — now stop it via tool for cleanup
            let _stop = handle_real_server_tool_call(
                &server,
                "tracedecay_dashboard",
                json!({ "action": "stop" }),
            )
            .await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "dashboard at {} did not serve /api/capabilities in time",
        url
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracedecay_dashboard_tool_is_idempotent_and_supports_stop() {
    let _guard = TEST_LOCK.lock().await;
    let (cg, _tmp, _home) = setup_minimal_project().await;
    let server = dashboard_test_server(cg).await;

    let res1 =
        handle_real_server_tool_call(&server, "tracedecay_dashboard", json!({"port": 0})).await;
    let text1 = extract_text(&res1);
    let url1 = extract_url(&text1);

    // second start returns same (already)
    let res2 =
        handle_real_server_tool_call(&server, "tracedecay_dashboard", json!({"port": 0})).await;
    let text2 = extract_text(&res2);
    assert!(
        text2.contains("already_running"),
        "second should be already: {}",
        text2
    );
    let url2 = extract_url(&text2);
    assert_eq!(url1, url2, "idempotent url");

    // stop
    let stop_res =
        handle_real_server_tool_call(&server, "tracedecay_dashboard", json!({"action": "stop"}))
            .await;
    let stop_text = extract_text(&stop_res);
    assert!(
        stop_text.contains("stopped"),
        "stop should report stopped: {}",
        stop_text
    );

    // stop again is not_running
    let stop2 =
        handle_real_server_tool_call(&server, "tracedecay_dashboard", json!({"action": "stop"}))
            .await;
    assert!(extract_text(&stop2).contains("not_running"));
}

fn extract_text(v: &Value) -> String {
    v.get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|t| t.get("text"))
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string()
}

#[cfg(feature = "test-transport")]
fn extract_url(text: &str) -> String {
    if let Some(start) = text.find("http://") {
        let rest = &text[start..];
        let end = rest.find(['"', ' ', '\n', '}']).unwrap_or(rest.len());
        let mut u = rest[..end].to_string();
        if !u.ends_with('/') {
            u.push('/');
        }
        return u;
    }
    "".into()
}
