//! Tests for the new `tokensave_dashboard` MCP tool (via direct handler dispatch).
//! Follows conventions from mcp_handler_test.rs: real TokenSave + handle_tool_call,
//! plus live HTTP probe of /api/capabilities on the returned URL.

mod common;

use std::fs;
use std::time::Duration;

use common::http_agent;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokensave::mcp::handle_tool_call;
use tokensave::tokensave::TokenSave;

/// The dashboard manager is process-global (one dashboard per MCP server
/// process), so these tests must not run concurrently: serialize them.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup_minimal_project() -> (TokenSave, TempDir) {
    let dir = TempDir::new().unwrap();
    let project = dir.path();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/main.rs"),
        r#"
fn main() { println!("hi"); }
#[test] fn t() {}
"#,
    )
    .unwrap();
    let cg = TokenSave::init(project).await.unwrap();
    cg.index_all().await.unwrap();
    (cg, dir)
}

// Multi-thread runtime: the blocking ureq probe must not starve the spawned
// axum server task (same reason dashboard_api_test.rs builds a 2-worker runtime).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tokensave_dashboard_tool_rejects_wildcard_host_without_starting() {
    let _guard = TEST_LOCK.lock().await;
    let (cg, _tmp) = setup_minimal_project().await;

    let err = match handle_tool_call(
        &cg,
        "tokensave_dashboard",
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
        "tokensave_dashboard",
        json!({ "action": "stop" }),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(extract_text(&stop_res.value).contains("not_running"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tokensave_dashboard_tool_starts_and_returns_url_and_serves_capabilities() {
    let _guard = TEST_LOCK.lock().await;
    let (cg, _tmp) = setup_minimal_project().await;

    // Start via the MCP dispatch (uses current cg's project)
    let res = handle_tool_call(
        &cg,
        "tokensave_dashboard",
        json!({ "host": "127.0.0.1", "port": 0 }),
        None,
        None,
    )
    .await
    .expect("dashboard start should succeed");

    let content_text = res
        .value
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|t| t.get("text"))
        .and_then(|s| s.as_str())
        .expect("text result");

    assert!(
        content_text.contains("\"status\": \"started\"")
            || content_text.contains("\"status\": \"already_running\""),
        "expected started or already: {}",
        content_text
    );
    assert!(
        content_text.contains("http://127.0.0.1:"),
        "missing url in {}",
        content_text
    );

    // Extract the URL (simple parse of our json output)
    let start = content_text.find("http://").expect("url start");
    let end = content_text[start..]
        .find('"')
        .unwrap_or(content_text.len() - start);
    let url = &content_text[start..start + end];
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
        if let Ok(mut resp) = agent.get(&cap_url).call() {
            if resp.status().as_u16() == 200 {
                let raw = resp.body_mut().read_to_string().unwrap_or_default();
                let body: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
                assert_eq!(body.get("name"), Some(&json!("tokensave-dashboard")));
                assert!(body.get("features").is_some());
                // success — now stop it via tool for cleanup
                let _stop = handle_tool_call(
                    &cg,
                    "tokensave_dashboard",
                    json!({ "action": "stop" }),
                    None,
                    None,
                )
                .await;
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "dashboard at {} did not serve /api/capabilities in time",
        url
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tokensave_dashboard_tool_is_idempotent_and_supports_stop() {
    let _guard = TEST_LOCK.lock().await;
    let (cg, _tmp) = setup_minimal_project().await;

    let res1 = handle_tool_call(&cg, "tokensave_dashboard", json!({"port": 0}), None, None)
        .await
        .unwrap();
    let text1 = extract_text(&res1.value);
    let url1 = extract_url(&text1);

    // second start returns same (already)
    let res2 = handle_tool_call(&cg, "tokensave_dashboard", json!({"port": 0}), None, None)
        .await
        .unwrap();
    let text2 = extract_text(&res2.value);
    assert!(
        text2.contains("already_running"),
        "second should be already: {}",
        text2
    );
    let url2 = extract_url(&text2);
    assert_eq!(url1, url2, "idempotent url");

    // stop
    let stop_res = handle_tool_call(
        &cg,
        "tokensave_dashboard",
        json!({"action": "stop"}),
        None,
        None,
    )
    .await
    .unwrap();
    let stop_text = extract_text(&stop_res.value);
    assert!(
        stop_text.contains("stopped"),
        "stop should report stopped: {}",
        stop_text
    );

    // stop again is not_running
    let stop2 = handle_tool_call(
        &cg,
        "tokensave_dashboard",
        json!({"action": "stop"}),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(extract_text(&stop2.value).contains("not_running"));
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
