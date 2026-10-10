use std::sync::{Arc, mpsc};
use std::time::Duration;

use tempfile::TempDir;
use tracedecay_code_index::graph_projection::CodeGraphCatalogReleaseV1;
use tracedecay_domain::{ProjectId, SymbolOccurrenceId, UtcMicros};
use tracedecay_graph_db::NeverCancelled;

use super::ProductionProjectCompositionHarnessV1;
use super::journey_test_support::{git, tool_payload};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn source_body_serves_while_the_released_catalog_cannot_rewarm() {
    let isolation = TempDir::new().unwrap();
    let project = isolation.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let source = "pub fn source_anchor() -> u32 { 7 }";
    std::fs::write(project.join("lib.rs"), source).unwrap();
    git(&project, &["init", "-q", "-b", "main"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "seed source body"]);
    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .unwrap();
    let lookup = tool_payload(
        &harness
            .call_tool(
                &project,
                "tracedecay_find_exact_symbol",
                serde_json::json!({"name": "source_anchor", "format": "json"}),
            )
            .await
            .unwrap(),
    );
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
    let node_id = lookup["matches"][0]["id"].as_str().unwrap();
    let occurrence = SymbolOccurrenceId::new(node_id).unwrap();
    // An admitted reader pins the catalog, so release it before retaining the engine.
    assert!(matches!(
        store.release_interactive_catalog(),
        CodeGraphCatalogReleaseV1::Released { .. }
    ));
    let reader = store
        .interactive_reader_with_cancellation(store.generation(), Arc::new(NeverCancelled))
        .unwrap();
    let held_store = Arc::clone(&store);
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holding = std::thread::spawn(move || {
        let _guard = held_store.hold_catalog_build_for_test().unwrap();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    held_rx.await.unwrap();
    // These point reads prove that the retained engine and occurrence are ready.
    let point = reader.symbol_summary(&occurrence, Arc::new(NeverCancelled));
    let catalog_pending = store.await_rewarm(Duration::ZERO);
    let deadline = UtcMicros(tracedecay_contracts::now_micros().0 + 1_000_000);
    let response = harness
        .call_tool_with_deadline(
            &project,
            "tracedecay_source_body",
            serde_json::json!({"node_id": node_id, "format": "json"}),
            deadline,
        )
        .await;
    let still_pending = store.await_rewarm(Duration::ZERO);
    drop(release_tx);
    holding.join().unwrap();
    let warmed = store.await_rewarm(Duration::from_secs(5));
    drop(reader);
    drop(store);
    drop(serving);
    harness.shutdown().await;

    assert!(point.unwrap().is_some());
    assert!(catalog_pending.is_err());
    assert!(
        still_pending.is_err(),
        "source read must finish before the held catalog warms"
    );
    assert_eq!(warmed, Ok(()));
    let payload = tool_payload(&response.unwrap());
    assert_eq!(payload["outcome"]["outcome"], "evidence", "{payload}");
    assert_eq!(payload["outcome"]["value"]["payload"]["body"], source);
}
