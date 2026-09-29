//! `tracedecay_dashboard` through the production MCP server and the project's
//! graph-tool owner, plus a live HTTP probe of /api/capabilities on the
//! returned URL.

#![cfg(feature = "test-transport")]

use std::time::Duration;

use serde_json::{Value, json};

use crate::common::http_agent;
use crate::support::{extract_text, handle_real_server_tool_call, production_composition_fixture};

/// The dashboard manager is process-global (one dashboard per MCP server
/// process), so these tests must not run concurrently: serialize them.
pub(crate) static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn dashboard_refusal(
    fixture: &crate::support::ProductionCompositionFixture,
    mut arguments: Value,
) -> String {
    arguments["format"] = json!("json");
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, "tracedecay_dashboard", arguments)
        .await
        .expect("production dashboard invocation");
    let result = serde_json::to_value(response.result.expect("refusal result")).unwrap();
    assert_eq!(result["isError"], json!(true), "{result}");
    let envelope: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().expect("refusal text"))
            .expect("problem envelope JSON");
    envelope["problem"]["message"]
        .as_str()
        .expect("problem message")
        .to_owned()
}

// Multi-thread runtime: the blocking ureq probe must not starve the spawned
// axum server task (same reason dashboard_api_test.rs builds a 2-worker runtime).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracedecay_dashboard_tool_refuses_without_starting() {
    let _guard = TEST_LOCK.lock().await;
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let wildcard = dashboard_refusal(&fixture, json!({ "host": "0.0.0.0", "port": 0 })).await;
    assert!(
        wildcard.contains("loopback-only"),
        "unexpected error: {wildcard}"
    );
    assert_eq!(
        dashboard_refusal(&fixture, json!({ "bind": "127.0.0.1", "port": 0 })).await,
        "invalid arguments for tracedecay_dashboard: unknown field `bind`, expected one of `action`, `host`, `port`"
    );
    assert_eq!(
        dashboard_refusal(&fixture, json!({ "port": 70000 })).await,
        "invalid arguments for tracedecay_dashboard: invalid value: integer `70000`, expected u16"
    );

    let stop_res =
        handle_real_server_tool_call(&server, "tracedecay_dashboard", json!({ "action": "stop" }))
            .await;
    assert_eq!(extract_text(&stop_res), r#"{"status":"not_running"}"#);
    fixture.harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracedecay_dashboard_tool_starts_and_returns_url_and_serves_capabilities() {
    let _guard = TEST_LOCK.lock().await;
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

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
    let port = payload["port"].as_u64().expect("bound port");
    assert_ne!(port, 0, "an ephemeral request reports the port it bound");
    let launch_url = payload["url"].as_str().expect("dashboard url").to_owned();
    let token = launch_url
        .strip_prefix(&format!("http://127.0.0.1:{port}/?token="))
        .unwrap_or_else(|| panic!("the returned URL is the listener's launch URL: {launch_url}"));
    assert!(
        token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()),
        "the launch token is 32 random bytes in hex: {token}"
    );
    assert_eq!(
        payload,
        json!({
            "status": "started",
            "url": launch_url,
            "host": "127.0.0.1",
            "port": port,
        })
    );

    // Any local account can reach the loopback port; without the token the
    // dashboard must refuse reads and mutations alike.
    let anonymous = http_agent();
    let origin = format!("http://127.0.0.1:{port}");
    let mut refused = anonymous
        .get(&format!("{origin}/api/capabilities"))
        .call()
        .expect("unauthenticated capabilities response");
    assert_eq!(refused.status().as_u16(), 401);
    let refusal: Value =
        serde_json::from_str(&refused.body_mut().read_to_string().unwrap()).unwrap();
    assert_eq!(refusal["error"], json!("dashboard_request_unauthenticated"));
    let mutation = anonymous
        .post(&format!("{origin}/api/automation/scheduler/pause"))
        .send_json(json!({}))
        .expect("unauthenticated mutation response");
    assert_eq!(mutation.status().as_u16(), 401);
    let forged = anonymous
        .get(&format!("{origin}/?token={}", "0".repeat(64)))
        .call()
        .expect("forged launch response");
    assert_eq!(forged.status().as_u16(), 401);

    // A browser opening the launch URL is redirected to the tokenless path
    // with a session cookie, and that cookie alone admits later requests.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(4)))
        .build()
        .into();
    let launched = agent.get(&launch_url).call().expect("launch URL response");
    assert_eq!(launched.status().as_u16(), 303);
    assert_eq!(launched.headers()["location"], "/");
    let session_cookie = format!("tracedecay_dashboard_{port}={token}");
    assert_eq!(
        launched.headers()["set-cookie"],
        format!("{session_cookie}; Path=/; HttpOnly; SameSite=Strict").as_str()
    );
    let cap_url = format!("{origin}/api/capabilities");
    // Give the background server a moment to accept (rarely needed but robust)
    for _ in 0..40 {
        if let Ok(mut resp) = agent.get(&cap_url).header("Cookie", &session_cookie).call()
            && resp.status().as_u16() == 200
        {
            let raw = resp.body_mut().read_to_string().unwrap_or_default();
            let body: Value = serde_json::from_str(&raw).unwrap_or(json!({}));
            assert_eq!(body.get("name"), Some(&json!("tracedecay-dashboard")));
            assert!(body.get("features").is_some());
            // success, now stop it via tool for cleanup
            let _stop = handle_real_server_tool_call(
                &server,
                "tracedecay_dashboard",
                json!({ "action": "stop" }),
            )
            .await;
            fixture.harness.shutdown().await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("dashboard at {origin} did not serve /api/capabilities in time");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tracedecay_dashboard_tool_is_idempotent_and_supports_stop() {
    let _guard = TEST_LOCK.lock().await;
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");

    let res1 =
        handle_real_server_tool_call(&server, "tracedecay_dashboard", json!({"port": 0})).await;
    let text1 = extract_text(&res1);
    let url1 = extract_url(text1);

    // second start returns same (already)
    let res2 =
        handle_real_server_tool_call(&server, "tracedecay_dashboard", json!({"port": 0})).await;
    let text2 = extract_text(&res2);
    assert!(
        text2.contains("already_running"),
        "second should be already: {}",
        text2
    );
    let url2 = extract_url(text2);
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
    fixture.harness.shutdown().await;
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
