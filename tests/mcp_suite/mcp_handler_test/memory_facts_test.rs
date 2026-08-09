#![cfg(feature = "test-transport")]

use crate::support::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tracedecay::tracedecay::TraceDecay;

/// The fact-store surfaces are daemon-owned application operations. Keep these
/// tests on the production composition so they cannot accidentally exercise
/// the removed direct broad-action handler.
struct FactStoreMcpFixture {
    production: ProductionCompositionFixture,
    server: Arc<tracedecay::mcp::McpServer>,
    graph: Arc<TraceDecay>,
}

impl std::ops::Deref for FactStoreMcpFixture {
    type Target = TraceDecay;

    fn deref(&self) -> &Self::Target {
        self.graph.as_ref()
    }
}

async fn fact_store_mcp_fixture() -> FactStoreMcpFixture {
    let production = production_composition_fixture().await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production fact-store MCP server");
    let graph = server.cg().await;
    FactStoreMcpFixture {
        production,
        server,
        graph,
    }
}

async fn setup_empty_project() -> (FactStoreMcpFixture, (), ()) {
    (fact_store_mcp_fixture().await, (), ())
}

async fn setup_project() -> (FactStoreMcpFixture, ()) {
    (fact_store_mcp_fixture().await, ())
}

/// Invoke an exact MCP operation through the production daemon executor and
/// project its typed operation payload for focused behavioral assertions.
async fn invoke_exact_tool(
    server: &tracedecay::mcp::McpServer,
    tool_name: &str,
    arguments: Value,
) -> tracedecay::errors::Result<tracedecay::mcp::ToolResult> {
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
        .pointer("/result/outcome/value/payload")
        .cloned()
        .unwrap_or(response_value);
    Ok(tracedecay::mcp::ToolResult::new(
        json!({"content": [{"type": "text", "text": payload.to_string()}]}),
        Vec::new(),
    ))
}

async fn invoke_production_tool(
    fixture: &FactStoreMcpFixture,
    tool_name: &str,
    arguments: Value,
    _server_stats: Option<Value>,
    _scope_prefix: Option<&str>,
) -> tracedecay::errors::Result<tracedecay::mcp::ToolResult> {
    invoke_exact_tool(&fixture.server, tool_name, arguments).await
}

