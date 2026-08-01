use crate::support::*;
use serde_json::{Value, json};
#[cfg(feature = "test-transport")]
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use tracedecay::storage::resolve_layout_for_current_profile;
use tracedecay::tracedecay::TraceDecay;

#[tokio::test]
async fn fact_store_large_list_response_reports_store_failure_actionably() {
    let (cg, _env, _dir) = setup_empty_project().await;
    for index in 0..4 {
        handle_tool_call(
            &cg,
            "tracedecay_fact_store",
            json!({
                "action": "add",
                "format": "json",
                "content": format!(
                    "STORE_FAILURE_MARKER_{index:02}: {}",
                    "large fact-store response should surface cache failures ".repeat(180)
                ),
                "category": "project",
                "trust": 0.9
            }),
            None,
            None,
        )
        .await
        .unwrap();
    }

    let handle_dir = response_handle_dir(&cg);
    fs::write(&handle_dir, "not-a-directory").unwrap();

    let listed = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "list", "format": "json", "category": "project", "min_trust": 0.0, "limit": 200}),
        None,
        None,
    )
    .await
    .unwrap();
    let envelope: Value = serde_json::from_str(extract_text(&listed.value)).unwrap();

    assert_eq!(envelope["truncated"], true, "{envelope}");
    assert_eq!(envelope["handle_available"], false, "{envelope}");
    assert!(envelope.get("handle").is_none(), "{envelope}");
    assert!(
        envelope["preview"]
            .as_str()
            .unwrap_or_default()
            .contains("STORE_FAILURE_MARKER_")
    );
    assert_eq!(
        envelope["handle_status"]["reason_code"],
        "handle_store_failed"
    );
    assert_eq!(envelope["handle_status"]["retryable"], true);
    assert!(
        envelope["handle_status"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("could not be cached locally")
    );
    assert!(
        envelope["handle_status"]["retry_instruction"]
            .as_str()
            .unwrap_or_default()
            .contains("re-run the original MCP tool")
    );
    fs::remove_file(&handle_dir).unwrap();
    fs::create_dir_all(&handle_dir).unwrap();
    close_test_graph(cg).await;
}

#[cfg(feature = "test-transport")]
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
        let added = handle_tool_call(
            &cg,
            "tracedecay_fact_store",
            json!({
                "action": "add",
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
            exact_fact_id = added["fact"]["fact_id"].as_i64();
        }
    }
    let exact_fact_id = exact_fact_id.expect("exact operational fact should be stored");

    let first = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "search",
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
        first_results[0]["fact"]["fact_id"].as_i64(),
        Some(exact_fact_id),
        "exact operational evidence must outrank unrelated V2 facts: {first}"
    );
    let after_first = cg.get_fact(exact_fact_id).await.unwrap().unwrap();
    assert_eq!(after_first.retrieval_count, 1);
    assert_eq!(after_first.access_count, 1);
    assert!(after_first.last_retrieved_at.is_some());
    assert!(after_first.last_recalled_at.is_some());

    let context = handle_tool_call(
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
            .any(|hit| hit["fact"]["fact_id"].as_i64() == Some(exact_fact_id))
    }));
    let after_context = cg.get_fact(exact_fact_id).await.unwrap().unwrap();
    assert_eq!(after_context.retrieval_count, 1);
    assert_eq!(after_context.access_count, 1);

    let rare = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "search",
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
        rare_results[0]["fact"]["fact_id"].as_i64(),
        Some(exact_fact_id)
    );
    assert!(rare_results[0]["fts_score"].as_f64().unwrap_or_default() > 0.0);
    let after_rare = cg.get_fact(exact_fact_id).await.unwrap().unwrap();
    assert_eq!(after_rare.retrieval_count, 2);
    assert_eq!(after_rare.access_count, 2);

    let server = real_mcp_server(cg).await;
    let analytics = handle_real_server_tool_call(
        &server,
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
    let retrieved_ids: BTreeSet<i64> = first_results
        .iter()
        .chain(rare_results.iter())
        .filter_map(|hit| hit["fact"]["fact_id"].as_i64())
        .collect();
    assert_eq!(
        analytics["facts"]["facts_retrieved"].as_i64(),
        Some(retrieved_ids.len() as i64),
        "analytics must count each retrieved fact once: {analytics}"
    );
}

