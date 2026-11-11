//! Exact-node reports use the sealed caller's evidence, including after a
//! restart, without turning an unknown receiver into a same-named target.

use std::fs;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_code_index::graph_projection::{
    CodeGraphCatalogReleaseV1, CodeGraphReadinessRequirement,
};
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::NeverCancelled;

use super::{node_call, occurrence_id, parse_json, tool_text};
use crate::support::{production_composition_fixture_with_sources, wait_for_current_graph};

const MAIN: &str = "import { Worker } from './worker';\n\
export function bound(worker: Worker): number { return worker.act(); }\n\
export function uncertain(receiver: any): void { receiver.act(); receiver.unknown(); }\n";
const OTHER: &str = "export function elsewhere(receiver: any): void { receiver.act(); }\n";
const GLOB: &str = "mod left;\nmod right;\n\
pub fn glob_uncertain() { use crate::left::*; use crate::right::*; g(); }\n";
const AMBIGUOUS: &str = "describe('scope', () => {\n\
  function scale(value: number) { return value; }\n\
  function scale(value: number) { return value + 1; }\n\
  it('ambiguous', () => { scale(1); });\n\
});\n";

fn payload(response: &Value) -> Value {
    assert_ne!(response["result"]["isError"], true, "{response}");
    parse_json(&tool_text(response))
}

fn assert_report_identity<'a>(node: &'a Value, caller: &str, file: &str) -> &'a Value {
    let report = &node["unresolved_calls"];
    let typed: tracedecay_contracts::retrieval::NodeUnresolvedCallReportV1 =
        serde_json::from_value(report.clone()).expect("typed unresolved-call report");
    assert_eq!(typed.caller_node_id.as_str(), caller);
    assert_eq!(typed.file, file);
    assert_eq!(node["id"], caller);
    assert_eq!(node["file"], file);
    report
}

fn assert_site_span(site: &Value, source: &str, spelling: &str) {
    assert_eq!(site["reference_name"], spelling, "{site}");
    let start = site["evidence_span"]["start_byte"].as_u64().unwrap() as usize;
    let end = site["evidence_span"]["end_byte"].as_u64().unwrap() as usize;
    assert_eq!(&source[start..end], spelling, "{site}");
    assert_eq!(site["reasons"], json!(["target_unresolved"]), "{site}");
}

