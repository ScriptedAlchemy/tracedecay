#![cfg(feature = "test-transport")]

use crate::support::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use super::memory_fact_assertions::assert_fact_list;

/// The fact-store surfaces are daemon-owned application operations. Keep these
/// tests on the production composition so they cannot accidentally exercise
/// the removed direct broad-action handler.
pub(super) struct FactStoreMcpFixture {
    production: ProductionCompositionFixture,
    server: Arc<tracedecay::mcp::McpServer>,
}

async fn fact_store_mcp_fixture() -> FactStoreMcpFixture {
    let production = production_composition_fixture().await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production fact-store MCP server");
    FactStoreMcpFixture { production, server }
}

pub(super) async fn setup_project() -> FactStoreMcpFixture {
    fact_store_mcp_fixture().await
}

/// Invoke an exact MCP operation through the production daemon executor and
/// project its typed operation payload for focused behavioral assertions.
async fn invoke_exact_tool(
    server: &tracedecay::mcp::McpServer,
    tool_name: &str,
    mut arguments: Value,
) -> tracedecay::errors::Result<Value> {
    arguments
        .as_object_mut()
        .expect("exact MCP request object")
        .insert("format".to_owned(), json!("json"));
    let response = handle_real_server_tool_call_raw(server, tool_name, arguments).await;
    if !response["error"].is_null() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: response["error"].to_string(),
        });
    }
    let mcp_result = response["result"].clone();
    let text = extract_real_server_text(&mcp_result);
    let response_value: Value = serde_json::from_str(text).map_err(|error| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!("{tool_name} returned invalid application JSON: {error}"),
        }
    })?;
    if mcp_result.get("isError").and_then(Value::as_bool) == Some(true) {
        let message = response_value
            .pointer("/result/message")
            .and_then(Value::as_str)
            .unwrap_or(text)
            .to_owned();
        return Err(tracedecay::errors::TraceDecayError::Config { message });
    }
    let payload = response_value
        .pointer("/outcome/value/payload")
        .cloned()
        .ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
            message: format!("{tool_name} omitted its canonical application payload"),
        })?;
    Ok(payload)
}

pub(super) async fn invoke_production_tool(
    fixture: &FactStoreMcpFixture,
    tool_name: &str,
    arguments: Value,
) -> tracedecay::errors::Result<Value> {
    invoke_exact_tool(&fixture.server, tool_name, arguments).await
}

pub(super) async fn close_test_graph(fixture: FactStoreMcpFixture) {
    fixture.production.harness.shutdown().await;
}

fn available_fact(projection: &Value) -> &Value {
    assert_eq!(projection["kind"], "available");
    projection
        .get("fact")
        .expect("available projection must contain a fact")
}

fn committed_add_result(payload: &Value) -> &Value {
    assert_eq!(payload["outcome"], "committed");
    let result = payload
        .get("result")
        .expect("committed add must contain its canonical result");
    assert_eq!(result["disposition"], "added");
    result
}

struct FactStoreCrossProjectFixture {
    harness: tracedecay::daemon::ProductionProjectCompositionHarnessV1,
    target_root: std::path::PathBuf,
    active_server: Arc<tracedecay::mcp::McpServer>,
    target_server: Arc<tracedecay::mcp::McpServer>,
    _isolation: TestTempDir,
}

fn initialize_production_fact_project(root: &Path) {
    fs::create_dir_all(root).expect("cross-project fact fixture root");
    crate::fixture::write_indexed_fixture_sources(root);
    let init = Command::new(crate::common::git_program())
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .expect("initialize cross-project fact fixture");
    assert!(init.success(), "git init should succeed");
    let add = Command::new(crate::common::git_program())
        .args(["add", "."])
        .current_dir(root)
        .status()
        .expect("stage cross-project fact fixture");
    assert!(add.success(), "git add should succeed");
    let commit = Command::new(crate::common::git_program())
        .args([
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "-qm",
            "production fact-store fixture",
        ])
        .current_dir(root)
        .status()
        .expect("commit cross-project fact fixture");
    assert!(commit.success(), "git commit should succeed");
}