async fn close_test_graph(fixture: FactStoreMcpFixture) {
    fixture.production.harness.shutdown().await;
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
    let (cg, _env, _dir) = setup_empty_project().await;
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
                "format": "json",
                "content": content,
                "category": "decision",
                "trust": 0.99,
                "source": "fact-ranking-regression"
            }),
            None,
            None,
        )
        .await
        .unwrap();
        let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
        if *content == exact {
            exact_fact_id = added["fact"]["fact_id"].as_str().map(str::to_owned);
        }
    }
    let exact_fact_id = exact_fact_id.expect("exact operational fact should be stored");

    let first = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_search",
        json!({
            "format": "json",
            "query": "stale tracedecay serve processes versions open database file descriptors doctor upgrade",
            "limit": 10,
            "min_trust": 0.0
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let first: Value = serde_json::from_str(extract_text(&first.value)).unwrap();
    let first_results = first["facts"].as_array().expect("fact search results");
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
            "format": "json",
            "memory_limit": 10,
            "memory_min_trust": 0.0
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let context: Value = serde_json::from_str(extract_text(&context.value)).unwrap();
    assert!(context["memory_matches"].as_array().is_some_and(|matches| {
        matches
            .iter()
            .any(|hit| hit["fact"]["fact_id"].as_str() == Some(exact_fact_id.as_str()))
    }));

    let rare = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_search",
        json!({
            "format": "json",
            "query": "22 long-lived 0.0.38 0.0.47 four 0.0.45",
            "limit": 10,
            "min_trust": 0.0
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let rare: Value = serde_json::from_str(extract_text(&rare.value)).unwrap();
    let rare_results = rare["facts"].as_array().expect("rare-term results");
    assert_eq!(
        rare_results.len(),
        1,
        "rare terms should exclude unrelated facts: {rare}"
    );
    assert_eq!(
        rare_results[0]["fact"]["fact_id"].as_str(),
        Some(exact_fact_id.as_str())
    );
    assert!(rare_results[0]["fts_score"].as_f64().unwrap_or_default() > 0.0);

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
async fn memory_fact_store_add_search_update_remove_and_wrappers() {
    let (cg, _dir) = setup_project().await;

    let added = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "format": "json",
            "content": "Project Phoenix uses Amari Memory in src/memory/types.rs",
            "category": "project",
            "entity": "Project Phoenix",
            "entities": ["Amari Memory"],
            "tags": ["memory", "holographic"],
            "source": "mcp-test",
            "metadata": {"plan": "holographic"}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"]
        .as_str()
        .expect("fact_store_add should return a canonical fact id")
        .to_owned();
    assert!(fact_id.starts_with("fact.v1."));
    assert!(added["fact"].get("id").is_none());
    assert!(added["fact"].get("trust").is_none());
    assert!(added["fact"]["trust_score"].as_f64().is_some());
    assert_eq!(added["fact"]["category"], "project");
    assert_eq!(added["fact"]["source"], "mcp-test");
    let added_mutation = &added["mutation"];
    assert!(added_mutation["operation_id"].as_str().is_some());
    assert_eq!(
        added_mutation["input_digest"].as_str().map(str::len),
        Some(64)
    );
    assert_eq!(added_mutation["commit"]["disposition"], "committed");
    assert!(added_mutation["expected_last_event_id"].is_null());
    assert!(added_mutation["commit"]["expected_last_event_id"].is_null());
    assert_eq!(
        added_mutation["committed_generation"],
        added_mutation["commit"]["last_event_id"]
    );
    assert_eq!(added_mutation["replayed"], false);
    let added_generation = added_mutation["committed_generation"].clone();

    let search = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_search",
        json!({
            "format": "json",
            "query": "Amari Memory",
            "category": "project",
            "min_trust": 0.1,
            "limit": 5
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let search: Value = serde_json::from_str(extract_text(&search.value)).unwrap();
    assert_eq!(search["count"].as_u64(), Some(1));
    assert_eq!(search["results"], search["facts"]);
    assert!(
        search["facts"]
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
            json!({"entity": "Project Phoenix", "format": "json"}),
        ),
        (
            "tracedecay_fact_store_related",
            "related",
            json!({"entity": "Amari Memory", "format": "json"}),
        ),
        (
            "tracedecay_fact_store_reason",
            "reason",
            json!({"entities": ["Project Phoenix", "Amari Memory"], "format": "json"}),
        ),
        (
            "tracedecay_fact_store_contradict",
            "contradict",
            json!({"category": "project", "threshold": 0.8, "format": "json"}),
        ),
        (
            "tracedecay_fact_store_list",
            "list",
            json!({"category": "project", "min_trust": 0.1, "format": "json"}),
        ),
    ] {
        let result = invoke_production_tool(&cg, tool_name, args, None, None)
            .await
            .unwrap();
        let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
        assert!(
            output["results"].is_array(),
            "{label} should include results array: {output}"
        );
        assert!(
            output["count"].is_number(),
            "{label} should include count: {output}"
        );
        if label == "related" {
            assert!(
                output["count"].as_u64().unwrap_or_default() > 0,
                "related should return facts connected through adjacent entities: {output}"
            );
        }
    }

    let updated = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_update",
        json!({
            "format": "json",
            "fact_id": fact_id.clone(),
            "content": "Project Phoenix uses deterministic Amari Memory",
            "entities": ["Project Phoenix", "Amari Memory"],
            "metadata": {"updated": true}
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let updated: Value = serde_json::from_str(extract_text(&updated.value)).unwrap();
    assert_eq!(
        updated["fact"]["content"],
        "Project Phoenix uses deterministic Amari Memory"
    );
    assert_eq!(updated["count"].as_u64(), Some(1));
    assert_eq!(
        updated["mutation"]["expected_last_event_id"],
        added_generation
    );
    assert_eq!(
        updated["mutation"]["commit"]["expected_last_event_id"],
        added_generation
    );
    assert_eq!(
        updated["mutation"]["committed_generation"],
        updated["mutation"]["commit"]["last_event_id"]
    );
    let updated_generation = updated["mutation"]["committed_generation"].clone();

    let removed = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_remove",
        json!({"format": "json", "fact_id": fact_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let removed: Value = serde_json::from_str(extract_text(&removed.value)).unwrap();
    assert_eq!(removed["removed"], true);
    assert_eq!(
        removed["mutation"]["expected_last_event_id"],
        updated_generation
    );
    assert_eq!(
        removed["mutation"]["commit"]["expected_last_event_id"],
        updated_generation
    );
    assert_eq!(
        removed["mutation"]["committed_generation"],
        removed["mutation"]["commit"]["last_event_id"]
    );
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
            "format": "json",
            "content": "Target selector fact stays with the registered target project",
            "category": "project",
            "entity": "Target selector"
        }),
    )
    .await
    .unwrap();
    let target_added = extract_json(&target_added.value);
    let target_fact_id = target_added["fact"]["fact_id"]
        .as_str()
        .expect("target add should return a canonical fact id")
        .to_owned();

    invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_add",
        json!({
            "format": "json",
            "content": "Active selector fact stays with the active project",
            "category": "project",
            "entity": "Active selector"
        }),
    )
    .await
    .unwrap();

    let target_list = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_list",
        json!({
            "format": "json",
            "project_path": target_project_path.clone(),
            "category": "project",
            "min_trust": 0.0
        }),
    )
    .await
    .unwrap();
    let target_list = extract_json(&target_list.value);
    assert_fact_results(
        &target_list,
        "Target selector fact",
        "Active selector fact",
        "project_path selector should read target-project facts",
    );

    let target_list_by_nested_project_path = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_list",
        json!({
            "format": "json",
            "project_selector": {"project_path": target_project_path.clone()},
            "category": "project",
            "min_trust": 0.0
        }),
    )
    .await
    .unwrap();
    let target_list_by_nested_project_path =
        extract_json(&target_list_by_nested_project_path.value);
    assert_fact_results(
        &target_list_by_nested_project_path,
        "Target selector fact",
        "Active selector fact",
        "nested project_path selector should read target-project facts",
    );

    let active_list = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_list",
        json!({"format": "json", "category": "project", "min_trust": 0.0}),
    )
    .await
    .unwrap();
    let active_list = extract_json(&active_list.value);
    assert_fact_results(
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
            "project_id": "proj_does_not_exist",
            "category": "project",
            "min_trust": 0.0
        }),
    )
    .await;
    assert!(
        typo_selector.is_err(),
        "an unresolved explicit selector must not fall back to the active project"
    );

    let hidden_top_level_path = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_fact_store_list",
        json!({
            "format": "json",
            "path": target_project_path,
            "category": "project",
            "min_trust": 0.0
        }),
    )
    .await;
    assert!(
        hidden_top_level_path.is_err(),
        "an undocumented top-level path must be rejected by the exact list schema"
    );

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
    let target_project_path = fixture.target_root.to_string_lossy().to_string();

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
    let active_status = extract_json(&active_status.value);
    assert_eq!(active_status["status"], "ok");
    assert_eq!(active_status["memory"]["fact_count"].as_u64(), Some(2));

    let target_status_by_id = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_memory_status",
        json!({"project_id": target_project_id}),
    )
    .await
    .unwrap();
    let target_status_by_id = extract_json(&target_status_by_id.value);
    assert_eq!(target_status_by_id["status"], "ok");
    assert_eq!(
        target_status_by_id["memory"]["fact_count"].as_u64(),
        Some(1),
        "project_id selector should report the target project's memory: {target_status_by_id}"
    );

    let target_status_by_path = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_memory_status",
        json!({"project_selector": {"path": target_project_path}}),
    )
    .await
    .unwrap();
    let target_status_by_path = extract_json(&target_status_by_path.value);
    assert_eq!(
        target_status_by_path["memory"]["fact_count"].as_u64(),
        Some(1),
        "nested path selector should report the target project's memory: {target_status_by_path}"
    );

    let missing_status = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_memory_status",
        json!({"project_id": "proj_does_not_exist"}),
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
            "format": "json",
            "min_trust": 0.0,
            "memory_scope": "user"
        }),
    )
    .await
    .unwrap();
    let project_facts = extract_json(&project_facts.value).to_string();
    let user_facts = extract_json(&user_facts.value).to_string();
    assert!(project_facts.contains("Project-only routing decision"));
    assert!(!project_facts.contains("User prefers concise technical answers"));
    assert!(user_facts.contains("User prefers concise technical answers"));
    assert!(!user_facts.contains("Project-only routing decision"));

    let user_status = invoke_exact_tool(
        &fixture.active_server,
        "tracedecay_memory_status",
        json!({"format": "json", "memory_scope": "user"}),
    )
    .await
    .unwrap();
    let user_status = extract_json(&user_status.value);
    assert_eq!(user_status["status"], "ok");
    assert_eq!(user_status["memory"]["fact_count"].as_u64(), Some(1));

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn memory_fact_store_update_rejects_secret_like_content_without_mutating_fact() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let added = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "format": "json",
            "content": "Project preference: never store provider API keys",
            "category": "project"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"]
        .as_str()
        .expect("fact-store add should return a canonical fact id")
        .to_owned();

    let rejected = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_update",
        json!({
            "format": "json",
            "fact_id": fact_id.clone(),
            "content": "api_key=sk-test-742913 must not be persisted"
        }),
        None,
        None,
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
        None,
        None,
    )
    .await
    .unwrap();
    let stored: Value = serde_json::from_str(extract_text(&stored.value)).unwrap();
    assert_eq!(
        stored["fact"]["content"],
        "Project preference: never store provider API keys"
    );
    assert!(
        !stored["fact"]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("sk-test-742913")
    );
}