#[tokio::test]
async fn memory_fact_store_add_search_update_remove_and_wrappers() {
    let (cg, _dir) = setup_project().await;

    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
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
        .as_i64()
        .expect("fact_store add should return numeric id");
    assert!(added["fact"].get("id").is_none());
    assert!(added["fact"].get("trust").is_none());
    assert!(added["fact"]["trust_score"].as_f64().is_some());
    assert_eq!(added["action"], "add");
    assert_eq!(added["fact"]["category"], "project");
    assert_eq!(added["fact"]["source"], "mcp-test");

    let search = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "search",
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
    assert_eq!(search["action"], "search");
    assert_eq!(search["count"].as_u64(), Some(1));
    assert_eq!(search["results"], search["facts"]);
    assert!(
        search["facts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["fact"]["fact_id"].as_i64() == Some(fact_id)),
        "search results should include added fact: {search}"
    );

    for (action, payload) in [
        ("probe", json!({"entity": "Project Phoenix"})),
        ("related", json!({"entity": "Amari Memory"})),
        (
            "reason",
            json!({"entities": ["Project Phoenix", "Amari Memory"]}),
        ),
        (
            "contradict",
            json!({"category": "project", "threshold": 0.8}),
        ),
        ("list", json!({"category": "project", "min_trust": 0.1})),
    ] {
        let mut args = payload;
        args["action"] = json!(action);
        args["format"] = json!("json");
        let result = handle_tool_call(&cg, "tracedecay_fact_store", args, None, None)
            .await
            .unwrap();
        let output: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
        assert_eq!(output["action"], action, "{action} should echo action");
        assert!(
            output["results"].is_array(),
            "{action} should include results array: {output}"
        );
        assert!(
            output["count"].is_number(),
            "{action} should include count: {output}"
        );
        if action == "related" {
            assert!(
                output["count"].as_u64().unwrap_or_default() > 0,
                "related should return facts connected through adjacent entities: {output}"
            );
        }
    }

    let updated = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "update",
            "format": "json",
            "fact_id": fact_id,
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

    let removed = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "remove", "format": "json", "fact_id": fact_id.to_string()}),
        None,
        None,
    )
    .await
    .unwrap();
    let removed: Value = serde_json::from_str(extract_text(&removed.value)).unwrap();
    assert_eq!(removed["removed"], true);
}

#[tokio::test]
async fn memory_fact_store_defaults_to_compact_markdown() {
    let (cg, _dir) = setup_project().await;

    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Project Phoenix uses readable fact rendering",
            "category": "project",
            "entity": "Project Phoenix",
            "entities": ["Fact Renderer"],
            "tags": ["memory"],
            "source": "mcp-test"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added_text = extract_text(&added.value);
    assert!(
        added_text.starts_with("## Fact Store"),
        "unexpected fact_store markdown:\n{added_text}"
    );
    assert!(added_text.contains("**action:** add"));
    assert!(added_text.contains("- #"));
    assert!(added_text.contains("Project Phoenix uses readable fact rendering"));
    assert!(!added_text.contains("| fact_id |"));

    let search = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "search",
            "query": "readable fact rendering",
            "category": "project",
            "min_trust": 0.1,
            "limit": 5
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let search_text = extract_text(&search.value);
    assert!(search_text.starts_with("## Fact Store"));
    assert!(search_text.contains("**action:** search"));
    assert!(search_text.contains("**query:** readable fact rendering"));
    assert!(search_text.contains("**category:** project"));
    assert!(search_text.contains("**count:** 1"));
    assert!(search_text.contains("### Facts"));
    assert!(search_text.contains("score "));
    assert!(search_text.contains("Project Phoenix uses readable fact rendering"));
    assert!(!search_text.contains("|"));
}

