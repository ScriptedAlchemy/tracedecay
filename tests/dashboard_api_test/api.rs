use crate::dashboard_api_support::*;

#[test]
fn retired_dashboard_routes_cannot_serve_placeholder_bundles() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent();

        for path in [
            "/legacy",
            "/shell/shell.js",
            "/dashboard-plugins/savings/dist/index.js",
        ] {
            let mut response = agent
                .get(format!("{}{path}", fixture.base_url))
                .call()
                .unwrap_or_else(|err| panic!("GET {path} failed: {err}"));
            assert_eq!(response.status().as_u16(), 200);
            assert_eq!(
                response
                    .headers()
                    .get("content-type")
                    .and_then(|value| value.to_str().ok()),
                Some("text/html; charset=utf-8"),
                "retired path must fall through to the canonical SPA, never a legacy asset"
            );
            let body = response.body_mut().read_to_string().unwrap();
            assert!(body.contains("<title>TraceDecay</title>"));
            assert!(!body.contains("rewrite in progress"));
        }
    });
}

#[test]
fn dashboard_memory_status_does_not_wait_for_the_writer_lane() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let cg = fixture
            .host_runtime
            .open_project_graph_for_test(
                &fixture.project_root,
                tracedecay::tracedecay::TraceDecayOpenOptions::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("reopen dashboard fixture project: {error}"));
        let writer = cg
            .db()
            .memory_writer()
            .await
            .unwrap_or_else(|error| panic!("hold dashboard writer: {error}"));

        let agent = http_agent_with_timeout(std::time::Duration::from_secs(2));
        let response = agent
            .get(&format!(
                "{}/api/plugins/holographic/status",
                fixture.base_url
            ))
            .call()
            .unwrap_or_else(|error| panic!("status GET waited for writer lane: {error}"));
        let (status, payload) = response_to_json(response);
        assert_eq!(status, 200, "status payload: {payload}");

        drop(writer);
        cg.close();
    });
}