#[tokio::test]
async fn memory_recall_updates_retrieval_count() {
    let (cg, _dir) = setup_project().await;
    let added = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "format": "json",
            "content": "Retrieval counters move after search",
            "entity": "Counter Entity"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"]
        .as_str()
        .expect("fact-store add should return a canonical fact id")
        .to_owned();

    invoke_production_tool(
        &cg,
        "tracedecay_fact_store_search",
        json!({"format": "json", "query": "Retrieval counters", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();

    let status = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_list",
        json!({"format": "json", "min_trust": 0.0, "limit": 10}),
        None,
        None,
    )
    .await
    .unwrap();
    let status: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    let fact = status["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|fact| fact["fact_id"].as_str() == Some(fact_id.as_str()))
        .unwrap();
    assert!(
        fact["retrieval_count"].as_i64().unwrap_or_default() > 0,
        "returned facts should increment retrieval_count: {status}"
    );
}

#[tokio::test]
async fn memory_feedback_and_status_include_trust_fields() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let added = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "format": "json",
            "content": "Helpful memory fact for feedback",
            "category": "general"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"]
        .as_str()
        .expect("fact-store add should return a canonical fact id")
        .to_owned();
    assert!(added["fact"].get("id").is_none());
    assert!(added["fact"].get("trust").is_none());
    assert!(added["fact"]["trust_score"].as_f64().is_some());

    let helpful = invoke_production_tool(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": fact_id.clone(), "format": "json", "helpful": true, "source": "mcp-test", "note": "matched"}),
        None,
        None,
    )
    .await
    .unwrap();
    let helpful: Value = serde_json::from_str(extract_text(&helpful.value)).unwrap();
    assert!(helpful["feedback"]["event_id"].as_i64().unwrap() > 0);
    assert_eq!(helpful["feedback"]["fact_id"], fact_id);
    assert_eq!(helpful["feedback"]["action"], "helpful");
    assert_eq!(helpful["feedback"]["old_trust"], 0.5);
    assert!(helpful["feedback"]["new_trust"].as_f64().unwrap() > 0.5);
    assert!(helpful["feedback"]["trust_delta"].as_f64().unwrap() > 0.0);
    assert_eq!(helpful["feedback"]["helpful_count"], 1);
    assert_eq!(helpful["feedback"]["unhelpful_count"], 0);

    let unhelpful = invoke_production_tool(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": fact_id.clone(), "format": "json", "unhelpful": true}),
        None,
        None,
    )
    .await
    .unwrap();
    let unhelpful: Value = serde_json::from_str(extract_text(&unhelpful.value)).unwrap();
    assert_eq!(unhelpful["feedback"]["action"], "unhelpful");
    assert!(
        unhelpful["feedback"]["new_trust"].as_f64().unwrap()
            < helpful["feedback"]["new_trust"].as_f64().unwrap()
    );
    assert_eq!(unhelpful["feedback"]["helpful_count"], 1);
    assert_eq!(unhelpful["feedback"]["unhelpful_count"], 1);

    let fetched = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_get",
        json!({"format": "json", "fact_id": fact_id.clone()}),
        None,
        None,
    )
    .await
    .unwrap();
    let fetched: Value = serde_json::from_str(extract_text(&fetched.value)).unwrap();
    assert_eq!(fetched["fact"]["fact_id"], fact_id);
    let trust_history = fetched["trust_history"]
        .as_array()
        .unwrap_or_else(|| panic!("expected trust_history array: {fetched}"));
    assert_eq!(trust_history.len(), 2);
    assert_eq!(trust_history[0]["action"], "helpful");
    assert_eq!(trust_history[0]["note"], "matched");
    assert_eq!(trust_history[1]["action"], "unhelpful");
    assert!(trust_history[1]["note"].is_null());

    let status = invoke_production_tool(&cg, "tracedecay_memory_status", json!({}), None, None)
        .await
        .unwrap();
    let status: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status["status"], "ok");
    assert!(status["memory"]["fact_count"].as_u64().unwrap() >= 1);
    assert!(status["memory"].get("trust_0_025_count").is_some());
    assert!(status["memory"].get("trust_025_050_count").is_some());
    assert!(status["memory"].get("trust_050_075_count").is_some());
    assert!(status["memory"].get("trust_075_100_count").is_some());
    assert!(status["memory"].get("helpful_count").is_some());
    assert!(status["memory"].get("unhelpful_count").is_some());
}