#[tokio::test]
async fn memory_fact_store_mutations_refresh_recorded_digest_exports() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let profile_root = tracedecay::storage::default_profile_root().unwrap();
    let prompt_path = cg.project_root().join("CLAUDE.md");
    tracedecay::automation::memory_digest::sync_memory_digest_export(
        &profile_root,
        tracedecay::automation::skill_targets::SkillInstallTarget::Claude,
        &prompt_path,
    )
    .unwrap();
    assert!(
        fs::read_to_string(&prompt_path)
            .unwrap()
            .contains("No durable facts exported yet")
    );

    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "format": "json",
            "content": "Refresh exported digest after MCP fact add",
            "category": "decision",
            "trust": 0.9
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();
    assert!(
        fs::read_to_string(&prompt_path)
            .unwrap()
            .contains("Refresh exported digest after MCP fact add")
    );

    handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "update",
            "format": "json",
            "fact_id": fact_id,
            "content": "Refresh exported digest after MCP fact update"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let rendered = fs::read_to_string(&prompt_path).unwrap();
    assert!(rendered.contains("Refresh exported digest after MCP fact update"));
    assert!(!rendered.contains("Refresh exported digest after MCP fact add"));

    handle_tool_call(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": fact_id, "unhelpful": true}),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        fs::read_to_string(&prompt_path)
            .unwrap()
            .contains("trust 0.80")
    );

    handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "remove", "format": "json", "fact_id": fact_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let rendered = fs::read_to_string(&prompt_path).unwrap();
    assert!(!rendered.contains("Refresh exported digest after MCP fact update"));
    assert!(rendered.contains("No durable facts exported yet"));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn memory_fact_store_project_selector_targets_registered_project() {
    let (active, target, _env) = setup_cross_project_memory_projects().await;
    let active_runtime = active.test_runtime_for_test().expect("active runtime");
    let target_runtime = target.test_runtime_for_test().expect("target runtime");
    let target_project_id = target
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("target project should have a profile project_id");
    let target_project_path = target.project_root().to_string_lossy().to_string();

    handle_tool_call_with_runtime(
        &target,
        &target_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "format": "json",
            "content": "Target selector fact stays with the registered target project",
            "category": "project",
            "entity": "Target selector"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "format": "json",
            "content": "Active selector fact stays with the active project",
            "category": "project",
            "entity": "Active selector"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let target_list = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "list",
            "format": "json",
            "project_path": target_project_path,
            "category": "project",
            "min_trust": 0.0
        }),
        None,
        None,
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

    let target_list_by_nested_project_path = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "list",
            "format": "json",
            "project_selector": {"project_path": target_project_path},
            "category": "project",
            "min_trust": 0.0
        }),
        None,
        None,
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

    let active_list = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({"action": "list", "format": "json", "category": "project", "min_trust": 0.0}),
        None,
        None,
    )
    .await
    .unwrap();
    let active_list = extract_json(&active_list.value);
    assert_fact_results(
        &active_list,
        "Active selector fact",
        "Target selector fact",
        "default fact_store scope should remain the active project",
    );

    let cross_project_write = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "project_selector": {"project_id": target_project_id},
            "content": "Cross-project writes should be rejected",
            "category": "project"
        }),
        None,
        None,
    )
    .await;
    let Err(err) = cross_project_write else {
        panic!("expected cross-project fact_store add to be rejected");
    };
    assert!(
        format!("{err}").contains("cross-project fact_store writes are not supported"),
        "unexpected cross-project write error: {err}"
    );

    let cross_project_feedback = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": 1,
            "action": "helpful",
            "project_selector": {"project_id": target_project_id}
        }),
        None,
        None,
    )
    .await;
    let Err(err) = cross_project_feedback else {
        panic!("expected cross-project fact feedback to be rejected");
    };
    assert!(
        format!("{err}").contains("does not accept project selectors"),
        "unexpected cross-project feedback error: {err}"
    );

    let typo_selector = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "list",
            "project_id": "proj_does_not_exist",
            "category": "project",
            "min_trust": 0.0
        }),
        None,
        None,
    )
    .await;
    let Err(err) = typo_selector else {
        panic!("expected unresolved explicit selector to fail");
    };
    assert!(
        format!("{err}").contains("registered project not found for selector"),
        "unresolved selector must not fall back to active project: {err}"
    );

    let hidden_top_level_path = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "list",
            "format": "json",
            "path": target_project_path,
            "category": "project",
            "min_trust": 0.0
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let hidden_top_level_path = extract_json(&hidden_top_level_path.value);
    assert_fact_results(
        &hidden_top_level_path,
        "Active selector fact",
        "Target selector fact",
        "top-level path should not act as an undocumented project selector",
    );

    close_test_graph(target).await;
    close_test_graph(active).await;
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn memory_status_project_selector_reports_registered_project_memory() {
    let (active, target, _env) = setup_cross_project_memory_projects().await;
    let active_runtime = active.test_runtime_for_test().expect("active runtime");
    let target_runtime = target.test_runtime_for_test().expect("target runtime");
    let target_project_id = target
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("target project should have a profile project_id");
    let target_project_path = target.project_root().to_string_lossy().to_string();

    for content in ["Active status fact one", "Active status fact two"] {
        handle_tool_call_with_runtime(
            &active,
            &active_runtime,
            "tracedecay_fact_store",
            json!({
                "action": "add",
                "content": content,
                "category": "project"
            }),
            None,
            None,
        )
        .await
        .unwrap();
    }

    handle_tool_call_with_runtime(
        &target,
        &target_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Target status fact",
            "category": "project"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let active_status = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_memory_status",
        json!({}),
        None,
        None,
    )
    .await
    .unwrap();
    let active_status = extract_json(&active_status.value);
    assert_eq!(active_status["status"], "ok");
    assert_eq!(active_status["memory"]["fact_count"].as_u64(), Some(2));

    let target_status_by_id = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_memory_status",
        json!({"project_id": target_project_id}),
        None,
        None,
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

    let target_status_by_path = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_memory_status",
        json!({"project_selector": {"path": target_project_path}}),
        None,
        None,
    )
    .await
    .unwrap();
    let target_status_by_path = extract_json(&target_status_by_path.value);
    assert_eq!(
        target_status_by_path["memory"]["fact_count"].as_u64(),
        Some(1),
        "nested path selector should report the target project's memory: {target_status_by_path}"
    );

    let missing_status = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_memory_status",
        json!({"project_id": "proj_does_not_exist"}),
        None,
        None,
    )
    .await;
    let Err(err) = missing_status else {
        panic!("expected unresolved memory_status selector to fail");
    };
    assert!(
        format!("{err}").contains("registered project not found for selector"),
        "unresolved memory_status selector must not fall back to active project: {err}"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn user_memory_scope_is_profile_level_and_isolated_from_project_memory() {
    let (active, target, _env) = setup_cross_project_memory_projects().await;
    let active_runtime = active.test_runtime_for_test().expect("active runtime");

    handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "Project-only routing decision",
            "category": "project"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "content": "User prefers concise technical answers",
            "category": "user_pref",
            "memory_scope": "user"
        }),
        None,
        None,
    )
    .await
    .unwrap();

    let project_facts = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({"action": "list", "format": "json", "min_trust": 0.0}),
        None,
        None,
    )
    .await
    .unwrap();
    let user_facts = handle_tool_call_with_runtime(
        &active,
        &active_runtime,
        "tracedecay_fact_store",
        json!({
            "action": "list",
            "format": "json",
            "min_trust": 0.0,
            "memory_scope": "user"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let project_facts = extract_json(&project_facts.value).to_string();
    let user_facts = extract_json(&user_facts.value).to_string();
    assert!(project_facts.contains("Project-only routing decision"));
    assert!(!project_facts.contains("User prefers concise technical answers"));
    assert!(user_facts.contains("User prefers concise technical answers"));
    assert!(!user_facts.contains("Project-only routing decision"));

    close_test_graph(target).await;
    close_test_graph(active).await;
}

#[tokio::test]
async fn memory_fact_store_update_rejects_secret_like_content_with_diff_report() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
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
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();

    let rejected = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "update",
            "format": "json",
            "fact_id": fact_id,
            "content": "api_key=sk-test-742913 must not be persisted"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let rejected: Value = serde_json::from_str(extract_text(&rejected.value)).unwrap();
    assert_eq!(rejected["action"], "update");
    assert_eq!(rejected["count"], 0);
    assert_eq!(rejected["diff"], "rejected_secret_like");
    assert!(rejected["fact"].is_null());
    assert!(
        rejected["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("secret"),
        "reason should describe the hygiene rejection: {rejected}"
    );

    let stored = cg.get_fact(fact_id).await.unwrap().unwrap();
    assert_eq!(
        stored.content,
        "Project preference: never store provider API keys"
    );
    assert!(!stored.content.contains("sk-test-742913"));
}

#[tokio::test]
async fn memory_recall_updates_retrieval_count() {
    let (cg, _dir) = setup_project().await;
    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
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
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();

    handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "search", "format": "json", "query": "Retrieval counters", "limit": 5}),
        None,
        None,
    )
    .await
    .unwrap();

    let status = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "list", "format": "json", "min_trust": 0.0, "limit": 10}),
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
        .find(|fact| fact["fact_id"].as_i64() == Some(fact_id))
        .unwrap();
    assert!(
        fact["retrieval_count"].as_i64().unwrap_or_default() > 0,
        "returned facts should increment retrieval_count: {status}"
    );
}

