mod common;
mod dashboard_api_support;

use dashboard_api_support::*;

#[test]
fn dashboard_plugin_manifest_assets_are_served() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let (status, plugins) = get_json(
            &agent,
            &format!("{}/api/dashboard/plugins", fixture.base_url),
        );
        assert_eq!(status, 200);
        for plugin in plugins
            .as_array()
            .unwrap_or_else(|| panic!("expected plugin manifest array"))
        {
            let name = plugin["name"]
                .as_str()
                .unwrap_or_else(|| panic!("plugin name should be a string: {plugin}"));
            for key in ["entry", "css"] {
                let Some(asset) = plugin[key].as_str() else {
                    continue;
                };
                let url = format!("{}/dashboard-plugins/{name}/{asset}", fixture.base_url);
                let response = agent
                    .get(&url)
                    .call()
                    .unwrap_or_else(|err| panic!("GET {url} failed: {err}"));
                assert_eq!(
                    response.status().as_u16(),
                    200,
                    "advertised plugin asset should be served: {name} {asset}"
                );
            }
        }
    });
}

#[test]
fn holographic_dashboard_endpoints_return_seeded_payloads() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/?q=cache&limit=5&graph_limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["providers"]["memory_provider"], "tracedecay");
        assert_eq!(overview["holographic"]["overview"]["facts"], 3);
        assert_eq!(overview["holographic"]["overview"]["banks"], 3);
        assert_eq!(overview["holographic"]["overview"]["entities"], 3);
        // Bank list counts must be live (consistent with the header fact
        // count). The stored bundle snapshot still stays exposed as
        // bundled_fact_count, but startup backfill rebuilds now refresh the
        // seeded project bank to the live membership count.
        let memory_banks = overview["holographic"]["overview"]["memory_banks"]
            .as_array()
            .unwrap_or_else(|| panic!("expected memory_banks array"));
        let project_bank = memory_banks
            .iter()
            .find(|bank| bank["bank_name"] == "project")
            .unwrap_or_else(|| panic!("expected project bank in memory_banks"));
        assert_eq!(
            project_bank["fact_count"], 2,
            "bank list must report live membership counts"
        );
        assert_eq!(
            project_bank["bundled_fact_count"], 2,
            "startup bank rebuild should refresh the bundled project snapshot to the live membership count"
        );
        let facts = overview["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected facts array in overview payload"));
        assert_eq!(facts.len(), 2, "query should filter to cache facts only");
        // Access tracking is part of every fact payload (seeded rows carry
        // the column defaults).
        assert!(
            facts
                .iter()
                .all(|fact| fact["access_count"].is_number()
                    && fact.get("last_recalled_at").is_some()),
            "fact list rows must surface access_count and last_recalled_at"
        );
        let graph_nodes = overview["holographic"]["graph"]["nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("expected graph nodes array"));
        assert!(
            graph_nodes.iter().any(|node| node["kind"] == "entity"),
            "graph should include entity nodes"
        );
        let growth = overview["holographic"]["overview"]["growth"]
            .as_array()
            .unwrap_or_else(|| panic!("expected growth series array"));
        assert!(
            !growth.is_empty(),
            "growth should cover seeded historical facts"
        );
        assert!(
            growth.iter().all(|day| day["cumulative_facts"].is_number()),
            "growth points should include cumulative fact counts"
        );
        assert_eq!(
            growth
                .last()
                .and_then(|day| day["cumulative_facts"].as_i64()),
            Some(3),
            "last cumulative growth point should include all seeded facts"
        );

        let (status, projection) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/projection?limit=5000",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(projection["limit"], 2000);
        assert_eq!(projection["method"], "pca");
        assert_eq!(projection["dim"], 2048);
        let projection_points = projection["points"]
            .as_array()
            .unwrap_or_else(|| panic!("expected projection points array"));
        assert!(
            projection_points.len() >= 2,
            "projection should include at least two PCA points"
        );
        assert!(
            projection_points[0]["x"].is_number() && projection_points[0]["y"].is_number(),
            "projection points should include numeric x/y coordinates"
        );
        let project_point = projection_points
            .iter()
            .find(|point| point["fact_id"].as_i64() == Some(101))
            .unwrap_or_else(|| panic!("expected projection point for fact 101"));
        assert_eq!(project_point["bank_name"], "project");
        assert!(
            project_point["bank_id"].is_number(),
            "projection point should include numeric bank_id"
        );
        assert_eq!(project_point["entity_count"], 1);
        assert_eq!(project_point["connection_count"], 1);
        let tool_point = projection_points
            .iter()
            .find(|point| point["fact_id"].as_i64() == Some(103))
            .unwrap_or_else(|| panic!("expected projection point for fact 103"));
        assert_eq!(tool_point["entity_count"], 2);
        assert_eq!(tool_point["connection_count"], 2);

        let (status, similarity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/similarity?min_similarity=0.0&limit=5000",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(similarity["limit"], 2000);
        assert_eq!(similarity["min_similarity"], 0.0);
        assert_eq!(similarity["dim"], 2048);
        assert_eq!(similarity["count"], 3);
        assert_eq!(similarity["total_pairs"], 3);
        let pairs = similarity["pairs"]
            .as_array()
            .unwrap_or_else(|| panic!("expected similarity pairs array"));
        assert_eq!(
            pairs.len(),
            3,
            "min_similarity=0 should return pairs below the previous 0.5 floor"
        );
        let duplicate_pair = pairs
            .iter()
            .find(|pair| pair["classification"] == "likely_duplicate")
            .unwrap_or_else(|| panic!("expected likely_duplicate similarity pair"));
        let duplicate_similarity = duplicate_pair["similarity"]
            .as_f64()
            .unwrap_or_else(|| panic!("expected numeric similarity"));
        assert!(
            duplicate_similarity < 1.0 && duplicate_similarity > 0.9999,
            "similarity should retain full precision instead of rounding to four decimals"
        );
        let distribution = &similarity["score_distribution"];
        let bins = distribution["bins"]
            .as_array()
            .unwrap_or_else(|| panic!("expected score distribution bins"));
        assert!(!bins.is_empty(), "score distribution should include bins");
        let binned_pairs: i64 = bins
            .iter()
            .map(|bin| bin["count"].as_i64().unwrap_or(0))
            .sum();
        assert_eq!(distribution["total_pairs"], 3);
        assert_eq!(
            binned_pairs, 3,
            "distribution bins should cover every computed pair"
        );
        assert_eq!(
            distribution["min"], distribution["min_score"],
            "bins should adapt to the observed score range"
        );
        assert_eq!(
            distribution["max"], distribution["max_score"],
            "bins should adapt to the observed score range"
        );
        let occupied_bins = bins
            .iter()
            .filter(|bin| bin["count"].as_i64().unwrap_or(0) > 0)
            .count();
        assert!(
            occupied_bins >= 2,
            "adaptive binning should spread near-duplicate and unrelated pairs across bins"
        );
        assert!(
            pairs
                .iter()
                .any(|pair| pair["classification"] == "likely_duplicate"),
            "fixture vectors should produce a likely_duplicate pair"
        );

        let (status, curation_status) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/status",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(curation_status["config"]["enabled"], true);

        let (status, curation_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=75",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(curation_activity["count"], 0);
        assert_eq!(curation_activity["events"], Value::Array(Vec::new()));

        let (status, curation_preview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert!(curation_preview["report"].is_null());
        assert_eq!(curation_preview["stale"], false);

        // Curation dry-run should return a valid plan (the fixture has a likely-duplicate pair).
        let (status, curate) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(curate["ran"], true);
        assert_eq!(curate["dry_run"], true);
        assert!(
            curate["actions"].as_array().is_some(),
            "curate dry-run should return an actions array"
        );
        // The deterministic hygiene candidate section is always present.
        for key in ["secret_like", "transient", "supersession"] {
            assert!(
                curate["hygiene_candidates"][key].as_array().is_some(),
                "curate dry-run should include hygiene_candidates.{key} proposals"
            );
        }
    });
}

#[test]
fn holographic_fact_detail_returns_full_content_and_entities() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        assert!(
            LONG_FACT_CONTENT.chars().count() > 200,
            "fixture must exceed the 200-char list/projection truncation"
        );

        // The projection payload truncates content at 200 chars by design.
        let (status, projection) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/projection?limit=2000",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let truncated_point = projection["points"]
            .as_array()
            .and_then(|points| {
                points
                    .iter()
                    .find(|point| point["fact_id"].as_i64() == Some(103))
            })
            .unwrap_or_else(|| panic!("expected projection point for fact 103"));
        assert_eq!(
            truncated_point["content"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .count(),
            200,
            "projection content stays truncated at 200 chars"
        );

        // The detail endpoint returns the complete row plus linked entities.
        let (status, detail) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/fact/103", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(detail["error"], "");
        assert_eq!(detail["fact"]["fact_id"], 103);
        assert_eq!(detail["fact"]["category"], "tool");
        assert_eq!(detail["fact"]["content"], LONG_FACT_CONTENT);
        assert_eq!(detail["fact"]["has_hrr"], 1);
        assert_eq!(detail["fact"]["trust_score"], 0.76);
        assert!(
            detail["fact"]["access_count"].is_number(),
            "fact detail must surface access_count"
        );
        assert!(
            detail["fact"].get("last_recalled_at").is_some(),
            "fact detail must surface last_recalled_at"
        );
        let entities = detail["fact"]["entities"]
            .as_array()
            .unwrap_or_else(|| panic!("expected entities array in fact detail"));
        let entity_names: Vec<&str> = entities
            .iter()
            .filter_map(|entity| entity["name"].as_str())
            .collect();
        assert_eq!(
            entity_names,
            vec!["LCMTab", "SimilarityView"],
            "fact detail must list linked entities sorted by name"
        );

        // Unknown ids are a 404 with the FastAPI-style detail body.
        let (status, missing) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/fact/99999", fixture.base_url),
        );
        assert_eq!(status, 404);
        assert!(
            missing["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("99999"),
            "404 body should carry the requested fact id"
        );
    });
}

