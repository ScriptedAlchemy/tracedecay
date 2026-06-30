mod common;
mod dashboard_api_support;

use dashboard_api_support::*;

#[test]
fn curate_hygiene_scans_unvectored_facts() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let conn = project_db_conn(&fixture).await;
        conn.execute(
            "INSERT INTO memory_facts
                (fact_id, content, category, tags, trust_score, created_at, updated_at, source, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            libsql::params![
                901_i64,
                "api_key=Zx9mQ4tR7wLp2NvK8sBd1FgH",
                "project",
                "[]",
                0.5_f64,
                1_700_000_200_i64,
                1_700_000_200_i64,
                "test",
                "{}"
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("failed to insert unvectored hygiene fact: {err}"));

        let agent = http_agent();
        let (status, curate) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );

        assert_eq!(status, 200);
        let secret_like = curate["hygiene_candidates"]["secret_like"]
            .as_array()
            .unwrap_or_else(|| panic!("expected hygiene_candidates.secret_like array"));
        let secret_candidate = secret_like
            .iter()
            .find(|action| action["fact_id"].as_i64() == Some(901))
            .unwrap_or_else(|| {
                panic!("hygiene scan must include secret-like facts without HRR vectors: {curate}")
            });
        assert_eq!(secret_candidate["status"], "candidate");
        assert_eq!(secret_candidate["review_required"], true);
        assert_eq!(secret_candidate["recommended_op"], "delete");

        let (status, applied) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": false }),
        );
        assert_eq!(status, 200);
        assert!(applied["hygiene_candidates"]["secret_like"]
            .as_array()
            .is_some_and(|candidates| candidates
                .iter()
                .any(|candidate| candidate["fact_id"].as_i64() == Some(901))));
        assert_eq!(
            count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
                901,
            )
            .await,
            1,
            "deterministic curate apply must not delete hygiene candidates without explicit review"
        );
    });
}