#[tokio::test]
async fn memory_tools_validate_malformed_inputs() {
    let (cg, _env, _dir) = setup_empty_project().await;

    let bad_category = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_list",
        json!({"category": "definitely-not-a-category"}),
        None,
        None,
    )
    .await;
    assert!(
        bad_category.is_err(),
        "the exact list schema must reject an unknown category"
    );

    let added = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({"content": "Feedback action validation needs a canonical fact id"}),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"]
        .as_str()
        .expect("fact-store add should return a canonical fact id")
        .to_owned();

    let missing_feedback_action = invoke_production_tool(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": fact_id}),
        None,
        None,
    )
    .await;
    assert!(
        missing_feedback_action.is_err(),
        "fact feedback must require its declared action"
    );
}

/// Status reports the canonical similarity projection shape through the
/// production memory authority; vector-bank repair is not an MCP concern.
#[tokio::test]
async fn memory_status_reports_canonical_similarity_projection_shape() {
    let (cg, _env, _dir) = setup_empty_project().await;
    invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "format": "json",
            "content": "Status should report repair state without repairing it",
            "category": "project",
            "entity": "Holographic Banks"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let status = invoke_production_tool(&cg, "tracedecay_memory_status", json!({}), None, None)
        .await
        .unwrap();
    let status: Value = serde_json::from_str(extract_text(&status.value)).unwrap();
    assert_eq!(status["status"], "ok");
    let fact_count = status["memory"]["fact_count"]
        .as_u64()
        .expect("status must expose the canonical fact count");
    assert_eq!(
        fact_count, 1,
        "status must include the stored fact: {status}"
    );
    assert_eq!(
        status["memory"]["algebra_name"], "amari_fhrr",
        "status must name the canonical similarity algebra: {status}"
    );
    assert_eq!(status["memory"]["hrr_dim"].as_u64(), Some(2048));
    assert_eq!(
        status["memory"]["estimated_capacity"].as_u64(),
        Some(fact_count * 2048),
        "capacity must derive from canonical fact count and dimension: {status}"
    );
    for retired_field in ["missing_vector_count", "bank_count", "repair"] {
        assert!(
            status["memory"].get(retired_field).is_none(),
            "status must not expose retired vector-bank state `{retired_field}`: {status}"
        );
    }
}