#[test]
fn automatic_fact_receipt_endpoints_expose_terminal_applied_and_quarantined_receipts() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        use tracedecay::application::memory::{MemoryApplication, automatic_fact_add_command};
        use tracedecay::store::memory::DatabaseFactStore;
        use tracedecay_domain::{
            ActorId, ComponentVersion, Confidence, FactCategoryV1, FactOwnerV1, PayloadReferenceV1,
            ProvenanceId, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
            SanitizerDispositionV1, SensitivityV1,
        };
        use tracedecay_store::{
            ProjectMemoryAutomaticFactEvidenceV1, ProjectMemoryFactAddCommandV1,
        };

        let fixture = start_dashboard_fixture(false).await;
        let cg = fixture
            .host_runtime
            .open_project_graph_for_test(
                &fixture.project_root,
                tracedecay::tracedecay::TraceDecayOpenOptions::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("open dashboard fixture project: {error}"));
        let project_id = cg
            .store_layout()
            .identity
            .project_id
            .as_deref()
            .and_then(|value| ProjectId::new(value.to_owned()).ok())
            .unwrap_or_else(|| panic!("dashboard fixture needs an authoritative project id"));
        let owner = FactOwnerV1::Project { project_id };
        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(cg.db()))
            .unwrap_or_else(|error| panic!("initialize automatic fact authority: {error}"));
        let apply_id = "automatic-fact-receipt-api-applied";
        let command = automatic_fact_add_command(
            owner,
            AddFactRequest {
                content: "Automatic receipt API preserves applied facts".to_owned(),
                category: MemoryCategory::Decision,
                source: Some("dashboard-api-test".to_owned()),
                tags: Vec::new(),
                entities: Vec::new(),
                trust: Some(0.9),
                metadata: serde_json::json!({}),
            },
            "run.dashboard-receipt-api",
            apply_id,
            Some(
                ActorId::new("automation:dashboard-api-test".to_owned())
                    .unwrap_or_else(|error| panic!("build automatic fact actor: {error}")),
            ),
        )
        .unwrap_or_else(|error| panic!("build automatic fact command: {error}"));
        memory
            .apply_project_memory_automatic_fact(
                ProvenanceId::new(apply_id.to_owned())
                    .unwrap_or_else(|error| panic!("build automatic fact receipt id: {error}")),
                command,
                ProjectMemoryAutomaticFactEvidenceV1::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("record automatic applied receipt: {error}"));

        let quarantined_apply_id = "automatic-fact-receipt-api-quarantined";
        let quarantined_content = "api_key=0000000000000000";
        let quarantined_material = serde_json::json!({
            "content": quarantined_content,
            "category": FactCategoryV1::Decision,
            "tags": [],
            "entities": [],
            "metadata": {},
        });
        let quarantine_receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.dashboard-api-quarantined".to_owned())
                    .unwrap_or_else(|error| panic!("build sanitization receipt id: {error}")),
                ComponentVersion::new("sanitizer.dashboard-api-test.v1".to_owned())
                    .unwrap_or_else(|error| panic!("build sanitizer version: {error}")),
            )
            .unwrap_or_else(|error| panic!("build sanitization receipt reference: {error}")),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(
                PayloadReferenceV1::for_payload(&quarantined_material)
                    .unwrap_or_else(|error| panic!("bind sanitization payload: {error}")),
            ),
        )
        .unwrap_or_else(|error| panic!("build accepted sanitization receipt: {error}"));
        let quarantined_command = ProjectMemoryFactAddCommandV1::new(
            owner,
            ProvenanceId::new("automatic-fact-receipt-api-quarantined-operation".to_owned())
                .unwrap_or_else(|error| panic!("build automatic operation id: {error}")),
            quarantined_content.to_owned(),
            FactCategoryV1::Decision,
            Some("dashboard-api-test".to_owned()),
            Vec::new(),
            Vec::new(),
            serde_json::json!({}),
            quarantine_receipt,
            Confidence::new(0.9).unwrap_or_else(|error| panic!("build fact confidence: {error}")),
            None,
        )
        .unwrap_or_else(|error| panic!("build quarantined automatic fact command: {error}"))
        .with_automation_run_id("run.dashboard-receipt-api".to_owned())
        .unwrap_or_else(|error| panic!("bind quarantined automatic fact run: {error}"));
        memory
            .apply_project_memory_automatic_fact(
                ProvenanceId::new(quarantined_apply_id.to_owned()).unwrap_or_else(|error| {
                    panic!("build quarantined automatic fact receipt id: {error}")
                }),
                quarantined_command,
                ProjectMemoryAutomaticFactEvidenceV1::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("record automatic quarantined receipt: {error}"));

        let agent = http_agent();
        let endpoint = format!(
            "{}/api/automation/automatic-fact-receipts",
            fixture.base_url
        );
        let (status, listed) = get_json(&agent, &format!("{endpoint}?state=applied"));
        assert_eq!(status, 200, "automatic receipt list failed: {listed}");
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["limit"], 50);
        assert_eq!(listed["error"], "");
        let receipts = listed["receipts"]
            .as_array()
            .unwrap_or_else(|| panic!("automatic receipt list must contain receipts: {listed}"));
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0]["apply_id"], apply_id);
        assert_eq!(receipts[0]["run_id"], "run.dashboard-receipt-api");
        assert_eq!(receipts[0]["state"], "applied");
        assert!(receipts[0]["applied_canonical_fact_id"].is_string());

        let (status, viewed) = get_json(&agent, &format!("{endpoint}/{apply_id}"));
        assert_eq!(status, 200, "automatic receipt view failed: {viewed}");
        assert_eq!(viewed["error"], "");
        assert_eq!(viewed["receipt"]["apply_id"], apply_id);
        assert_eq!(viewed["receipt"]["state"], "applied");

        let (status, quarantined) = get_json(&agent, &format!("{endpoint}?state=quarantined"));
        assert_eq!(
            status, 200,
            "quarantined receipt list failed: {quarantined}"
        );
        assert_eq!(quarantined["count"], 1);
        assert_eq!(quarantined["receipts"][0]["apply_id"], quarantined_apply_id);
        assert_eq!(quarantined["receipts"][0]["state"], "quarantined");
        assert!(quarantined["receipts"][0]["quarantine_reason"].is_string());
        assert!(quarantined["receipts"][0]["applied_canonical_fact_id"].is_null());

        let (status, rejected_filter) = get_json(&agent, &format!("{endpoint}?state=staged"));
        assert_eq!(status, 400);
        assert!(
            rejected_filter["error"]
                .as_str()
                .is_some_and(|error| error.contains("expected applied or quarantined")),
            "non-terminal filter must be rejected: {rejected_filter}"
        );
        cg.close();
    });
}

