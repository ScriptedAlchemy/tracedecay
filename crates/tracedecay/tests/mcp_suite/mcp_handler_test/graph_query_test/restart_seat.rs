//! `tracedecay_context` right after a restart, while the generation the
//! restart retained is still seating: it waits for that seat within its own
//! deadline and answers from the retained generation, or, when its deadline
//! cannot cover the seat, reports the typed `graph_warming` partial instead
//! of an answer that looks like nothing was indexed.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_code_index_runtime::CodeIndexSchedulerRegistryV1;
use tracedecay_contracts::clock::now_micros;
use tracedecay_domain::UtcMicros;

use crate::support::{commit_worktree, harness_wait_for_readiness, test_temp_dir};

fn context_arguments() -> Value {
    json!({
        "format": "json",
        "mode": "plan",
        "task": "how does resolve_config_file load the app config",
    })
}

fn context_payload(response: tracedecay_mcp::JsonRpcResponse) -> Value {
    assert!(response.error.is_none(), "context: {:?}", response.error);
    let result = response.result.expect("context result");
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap_or_default())
        .unwrap_or_else(|error| panic!("context JSON: {error}: {result}"))
}

fn matched_sites(context: &Value) -> Vec<(String, String)> {
    context["search_matches"]
        .as_array()
        .unwrap_or_else(|| panic!("context search matches: {context}"))
        .iter()
        .map(|symbol| {
            (
                symbol["name"].as_str().unwrap_or_default().to_owned(),
                symbol["file"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn anchor_site() -> (String, String) {
    ("resolve_config_file".to_owned(), "src/config.rs".to_owned())
}

async fn open(isolation: &Path, project: &Path) -> ProductionProjectCompositionHarnessV1 {
    Box::pin(
        ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
            isolation,
            [project.to_path_buf()],
        ),
    )
    .await
    .expect("production composition")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_after_restart_waits_for_the_retained_generation_to_seat() {
    crate::common::register_process_product_runtime();
    crate::common::register_process_runtime_ports();
    let isolation = test_temp_dir();
    let project = isolation.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/config.rs"),
        "pub fn resolve_config_file() -> u32 { 7 }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "mod config;\npub fn create_app() -> u32 { config::resolve_config_file() }\n",
    )
    .unwrap();
    commit_worktree(&project, "restart seat fixture");

    let harness = open(isolation.path(), &project).await;
    harness_wait_for_readiness(&harness, &project, "ready", Duration::from_secs(60)).await;
    let indexed = context_payload(
        harness
            .call_tool(&project, "tracedecay_context", context_arguments())
            .await
            .expect("context before restart"),
    );
    assert!(
        matched_sites(&indexed).contains(&anchor_site()),
        "the indexed generation must answer with the anchor: {indexed}"
    );
    harness.shutdown().await;

    // Hold the restart's mount so the retained generation cannot seat until
    // the journey releases it.
    let (mount_paused, release_mount) =
        CodeIndexSchedulerRegistryV1::pause_next_cold_mount_before_final_commit(
            project.canonicalize().unwrap(),
        )
        .await;
    let restarted = Arc::new(open(isolation.path(), &project).await);
    tokio::time::timeout(Duration::from_secs(30), mount_paused)
        .await
        .expect("the restart mount reaches its final commit")
        .expect("final-commit gate");

    let short_deadline = UtcMicros(now_micros().0 + 1_000_000);
    let warming = context_payload(
        restarted
            .call_tool_with_deadline(
                &project,
                "tracedecay_context",
                context_arguments(),
                short_deadline,
            )
            .await
            .expect("context under a deadline the seat cannot meet"),
    );
    let warming_lane = json!({"status": "unavailable", "reason": "graph_warming"});
    assert_eq!(
        (
            &warming["coverage"]["exact"],
            &warming["coverage"]["lexical"],
            &warming["coverage"]["graph"],
            &warming["retrieval"]["search"]["state"],
            &warming["freshness"]["indexing"]["reason"],
            &warming["lexical_anchors"],
            &warming["symbols"],
        ),
        (
            &warming_lane,
            &warming_lane,
            &warming_lane,
            &json!("unavailable"),
            &json!("graph_warming"),
            &json!([{"anchor": "resolve_config_file", "outcome": "not_served"}]),
            &json!([]),
        ),
        "a deadline that cannot cover the seat reports it warming: {warming}"
    );

    let waiting = tokio::spawn({
        let restarted = Arc::clone(&restarted);
        let project = project.clone();
        async move {
            restarted
                .call_tool(&project, "tracedecay_context", context_arguments())
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !waiting.is_finished(),
        "context answered while its retained generation was still unmounted"
    );
    release_mount.send(()).expect("release the restart mount");
    let seated = context_payload(
        tokio::time::timeout(Duration::from_secs(60), waiting)
            .await
            .expect("context settles once the retained generation seats")
            .expect("context task")
            .expect("context after the seat"),
    );
    assert_eq!(
        (
            matched_sites(&seated).contains(&anchor_site()),
            &seated["retrieval"]["search"]["state"],
            &seated["code_generation"],
        ),
        (true, &json!("ran"), &indexed["code_generation"]),
        "context must answer from the retained generation: {seated}"
    );
    Arc::into_inner(restarted)
        .expect("the context task released the harness")
        .shutdown()
        .await;
}
