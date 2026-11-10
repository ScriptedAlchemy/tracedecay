//! Context keeps useful lexical results while an unavailable graph prevents
//! an exact call-path answer. This exercises the serving owner's real catalog.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_code_index::graph_projection::{
    CodeGraphCatalogReleaseV1, CodeGraphReadinessRequirement,
};
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::NeverCancelled;

use super::context_json;
use crate::support::{production_composition_fixture_with_sources, warm_code_index_search};

fn context_payload(response: tracedecay_mcp::JsonRpcResponse) -> Value {
    assert!(response.error.is_none(), "context: {:?}", response.error);
    let result = response.result.expect("context result");
    assert_ne!(result["isError"], true, "context: {result}");
    serde_json::from_str(result["content"][0]["text"].as_str().expect("context text"))
        .unwrap_or_else(|error| panic!("context JSON: {error}: {result}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_keeps_search_and_discloses_call_path_unavailable_during_catalog_rewarm() {
    let fixture = production_composition_fixture_with_sources(|project| {
        std::fs::write(
            project.join("lib.rs"),
            "pub fn request_anchor() -> u32 { target() }\npub fn target() -> u32 { 7 }\n",
        )
        .unwrap();
    })
    .await;
    let server = fixture.harness.server(&fixture.project_root).unwrap();
    warm_code_index_search(&server, "request_anchor").await;
    let from = context_json(
        &server,
        json!({"task": "request_anchor", "max_nodes": 1, "include_code": true}),
    )
    .await;
    let to = context_json(
        &server,
        json!({"task": "target", "max_nodes": 1, "include_code": true}),
    )
    .await;
    assert_eq!(from["symbols"][0]["name"], "request_anchor", "{from}");
    assert_eq!(to["symbols"][0]["name"], "target", "{to}");
    let from_id = from["symbols"][0]["node_id"].as_str().unwrap();
    let to_id = to["symbols"][0]["node_id"].as_str().unwrap();

    let store = fixture
        .harness
        .published_code_graph_store_for_test(&fixture.project_root)
        .await
        .expect("published production graph");
    let admission = store
        .await_rewarm_and_retain_for(
            Duration::from_secs(5),
            CodeGraphReadinessRequirement::Engine,
        )
        .unwrap();
    store
        .warm_interactive_catalog_with_cancellation(None, Arc::new(NeverCancelled))
        .unwrap();
    assert!(matches!(
        tracedecay_store_runtime::release_idle_interactive_catalog(&store).await,
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

    let ordinary = fixture
        .harness
        .call_tool_with_deadline(
            &fixture.project_root,
            "tracedecay_context",
            json!({"task": "request_anchor", "format": "json"}),
            UtcMicros(tracedecay_contracts::now_micros().0 + 5_000_000),
        )
        .await;
    let ordinary_pending = store.await_rewarm(Duration::ZERO);
    let requested_path = fixture
        .harness
        .call_tool_with_deadline(
            &fixture.project_root,
            "tracedecay_context",
            json!({
                "task": "request_anchor",
                "format": "json",
                "call_path": {"from_node_id": from_id, "to_node_id": to_id}
            }),
            UtcMicros(tracedecay_contracts::now_micros().0 + 5_000_000),
        )
        .await;
    let path_pending = store.await_rewarm(Duration::ZERO);

    // Release the real background worker before asserting response content,
    // so a failing assertion cannot leave a catalog builder blocked.
    drop(release_tx);
    holding.join().unwrap();
    drop(admission);
    drop(store);
    fixture.harness.shutdown().await;

    assert!(
        ordinary_pending.is_err(),
        "ordinary context waited for the held catalog"
    );
    assert!(
        path_pending.is_err(),
        "call-path context waited for the held catalog"
    );
    let ordinary = context_payload(ordinary.unwrap());
    assert!(ordinary["call_path"].is_null(), "{ordinary}");
    assert_eq!(
        ordinary["code"],
        json!([]),
        "source stays opt-in: {ordinary}"
    );
    let requested_path = context_payload(requested_path.unwrap());
    for response in [&ordinary, &requested_path] {
        assert!(
            response["search_matches"]
                .as_array()
                .unwrap()
                .iter()
                .any(|symbol| symbol["name"] == "request_anchor"),
            "the lexical lane remains useful: {response}"
        );
    }
    let path = &requested_path["call_path"];
    assert_eq!(path["outcome"]["state"], "unavailable", "{requested_path}");
    assert_eq!(
        path["outcome"]["evidence"]["reason_code"], "code-graph-unavailable",
        "{requested_path}"
    );
    assert!(path["code_generation"].is_null(), "{requested_path}");
    assert!(path["freshness"].is_null(), "{requested_path}");
}
