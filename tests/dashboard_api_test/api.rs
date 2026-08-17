use crate::dashboard_api_support::*;
use serde_json::json;

fn absent_canonical_fact_id(existing: &FactId) -> String {
    let raw = existing.as_str();
    let replacement = if raw.ends_with('0') { '1' } else { '0' };
    format!("{}{replacement}", &raw[..raw.len() - 1])
}

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
            serde_json::json!(applied_receipt.fact_id)
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
        assert_eq!(overview["holographic"]["overview"]["entities"], 3);
        assert_eq!(overview["holographic"]["reads"]["facts"]["state"], "ready");
        assert_eq!(
            overview["holographic"]["reads"]["entities"]["state"],
            "ready"
        );
        assert_eq!(overview["holographic"]["reads"]["graph"]["state"], "ready");
        assert_eq!(
            overview["holographic"]["facts_coverage"]["completeness"],
            "complete"
        );
        assert_eq!(overview["holographic"]["facts_coverage"]["limit"], 5);
        assert_eq!(
            overview["holographic"]["facts_coverage"]["graph"]["kind"], "complete",
            "ranked fact search must report its mounted graph-assist coverage: {overview}"
        );
        assert!(
            overview["holographic"]["facts_coverage"]["graph"]["root_count"].is_number()
                && overview["holographic"]["facts_coverage"]["graph"]["relation_count"].is_number()
                && overview["holographic"]["facts_coverage"]["graph"]["expanded_fact_count"]
                    .is_number(),
            "complete graph-assist coverage carries its measured counts: {overview}"
        );

        let (status, entity_bounded) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/?limit=1&graph_limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(entity_bounded["domain_state"], "partial");
        assert_eq!(
            entity_bounded["payload"]["holographic"]["reads"]["entities"]["state"],
            "partial"
        );
        assert_eq!(
            entity_bounded["payload"]["holographic"]["reads"]["entities"]["code"],
            "entity_limit_reached"
        );
        assert_eq!(
            entity_bounded["payload"]["holographic"]["entities"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let facts = overview["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected facts array in overview payload"));
        assert_eq!(facts.len(), 2, "query should filter to cache facts only");
        assert!(
            facts.iter().all(|fact| {
                fact["fact_id"]
                    .as_str()
                    .is_some_and(|raw| FactId::new(raw.to_owned()).is_ok())
                    && fact["payload_access"] == "eligible"
                    && fact["access_count"].is_number()
                    && fact.get("last_recalled_at").is_some()
            }),
            "fact rows must expose canonical identities and exact payload availability: {facts:?}"
        );
        let graph_nodes = overview["holographic"]["graph"]["nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("expected graph nodes array"));
        assert!(
            graph_nodes.iter().any(|node| node["kind"] == "entity"),
            "graph should include entity nodes"
        );
        let graph_fact_nodes = graph_nodes
            .iter()
            .filter(|node| node["kind"] == "fact")
            .collect::<Vec<_>>();
        assert!(
            !graph_fact_nodes.is_empty()
                && graph_fact_nodes.iter().all(|node| node["fact_id"]
                    .as_str()
                    .is_some_and(|raw| FactId::new(raw.to_owned()).is_ok())),
            "every Grafeo fact node must preserve canonical identity: {graph_nodes:?}"
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
        assert_eq!(
            overview["holographic"]["graph"]["coverage"]["completeness"],
            "complete"
        );
        assert_eq!(
            overview["holographic"]["graph"]["coverage"]["omission_reasons"],
            Value::Array(Vec::new())
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
        assert_eq!(memory_status["memory"]["fact_count"], 3);
        assert_eq!(memory_status["memory"]["entity_count"], 3);
        assert_eq!(memory_status["memory"]["algebra"]["name"], "amari_fhrr");
        assert_eq!(memory_status["memory"]["algebra"]["hrr_dim"], 2048);

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
        assert_eq!(projection["coverage"]["completeness"], "complete");
        assert_eq!(projection["coverage"]["limit"], 2000);
        assert!(projection["coverage"]["examined"].is_number());
        assert_eq!(projection["coverage"]["omission_reasons"], json!([]));
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
            .find(|point| point["fact_id"] == project_fact_id.as_str())
            .unwrap_or_else(|| panic!("expected projection point for seeded project fact"));
        assert_eq!(project_point["entity_count"], 1);
        assert_eq!(project_point["payload_access"], "eligible");
        let tool_point = projection_points
            .iter()
            .find(|point| point["fact_id"] == tool_fact_id.as_str())
            .unwrap_or_else(|| panic!("expected projection point for seeded tool fact"));
        assert_eq!(tool_point["entity_count"], 2);
        assert!(projection_points.iter().all(|point| {
            point["fact_id"]
                .as_str()
                .is_some_and(|raw| FactId::new(raw.to_owned()).is_ok())
        }));

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
        assert!(
            pairs.iter().all(|pair| {
                ["a_id", "b_id"].iter().all(|field| {
                    pair[field]
                        .as_str()
                        .is_some_and(|raw| FactId::new(raw.to_owned()).is_ok())
                })
            }),
            "similarity must preserve canonical fact identities: {pairs:?}"
        );
        let high_similarity_pair = pairs
            .iter()
            .find(|pair| pair["classification"] == "high_similarity")
            .unwrap_or_else(|| panic!("expected high_similarity pair: {pairs:?}"));
        let high_similarity = high_similarity_pair["similarity"]
            .as_f64()
            .unwrap_or_else(|| panic!("expected numeric similarity"));
        let rounded_similarity = (high_similarity * 10_000.0).round() / 10_000.0;
        assert!(
            (high_similarity - rounded_similarity).abs() > f64::EPSILON,
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
                .any(|pair| pair["classification"] == "high_similarity"),
            "fixture vectors should produce a high_similarity pair"
        );
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
                    .find(|point| point["fact_id"] == tool_fact_id.as_str())
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
        assert_eq!(
            detail["fact"]["fact_id"].as_str(),
            Some(tool_fact_id.as_str())
        );
        assert_eq!(detail["fact"]["category"], "tool");
        assert_eq!(detail["fact"]["content"], LONG_FACT_CONTENT);
        assert_eq!(
            detail["fact"]["entities"],
            serde_json::json!(["LCMTab", "SimilarityView"]),
            "canonical fact payload entities stay normalized strings"
        );
        assert_eq!(detail["fact"]["trust_score"], 0.66);
        assert!(
            detail["fact"]["access_count"].is_number(),
            "fact detail must surface access_count"
        );
        assert!(
            detail["fact"].get("last_recalled_at").is_some(),
            "fact detail must surface last_recalled_at"
        );
        let entities = detail["fact"]["linked_entities"]
            .as_array()
            .unwrap_or_else(|| panic!("expected linked_entities array in fact detail: {detail}"));
        let entity_names: Vec<&str> = entities
            .iter()
            .filter_map(|entity| entity["name"].as_str())
            .collect();
        assert_eq!(
            entity_names,
            vec!["LCMTab", "SimilarityView"],
            "fact detail must list linked entities sorted by name"
        );

        let missing_fact_id = absent_canonical_fact_id(&tool_fact_id);
        FactId::new(missing_fact_id.clone())
            .unwrap_or_else(|error| panic!("missing fixture id must remain canonical: {error}"));
        let (status, missing) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/{missing_fact_id}",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(missing["domain_state"], "complete_zero_findings");
        assert_eq!(missing["payload"], Value::Null);

        let (status, numeric) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/fact/99999", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(numeric["domain_state"], "error");
        assert!(
            numeric["coverage"]["omission_reasons"]
                .as_array()
                .is_some_and(|reasons| reasons.iter().any(|reason| reason
                    .as_str()
                    .is_some_and(|reason| reason.contains("invalid canonical fact id")))),
            "numeric legacy identity must fail closed: {numeric}"
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
        assert_eq!(history["fact_id"].as_str(), Some(tool_fact_id.as_str()));
        assert_eq!(history["limit"], 300);
        assert_eq!(history["completeness"], "complete");
        assert!(history["next_after"].is_null());
        assert!(history.get("repair").is_none());
        let trail = history["trust_history"]
            .as_array()
            .unwrap_or_else(|| panic!("expected trust_history array: {history}"));
        assert_eq!(trail.len(), 2);
        assert!(trail.iter().all(|entry| entry["event_id"].is_string()));
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
        assert_eq!(trail[0]["details_availability"], "available");
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
        assert_eq!(
            empty_history["fact_id"].as_str(),
            Some(project_fact_id.as_str())
        );
        assert_eq!(
            empty_history["trust_history"]
                .as_array()
                .map(|rows| rows.len()),
            Some(0)
        );
        assert_eq!(empty_history["completeness"], "complete");
        assert!(empty_history["next_after"].is_null());

        let missing_fact_id = absent_canonical_fact_id(&project_fact_id);
        FactId::new(missing_fact_id.clone())
            .unwrap_or_else(|error| panic!("missing fixture id must remain canonical: {error}"));
        let (status, missing) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/{missing_fact_id}/trust-history",
                fixture.base_url,
            ),
        );
        assert_eq!(status, 404);
        assert!(
            missing["detail"]
                .as_str()
                .unwrap_or_default()
                .contains(&missing_fact_id),
            "404 body should carry the requested fact id"
        );

        let (status, numeric) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/99999/trust-history",
                fixture.base_url
            ),
        );
        assert_eq!(status, 400);
        assert!(
            numeric["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("invalid canonical fact id")),
            "numeric legacy identity must fail closed: {numeric}"
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
        assert_eq!(
            overview["payload"]["exists"], true,
            "seeded LCM overview must serve a ready payload: {overview}"
        );
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
        assert_eq!(
            overview["payload"]["storage_scope"], "profile_sharded",
            "project-store LCM overview must serve a ready payload: {overview}"
        );
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
        assert_eq!(
            overview["payload"]["storage_scope"], "profile_sharded",
            "override-pinned LCM overview must serve a ready payload: {overview}"
        );
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