#[test]
fn curation_delete_lifecycle() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        // --- Dry-run curation: expect a delete plan for the likely-duplicate pair ---
        let (status, dry) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(dry["ran"], true);
        assert_eq!(dry["dry_run"], true);
        assert_eq!(dry["llm_calls"], 0);
        let actions = dry["actions"]
            .as_array()
            .unwrap_or_else(|| panic!("expected actions array"));
        assert!(
            !actions.is_empty(),
            "fixture with likely-duplicate vectors should produce at least one delete action"
        );
        assert_eq!(actions[0]["op"], "delete");
        assert!(
            actions[0]["fact_id"].is_number(),
            "action must have fact_id"
        );
        assert!(
            actions[0]["duplicate_of"].is_number(),
            "action must reference the surviving duplicate"
        );
        let planned_delete_id = actions[0]["fact_id"]
            .as_i64()
            .unwrap_or_else(|| panic!("fact_id must be an integer"));
        assert_eq!(dry["counts"]["delete"], actions.len() as i64);
        assert_eq!(dry["coverage"]["active_total"], 3);

        // Preview should now be available and fresh.
        let (status, preview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert!(
            !preview["report"].is_null(),
            "preview should be non-null after a dry-run"
        );
        assert_eq!(preview["stale"], false);

        // Curation status should reflect the preview timestamp.
        let (status, curation_status) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/status",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(curation_status["config"]["enabled"], true);
        assert!(
            !curation_status["state"]["last_preview_at"].is_null(),
            "last_preview_at should be set after dry-run"
        );

        let (status, dry_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=75",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(dry_activity["error"], "");
        assert_eq!(dry_activity["limit"], 75);
        let dry_events = dry_activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected dry-run activity events array"));
        assert_eq!(
            dry_activity["count"].as_u64(),
            Some(dry_events.len() as u64)
        );
        assert!(
            !dry_events.is_empty(),
            "dry-run curation should emit activity events"
        );
        let dry_phases: Vec<_> = dry_events
            .iter()
            .filter_map(|event| event["phase"].as_str())
            .collect();
        for phase in [
            "queued",
            "start",
            "evidence",
            "backend",
            "validation",
            "report",
            "finish",
        ] {
            assert!(
                dry_phases.contains(&phase),
                "dry-run curation should emit {phase} activity; phases={dry_phases:?}"
            );
        }
        assert!(
            dry_events.iter().any(|event| {
                event["phase"] == "finish"
                    && event["dry_run"] == true
                    && event["message"]
                        .as_str()
                        .is_some_and(|message| !message.is_empty())
                    && event["ts"].as_str().is_some_and(|ts| !ts.is_empty())
            }),
            "dry-run curation should emit a finish activity event"
        );

        // --- Apply curation: hard-delete the duplicate ---
        let (status, applied) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": false }),
        );
        assert_eq!(status, 200);
        assert_eq!(applied["ran"], true);
        assert_eq!(applied["dry_run"], false);
        assert!(
            applied["applied_counts"]["delete"].as_i64().unwrap_or(0) > 0,
            "apply should report at least one deleted fact"
        );

        let (status, apply_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=75",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let apply_events = apply_activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected apply activity events array"));
        assert_eq!(
            apply_activity["count"].as_u64(),
            Some(apply_events.len() as u64)
        );
        assert!(
            apply_events.len() > dry_events.len(),
            "apply should append activity events after dry-run events"
        );
        let apply_phases: Vec<_> = apply_events
            .iter()
            .filter_map(|event| event["phase"].as_str())
            .collect();
        for phase in ["queued", "backend", "validation", "report", "apply"] {
            assert!(
                apply_phases.contains(&phase),
                "apply curation should emit {phase} activity; phases={apply_phases:?}"
            );
        }
        assert!(
            apply_events
                .iter()
                .rev()
                .any(|event| event["phase"] == "finish" && event["dry_run"] == false),
            "apply curation should emit a finish activity event"
        );

        let (status, status_after_apply) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/status",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(status_after_apply["state"]["run_count"], 1);
        assert!(
            status_after_apply["state"]["last_run_at"]
                .as_str()
                .is_some_and(|ts| !ts.is_empty()),
            "last_run_at should be set after apply"
        );
        assert!(
            status_after_apply["state"]["last_run_summary"]
                .as_str()
                .is_some_and(|summary| summary.contains("deleted")),
            "last_run_summary should describe the apply result"
        );
        assert!(
            status_after_apply["snapshots"]
                .as_array()
                .is_some_and(|snapshots| !snapshots.is_empty()),
            "status snapshots should include recent apply history"
        );

        // --- Overview should show fewer facts and not contain the deleted one ---
        let (status, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/", fixture.base_url),
        );
        assert_eq!(status, 200);
        let fact_count = overview["holographic"]["overview"]["facts"]
            .as_i64()
            .unwrap_or(3);
        assert!(
            fact_count < 3,
            "overview fact count should decrease after deletion"
        );
        let facts = overview["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected facts array"));
        assert!(
            facts
                .iter()
                .all(|fact| fact["fact_id"].as_i64() != Some(planned_delete_id)),
            "deleted fact must not appear in the overview fact list"
        );

        // --- The row and its entity links must be gone from the store that
        //     tracedecay_fact_store recall reads (hard delete, not soft). ---
        let remaining = count_in_project_db(
            &fixture,
            "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
            planned_delete_id,
        )
        .await;
        assert_eq!(
            remaining, 0,
            "deleted fact row must be gone from memory_facts"
        );
        let remaining_links = count_in_project_db(
            &fixture,
            "SELECT COUNT(*) FROM memory_fact_entities WHERE fact_id = ?1",
            planned_delete_id,
        )
        .await;
        assert_eq!(
            remaining_links, 0,
            "entity links of a deleted fact must be cleaned up"
        );

        // Apply invalidates the saved preview.
        let (status, preview_after) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert!(preview_after["report"].is_null());
    });
}

#[test]
fn curation_preview_marks_same_count_updates_stale() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let (status, dry) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(dry["dry_run"], true);

        let conn = project_db_conn(&fixture).await;
        conn.execute(
            "UPDATE memory_facts
             SET content = content || ' after preview', updated_at = updated_at + 1
             WHERE fact_id = 101",
            (),
        )
        .await
        .unwrap();

        let (status, preview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(
            preview["stale"], true,
            "same-count edits must stale previews"
        );
        assert!(
            preview["stale_reason"]
                .as_str()
                .unwrap_or_default()
                .contains("changed"),
            "stale response should explain the memory store changed: {preview}"
        );
    });
}

