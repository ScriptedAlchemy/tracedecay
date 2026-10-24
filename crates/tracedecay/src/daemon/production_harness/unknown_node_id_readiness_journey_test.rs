use std::sync::{Arc, mpsc};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_code_index::graph_projection::CodeGraphCatalogReleaseV1;
use tracedecay_domain::{ProjectId, UtcMicros};
use tracedecay_graph_db::NeverCancelled;

use super::ProductionProjectCompositionHarnessV1;
use super::journey_test_support::{tool_answer, tool_payload};
use crate::test_support::git::git;

const UNKNOWN: &str =
    "symbol.v1.sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn names_absent_node(payload: &Value) -> bool {
    payload["problem"]["kind"] == "not_found_or_not_authorized" || payload["status"] == "not_found"
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_node_ids_are_answered_while_the_released_catalog_cannot_rewarm() {
    let isolation = TempDir::new().unwrap();
    let project = isolation.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("lib.rs"),
        "pub fn anchor() -> u32 { helper() }\npub fn helper() -> u32 { 7 }\n",
    )
    .unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "seed unknown node ids"]);
    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .unwrap();
    let lookup = tool_payload(
        &harness
            .call_tool(
                &project,
                "tracedecay_find_exact_symbol",
                json!({"name": "anchor", "format": "json"}),
            )
            .await
            .unwrap(),
    );
    let known = lookup["matches"][0]["id"].as_str().unwrap().to_owned();
    let project_id = ProjectId::new(harness.project_id(&project).await.unwrap()).unwrap();
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(&project, &project_id).unwrap();
    let serving = harness
        .resources
        .as_ref()
        .unwrap()
        .invocation
        .code_index_schedulers
        .latest_text_serving_for_scope(&scope)
        .await
        .unwrap();
    let store = serving.interactive_graph_store().unwrap();
    store
        .warm_interactive_catalog_with_cancellation(None, Arc::new(NeverCancelled))
        .unwrap();
    assert!(matches!(
        crate::test_support::release_idle_interactive_catalog(&store).await,
        CodeGraphCatalogReleaseV1::Released { .. }
    ));
    let held_store = Arc::clone(&store);
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holding = std::thread::spawn(move || {
        let _guard = held_store.hold_catalog_build_for_test().unwrap();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    held_rx.await.unwrap();

    let calls = [
        (
            "tracedecay_callers",
            json!({"node_id": UNKNOWN, "format": "json"}),
        ),
        (
            "tracedecay_callees",
            json!({"node_id": UNKNOWN, "format": "json"}),
        ),
        (
            "tracedecay_call_chain",
            json!({"from_node_id": UNKNOWN, "to_node_id": known, "format": "json"}),
        ),
        (
            "tracedecay_impact",
            json!({"node_id": UNKNOWN, "format": "json"}),
        ),
        (
            "tracedecay_node",
            json!({"node_id": UNKNOWN, "format": "json"}),
        ),
        (
            "tracedecay_type_hierarchy",
            json!({"node_id": UNKNOWN, "format": "json"}),
        ),
        (
            "tracedecay_source_body",
            json!({"node_id": UNKNOWN, "format": "json"}),
        ),
        (
            "tracedecay_test_map",
            json!({"node_id": UNKNOWN, "format": "json"}),
        ),
    ];
    let mut answers = Vec::new();
    for (tool, arguments) in calls {
        // A read that waited on the held catalog would run into this
        // deadline and answer the dispatch deadline instead of the node.
        let expires_at = UtcMicros(tracedecay_contracts::now_micros().0 + 5_000_000);
        let response = harness
            .call_tool_with_deadline(&project, tool, arguments, expires_at)
            .await
            .unwrap();
        answers.push((tool, response));
    }
    let still_pending = store.await_rewarm(std::time::Duration::ZERO);
    drop(release_tx);
    holding.join().unwrap();
    drop(store);
    drop(serving);
    harness.shutdown().await;

    assert!(
        still_pending.is_err(),
        "every answer must arrive before the held catalog warms"
    );
    for (tool, response) in answers {
        assert!(
            response.error.is_none(),
            "{tool} must answer instead of waiting out the catalog: {:?}",
            response.error
        );
        let (refused, payload) = tool_answer(&response);
        if tool == "tracedecay_test_map" {
            assert!(!refused, "{payload}");
            assert_eq!(payload["covered_symbols"], 0, "{payload}");
            assert_eq!(payload["uncovered_symbols"], 0, "{payload}");
        } else {
            assert!(refused, "{tool} must refuse an unknown node: {payload}");
            assert!(
                names_absent_node(&payload),
                "{tool} must answer an unknown node as not found: {payload}"
            );
        }
    }
}