#[tokio::test]
async fn fact_store_reason_requires_an_entity_selection() {
    let (cg, _dir) = setup_project().await;

    let result = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_reason",
        json!({"format": "json"}),
        None,
        None,
    )
    .await;
    assert!(
        result.is_err(),
        "the exact reason route must reject an empty entity selection"
    );
}

#[tokio::test]
async fn fact_store_add_rejects_out_of_range_trust() {
    let (cg, _dir) = setup_project().await;

    let result = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({
            "format": "json",
            "content": "Trust out of range must be rejected with an actionable message",
            "category": "project",
            "trust": 1.5
        }),
        None,
        None,
    )
    .await;
    assert!(
        result.is_err(),
        "the exact add route must reject a trust value outside its schema range"
    );
}

#[tokio::test]
async fn fact_feedback_on_nonexistent_fact_id_fails_fast() {
    let (cg, _dir) = setup_project().await;

    let added = invoke_production_tool(
        &cg,
        "tracedecay_fact_store_add",
        json!({"content": "A removed fact must reject later feedback"}),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"]
        .as_str()
        .expect("fact-store add should return a canonical fact id")
        .to_owned();
    invoke_production_tool(
        &cg,
        "tracedecay_fact_store_remove",
        json!({"fact_id": fact_id.clone()}),
        None,
        None,
    )
    .await
    .unwrap();

    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        invoke_production_tool(
            &cg,
            "tracedecay_fact_feedback",
            json!({"fact_id": fact_id, "action": "helpful", "format": "json"}),
            None,
            None,
        ),
    )
    .await
    .expect("fact_feedback on a nonexistent fact must not hang until a client deadline");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "fact_feedback on a nonexistent fact must fail fast like fact_store_get: {:?}",
        started.elapsed()
    );
    assert!(
        result.is_err(),
        "fact_feedback must reject a nonexistent fact identifier"
    );
}
