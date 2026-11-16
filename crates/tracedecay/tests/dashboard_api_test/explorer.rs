use crate::dashboard_api_support::*;
use serde_json::{Value, json};

const EXPLORER_QUERY_ROUTE: &str = "/api/explorer/queries";

#[test]
fn explorer_query_coordinates_real_sources_without_inventing_a_merge() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let agent = http_agent();

        let (status, frames) = post_explorer_stream(
            &agent,
            &format!("{}{EXPLORER_QUERY_ROUTE}", fixture.base_url),
            &json!({"query": "cache", "limit": 25, "offset": 0}),
        );
        assert_eq!(status, 200, "query should stream: {frames:?}");
        assert_eq!(frames.len(), 4, "three sources and one terminal event");
        assert!(frames[..3].iter().all(|frame| frame["kind"] == "source"));
        let completed = &frames[3];
        // Every source answers, but the canonical temporal search carries no
        // denominator, so the run truthfully stays partial rather than
        // claiming complete coverage it cannot account for.
        assert_eq!(completed["kind"], "final");
        assert_eq!(completed["state"], "partial");
        let sources: Vec<&Value> = frames[..3].iter().map(|frame| &frame["source"]).collect();
        assert_eq!(sources.len(), 3);
        for source_id in ["code_graph", "sessions", "knowledge"] {
            let source = sources
                .iter()
                .find(|source| source["source_id"] == source_id)
                .unwrap_or_else(|| panic!("missing {source_id} source: {completed}"));
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
        assert_eq!(
            sessions["coverage"]["completeness"], "unknown",
            "the temporal search is the only source without a denominator: {sessions}"
        );
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
            completed.get("merged_results").is_none(),
            "the coordinator must not invent a cross-source merge"
        );
    });
}

#[test]
fn explorer_session_routes_reuse_lcm_size_and_read_context_authority() {
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
