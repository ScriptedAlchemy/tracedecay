use crate::dashboard_api_support::*;

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
        let winner_id = fixture_fact_id(
            &agent,
            &fixture,
            "Cache invalidation policy must be explicit",
        );
        let loser_id = fixture_fact_id(
            &agent,
            &fixture,
            "Cache invalidation policy must stay explicit",
        );
        let tool_id = fixture_fact_id(&agent, &fixture, "LCM dashboard empty states");

        // Fresh fixture: no operations recorded yet.
        let (status, empty_oplog) = get_json(&agent, &oplog_url);
        assert_eq!(status, 200);
        assert_eq!(empty_oplog["count"], 0);
        assert_eq!(empty_oplog["error"], "");

        // Merge the second project fact into the first with rewritten content,
        // plus an explicit tool deletion and invalid operations. Partial
        // failure stays per-op.
        let (status, response) = post_json_body(
            &agent,
            &apply_url,
            &serde_json::json!({
                "ops": [
                    {
                        "op": "merge",
                        "winner_id": winner_id,
                        "loser_ids": [loser_id],
                        "merged_content": "Cache invalidation policy must be explicit (merged)"
                    },
                    { "op": "delete", "fact_id": tool_id, "reason": "manual cleanup" },
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
        assert_eq!(results[0]["deleted_loser_ids"], serde_json::json!([loser_id]));

        assert_eq!(results[1]["op"], "delete");
        assert_eq!(results[1]["status"], "deleted");
        assert_eq!(results[1]["fact_id"], tool_id);

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
            1,
            "the authority emits one privacy-safe merge audit receipt"
        );
        assert_eq!(events[0]["op"], "curate_apply");
        assert!(
            events[0]["detail"]["redacted"] == true,
            "authority oplog details must remain redacted: {oplog}"
        );
        assert_eq!(events[0]["fact_id"], winner_id);
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

        // Deleted facts are absent from the public projection regardless of
        // their canonical tombstone retention.
        for gone_id in [loser_id, tool_id] {
            let (status, missing) = get_json(
                &agent,
                &format!("{}/api/plugins/holographic/fact/{gone_id}", fixture.base_url),
            );
            assert_eq!(status, 200, "fact {gone_id} read must answer typed");
            assert_eq!(
                missing["domain_state"], "complete_zero_findings",
                "fact {gone_id} must no longer be visible: {missing}"
            );
            assert_eq!(
                missing["payload"],
                Value::Null,
                "deleted fact {gone_id} must not fabricate a payload"
            );
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
        let facts = overview["payload"]["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected facts array"));
        assert!(
            facts.iter().any(|fact| {
                fact["fact_id"].as_i64() == Some(winner_id)
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
                "ops": [{ "op": "merge", "winner_id": 4242, "loser_ids": [winner_id] }]
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(response["results"][0]["status"], "error");
        assert_eq!(response["counts"]["errors"], 1);
        let (status, survivor) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/fact/{winner_id}", fixture.base_url),
        );
        assert_eq!(status, 200, "loser must be untouched when the winner is missing");
        assert!(
            survivor["payload"]["fact"]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("(merged)"),
            "failed merge must not alter the existing winner"
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
        let winner_id = fixture_fact_id(
            &agent,
            &fixture,
            "Cache invalidation policy must be explicit",
        );
        let loser_id = fixture_fact_id(
            &agent,
            &fixture,
            "Cache invalidation policy must stay explicit",
        );
        let (status, original) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/{winner_id}",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let original_winner = original["payload"]["fact"]["content"].clone();

        let (status, response) = post_json_body(
            &agent,
            &apply_url,
            &serde_json::json!({
                "ops": [{
                    "op": "merge",
                    "winner_id": winner_id,
                    "loser_ids": [loser_id, 99999],
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

        let (status, winner_after) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/{winner_id}",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(
            winner_after["payload"]["fact"]["content"], original_winner,
            "failed merge must not update winner content"
        );
        let (status, loser_after) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/{loser_id}",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200, "failed merge must not delete valid losers");
        assert_eq!(loser_after["payload"]["fact"]["fact_id"], loser_id);
    });
}