#[tokio::test]
async fn node_reports_exact_unresolved_sites_and_preserves_them_after_restore() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/worker.ts"),
            "export class Worker { act(): number { return 1; } }\n",
        )
        .unwrap();
        fs::write(project.join("src/main.ts"), MAIN).unwrap();
        fs::write(project.join("src/other.ts"), OTHER).unwrap();
        fs::write(project.join("src/lib.rs"), GLOB).unwrap();
        for module in ["left", "right"] {
            fs::write(project.join(format!("src/{module}.rs")), "pub fn g() {}\n").unwrap();
        }
        fs::write(project.join("src/ambiguous.ts"), AMBIGUOUS).unwrap();
    })
    .await;
    let server = fixture.harness.server(&fixture.project_root).unwrap();
    wait_for_current_graph(&server).await;
    let uncertain = occurrence_id(&server, "uncertain", "function").await;
    let bound = occurrence_id(&server, "bound", "function").await;
    let elsewhere = occurrence_id(&server, "elsewhere", "function").await;
    let glob = occurrence_id(&server, "glob_uncertain", "function").await;
    let ambiguous = occurrence_id(&server, "ambiguous", "function").await;
    let store = fixture
        .harness
        .published_code_graph_store_for_test(&fixture.project_root)
        .await
        .unwrap();
    let serving = store
        .await_rewarm_and_retain_for(
            Duration::from_secs(5),
            CodeGraphReadinessRequirement::Catalog,
        )
        .unwrap();

    let response = node_call(&server, json!({"node_id": uncertain, "format": "json"})).await;
    let uncertain_node = payload(&response);
    let report = assert_report_identity(&uncertain_node, &uncertain, "src/main.ts");
    assert_eq!(report["outcome"]["state"], "complete", "{report}");
    let sites = report["outcome"]["sites"].as_array().unwrap();
    assert_eq!(sites.len(), 2, "{report}");
    assert_site_span(&sites[0], MAIN, "receiver.act");
    assert_site_span(&sites[1], MAIN, "receiver.unknown");
    assert!(
        sites
            .iter()
            .all(|site| site.get("argument_count").is_none())
    );
    let cost = response["result"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["text"].as_str())
        .find(|text| text.trim_start().starts_with("tracedecay_cost:"))
        .expect("the node read returns its actual cost receipt");
    assert!(
        cost.split_whitespace()
            .any(|field| field == "catalog_symbols=1"),
        "{cost}"
    );
    assert!(
        cost.split_whitespace()
            .any(|field| field == "adjacency_rows=0"),
        "{cost}"
    );

    let bound_node =
        payload(&node_call(&server, json!({"node_id": bound, "format": "json"})).await);
    let bound_report = assert_report_identity(&bound_node, &bound, "src/main.ts");
    assert_eq!(
        bound_report["outcome"],
        json!({"state": "complete", "sites": []}),
        "the proven Worker.act call must not remain unresolved: {bound_node}"
    );
    assert_eq!(
        bound_report["file_occurrence_id"],
        report["file_occurrence_id"]
    );
    let other_node =
        payload(&node_call(&server, json!({"node_id": elsewhere, "format": "json"})).await);
    let other_report = assert_report_identity(&other_node, &elsewhere, "src/other.ts");
    assert_ne!(
        other_report["file_occurrence_id"],
        report["file_occurrence_id"]
    );
    assert_eq!(other_report["outcome"]["state"], "complete");
    let other_sites = other_report["outcome"]["sites"].as_array().unwrap();
    assert_eq!(other_sites.len(), 1);
    assert_site_span(&other_sites[0], OTHER, "receiver.act");

    for (caller, file, source, spelling, reason) in [
        (&glob, "src/lib.rs", GLOB, "g", "unmodeled_block_glob"),
        (
            &ambiguous,
            "src/ambiguous.ts",
            AMBIGUOUS,
            "scale",
            "ambiguous_local",
        ),
    ] {
        let node = payload(&node_call(&server, json!({"node_id": caller, "format": "json"})).await);
        let report = assert_report_identity(&node, caller, file);
        assert_eq!(report["outcome"]["state"], "complete", "{report}");
        let sites = report["outcome"]["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 1, "{report}");
        assert_eq!(sites[0]["reference_name"], spelling, "{report}");
        let start = sites[0]["evidence_span"]["start_byte"].as_u64().unwrap() as usize;
        let end = sites[0]["evidence_span"]["end_byte"].as_u64().unwrap() as usize;
        assert_eq!(&source[start..end], spelling, "{report}");
        assert_eq!(sites[0]["reasons"], json!([reason]), "{report}");
    }

    let markdown = tool_text(&node_call(&server, json!({"node_id": uncertain})).await);
    assert!(
        markdown.contains("## Recorded unresolved calls"),
        "{markdown}"
    );
    assert!(
        markdown.contains("Complete for recorded evidence: 2"),
        "{markdown}"
    );
    assert!(markdown.contains("receiver.act") && markdown.contains("target unresolved"));
    assert!(markdown.contains("not all possible runtime calls"));
    assert!(
        !markdown.contains("receiver.act();"),
        "the report contains no source body"
    );

    drop(serving);
    drop(store);
    drop(server);
    let fixture = fixture.reopen().await;
    let server = fixture.harness.server(&fixture.project_root).unwrap();
    wait_for_current_graph(&server).await;
    let store = fixture
        .harness
        .published_code_graph_store_for_test(&fixture.project_root)
        .await
        .unwrap();
    let serving = store
        .await_rewarm_and_retain_for(
            Duration::from_secs(5),
            CodeGraphReadinessRequirement::Catalog,
        )
        .unwrap();
    let restored =
        payload(&node_call(&server, json!({"node_id": uncertain, "format": "json"})).await);
    assert_eq!(
        restored["unresolved_calls"],
        report.clone(),
        "unchanged caller evidence must survive sealed restore"
    );
    drop(serving);
    drop(store);
    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn node_discloses_the_recorded_site_limit() {
    let source = format!(
        "export function many(receiver: any): void {{\n{}}}\n",
        (0..20)
            .map(|index| format!("  receiver.m{index:02}();\n"))
            .collect::<String>()
    );
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::write(project.join("many.ts"), &source).unwrap();
    })
    .await;
    let server = fixture.harness.server(&fixture.project_root).unwrap();
    wait_for_current_graph(&server).await;
    let caller = occurrence_id(&server, "many", "function").await;
    let store = fixture
        .harness
        .published_code_graph_store_for_test(&fixture.project_root)
        .await
        .unwrap();
    let serving = store
        .await_rewarm_and_retain_for(
            Duration::from_secs(5),
            CodeGraphReadinessRequirement::Catalog,
        )
        .unwrap();
    let node = payload(&node_call(&server, json!({"node_id": caller, "format": "json"})).await);
    let report = assert_report_identity(&node, &caller, "many.ts");
    assert_eq!(report["outcome"]["state"], "truncated", "{report}");
    assert_eq!(report["outcome"]["maximum_references"], 16, "{report}");
    let sites = report["outcome"]["sites"].as_array().unwrap();
    assert_eq!(sites.len(), 16, "{report}");
    for (index, site) in sites.iter().enumerate() {
        assert_site_span(site, &source, &format!("receiver.m{index:02}"));
    }
    let markdown = tool_text(&node_call(&server, json!({"node_id": caller})).await);
    assert!(
        markdown.contains("Truncated after 16 retained evidence records"),
        "{markdown}"
    );
    assert!(!markdown.contains("receiver.m16"), "{markdown}");
    drop(serving);
    drop(store);
    fixture.harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_keeps_metadata_when_unresolved_evidence_catalog_is_rewarming() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::write(project.join("main.ts"), OTHER).unwrap();
    })
    .await;
    let server = fixture.harness.server(&fixture.project_root).unwrap();
    wait_for_current_graph(&server).await;
    let caller = occurrence_id(&server, "elsewhere", "function").await;
    let store = fixture
        .harness
        .published_code_graph_store_for_test(&fixture.project_root)
        .await
        .unwrap();
    let serving = store
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
    let response = fixture
        .harness
        .call_tool_with_deadline(
            &fixture.project_root,
            "tracedecay_node",
            json!({"node_id": caller, "format": "json"}),
            UtcMicros(tracedecay_contracts::now_micros().0 + 5_000_000),
        )
        .await;
    let still_pending = store.await_rewarm(Duration::ZERO);
    // Release the real worker before assertions so a failure cannot strand it.
    drop(release_tx);
    holding.join().unwrap();
    drop(serving);
    drop(store);
    fixture.harness.shutdown().await;

    assert!(still_pending.is_err(), "node waited for the held catalog");
    let response = response.unwrap();
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.expect("node result");
    assert_ne!(result["isError"], true, "{result}");
    let node = parse_json(result["content"][0]["text"].as_str().unwrap());
    assert_eq!(node["name"], "elsewhere", "{node}");
    let report = assert_report_identity(&node, &caller, "main.ts");
    assert_eq!(report["outcome"]["state"], "unavailable", "{report}");
    assert_eq!(
        report["outcome"]["evidence"]["reason_code"],
        "code-graph-unavailable"
    );
    assert!(report["outcome"].get("sites").is_none(), "{report}");
}