#[tokio::test]
async fn memory_fact_store_update_trust_delta_uses_direct_fact_lookup() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let first = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "format": "json",
            "content": "First fact should remain updateable after many later facts",
            "trust": 0.4
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let first: Value = serde_json::from_str(extract_text(&first.value)).unwrap();
    let first_id = first["fact"]["fact_id"].as_i64().unwrap();

    let db = cg.open_project_store_db().await.unwrap();
    let mut fixture_conn = rusqlite::Connection::open(db.database_path()).unwrap();
    let fixture_tx = fixture_conn.transaction().unwrap();
    for i in 0..205i64 {
        fixture_tx
            .execute(
                "INSERT INTO memory_facts (
                    content, category, tags, trust_score, created_at, updated_at, source, metadata
                 )
                 VALUES (?1, 'general', '[]', 0.5, ?2, ?2, 'test', '{}')",
                rusqlite::params![
                    format!("Later fact {i} should not hide the first fact"),
                    9_000_000_000i64 + i,
                ],
            )
            .unwrap();
    }
    fixture_tx.commit().unwrap();

    let updated = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "update",
            "format": "json",
            "fact_id": first_id,
            "trust_delta": 0.2
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let updated: Value = serde_json::from_str(extract_text(&updated.value)).unwrap();
    assert_eq!(updated["fact"]["fact_id"].as_i64(), Some(first_id));
    assert!(
        (updated["fact"]["trust_score"].as_f64().unwrap() - 0.6).abs() < 0.000_001,
        "trust_delta should apply through direct fact lookup: {updated}"
    );
}