#[test]
fn holographic_fact_trust_history_returns_feedback_trail_and_empty_for_unreviewed_facts() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let conn = project_db_conn(&fixture).await;
        conn.execute(
            "INSERT INTO memory_feedback_events
                (fact_id, action, trust_delta, old_trust, new_trust, created_at, source, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            libsql::params![
                103_i64,
                "helpful",
                0.05_f64,
                0.71_f64,
                0.76_f64,
                1_700_000_450_i64,
                "dashboard-test",
                "confirmed durable"
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("failed to insert helpful feedback row: {err}"));
        conn.execute(
            "INSERT INTO memory_feedback_events
                (fact_id, action, trust_delta, old_trust, new_trust, created_at, source, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            libsql::params![
                103_i64,
                "unhelpful",
                -0.10_f64,
                0.76_f64,
                0.66_f64,
                1_700_000_460_i64,
                "dashboard-test",
                libsql::Value::Null
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("failed to insert unhelpful feedback row: {err}"));

        let agent = http_agent();
        let (status, history) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/103/trust-history",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(history["error"], "");
        assert_eq!(history["fact_id"], 103);
        let trail = history["trust_history"]
            .as_array()
            .unwrap_or_else(|| panic!("expected trust_history array: {history}"));
        assert_eq!(trail.len(), 2);
        assert_eq!(trail[0]["timestamp"], 1_700_000_450_i64);
        assert_eq!(trail[0]["action"], "helpful");
        assert_eq!(trail[0]["old_trust"], 0.71);
        assert_eq!(trail[0]["new_trust"], 0.76);
        assert_eq!(trail[0]["delta"], 0.05);
        assert_eq!(trail[0]["source"], "dashboard-test");
        assert_eq!(trail[0]["note"], "confirmed durable");
        assert_eq!(trail[1]["action"], "unhelpful");
        assert!(trail[1]["note"].is_null());

        let (status, empty_history) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/101/trust-history",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(empty_history["fact_id"], 101);
        assert_eq!(
            empty_history["trust_history"]
                .as_array()
                .map(|rows| rows.len()),
            Some(0)
        );

        let (status, missing) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/99999/trust-history",
                fixture.base_url
            ),
        );
        assert_eq!(status, 404);
        assert!(
            missing["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("99999"),
            "404 body should carry the requested fact id"
        );
    });
}

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
fn memory_oplog_endpoint_lists_recent_operations() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        // Fresh fixture: no operations recorded yet.
        let (status, empty) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/oplog?limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(empty["count"], 0);
        assert_eq!(empty["error"], "");

        // An explicit-ops delete writes a per-fact "remove" row plus a
        // "curate_apply" summary row.
        let (status, applied) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate/apply", fixture.base_url),
            &serde_json::json!({
                "ops": [{ "op": "delete", "fact_id": 103, "reason": "oplog fixture" }]
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(applied["counts"]["deleted"], 1);

        let (status, oplog) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/oplog?limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(oplog["error"], "");
        let events = oplog["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected oplog events array"));
        assert_eq!(events.len(), 2, "expected remove + curate_apply rows");

        // Newest first: the curate_apply summary follows the per-fact remove.
        assert_eq!(events[0]["op"], "curate_apply");
        assert_eq!(events[0]["detail"]["deleted"], 1);
        assert_eq!(events[1]["op"], "remove");
        assert_eq!(events[1]["fact_id"], 103);
        let remove_detail = events[1]["detail"].to_string();
        assert!(
            remove_detail.contains("content_hash"),
            "remove rows must carry a content hash: {remove_detail}"
        );
        assert!(
            !remove_detail.contains("empty states"),
            "remove rows must not leak deleted fact content: {remove_detail}"
        );
        assert!(
            events.iter().all(|event| event["ts"].is_number()),
            "every oplog row carries a timestamp"
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

#[test]
fn lcm_endpoints_cover_seeded_fts_and_like_fallback() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/overview?q=vector&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["exists"], true);
        assert_eq!(
            overview["storage_scope"], "profile_sharded",
            "LCM serves the resolved project session store even when TRACEDECAY_GLOBAL_DB is set for accounting"
        );
        assert_eq!(overview["overview"]["messages_total"], 3);
        assert_eq!(overview["overview"]["sessions_total"], 1);
        assert_eq!(overview["overview"]["summary_nodes_total"], 1);
        assert_eq!(
            overview["overview"]["compression"]["source_token_count"],
            180
        );
        assert_eq!(overview["overview"]["compression"]["token_count"], 72);
        let latest_sessions = overview["latest_sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("expected latest_sessions array"));
        assert_eq!(latest_sessions.len(), 1);
        let matches_messages = overview["matches"]["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("expected overview.matches.messages array"));
        assert!(
            !matches_messages.is_empty(),
            "overview?q=vector should return message matches"
        );

        let (status, search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=vector&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(search["engine"], "fts");
        let search_messages = search["matches"]["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("expected search.matches.messages array"));
        let search_nodes = search["matches"]["summary_nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("expected search.matches.summary_nodes array"));
        assert!(
            !search_messages.is_empty(),
            "FTS search should match seeded messages"
        );
        assert!(
            !search_nodes.is_empty(),
            "FTS search should match seeded summary nodes"
        );

        let (status, like_search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=!!!&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(like_search["engine"], "like");
    });
}

#[test]
fn lcm_endpoints_return_empty_state_when_no_rows_exist() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/overview?limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["exists"], true);
        assert_eq!(overview["overview"]["messages_total"], 0);
        assert_eq!(overview["overview"]["summary_nodes_total"], 0);
        assert_eq!(
            overview["latest_sessions"],
            Value::Array(Vec::new()),
            "empty LCM store should have no latest sessions"
        );
        assert_eq!(
            overview["latest_summary_nodes"],
            Value::Array(Vec::new()),
            "empty LCM store should have no summary nodes"
        );

        let (status, search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=vector&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(search["engine"], "fts");
        assert_eq!(
            search["matches"]["messages"],
            Value::Array(Vec::new()),
            "empty LCM store search should have zero message matches"
        );
        assert_eq!(
            search["matches"]["summary_nodes"],
            Value::Array(Vec::new()),
            "empty LCM store search should have zero summary-node matches"
        );
    });
}

/// Opens (creating if needed) the resolved project session store — profile
/// sharded by default, project-local only for explicit or legacy projects.
async fn open_project_session_store(project_root: &Path) -> GlobalDb {
    let db_path = tracedecay::sessions::cursor::project_session_db_path(project_root);
    match GlobalDb::open_at(&db_path).await {
        Some(db) => db,
        None => panic!(
            "failed to open project session store at {}",
            db_path.display()
        ),
    }
}

/// Without a `TRACEDECAY_GLOBAL_DB` override the dashboard must serve the
/// resolved project session store, profile-sharded by default, and report it
/// via the additive `storage_scope` payload field.
#[test]
fn lcm_serves_project_session_store_without_global_override() {
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
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::unset(GLOBAL_DB_ENV);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let session_store = open_project_session_store(&project_root).await;
        let expected_session_path =
            tracedecay::sessions::cursor::project_session_db_path(&project_root);
        seed_lcm_fixture(&session_store, &project_root).await;
        drop(session_store);

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);

        let agent = http_agent();
        wait_for_dashboard(&agent, &base_url).await;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["lcm_scope"], "profile_sharded");
        assert_eq!(capabilities["features"]["lcm"], true);
        let lcm_db = capabilities["lcm_db"]
            .as_str()
            .unwrap_or_else(|| panic!("expected capabilities.lcm_db string"));
        assert!(
            Path::new(lcm_db) == expected_session_path,
            "capabilities.lcm_db should be the resolved project session store, got {lcm_db}"
        );

        let (status, overview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/overview?limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["storage_scope"], "profile_sharded");
        assert_eq!(overview["exists"], true);
        assert_eq!(overview["overview"]["messages_total"], 3);
        assert_eq!(overview["overview"]["sessions_total"], 1);
        assert_eq!(overview["overview"]["summary_nodes_total"], 1);
        let path = overview["path"]
            .as_str()
            .unwrap_or_else(|| panic!("expected overview.path string"));
        assert!(
            Path::new(path) == expected_session_path,
            "overview.path should be the resolved project session store, got {path}"
        );

        let (status, search) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/search?q=vector&limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(search["storage_scope"], "profile_sharded");
        let search_messages = search["matches"]["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("expected search.matches.messages array"));
        assert!(
            !search_messages.is_empty(),
            "project-store search should match seeded messages"
        );

        server.stop();
    });
}

/// `TRACEDECAY_GLOBAL_DB` pins savings/accounting, but LCM sessions still
/// come from the resolved project store that transcript ingest writes.
#[test]
fn lcm_project_store_wins_over_global_accounting_override() {
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
        // The project store has rows; the overridden global accounting store has none.
        let session_store = open_project_session_store(&project_root).await;
        let expected_session_path =
            tracedecay::sessions::cursor::project_session_db_path(&project_root);
        seed_lcm_fixture(&session_store, &project_root).await;
        drop(session_store);

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);

        let agent = http_agent();
        wait_for_dashboard(&agent, &base_url).await;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["lcm_scope"], "profile_sharded");

        let (status, overview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/overview?limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["storage_scope"], "profile_sharded");
        assert_eq!(overview["exists"], true);
        assert_eq!(
            overview["overview"]["messages_total"], 3,
            "LCM must serve the project store, not the empty accounting DB"
        );
        let path = overview["path"]
            .as_str()
            .unwrap_or_else(|| panic!("expected overview.path string"));
        assert!(
            Path::new(path) == expected_session_path,
            "expected resolved project session DB path, got {path}"
        );

        server.stop();
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
