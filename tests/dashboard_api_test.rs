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
fn dashboard_projects_endpoint_lists_registered_projects_and_active_project() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let target_root = fixture
            ._tmp
            .path()
            .canonicalize()
            .expect("fixture root should canonicalize")
            .join("target-project");
        let target_cg = setup_project(&target_root).await;
        seed_memory_fixture(&target_cg).await;
        drop(target_cg);

        let (status, projects) = get_json(&agent, &format!("{}/api/projects", fixture.base_url));
        assert_eq!(status, 200);
        assert_eq!(projects["status"], "ok");
        assert_eq!(
            projects["active_project_root"],
            fixture.project_root.display().to_string()
        );
        let rows = projects["projects"]
            .as_array()
            .unwrap_or_else(|| panic!("expected project list array: {projects}"));
        assert!(
            rows.iter().any(|row| row["project_root"]
                == fixture.project_root.display().to_string()
                && row["is_active"] == true),
            "active project should be identified in daemon project list: {projects}"
        );
        assert!(
            rows.iter().any(
                |row| row["project_root"] == target_root.display().to_string()
                    && row["is_active"] == false
            ),
            "other registered project should be listed for selection: {projects}"
        );
    });
}

#[test]
fn project_scoped_plugin_routes_read_selected_project_store() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let target_root = fixture
            ._tmp
            .path()
            .canonicalize()
            .expect("fixture root should canonicalize")
            .join("target-project");
        let target_cg = setup_project(&target_root).await;
        let target_project_id = target_cg
            .store_layout()
            .identity
            .project_id
            .clone()
            .expect("profile-backed target should have project_id");
        target_cg
            .db()
            .conn()
            .execute(
                "INSERT INTO memory_facts
                    (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                libsql::params![
                    201_i64,
                    "Target daemon project selector fact",
                    "project",
                    "[\"selector\"]",
                    0.91_f64,
                    1_i64,
                    1_i64,
                    1_700_010_000_i64,
                    1_700_010_100_i64
                ],
            )
            .await
            .expect("target fact should insert");
        target_cg
            .checkpoint()
            .await
            .expect("target project DB should checkpoint before dashboard reopen");
        target_cg.close();

        let (active_status, active_payload) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/?q=selector&limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(active_status, 200);
        assert_eq!(
            active_payload["holographic"]["facts"]
                .as_array()
                .map(Vec::len),
            Some(0),
            "active project should not contain target-only selector fact"
        );

        let (selected_status, selected_payload) = get_json(
            &agent,
            &format!(
                "{}/api/projects/{}/plugins/holographic/?q=selector&limit=10",
                fixture.base_url, target_project_id
            ),
        );
        assert_eq!(selected_status, 200);
        let selected_facts = selected_payload["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected selected project facts: {selected_payload}"));
        assert_eq!(selected_facts.len(), 1);
        assert_eq!(
            selected_facts[0]["content"],
            "Target daemon project selector fact"
        );
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
