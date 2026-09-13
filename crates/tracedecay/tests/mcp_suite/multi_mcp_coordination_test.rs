#[cfg(feature = "test-transport")]
use std::{path::Path, process::Command, sync::Arc, time::Duration};

#[cfg(feature = "test-transport")]
use serde_json::{Value, json};
#[cfg(feature = "test-transport")]
use tempfile::tempdir;
#[cfg(feature = "test-transport")]
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
#[cfg(feature = "test-transport")]
use tracedecay::mcp::McpServer;

#[cfg(feature = "test-transport")]
fn initialize_project(project: &Path) {
    std::fs::create_dir_all(project).unwrap();
    std::fs::write(project.join("a.rs"), "fn shared_scheduler_initial() {}\n").unwrap();
    for args in [
        &["init", "--quiet"][..],
        &["add", "."][..],
        &[
            "-c",
            "user.name=TraceDecay Tests",
            "-c",
            "user.email=tests@tracedecay.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ][..],
    ] {
        let output = Command::new(crate::common::git_program())
            .args(args)
            .current_dir(project)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(feature = "test-transport")]
fn tool_payload(result: &Value) -> Value {
    serde_json::from_str(crate::support::extract_real_server_text(result)).unwrap()
}

#[cfg(feature = "test-transport")]
async fn wait_for_symbol(server: &McpServer, symbol: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let result = crate::support::handle_real_server_tool_call(
            server,
            "tracedecay_search",
            json!({"query": symbol, "limit": 1}),
        )
        .await;
        let payload = tool_payload(&result);
        let found = payload["results"].as_array().is_some_and(|results| {
            results.iter().any(|result| {
                result["display"]["name"]
                    .as_str()
                    .is_some_and(|name| name == symbol)
            })
        });
        if found {
            return payload["code_generation"]
                .as_str()
                .unwrap_or_else(|| panic!("symbol result lacks a code generation: {payload}"))
                .to_owned();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "production code index did not publish {symbol:?}: {payload}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(feature = "test-transport")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_mcp_clients_converge_through_one_project_scheduler() {
    let isolation = tempdir().unwrap();
    let project = isolation.path().join("project");
    initialize_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .unwrap();
    let server1 = harness.server(&project).unwrap();
    let server2 = Arc::clone(&server1);

    let initial_generation_1 = wait_for_symbol(&server1, "shared_scheduler_initial").await;
    let initial_generation_2 = wait_for_symbol(&server2, "shared_scheduler_initial").await;
    assert_eq!(initial_generation_1, initial_generation_2);

    std::fs::write(
        project.join("b.rs"),
        "fn shared_scheduler_added_by_peer() {}\n",
    )
    .unwrap();

    let (queued1, queued2) = tokio::join!(
        crate::support::handle_real_server_tool_call(
            &server1,
            "tracedecay_admin_sync",
            json!({"force": true}),
        ),
        crate::support::handle_real_server_tool_call(
            &server2,
            "tracedecay_admin_sync",
            json!({"force": true}),
        ),
    );
    assert_eq!(tool_payload(&queued1)["status"], "queued");
    assert_eq!(tool_payload(&queued2)["status"], "queued");

    let (generation1, generation2) = tokio::join!(
        wait_for_symbol(&server1, "shared_scheduler_added_by_peer"),
        wait_for_symbol(&server2, "shared_scheduler_added_by_peer"),
    );
    assert_ne!(generation1, initial_generation_1);
    assert_eq!(generation1, generation2);

    harness.shutdown().await;
}