#[tokio::test]
async fn memory_feedback_and_status_include_trust_fields() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
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
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();
    assert!(added["fact"].get("id").is_none());
    assert!(added["fact"].get("trust").is_none());
    assert!(added["fact"]["trust_score"].as_f64().is_some());

    let helpful = handle_tool_call(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": fact_id, "format": "json", "helpful": true, "source": "mcp-test", "note": "matched"}),
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

    let unhelpful = handle_tool_call(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": fact_id, "format": "json", "unhelpful": true}),
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

    let fetched = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "get", "format": "json", "fact_id": fact_id}),
        None,
        None,
    )
    .await
    .unwrap();
    let fetched: Value = serde_json::from_str(extract_text(&fetched.value)).unwrap();
    assert_eq!(fetched["action"], "get");
    assert_eq!(fetched["fact"]["fact_id"], fact_id);
    let trust_history = fetched["trust_history"]
        .as_array()
        .unwrap_or_else(|| panic!("expected trust_history array: {fetched}"));
    assert_eq!(trust_history.len(), 2);
    assert_eq!(trust_history[0]["action"], "helpful");
    assert_eq!(trust_history[0]["note"], "matched");
    assert_eq!(trust_history[1]["action"], "unhelpful");
    assert!(trust_history[1]["note"].is_null());

    let markdown_feedback = handle_tool_call(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": fact_id, "helpful": true, "source": "mcp-test", "note": "markdown"}),
        None,
        None,
    )
    .await
    .unwrap();
    let markdown_feedback = extract_text(&markdown_feedback.value);
    assert!(markdown_feedback.contains("**status:** recorded"));
    assert!(markdown_feedback.contains("**action:** helpful"));
    assert!(markdown_feedback.contains("**fact_id:**"));
    assert!(!markdown_feedback.contains("|"));

    let status = handle_tool_call(&cg, "tracedecay_memory_status", json!({}), None, None)
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
    assert!(status["memory"].get("missing_vector_count").is_some());
}

