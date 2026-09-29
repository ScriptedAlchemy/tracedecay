//! Graph reads while the project has no sealed code graph, as right after a
//! daemon restart: every read answers one typed unavailable state instead of
//! an empty success, and answers normally once the generation seals.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_code_index_runtime::CodeIndexSchedulerRegistryV1;

use crate::support::{commit_worktree, harness_wait_for_readiness, test_temp_dir};

async fn tool_result(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool: &str,
    arguments: Value,
) -> Value {
    let response = harness
        .call_tool(project, tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool}: {error}"));
    assert!(response.error.is_none(), "{tool}: {:?}", response.error);
    response
        .result
        .unwrap_or_else(|| panic!("{tool} returned no result"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn graph_reads_refuse_as_unavailable_until_the_graph_seals() {
    crate::common::register_process_product_runtime();
    crate::common::register_process_runtime_ports();
    let isolation = test_temp_dir();
    let project = isolation.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/walk.rs"), "pub fn step() {}\n").unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "mod walk;\npub fn run() { walk::step(); }\n",
    )
    .unwrap();
    commit_worktree(&project, "unsealed graph fixture");
    let (mount_paused, release_mount) =
        CodeIndexSchedulerRegistryV1::pause_next_cold_mount_before_final_commit(
            project.canonicalize().unwrap(),
        )
        .await;
    let harness = Box::pin(
        ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
            isolation.path(),
            [project.clone()],
        ),
    )
    .await
    .expect("production composition");
    tokio::time::timeout(Duration::from_secs(30), mount_paused)
        .await
        .expect("the code-index mount reaches its final commit")
        .expect("final-commit gate");

    let unavailable = json!({
        "kind": "unavailable",
        "code": "application.code-graph.unavailable",
        "message": "The project's verified code graph is not serving yet; retry after the \
                    code index seals a generation.",
        "retry": "after_delay",
        "legal_actions": ["retry"],
    });
    for (tool, arguments) in [
        (
            "tracedecay_file_dependents",
            json!({"file": "src/walk.rs", "format": "json"}),
        ),
        (
            "tracedecay_qualified_name",
            json!({
                "qualified_name": "src/walk.rs::step",
                "page": {"page_size": 10, "cursor": null},
                "format": "json",
            }),
        ),
        (
            "tracedecay_module_api",
            json!({"path": "src/walk.rs", "format": "json"}),
        ),
    ] {
        let result = tool_result(&harness, &project, tool, arguments).await;
        let problem = &result["structuredContent"]["problem"];
        assert_eq!(
            (
                &result["isError"],
                json!({
                    "kind": problem["kind"],
                    "code": problem["code"],
                    "message": problem["message"],
                    "retry": problem["retry"],
                    "legal_actions": problem["legal_actions"],
                }),
            ),
            (&json!(true), unavailable.clone()),
            "{tool}: {result}"
        );
    }
    let markdown = tool_result(
        &harness,
        &project,
        "tracedecay_file_dependents",
        json!({"file": "src/walk.rs"}),
    )
    .await;
    let markdown = markdown["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        markdown.contains("- Status: `problem`")
            && markdown.contains("- Problem: `application.code-graph.unavailable`")
            && !markdown.contains("- Status: `success`"),
        "{markdown}"
    );

    release_mount
        .send(())
        .expect("release the code-index mount");
    harness_wait_for_readiness(&harness, &project, "ready", Duration::from_secs(30)).await;
    let sealed = tool_result(
        &harness,
        &project,
        "tracedecay_file_dependents",
        json!({"file": "src/walk.rs", "format": "json"}),
    )
    .await;
    let evidence: Value =
        serde_json::from_str(sealed["content"][0]["text"].as_str().unwrap_or_default())
            .unwrap_or_else(|error| panic!("file dependents JSON: {error}: {sealed}"));
    assert_eq!(
        evidence["outcome"]["value"]["payload"]["dependent_files"],
        json!(["src/lib.rs"]),
        "{evidence}"
    );
    harness.shutdown().await;
}
