use std::time::Duration;

use crate::dashboard_api_support::*;
use serde_json::{Value, json};

fn wait_for_query(agent: &ureq::Agent, base_url: &str, run_id: &str) -> Value {
    for _ in 0..100 {
        let (status, body) = get_json(agent, &format!("{base_url}/api/explorer/queries/{run_id}"));
        assert_eq!(status, 200, "query status should resolve: {body}");
        if matches!(
            body["payload"]["state"].as_str(),
            Some("completed" | "partial" | "cancelled" | "error")
        ) {
            return body;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("explorer query {run_id} did not reach a terminal state");
}

#[test]
fn explorer_query_coordinates_real_sources_without_inventing_a_merge() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let agent = http_agent();

        let (status, accepted) = post_json_body(
            &agent,
            &format!("{}/api/explorer/queries", fixture.base_url),
            &json!({"query": "cache", "limit": 25, "offset": 0}),
        );
        assert_eq!(status, 202, "query should be admitted: {accepted}");
        assert_eq!(accepted["schema_revision"], 1);
        assert_eq!(accepted["domain_state"], "loading");
        assert_eq!(accepted["payload"]["state"], "pending");
        assert_eq!(
            accepted["payload"]["required_source_ids"],
            json!(["code_graph", "sessions", "knowledge"])
        );
        assert_eq!(
            accepted["payload"]["ordering_policy"],
            "source_local_no_cross_source_merge"
        );
        let run_id = accepted["payload"]["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("accepted query needs a run id: {accepted}"));

        let completed = wait_for_query(&agent, &fixture.base_url, run_id);
        assert_eq!(completed["domain_state"], "partial");
        assert_eq!(completed["payload"]["state"], "partial");
        assert_eq!(completed["payload"]["finality"], "partial");
        let sources = completed["payload"]["sources"]
            .as_array()
            .unwrap_or_else(|| panic!("query sources missing: {completed}"));
        assert_eq!(sources.len(), 3);
        for source_id in ["code_graph", "sessions", "knowledge"] {
            let source = sources
                .iter()
                .find(|source| source["source_id"] == source_id)
                .unwrap_or_else(|| panic!("missing {source_id} source: {completed}"));
            assert_eq!(source["phase"], "completed");
            assert_eq!(
                source["outcome"], "ready",
                "{source_id} source must be ready: {source}"
            );
            assert!(
                source["page"]["rows"].is_array(),
                "{source_id} must expose its real source-local page: {source}"
            );
        }
        let sessions = sources
            .iter()
            .find(|source| source["source_id"] == "sessions")
            .expect("sessions source");
        assert!(
            sessions["page"]["rows"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| {
                    row["content"]
                        .as_str()
                        .is_some_and(|content| content.contains("cache"))
                })),
            "session page should contain the seeded cache result: {sessions}"
        );
        let knowledge = sources
            .iter()
            .find(|source| source["source_id"] == "knowledge")
            .expect("knowledge source");
        // The final-v2 memory floor reports genuine coverage: a complete fact
        // page with complete verified-graph coverage carries an exact total.
        assert_eq!(knowledge["coverage"]["completeness"], "complete");
        let knowledge_rows = knowledge["page"]["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("knowledge rows missing: {knowledge}"))
            .len() as u64;
        assert_eq!(
            knowledge["total_units"].as_u64(),
            Some(knowledge_rows),
            "complete knowledge coverage must carry the exact unit total: {knowledge}"
        );
        assert!(
            knowledge["page"]["rows"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| {
                    row["content"]
                        .as_str()
                        .is_some_and(|content| content.contains("Cache invalidation"))
                })),
            "knowledge page should contain the seeded cache fact: {knowledge}"
        );
        assert!(
            completed["payload"].get("merged_results").is_none(),
            "the coordinator must not invent a cross-source merge"
        );
    });
}

#[test]
fn explorer_session_routes_reuse_lcm_size_and_read_context_authority() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let agent = http_agent();

        let (status, size) = get_json(
            &agent,
            &format!(
                "{}/api/explorer/sessions/sess-dashboard-1/size",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200, "session size should resolve: {size}");
        assert_eq!(size["schema_revision"], 1);
        assert_eq!(
            size["domain_state"], "ready",
            "session size read must be ready: {size}"
        );
        assert_eq!(size["payload"]["session_id"], "sess-dashboard-1");
        assert_eq!(size["payload"]["counts"]["message_count"], 3);
        assert!(
            size["payload"]["counts"]["token_estimate_total"]
                .as_i64()
                .is_some_and(|tokens| tokens > 0),
            "session size must retain the LCM token estimate: {size}"
        );

        let (status, context) = get_json(
            &agent,
            &format!(
                "{}/api/explorer/sessions/sess-dashboard-1/read-context?limit=2&offset=0&order=asc",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200, "read context should resolve: {context}");
        assert_eq!(context["domain_state"], "partial");
        assert_eq!(context["payload"]["session_id"], "sess-dashboard-1");
        assert_eq!(
            context["payload"]["messages"]
                .as_array()
                .map_or(0, Vec::len),
            2
        );
        assert_eq!(context["payload"]["has_more_messages"], true);
        assert_eq!(
            context["coverage"]["completeness"], "partial",
            "pagination must stay visible as partial coverage: {context}"
        );
    });
}