async fn fact_store_cross_project_fixture() -> FactStoreCrossProjectFixture {
    let isolation = test_temp_dir();
    let active_root = isolation.path().join("active");
    let target_root = isolation.path().join("target");
    initialize_production_fact_project(&active_root);
    initialize_production_fact_project(&target_root);
    let harness = tracedecay::daemon::ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        vec![active_root.clone(), target_root.clone()],
    )
    .await
    .expect("production cross-project fact fixture");
    let active_server = harness
        .server(&active_root)
        .expect("active production fact server");
    let target_server = harness
        .server(&target_root)
        .expect("target production fact server");
    FactStoreCrossProjectFixture {
        harness,
        target_root,
        active_server,
        target_server,
        _isolation: isolation,
    }
}

#[tokio::test]
async fn fact_search_ranks_exact_operational_evidence_and_tracks_once() {
    let cg = setup_project().await;
    let exact = "22 long-lived tracedecay serve processes spanning 0.0.38 through 0.0.47; four 0.0.45 processes hold selected tracedecay.db file descriptors; doctor/upgrade should report stale PIDs/versions/open holders, never kill.";
    let unrelated = [
        "TraceDecay V2 multi-agent task execution spans several repositories and decomposes into independently claimable task subgraphs with versioned compact context packets.",
        "TraceDecay V2 task-graph scoping uses one profile-owned canonical task graph with Kanban, DAG, timeline, workload, initiative, and saved-query projections.",
        "TraceDecay V2 task execution relates tickets to threads, sessions, turns, agents, tool calls, files, symbols, worktrees, commits, pull requests, and evidence.",
        "TraceDecay V2 may run a daemon-side context scout that observes bounded turn events and emits compact relevance-scored suggestion envelopes.",
        "TraceDecay V2 session and LCM retrieval distinguishes current truth from historical evidence and ranks explicit scope, thread, project, worktree, trust, and current-state signals.",
    ];

    let mut contents = vec![exact];
    contents.extend(unrelated);
    let mut exact_fact_id = None;
    for content in &contents {
        let added = invoke_production_tool(
            &cg,
            "tracedecay_fact_store_add",
            json!({
                "content": content,
                "category": "decision",
                "trust": 0.99,
                "source_label": "fact-ranking-regression"
            }),
        )
        .await
        .unwrap();
        if *content == exact {
            exact_fact_id = available_fact(&committed_add_result(&added)["fact"])["fact_id"]
                .as_str()
                .map(str::to_owned);
        }
    }
    let exact_fact_id = exact_fact_id.expect("exact operational fact should be stored");

    let first = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_search",
        json!({
            "query": "stale tracedecay serve processes versions open database file descriptors doctor upgrade",
            "limit": 10,
            "min_trust": 0.0
        })
    )
    .await
    .unwrap();
    let first_results = first["hits"].as_array().expect("fact search hits");
    assert_eq!(
        first_results[0]["fact"]["fact_id"].as_str(),
        Some(exact_fact_id.as_str()),
        "exact operational evidence must outrank unrelated V2 facts: {first}"
    );

    let context = invoke_production_tool(
        &cg,
        "tracedecay_context",
        json!({
            "task": "stale tracedecay serve processes versions open database file descriptors doctor upgrade",
            "memory_limit": 10,
            "memory_min_trust": 0.0
        })
    )
    .await
    .unwrap();
    assert!(context["memory_matches"].as_array().is_some_and(|matches| {
        matches
            .iter()
            .any(|hit| hit["fact"]["fact_id"].as_str() == Some(exact_fact_id.as_str()))
    }));

    let rare = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_search",
        json!({
            "query": "22 long-lived 0.0.38 0.0.47 four 0.0.45",
            "limit": 10,
            "min_trust": 0.0
        }),
    )
    .await
    .unwrap();
    let rare_results = rare["hits"].as_array().expect("rare-term hits");
    assert_eq!(
        rare_results.len(),
        1,
        "rare terms should exclude unrelated facts: {rare}"
    );
    assert_eq!(
        rare_results[0]["fact"]["fact_id"].as_str(),
        Some(exact_fact_id.as_str())
    );
    assert!(
        rare_results[0]["scores"]["fts_score_millionths"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );

    let analytics = handle_real_server_tool_call(
        &cg.server,
        "tracedecay_analytics",
        json!({"section": "facts", "format": "json"}),
    )
    .await;
    let analytics: Value = serde_json::from_str(extract_real_server_text(&analytics)).unwrap();
    assert_eq!(
        analytics["facts"]["facts"].as_i64(),
        Some(contents.len() as i64)
    );
    assert_eq!(
        analytics["facts"]["retrievals"].as_i64(),
        Some(first_results.len() as i64 + rare_results.len() as i64)
    );
    // Every fact the two searches returned must be counted exactly once, so
    // the distinct-fact tally is the size of the returned id set — not the
    // number of stored facts, which would also assert how many weak matches
    // the ranker chooses to return.
    let retrieved_ids: BTreeSet<String> = first_results
        .iter()
        .chain(rare_results.iter())
        .filter_map(|hit| hit["fact"]["fact_id"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        analytics["facts"]["facts_retrieved"].as_i64(),
        Some(retrieved_ids.len() as i64),
        "analytics must count each retrieved fact once: {analytics}"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn memory_fact_store_add_search_update_and_remove() {
    let cg = setup_project().await;

    let added = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "content": "Project Phoenix uses Amari Memory in src/memory/types.rs",
            "category": "project",
            "entities": ["Amari Memory", "Project Phoenix"],
            "tags": ["memory", "holographic"],
            "source_label": "mcp-test",
            "metadata": {"plan": "holographic"}
        }),
    )
    .await
    .unwrap();
    let added_result = committed_add_result(&added);
    assert_eq!(added_result["disposition"], "added");
    let added_fact = available_fact(&added_result["fact"]);
    let fact_id = added_fact["fact_id"]
        .as_str()
        .expect("fact_store_add should return a canonical fact id")
        .to_owned();
    assert!(fact_id.starts_with("fact.v1."));
    assert!(added_fact.get("id").is_none());
    assert!(added_fact.get("trust").is_none());
    assert!(added_fact["trust_score_millionths"].as_u64().is_some());
    assert_eq!(added_fact["category"], "project");
    assert_eq!(added_fact["source_label"], "mcp-test");
    assert_eq!(added_fact["source"]["kind"], "application");
    assert!(added_fact["source"]["operation_id"].as_str().is_some());
    assert_eq!(added_result["commit"]["disposition"], "committed");
    assert_eq!(added_result["commit"]["fact_id"], fact_id);
    let added_generation = added_result["commit"]["last_event_id"].clone();

    let search = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_search",
        json!({
            "query": "Amari Memory",
            "category": "project",
            "min_trust": 0.1,
            "limit": 5
        }),
    )
    .await
    .unwrap();
    assert!(
        search["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["fact"]["fact_id"].as_str() == Some(fact_id.as_str())),
        "search results should include added fact: {search}"
    );

    for (tool_name, label, args) in [
        (
            "tracedecay_fact_store_probe",
            "probe",
            json!({"entity": "Project Phoenix"}),
        ),
        (
            "tracedecay_fact_store_related",
            "related",
            json!({"entity": "Amari Memory"}),
        ),
        (
            "tracedecay_fact_store_reason",
            "reason",
            json!({"entities": ["Amari Memory", "Project Phoenix"]}),
        ),
        (
            "tracedecay_fact_store_contradict",
            "contradict",
            json!({"category": "project", "threshold_millionths": 800_000}),
        ),
        (
            "tracedecay_fact_store_list",
            "list",
            json!({"category": "project", "min_trust": 0.1}),
        ),
    ] {
        let result = invoke_production_tool(&cg, tool_name, args).await.unwrap();
        let output = result;
        let result_key = match label {
            "probe" | "related" | "reason" => "hits",
            "contradict" => "contradictions",
            "list" => "facts",
            _ => unreachable!("closed exact fact read set"),
        };
        let results = output[result_key]
            .as_array()
            .unwrap_or_else(|| panic!("{label} should include {result_key}: {output}"));
        if label == "related" {
            assert!(
                !results.is_empty(),
                "related should return facts connected through adjacent entities: {output}"
            );
        }
    }

    let updated = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_update",
        json!({
            "fact_id": fact_id.clone(),
            "expected_last_event_id": added_generation,
            "content": "Project Phoenix uses deterministic Amari Memory",
            "entities": ["Amari Memory", "Project Phoenix"],
            "metadata": {"updated": true}
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        available_fact(&updated["fact"])["content"],
        "Project Phoenix uses deterministic Amari Memory"
    );
    assert_eq!(updated["commit"]["fact_id"], fact_id);
    let updated_generation = updated["commit"]["last_event_id"].clone();

    let removed = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_remove",
        json!({
            "fact_id": fact_id.clone(),
            "expected_last_event_id": updated_generation
        }),
    )
    .await
    .unwrap();
    assert_eq!(removed["outcome"], "removed");
    assert_eq!(removed["commit"]["fact_id"], fact_id);
    assert_eq!(removed["fact"]["kind"], "unavailable");
    assert_eq!(removed["fact"]["status"]["fact_id"], fact_id);
    assert_eq!(removed["fact"]["status"]["payload_access"], "deleted");
    close_test_graph(cg).await;
}

