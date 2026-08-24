use std::time::Duration;

use serde_json::{Value, json};

use crate::support::extract_text;

use super::{AnalysisToolHost, handle_tool_call};

pub(super) async fn wait_for_current_graph(host: &impl AnalysisToolHost) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let status = handle_tool_call(
                host,
                "tracedecay_status",
                json!({
                    "format": "json",
                    "include_branch_diagnostics": false,
                    "include_storage_health": false,
                    "include_session_ingest": false,
                    "include_staleness": false,
                }),
                None,
                None,
            )
            .await
            .expect("typed project status while awaiting the current graph");
            let status: Value = serde_json::from_str(extract_text(&status.value))
                .expect("typed project status JSON");
            match status["code_index_freshness"]["status"].as_str() {
                Some("current") => break,
                Some("warming") => tokio::task::yield_now().await,
                actual => panic!("graph readiness became {actual:?}: {status}"),
            }
        }
    })
    .await
    .expect("graph did not become current within the publication budget");
}

pub(super) async fn find_node_id(host: &impl AnalysisToolHost, name: &str) -> String {
    wait_for_current_graph(host).await;
    let result = handle_tool_call(
        host,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20}),
        None,
        None,
    )
    .await
    .unwrap_or_else(|error| panic!("production exact-symbol read failed: {error}"));
    let payload: Value =
        serde_json::from_str(extract_text(&result.value)).expect("exact-symbol JSON");
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches
                .iter()
                .find(|result| result["name"].as_str() == Some(name))
        })
        .and_then(|result| result["id"].as_str())
        .unwrap_or_else(|| panic!("node '{name}' not found in production generation: {payload}"))
        .to_owned()
}