#[tokio::test]
async fn memory_fact_store_uses_project_store_when_serving_branch_db() {
    fn git(project: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(project)
            .output()
            .unwrap_or_else(|err| panic!("git {args:?} failed to spawn: {err}"));
        assert!(
            output.status.success(),
            "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let _guard = GLOBAL_DB_ENV_LOCK.lock().await;
    let dir = test_temp_dir();
    let project = dir.path().join("repo");
    let home = dir.path().join("home");
    let _home_guard = HomeEnvGuard::set(&home);
    let _global_db_guard = GlobalDbEnvGuard::set(&home.join(".tracedecay/global.db"));

    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn f() -> u32 { 1 }\n").unwrap();
    git(&project, &["init"]);
    git(&project, &["config", "user.email", "test@test.com"]);
    git(&project, &["config", "user.name", "Test"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "initial"]);
    git(&project, &["branch", "-M", "main"]);

    let cg = TestTraceDecay::new(TraceDecay::init(&project).await.unwrap());
    index_all_retrying_sync_lock(&cg).await;
    git(&project, &["checkout", "-b", "feature"]);
    let cg = TestTraceDecay::new(TraceDecay::open(&project).await.unwrap());
    assert_ne!(
        cg.db_path(),
        cg.store_layout().graph_db_path,
        "test must serve a branch DB distinct from the shared project store"
    );

    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "format": "json",
            "content": "Branch memory writes stay project-scoped",
            "category": "project",
            "entity": "Branch memory"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"]
        .as_i64()
        .expect("fact_store add should return numeric id");

    let (branch_db, _) = crate::common::open_test_database(&cg.db_path())
        .await
        .unwrap();
    let branch_writer = branch_db.memory_writer().await.unwrap();
    assert!(
        branch_writer
            .store()
            .get_fact(fact_id)
            .await
            .unwrap()
            .is_none(),
        "MCP memory writes must not be scoped to the branch graph DB"
    );

    let (project_db, _) = crate::common::open_test_database(&cg.store_layout().graph_db_path)
        .await
        .unwrap();
    let project_writer = project_db.memory_writer().await.unwrap();
    assert!(
        project_writer
            .store()
            .get_fact(fact_id)
            .await
            .unwrap()
            .is_some(),
        "MCP memory writes must land in the shared project memory store"
    );
}

#[tokio::test]
async fn memory_tools_validate_malformed_inputs() {
    let (cg, _env, _dir) = setup_empty_project().await;

    let missing_action =
        handle_tool_call(&cg, "tracedecay_fact_store", json!({}), None, None).await;
    assert!(expect_tool_error(missing_action).contains("action"));

    let bad_action = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "teleport"}),
        None,
        None,
    )
    .await;
    assert!(expect_tool_error(bad_action).contains("unknown fact_store action"));

    let bad_category = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "list", "category": "definitely-not-a-category"}),
        None,
        None,
    )
    .await;
    assert!(expect_tool_error(bad_category).contains("category"));

    let missing_feedback_action = handle_tool_call(
        &cg,
        "tracedecay_fact_feedback",
        json!({"fact_id": 123}),
        None,
        None,
    )
    .await;
    assert!(expect_tool_error(missing_feedback_action).contains("helpful"));
}