#[test]
fn curate_apply_ops_contract() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();
        let apply_url = format!("{}/api/plugins/holographic/curate/apply", fixture.base_url);
        let oplog_url = format!("{}/api/plugins/holographic/oplog?limit=10", fixture.base_url);

        // Fresh fixture: no operations recorded yet.
        let (status, empty_oplog) = get_json(&agent, &oplog_url);
        assert_eq!(status, 200);
        assert_eq!(empty_oplog["count"], 0);
        assert_eq!(empty_oplog["error"], "");

        // Merge: fact 102 into 101 with rewritten content, plus an explicit
        // delete of 103, plus an invalid delete — partial failure stays per-op.
        let (status, response) = post_json_body(
            &agent,
            &apply_url,
            &serde_json::json!({
                "ops": [
                    {
                        "op": "merge",
                        "winner_id": 101,
                        "loser_ids": [102],
                        "merged_content": "Cache invalidation policy must be explicit (merged)"
                    },
                    { "op": "delete", "fact_id": 103, "reason": "manual cleanup" },
                    { "op": "delete", "fact_id": 99999 },
                    { "op": "frobnicate" }
                ]
            }),
        );
        assert_eq!(status, 200, "partial failures must not fail the request");
        let results = response["results"]
            .as_array()
            .unwrap_or_else(|| panic!("expected results array"));
        assert_eq!(results.len(), 4);

        assert_eq!(results[0]["op"], "merge");
        assert_eq!(
            results[0]["status"], "merged",
            "merge op failed: {response}"
        );
        assert_eq!(results[0]["content_updated"], true);
        assert_eq!(results[0]["deleted_loser_ids"], serde_json::json!([102]));

        assert_eq!(results[1]["op"], "delete");
        assert_eq!(results[1]["status"], "deleted");
        assert_eq!(results[1]["fact_id"], 103);

        assert_eq!(results[2]["status"], "error");
        assert!(
            results[2]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("not found"),
            "invalid fact_id must produce a per-op not-found error"
        );

        assert_eq!(results[3]["status"], "error");
        assert!(
            results[3]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("unsupported op"),
            "unknown op kinds must produce a per-op error"
        );

        assert_eq!(response["counts"]["deleted"], 1);
        assert_eq!(response["counts"]["merged"], 1);
        assert_eq!(response["counts"]["errors"], 2);

        let (status, oplog) = get_json(&agent, &oplog_url);
        assert_eq!(status, 200);
        assert_eq!(oplog["error"], "");
        let events = oplog["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected oplog events array"));
        assert_eq!(
            events.len(),
            4,
            "expected update + loser remove + explicit remove + curate_apply rows"
        );

        // Newest first: the curate_apply summary follows the per-fact rows.
        assert_eq!(events[0]["op"], "curate_apply");
        assert_eq!(events[0]["detail"]["deleted"], 1);
        assert_eq!(events[0]["detail"]["merged"], 1);
        assert_eq!(events[0]["detail"]["errors"], 2);
        assert_eq!(events[1]["op"], "remove");
        assert_eq!(events[1]["fact_id"], 103);
        let explicit_remove_detail = events[1]["detail"].to_string();
        assert!(
            explicit_remove_detail.contains("content_hash"),
            "remove rows must carry a content hash: {explicit_remove_detail}"
        );
        assert!(
            !explicit_remove_detail.contains("empty states"),
            "remove rows must not leak deleted fact content: {explicit_remove_detail}"
        );
        assert_eq!(events[2]["op"], "remove");
        assert_eq!(events[2]["fact_id"], 102);
        assert_eq!(events[3]["op"], "update");
        assert_eq!(events[3]["fact_id"], 101);
        assert!(
            events.iter().all(|event| event["ts"].is_number()),
            "every oplog row carries a timestamp"
        );

        let (status, apply_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=25",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let apply_events = apply_activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected generic apply activity events array"));
        assert!(
            apply_events.iter().any(|event| {
                event["phase"] == "finish"
                    && event["dry_run"] == false
                    && event["message"].as_str().is_some_and(|message| {
                        message.contains("Explicit apply completed")
                            && message.contains("1 delete")
                            && message.contains("1 merge")
                            && message.contains("2 op(s) errored")
                    })
                    && event["ts"].as_str().is_some_and(|ts| !ts.is_empty())
            }),
            "/curate/apply should emit a finish activity event: {apply_activity}"
        );
        for phase in ["queued", "apply", "validation", "report"] {
            assert!(
                apply_events
                    .iter()
                    .any(|event| event["phase"].as_str() == Some(phase)),
                "/curate/apply should emit {phase} activity: {apply_activity}"
            );
        }
        assert!(
            apply_events.iter().any(|event| {
                event["phase"] == "rejection"
                    && event["level"] == "warning"
                    && event["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("2 explicit curation op(s)"))
            }),
            "/curate/apply should emit a rejection activity event for invalid ops: {apply_activity}"
        );

        let (status, rejected_only) = post_json_body(
            &agent,
            &apply_url,
            &serde_json::json!({
                "ops": [
                    { "op": "delete", "fact_id": 99999 },
                    { "op": "frobnicate" }
                ]
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(rejected_only["counts"]["deleted"], 0);
        assert_eq!(rejected_only["counts"]["merged"], 0);
        assert_eq!(rejected_only["counts"]["errors"], 2);
        let (status, rejected_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=25",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let rejected_events = rejected_activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected rejected activity events array: {rejected_activity}"));
        for phase in ["queued", "apply", "validation", "rejection", "report", "failure"] {
            assert!(
                rejected_events
                    .iter()
                    .any(|event| event["phase"].as_str() == Some(phase)),
                "all-rejected apply should emit {phase} activity: {rejected_activity}"
            );
        }
        assert!(
            rejected_events.iter().any(|event| {
                    event["phase"] == "finish"
                        && event["dry_run"] == false
                        && event["message"].as_str().is_some_and(|message| {
                            message.contains("0 delete")
                                && message.contains("0 merge")
                                && message.contains("2 op(s) errored")
                        })
            }),
            "all-rejected apply requests should still emit a terminal finish event: {rejected_activity}"
        );

        let (status, apply_status) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/status",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(apply_status["state"]["run_count"], 2);
        assert!(
            apply_status["state"]["last_run_at"]
                .as_str()
                .is_some_and(|ts| !ts.is_empty()),
            "last_run_at should be set after /curate/apply"
        );
        let summary = apply_status["state"]["last_run_summary"]
            .as_str()
            .unwrap_or_default();
        assert!(
            summary.contains("Explicit apply completed")
                && summary.contains("0 delete")
                && summary.contains("0 merge")
                && summary.contains("2 op(s) errored"),
            "/curate/apply should drive the status summary: {apply_status}"
        );
        assert!(
            apply_status["snapshots"]
                .as_array()
                .is_some_and(|snapshots| {
                    snapshots.iter().any(|snapshot| {
                        snapshot["summary"]
                            .as_str()
                            .is_some_and(|summary| summary.contains("Explicit apply completed"))
                    })
                }),
            "/curate/apply should appear in status snapshots: {apply_status}"
        );

        // Hard deletes: rows + entity links gone from the project DB.
        for gone_id in [102_i64, 103] {
            let remaining = count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
                gone_id,
            )
            .await;
            assert_eq!(remaining, 0, "fact {gone_id} must be hard-deleted");
            let links = count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_fact_entities WHERE fact_id = ?1",
                gone_id,
            )
            .await;
            assert_eq!(links, 0, "entity links of fact {gone_id} must be gone");
        }

        // Winner survived with merged content.
        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/?q=merged&limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let facts = overview["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected facts array"));
        assert!(
            facts.iter().any(|fact| {
                fact["fact_id"].as_i64() == Some(101)
                    && fact["content"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("(merged)")
            }),
            "winner fact must survive with the merged content"
        );

        // Merge with a missing winner: per-op error, losers untouched.
        let (status, response) = post_json_body(
            &agent,
            &apply_url,
            &serde_json::json!({
                "ops": [{ "op": "merge", "winner_id": 4242, "loser_ids": [101] }]
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(response["results"][0]["status"], "error");
        assert_eq!(response["counts"]["errors"], 1);
        let survivor = count_in_project_db(
            &fixture,
            "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
            101,
        )
        .await;
        assert_eq!(
            survivor, 1,
            "loser must be untouched when the winner is missing"
        );

        // Malformed body (no ops field) is the only whole-request failure mode.
        let (status, _) = post_json(&agent, &apply_url);
        assert!(
            status == 400 || status == 415 || status == 422,
            "missing/malformed body should be rejected, got {status}"
        );
    });
}

#[test]
fn curate_apply_merge_with_missing_loser_is_atomic() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();
        let apply_url = format!("{}/api/plugins/holographic/curate/apply", fixture.base_url);

        let (status, dry) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(dry["dry_run"], true);

        let original_winner = string_in_project_db(
            &fixture,
            "SELECT content FROM memory_facts WHERE fact_id = ?1",
            101,
        )
        .await
        .expect("winner content");

        let (status, response) = post_json_body(
            &agent,
            &apply_url,
            &serde_json::json!({
                "ops": [{
                    "op": "merge",
                    "winner_id": 101,
                    "loser_ids": [102, 99999],
                    "merged_content": "Cache invalidation policy should not partially merge"
                }]
            }),
        );
        assert_eq!(status, 200, "per-op failures stay in-band");
        assert_eq!(response["counts"]["deleted"], 0);
        assert_eq!(response["counts"]["merged"], 0);
        assert_eq!(response["counts"]["errors"], 1);
        assert_eq!(response["results"][0]["op"], "merge");
        assert_eq!(response["results"][0]["status"], "error");
        assert!(
            response["results"][0]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("loser fact 99999 not found"),
            "missing loser should be reported before mutation: {response}"
        );

        let winner_after = string_in_project_db(
            &fixture,
            "SELECT content FROM memory_facts WHERE fact_id = ?1",
            101,
        )
        .await
        .expect("winner content after failed merge");
        assert_eq!(
            winner_after, original_winner,
            "failed merge must not update winner content"
        );
        assert_eq!(
            count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
                102,
            )
            .await,
            1,
            "failed merge must not delete valid losers"
        );
        assert_eq!(
            count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_oplog WHERE fact_id = ?1",
                101,
            )
            .await,
            0,
            "failed merge must not write a winner update oplog"
        );
        assert_eq!(
            count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_oplog WHERE fact_id = ?1",
                102,
            )
            .await,
            0,
            "failed merge must not write loser delete oplogs"
        );

        let (status, preview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert!(
            !preview["report"].is_null(),
            "failed merge must not clear saved preview"
        );
        assert_eq!(
            preview["stale"], false,
            "unchanged store should leave preview fresh"
        );
    });
}