#[test]
fn automation_outcomes_endpoint_returns_live_read_only_outcomes() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        use tracedecay_agent_hosts::automation::managed_skills::{
            ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, create_managed_skill,
            default_managed_skill_targets,
        };

        let fixture = start_dashboard_fixture(false).await;
        let profile_root = tracedecay::storage::default_profile_root()
            .unwrap_or_else(|err| panic!("expected dashboard fixture profile root: {err}"));
        create_managed_skill(
            &profile_root,
            ManagedSkillDraft {
                id: "dashboard-outcome-skill".to_string(),
                title: "Dashboard outcome skill".to_string(),
                summary: "Fixture skill for outcome endpoint coverage.".to_string(),
                category: "maintenance".to_string(),
                targets: default_managed_skill_targets(),
                body_markdown: "Use when validating the outcomes endpoint.".to_string(),
                support_files: Vec::new(),
                provenance: ManagedSkillProvenance {
                    source: ManagedSkillSource::AutomationRun,
                    actor: "tracedecay".to_string(),
                    run_id: Some("run_dashboard_outcomes".to_string()),
                },
            },
        )
        .await
        .unwrap();

        let cg = fixture
            .host_runtime
            .open_project_graph_for_test(
                &fixture.project_root,
                tracedecay::tracedecay::TraceDecayOpenOptions::default(),
            )
            .await
            .unwrap_or_else(|err| panic!("failed to reopen dashboard fixture project: {err}"));
        let applied_receipt = record_dashboard_automatic_fact(
            &cg,
            "run_dashboard_outcomes",
            "Dashboard outcome reads use automatic fact receipt authority",
        )
        .await;

        let agent = http_agent();
        let (status, payload) = get_json(
            &agent,
            &format!("{}/api/automation/outcomes", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert!(payload["generated_at"].is_number());
        assert_eq!(payload["error"], "");

        let skills = payload["skills"]
            .as_array()
            .unwrap_or_else(|| panic!("expected skill outcomes array: {payload}"));
        let skill = skills
            .iter()
            .find(|skill| skill["skill_id"] == "dashboard-outcome-skill")
            .unwrap_or_else(|| panic!("expected activated skill outcome: {payload}"));
        assert_eq!(skill["verdict"], "too_early");
        assert!(skill["activated_at"].is_number());
        assert!(skill["days_since_activation"].is_number());

        let facts = payload["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected fact outcomes array: {payload}"));
        let fact = facts
            .iter()
            .find(|fact| fact["apply_id"].as_str() == Some(applied_receipt.apply_id.as_str()))
            .unwrap_or_else(|| panic!("expected applied fact outcome: {payload}"));
        assert_eq!(
            fact["canonical_fact_id"],
            serde_json::json!(applied_receipt.canonical_fact_id)
        );
        assert_eq!(fact["run_id"], "run_dashboard_outcomes");
        assert_eq!(fact["verdict"], "never_recalled");
        assert_eq!(fact["still_exists"], true);
        assert_eq!(fact["access_count"], 0);
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
        let project_fact_id = fixture_fact_id(
            &agent,
            &fixture,
            "Cache invalidation policy must be explicit",
        );
        let tool_fact_id = fixture_fact_id(&agent, &fixture, "LCM dashboard empty states");

        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/?q=cache&limit=5&graph_limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["schema_revision"], 1);
        let overview = &overview["payload"];
        assert_eq!(overview["providers"]["memory_provider"], "tracedecay");
        assert_eq!(overview["holographic"]["overview"]["facts"], 3);
        assert_eq!(overview["holographic"]["overview"]["banks"], 3);
        assert_eq!(overview["holographic"]["overview"]["entities"], 3);
        assert_eq!(overview["holographic"]["reads"]["facts"]["state"], "ready");
        assert_eq!(overview["holographic"]["reads"]["entities"]["state"], "ready");
        assert_eq!(overview["holographic"]["reads"]["graph"]["state"], "ready");
        assert_eq!(
            overview["holographic"]["facts_coverage"]["completeness"],
            "bounded"
        );
        assert_eq!(overview["holographic"]["facts_coverage"]["limit"], 5);
        assert_eq!(
            overview["holographic"]["facts_coverage"]["query_applied_after_limit"],
            true
        );
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
        let graph_edges = overview["holographic"]["graph"]["edges"]
            .as_array()
            .unwrap_or_else(|| panic!("expected graph edges array"));
        assert!(
            graph_edges
                .iter()
                .any(|edge| edge["kind"] == "active_assertion"),
            "the mounted memory relation graph should expose canonical assertion topology"
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

        let (status, memory_status) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/status", fixture.base_url),
        );
        assert_eq!(status, 200);
        let memory_status = &memory_status["payload"];
        assert!(
            memory_status["feedback_history_repair"]["state"].is_string()
                && memory_status["feedback_history_repair"]["processed"].is_number()
                && memory_status["feedback_history_repair"]["remaining"].is_number(),
            "status must expose authoritative feedback-history repair progress: {memory_status}"
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
            .find(|point| point["fact_id"].as_i64() == Some(project_fact_id))
            .unwrap_or_else(|| panic!("expected projection point for seeded project fact"));
        assert_eq!(project_point["bank_name"], "project");
        assert!(
            project_point["bank_id"].is_null() || project_point["bank_id"].is_number(),
            "projection point may omit an unavailable legacy bank identity"
        );
        assert_eq!(project_point["entity_count"], 1);
        assert_eq!(project_point["connection_count"], 1);
        let tool_point = projection_points
            .iter()
            .find(|point| point["fact_id"].as_i64() == Some(tool_fact_id))
            .unwrap_or_else(|| panic!("expected projection point for seeded tool fact"));
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
        let tool_fact_id = fixture_fact_id(&agent, &fixture, "LCM dashboard empty states");

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
                    .find(|point| point["fact_id"].as_i64() == Some(tool_fact_id))
            })
            .unwrap_or_else(|| panic!("expected projection point for seeded tool fact"));
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
            &format!(
                "{}/api/plugins/holographic/fact/{tool_fact_id}",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(detail["domain_state"], "ready");
        let detail = &detail["payload"];
        assert_eq!(detail["error"], "");
        assert_eq!(detail["fact"]["fact_id"], tool_fact_id);
        assert_eq!(detail["fact"]["category"], "tool");
        assert_eq!(detail["fact"]["content"], LONG_FACT_CONTENT);
        assert_eq!(detail["fact"]["has_hrr"], 1);
        assert_eq!(detail["fact"]["trust_score"], 0.66);
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

        // Unknown ids are a typed empty read: HTTP 200 with a
        // complete-zero-findings envelope and no fabricated payload.
        let (status, missing) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/fact/99999", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(missing["domain_state"], "complete_zero_findings");
        assert_eq!(missing["payload"], Value::Null);
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
        let agent = http_agent();
        let tool_fact_id = fixture_fact_id(&agent, &fixture, "LCM dashboard empty states");
        let project_fact_id = fixture_fact_id(
            &agent,
            &fixture,
            "Cache invalidation policy must be explicit",
        );
        let (status, history) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/{tool_fact_id}/trust-history",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(history["error"], "");
        assert_eq!(history["fact_id"], tool_fact_id);
        let trail = history["trust_history"]
            .as_array()
            .unwrap_or_else(|| panic!("expected trust_history array: {history}"));
        assert_eq!(trail.len(), 2);
        assert!(trail[0]["timestamp"].is_number());
        assert_eq!(trail[0]["action"], "helpful");
        assert_eq!(trail[0]["old_trust"], 0.71);
        assert_eq!(trail[0]["new_trust"], 0.76);
        assert!(
            (trail[0]["delta"]
                .as_f64()
                .unwrap_or_else(|| panic!("expected numeric trust delta: {}", trail[0]))
                - 0.05)
                .abs()
                < 1e-12
        );
        assert_eq!(trail[0]["source"], "dashboard-test");
        assert_eq!(trail[0]["note"], "confirmed durable");
        assert_eq!(trail[1]["action"], "unhelpful");
        assert_eq!(trail[1]["old_trust"], 0.76);
        assert_eq!(trail[1]["new_trust"], 0.66);
        assert!(trail[1]["note"].is_null());

        let (status, empty_history) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/{project_fact_id}/trust-history",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(empty_history["fact_id"], project_fact_id);
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
        assert_eq!(overview["payload"]["exists"], true);
        assert_eq!(
            overview["payload"]["storage_scope"], "profile_sharded",
            "LCM serves the resolved project session store even when TRACEDECAY_GLOBAL_DB is set for accounting"
        );
        assert_eq!(overview["payload"]["overview"]["messages_total"], 3);
        assert_eq!(overview["payload"]["overview"]["sessions_total"], 1);
        assert_eq!(overview["payload"]["overview"]["summary_nodes_total"], 1);
        assert_eq!(
            overview["payload"]["overview"]["compression"]["source_token_count"],
            180
        );
        assert_eq!(overview["payload"]["overview"]["compression"]["token_count"], 72);
        let latest_sessions = overview["payload"]["latest_sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("expected latest_sessions array"));
        assert_eq!(latest_sessions.len(), 1);
        let matches_messages = overview["payload"]["matches"]["messages"]
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
        assert_eq!(search["payload"]["engine"], "canonical_temporal");
        let search_messages = search["payload"]["matches"]["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("expected search.matches.messages array"));
        let search_nodes = search["payload"]["matches"]["summary_nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("expected search.matches.summary_nodes array"));
        assert!(
            !search_messages.is_empty(),
            "canonical search should match seeded messages"
        );
        assert!(
            !search_nodes.is_empty(),
            "canonical search should match seeded summary nodes"
        );

        let (status, like_search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=!!!&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(like_search["payload"]["engine"], "canonical_temporal");
        assert!(
            like_search["payload"]["matches"]["messages"].is_array(),
            "a non-tokenizable query must stay a valid empty search, not an error: {like_search}"
        );
    });
}

#[test]
fn lcm_endpoints_return_empty_state_when_no_rows_exist() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture_without_memory().await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/overview?limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["payload"]["exists"], true);
        assert_eq!(overview["payload"]["overview"]["messages_total"], 0);
        assert_eq!(overview["payload"]["overview"]["summary_nodes_total"], 0);
        assert_eq!(
            overview["payload"]["latest_sessions"],
            Value::Array(Vec::new()),
            "empty LCM store should have no latest sessions"
        );
        assert_eq!(
            overview["payload"]["latest_summary_nodes"],
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
        assert_eq!(search["payload"]["engine"], "canonical_temporal");
        assert_eq!(
            search["payload"]["matches"]["messages"],
            Value::Array(Vec::new()),
            "empty LCM store search should have zero message matches"
        );
        assert_eq!(
            search["payload"]["matches"]["summary_nodes"],
            Value::Array(Vec::new()),
            "empty LCM store search should have zero summary-node matches"
        );
    });
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

        let (cg, session_store) = setup_project(&project_root).await;
        seed_lcm_fixture(&session_store, &project_root).await;

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server_with_host_runtime(
            cg,
            session_store,
            dashboard::DashboardTestProjectGraphsV1::default(),
            port,
        );

        let agent = http_agent();
        wait_for_dashboard(&agent, &base_url).await;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["lcm_scope"], "profile_sharded");
        assert_eq!(capabilities["features"]["lcm"], true);
        assert!(
            capabilities["lcm_db"]
                .as_str()
                .is_some_and(|path| !path.is_empty())
        );

        let (status, overview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/overview?limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["payload"]["storage_scope"], "profile_sharded");
        assert_eq!(overview["payload"]["exists"], true);
        assert_eq!(overview["payload"]["overview"]["messages_total"], 3);
        assert_eq!(overview["payload"]["overview"]["sessions_total"], 1);
        assert_eq!(overview["payload"]["overview"]["summary_nodes_total"], 1);
        assert!(
            overview["payload"]["path"]
                .as_str()
                .is_some_and(|path| !path.is_empty())
        );

        let (status, search) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/search?q=vector&limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(search["payload"]["storage_scope"], "profile_sharded");
        let search_messages = search["payload"]["matches"]["messages"]
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
        let (cg, session_store) = setup_project(&project_root).await;
        // The project store has rows; the overridden global accounting store has none.
        seed_lcm_fixture(&session_store, &project_root).await;

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server_with_host_runtime(
            cg,
            session_store,
            dashboard::DashboardTestProjectGraphsV1::default(),
            port,
        );

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
        assert_eq!(overview["payload"]["storage_scope"], "profile_sharded");
        assert_eq!(overview["payload"]["exists"], true);
        assert_eq!(
            overview["payload"]["overview"]["messages_total"], 3,
            "LCM must serve the project store, not the empty accounting DB"
        );
        assert!(
            overview["payload"]["path"]
                .as_str()
                .is_some_and(|path| !path.is_empty())
        );

        server.stop();
    });
}