/// A plain `tracedecay_memory_status` read must never repair derived vectors
/// or rebuild dirty banks as a side effect (repair is owned by the daemon's
/// bounded memory-repair scheduler and the explicit repair entry point). The
/// status projection must still report the repair/backlog fields from stored
/// state once an explicit repair has actually run.
#[tokio::test]
async fn memory_status_reports_repair_state_without_repairing() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let added = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
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
    let added: Value = serde_json::from_str(extract_text(&added.value)).unwrap();
    let fact_id = added["fact"]["fact_id"].as_i64().unwrap();
    let db_path = project_graph_db(&cg);
    let (db, _) = crate::common::open_test_database(&db_path).await.unwrap();
    let changed = rusqlite::Connection::open(db.database_path())
        .unwrap()
        .execute(
            "UPDATE memory_facts
             SET hrr_vector = NULL, hrr_algebra = 'legacy', hrr_dim = 8
             WHERE fact_id = ?1",
            rusqlite::params![fact_id],
        )
        .unwrap();
    assert_eq!(changed, 1, "the out-of-band corruption must hit one fact");
    db.close();

    // A status read alone must not repair the missing vector or rebuild banks.
    let unrepaired = handle_tool_call(&cg, "tracedecay_memory_status", json!({}), None, None)
        .await
        .unwrap();
    let unrepaired: Value = serde_json::from_str(extract_text(&unrepaired.value)).unwrap();
    assert_eq!(unrepaired["status"], "ok");
    assert_eq!(
        unrepaired["memory"]["missing_vector_count"].as_u64(),
        Some(1),
        "a status read must not repair the missing vector as a side effect: {unrepaired}"
    );
    assert_eq!(
        unrepaired["memory"]["repair"]["missing_vectors_repaired"].as_u64(),
        Some(0),
        "a status read must not report repair work it never performed: {unrepaired}"
    );

    // Seed the repaired state via the explicit repair entry point instead of
    // relying on a status read to trigger it.
    let project_id = resolve_layout_for_current_profile(cg.project_root())
        .unwrap()
        .identity
        .project_id
        .expect("test project has a resolved project id");
    let owner = tracedecay_domain::FactOwnerV1::Project {
        project_id: tracedecay_domain::ProjectId::new(project_id).unwrap(),
    };
    let (repair_db, _) = crate::common::open_test_database(&db_path).await.unwrap();
    let memory = tracedecay::application::memory::MemoryApplication::new(
        owner.clone(),
        tracedecay::store::memory::DatabaseFactStore::new(&repair_db),
    )
    .unwrap();
    let repair = memory
        .dashboard_repair_v1(
            tracedecay::application::memory::MemoryOperationContext::generated(
                &owner,
                "explicit-status-test-repair",
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        repair.missing_vectors_repaired() >= 1,
        "explicit repair should have repaired the seeded missing vector"
    );
    repair_db.close();

    // Now a plain status read must report the already-repaired stored state,
    // without needing to repair anything itself.
    let repaired = handle_tool_call(&cg, "tracedecay_memory_status", json!({}), None, None)
        .await
        .unwrap();
    let repaired: Value = serde_json::from_str(extract_text(&repaired.value)).unwrap();
    assert_eq!(repaired["status"], "ok");
    assert!(
        repaired["memory"]["bank_count"]
            .as_u64()
            .unwrap_or_default()
            >= 2,
        "status should report the banks the explicit repair rebuilt: {repaired}"
    );
    assert_eq!(
        repaired["memory"]["missing_vector_count"].as_u64(),
        Some(0),
        "status should report the vector the explicit repair fixed: {repaired}"
    );
}

#[tokio::test]
async fn fact_store_reason_without_entities_names_the_missing_parameter() {
    let (cg, _dir) = setup_project().await;

    let err = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({"action": "reason", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("entities"),
        "error should name the missing `entities` parameter instead of a bare \
         canonicalization failure: {message}"
    );
    assert!(
        !message.contains("is not canonical"),
        "error should not surface the internal contract-violation phrasing \
         for a plain missing parameter: {message}"
    );
}

#[tokio::test]
async fn fact_store_add_out_of_range_trust_states_the_valid_range() {
    let (cg, _dir) = setup_project().await;

    let err = handle_tool_call(
        &cg,
        "tracedecay_fact_store",
        json!({
            "action": "add",
            "format": "json",
            "content": "Trust out of range must be rejected with an actionable message",
            "category": "project",
            "trust": 1.5
        }),
        None,
        None,
    )
    .await
    .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("0.0") && message.contains("1.0"),
        "error should state the valid 0.0-1.0 trust range: {message}"
    );
}

#[tokio::test]
async fn fact_feedback_on_nonexistent_fact_id_fails_fast_like_get() {
    let (cg, _dir) = setup_project().await;

    let started = std::time::Instant::now();
    let err = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        handle_tool_call(
            &cg,
            "tracedecay_fact_feedback",
            json!({"fact_id": 999_999_999_i64, "action": "helpful", "format": "json"}),
            None,
            None,
        ),
    )
    .await
    .expect("fact_feedback on a nonexistent fact must not hang until a client deadline")
    .unwrap_err();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "fact_feedback on a nonexistent fact must fail fast like fact_store get: {:?}",
        started.elapsed()
    );
    let message = err.to_string().to_lowercase();
    assert!(
        message.contains("not found")
            || message.contains("missing")
            || message.contains("unavailable"),
        "error should clearly report the fact as absent: {message}"
    );
}