/// The dry-run curation preview must survive a dashboard restart: it is
/// mirrored to the resolved dashboard sidecar path and re-hydrated by
/// `build_state`, and applying curation clears both the memory copy and the
/// sidecar.
#[test]
fn curation_preview_persists_across_dashboard_restarts() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        seed_memory_fixture(&cg).await;
        let agent = http_agent();
        let sidecar = cg
            .store_layout()
            .dashboard_root
            .join("curation_preview.json");

        async fn start_server(cg: TraceDecay) -> (String, DashboardServer) {
            let port = pick_free_port();
            let base_url = format!("http://127.0.0.1:{port}");
            let server = spawn_dashboard_server(cg, port);
            (base_url, server)
        }

        fn stop_server(mut server: DashboardServer) {
            server.stop();
        }

        async fn reopen_project(project_root: &Path) -> TraceDecay {
            match TraceDecay::open(project_root).await {
                Ok(cg) => cg,
                Err(err) => panic!("failed to reopen fixture project: {err}"),
            }
        }

        // Server 1: a dry-run saves the preview and writes the sidecar.
        let (base_url, server) = start_server(cg).await;
        wait_for_dashboard(&agent, &base_url).await;
        let (status, curate) = post_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curate"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(curate["dry_run"], true);
        let (status, preview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/preview"),
        );
        assert_eq!(status, 200);
        assert!(!preview["report"].is_null(), "dry-run must save a preview");
        let saved_at = preview["saved_at"].clone();
        assert!(saved_at.is_string(), "preview must carry saved_at");
        stop_server(server);
        assert!(
            sidecar.exists(),
            "dry-run must persist the preview sidecar at {}",
            sidecar.display()
        );

        // Server 2 (fresh state): the preview is re-hydrated from disk.
        let cg = reopen_project(&project_root).await;
        let (base_url, server) = start_server(cg).await;
        wait_for_dashboard(&agent, &base_url).await;
        let (status, preview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/preview"),
        );
        assert_eq!(status, 200);
        assert!(
            !preview["report"].is_null(),
            "preview must survive a server restart"
        );
        assert_eq!(
            preview["saved_at"], saved_at,
            "re-hydrated preview must keep its original timestamp"
        );
        assert_eq!(
            preview["stale"], false,
            "fact count is unchanged, so the restored preview is not stale"
        );
        let (status, status_payload) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/status"),
        );
        assert_eq!(status, 200);
        assert_eq!(
            status_payload["state"]["last_preview_at"], saved_at,
            "curation status must reflect the restored preview"
        );

        // Applying curation clears both the in-memory copy and the sidecar.
        let (status, applied) = post_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curate"),
            &serde_json::json!({ "dry_run": false }),
        );
        assert_eq!(status, 200);
        assert_eq!(applied["dry_run"], false);
        let (status, preview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/preview"),
        );
        assert_eq!(status, 200);
        assert!(preview["report"].is_null(), "apply must clear the preview");
        assert!(
            !sidecar.exists(),
            "apply must remove the persisted preview sidecar"
        );
        stop_server(server);

        // Server 3: nothing is restored after the apply cleared the sidecar.
        let cg = reopen_project(&project_root).await;
        let (base_url, server) = start_server(cg).await;
        wait_for_dashboard(&agent, &base_url).await;
        let (status, preview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/preview"),
        );
        assert_eq!(status, 200);
        assert!(
            preview["report"].is_null(),
            "no preview may reappear after curation was applied"
        );
        stop_server(server);
    });
}
