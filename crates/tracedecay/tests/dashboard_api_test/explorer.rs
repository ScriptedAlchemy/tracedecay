use crate::dashboard_api_support::*;
use serde_json::{Value, json};

/// One search stream, read to its close: the response status and every
/// `event:`/`data:` frame in arrival order.
fn search_stream(agent: &ureq::Agent, base_url: &str, body: &Value) -> (u16, Vec<(String, Value)>) {
    search_stream_at(agent, &format!("{base_url}/api/explorer/queries"), body)
}

fn search_stream_at(agent: &ureq::Agent, url: &str, body: &Value) -> (u16, Vec<(String, Value)>) {
    let mut response = crate::common::http_call_with_retry(&format!("POST {url} (stream)"), || {
        agent.post(url).send_json(body)
    });
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|error| panic!("search stream body unreadable: {error}"));
    if status != 200 {
        return (
            status,
            vec![(
                "error".to_owned(),
                text.parse::<Value>().unwrap_or(Value::Null),
            )],
        );
    }
    let frames = text
        .split("\n\n")
        .filter(|frame| frame.contains("event:"))
        .map(|frame| {
            let field = |name: &str| {
                frame
                    .lines()
                    .find_map(|line| line.strip_prefix(name))
                    .map(str::trim)
                    .unwrap_or_else(|| panic!("frame lacks {name}: {frame}"))
            };
            let data: Value = serde_json::from_str(field("data:"))
                .unwrap_or_else(|error| panic!("frame data is not JSON ({error}): {frame}"));
            (field("event:").to_owned(), data)
        })
        .collect();
    (status, frames)
}

#[test]
fn explorer_search_streams_real_sources_without_inventing_a_merge() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let agent = http_agent();

        let (status, frames) = search_stream(
            &agent,
            &fixture.base_url,
            &json!({"query": "cache", "limit": 25}),
        );
        assert_eq!(status, 200, "search should stream: {frames:?}");
        let (last_event, last) = frames.last().expect("the stream ends with a frame");
        assert_eq!(last_event, "final", "{frames:?}");
        assert_eq!(
            frames.iter().filter(|(event, _)| event == "final").count(),
            1
        );
        // Every source answers, but the canonical temporal search carries no
        // denominator, so the search truthfully stays partial rather than
        // claiming complete coverage it cannot account for.
        assert_eq!(last["outcome"], "partial", "{last}");
        assert!(last.get("merged_results").is_none());

        let sources: Vec<&Value> = frames
            .iter()
            .filter(|(event, _)| event == "source")
            .map(|(_, data)| &data["source"])
            .collect();
        assert_eq!(
            sources.len(),
            3,
            "each source publishes exactly once: {frames:?}"
        );
        for source_id in ["code_graph", "sessions", "knowledge"] {
            let source = sources
                .iter()
                .find(|source| source["source_id"] == source_id)
                .unwrap_or_else(|| panic!("missing {source_id} source: {frames:?}"));
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
    });
}

#[test]
fn explorer_search_leaves_no_result_registry_to_poll_or_exhaust() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let agent = http_agent();

        // Exceed the retired 256-result capacity with distinct requests.
        // Every response must own and release its result, not consume a slot.
        for round in 0..300 {
            let (status, frames) = search_stream(
                &agent,
                &fixture.base_url,
                &json!({"query": format!("cache {round}"), "limit": 1}),
            );
            assert_eq!(status, 200, "round {round}: {frames:?}");
            assert_eq!(
                frames.iter().filter(|(event, _)| event == "source").count(),
                3,
                "round {round} must release all three source reads: {frames:?}"
            );
            assert_eq!(
                frames.last().map(|(event, _)| event.as_str()),
                Some("final")
            );
        }
        for method_url in [format!(
            "{}/api/explorer/queries/explorer-run.00",
            fixture.base_url
        )] {
            let response = crate::common::http_call_with_retry("retired explorer status", || {
                agent.get(&method_url).call()
            });
            let status = response.status().as_u16();
            assert!(
                matches!(status, 404 | 405),
                "no per-search status route remains: {status}"
            );
            let response = crate::common::http_call_with_retry("retired explorer cancel", || {
                agent.delete(&method_url).call()
            });
            assert!(
                matches!(response.status().as_u16(), 404 | 405),
                "no per-search cancel route remains"
            );
        }
    });
}

#[test]
fn explorer_search_rejects_an_empty_query_and_an_unsupported_offset() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let agent = http_agent();

        let (status, _) = search_stream(&agent, &fixture.base_url, &json!({"query": "  "}));
        assert_eq!(status, 400);
        let (status, _) = search_stream(
            &agent,
            &fixture.base_url,
            &json!({"query": "cache", "offset": 5}),
        );
        assert!(
            matches!(status, 400 | 422),
            "a request carrying a retired offset field is refused: {status}"
        );
    });
}

#[test]
fn explorer_search_requires_listener_authentication_and_same_origin_admission() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let agent = http_agent();
        let (_, authority) = fixture.base_url.rsplit_once('@').expect("fixture access");
        let (status, frames) = search_stream_at(
            &agent,
            &format!("http://{authority}/api/explorer/queries"),
            &json!({"query": "cache"}),
        );
        assert_eq!(status, 401, "{frames:?}");
        assert!(frames.iter().all(|(event, _)| event == "error"));

        let response = agent
            .post(&format!("{}/api/explorer/queries", fixture.base_url))
            .header("Origin", "http://attacker.example")
            .send_json(json!({"query": "cache"}))
            .expect("cross-origin request returns a refusal");
        let (status, refusal) = response_to_json(response);
        assert_eq!(status, 403, "{refusal}");
        assert_eq!(refusal["error"], "dashboard_request_forbidden");
    });
}

#[test]
fn explorer_search_for_a_non_active_project_is_refused_at_the_gateway() {
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let agent = http_agent();
        let (_target_root, target_cg) = crate::projects::setup_target_project(&fixture).await;
        let target_project_id = crate::projects::project_id(&target_cg);

        let (status, frames) = search_stream_at(
            &agent,
            &format!(
                "{}/api/projects/{target_project_id}/explorer/queries",
                fixture.base_url
            ),
            &json!({"query": "target"}),
        );
        assert_eq!(status, 405, "{frames:?}");
        assert!(
            frames.iter().all(|(event, _)| event == "error"),
            "a refused search streams no source frame: {frames:?}"
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