#[tokio::test]
async fn memory_fact_store_project_selector_targets_registered_project() {
    let fixture = fact_store_cross_project_fixture().await;
    let target_graph = fixture.target_server.cg().await;
    let target_project_id = target_graph
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("target project should have a profile project_id")
        .to_owned();
    let target_project_path = fixture.target_root.to_string_lossy().to_string();

    let target_added = invoke_exact_tool(
        &fixture.target_server,
        "tracedecay_fact_store_add",
        json!({
            "content": "Target selector fact stays with the registered target project",
            "category": "project",
            "entities": ["Target selector"]
        }),
    )
    .await
    .unwrap();
    let target_fact_id = available_fact(&committed_add_result(&target_added)["fact"])["fact_id"]
        .as_str()
        .expect("target add should return a canonical fact id")
        .to_owned();

    invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_add",
        json!({
            "content": "Active selector fact stays with the active project",
            "category": "project",
            "entities": ["Active selector"]
        }),
    )
    .await
    .unwrap();

    let target_list = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_list",
        json!({
            "project_selector": {"project_id": target_project_id.clone()},
            "category": "project",
            "min_trust": 0.0
        }),
    )
    .await
    .unwrap();
    assert_fact_list(
        &target_list,
        "Target selector fact",
        "Active selector fact",
        "canonical project selector should read target-project facts",
    );

    let active_list = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_list",
        json!({"category": "project", "min_trust": 0.0}),
    )
    .await
    .unwrap();
    assert_fact_list(
        &active_list,
        "Active selector fact",
        "Target selector fact",
        "default exact fact-tool scope should remain the active project",
    );

    let cross_project_write = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_add",
        json!({
            "project_selector": {"project_id": target_project_id.clone()},
            "content": "Cross-project writes should be rejected",
            "category": "project"
        }),
    )
    .await;
    assert!(
        cross_project_write.is_err(),
        "the exact add route must reject cross-project writes"
    );

    let cross_project_feedback = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": target_fact_id,
            "action": "helpful",
            "project_selector": {"project_id": target_project_id.clone()}
        }),
    )
    .await;
    assert!(
        cross_project_feedback.is_err(),
        "fact feedback must reject cross-project selectors"
    );

    let typo_selector = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_list",
        json!({
            "project_selector": {"project_id": "project.missing"},
            "category": "project",
            "min_trust": 0.0
        }),
    )
    .await;
    assert!(
        typo_selector.is_err(),
        "an unresolved explicit selector must not fall back to the active project"
    );

    for legacy_selector in [
        json!({"format": "json", "project_id": target_project_id}),
        json!({"format": "json", "project_path": target_project_path.clone()}),
        json!({"format": "json", "project_root": target_project_path.clone()}),
        json!({"format": "json", "project_selector": {"path": target_project_path.clone()}}),
        json!({"format": "json", "project_selector": {"project_path": target_project_path}}),
    ] {
        assert!(
            invoke_exact_tool(
                &fixture.active_server,
                "tracedecay_fact_store_list",
                legacy_selector,
            )
            .await
            .is_err(),
            "legacy project selector aliases must be rejected"
        );
    }

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn memory_status_project_selector_reports_registered_project_memory() {
    let fixture = fact_store_cross_project_fixture().await;
    let target_graph = fixture.target_server.cg().await;
    let target_project_id = target_graph
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("target project should have a profile project_id")
        .to_owned();

    for content in ["Active status fact one", "Active status fact two"] {
        invoke_exact_tool(
            &fixture.active_server,
            "tracedecay_fact_store_add",
            json!({
                "content": content,
                "category": "project"
            }),
        )
        .await
        .unwrap();
    }

    invoke_exact_tool(
        &fixture.target_server,
        "tracedecay_fact_store_add",
        json!({
            "content": "Target status fact",
            "category": "project"
        }),
    )
    .await
    .unwrap();

    let active_status = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_memory_status",
        json!({}),
    )
    .await
    .unwrap();
    assert!(active_status.get("status").is_none());
    assert_eq!(active_status["memory"]["fact_count"].as_u64(), Some(2));

    let target_status_by_id = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_memory_status",
        json!({
            "project_selector": {"project_id": target_project_id}
        }),
    )
    .await
    .unwrap();
    assert!(target_status_by_id.get("status").is_none());
    assert_eq!(
        target_status_by_id["memory"]["fact_count"].as_u64(),
        Some(1),
        "project_id selector should report the target project's memory: {target_status_by_id}"
    );

    let missing_status = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_memory_status",
        json!({
            "project_selector": {"project_id": "project.missing"}
        }),
    )
    .await;
    assert!(
        missing_status.is_err(),
        "an unresolved memory-status selector must not fall back to the active project"
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn user_memory_scope_is_profile_level_and_isolated_from_project_memory() {
    let fixture = fact_store_cross_project_fixture().await;

    invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_add",
        json!({
            "content": "Project-only routing decision",
            "category": "project"
        }),
    )
    .await
    .unwrap();
    invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_add",
        json!({
            "content": "User prefers concise technical answers",
            "category": "user_pref",
            "memory_scope": "user"
        }),
    )
    .await
    .unwrap();

    let project_facts = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_list",
        json!({"format": "json", "min_trust": 0.0}),
    )
    .await
    .unwrap();
    let user_facts = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_list",
        json!({
            "min_trust": 0.0,
            "memory_scope": "user"
        }),
    )
    .await
    .unwrap();
    assert_fact_list(
        &project_facts,
        "Project-only routing decision",
        "User prefers concise technical answers",
        "project scope",
    );
    assert_fact_list(
        &user_facts,
        "User prefers concise technical answers",
        "Project-only routing decision",
        "user scope",
    );

    let user_status = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_memory_status",
        json!({"format": "json", "memory_scope": "user"}),
    )
    .await
    .unwrap();
    assert!(user_status.get("status").is_none());
    assert_eq!(user_status["memory"]["fact_count"].as_u64(), Some(1));

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn memory_fact_store_update_rejects_secret_like_content_without_mutating_fact() {
    let cg = setup_project().await;
    let added = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "content": "Project preference: never store provider API keys",
            "category": "project"
        }),
    )
    .await
    .unwrap();
    let fact_id = available_fact(&committed_add_result(&added)["fact"])["fact_id"]
        .as_str()
        .expect("fact-store add should return a canonical fact id")
        .to_owned();

    let rejected = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_update",
        json!({
            "fact_id": fact_id.clone(),
            "content": "api_key=sk-test-742913 must not be persisted"
        }),
    )
    .await;
    assert!(
        rejected.is_err(),
        "the exact update route must reject secret-like content"
    );

    let stored = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_get",
        json!({"format": "json", "fact_id": fact_id}),
    )
    .await
    .unwrap();
    let stored_fact = available_fact(&stored["fact"]);
    assert_eq!(
        stored_fact["content"],
        "Project preference: never store provider API keys"
    );
    assert!(
        !stored_fact["content"]
            .as_str()
            .unwrap_or_default()
            .contains("sk-test-742913")
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn memory_recall_updates_retrieval_count() {
    let cg = setup_project().await;
    let added = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "content": "Retrieval counters move after search",
            "entities": ["Counter Entity"]
        }),
    )
    .await
    .unwrap();
    let fact_id = available_fact(&committed_add_result(&added)["fact"])["fact_id"]
        .as_str()
        .expect("fact-store add should return a canonical fact id")
        .to_owned();

    invoke_production_tool(
        &cg,
        "tracedecay_fact_store_search",
        json!({"format": "json", "query": "Retrieval counters", "limit": 5}),
    )
    .await
    .unwrap();

    let status = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_list",
        json!({"format": "json", "min_trust": 0.0, "limit": 10}),
    )
    .await
    .unwrap();
    let fact = status["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(available_fact)
        .find(|fact| fact["fact_id"].as_str() == Some(fact_id.as_str()))
        .unwrap();
    assert!(
        fact["telemetry"]["retrieval_count"]
            .as_u64()
            .unwrap_or_default()
            > 0,
        "returned facts should increment retrieval_count: {status}"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn memory_list_rejects_an_unknown_category() {
    let cg = setup_project().await;

    let bad_category = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_list",
        json!({"category": "definitely-not-a-category"}),
    )
    .await;
    assert!(
        bad_category.is_err(),
        "the exact list schema must reject an unknown category"
    );
    close_test_graph(cg).await;
}

/// Status reports the canonical algebra and counters through the production
/// memory authority.
#[tokio::test]
async fn memory_status_reports_canonical_similarity_projection_shape() {
    let cg = setup_project().await;
    invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "content": "Status should report canonical memory algebra and counters",
            "category": "project",
            "entities": ["Holographic Memory"]
        }),
    )
    .await
    .unwrap();
    let status = invoke_production_tool(&cg, "tracedecay_memory_status", json!({}))
        .await
        .unwrap();
    assert!(status.get("status").is_none());
    let fact_count = status["memory"]["fact_count"]
        .as_u64()
        .expect("status must expose the canonical fact count");
    assert_eq!(
        fact_count, 1,
        "status must include the stored fact: {status}"
    );
    assert_eq!(
        status["memory"]["algebra"]["name"], "amari_fhrr",
        "status must name the canonical similarity algebra: {status}"
    );
    assert_eq!(status["memory"]["algebra"]["hrr_dim"].as_u64(), Some(2048));
    assert!(
        status["memory"]["algebra"]["estimated_capacity"]
            .as_u64()
            .is_some_and(|capacity| capacity > 0)
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn fact_store_reason_requires_an_entity_selection() {
    let cg = setup_project().await;

    let result = invoke_production_tool(&cg, "tracedecay_fact_store_reason", json!({})).await;
    assert!(
        result.is_err(),
        "the exact reason route must reject an empty entity selection"
    );
    close_test_graph(cg).await;
}

#[tokio::test]
async fn fact_store_add_rejects_out_of_range_trust() {
    let cg = setup_project().await;

    let result = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "content": "Trust out of range must be rejected with an actionable message",
            "category": "project",
            "trust": 1.5
        }),
    )
    .await;
    assert!(
        result.is_err(),
        "the exact add route must reject a trust value outside its schema range"
    );
    close_test_graph(cg).await;
}
