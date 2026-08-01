use std::path::Path;

use tracedecay_domain::{HydrationStateV1, RetrievalGrainV1, SessionId};
use tracedecay_sessions::lcm::contracts::{LcmDataFreshness, LcmRetrievalOutcome};

use super::super::super::*;
use super::super::test_support::*;
use super::*;
use crate::application::session::SessionRetrievalScope;
use crate::mcp::tools::handlers::session::message_search::LcmExpandServiceOutcome;
use crate::sessions::lcm::{LcmContentRange, LcmExpandResponse};

#[tokio::test]
async fn malformed_expand_query_selectors_never_call_the_service() {
    for (args, field) in [
        (
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "prompt": "question",
                "query": false,
                "format": "json"
            }),
            "query",
        ),
        (
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "prompt": "question",
                "node_ids": [7],
                "format": "json"
            }),
            "node_ids",
        ),
    ] {
        let service = RecordingService::new(complete("unused", "user", None));
        let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
        let error = handle_lcm_expand_query(context, args).await.unwrap_err();
        assert!(error.to_string().contains(field), "{error}");
        assert_eq!(service.calls(), 0);
        assert_eq!(service.expand_calls(), 0);
    }
}

#[tokio::test]
async fn expand_query_translates_search_through_the_retrieval_service() {
    let service = RecordingService::new(complete(
        "canonical context only",
        "assistant",
        Some("expand-query-next"),
    ));
    let response = payload(
        handle_lcm_expand_query(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "prompt": "What did we decide?",
                "query": "decision",
                "max_results": 3,
                "max_tokens": 512,
                "context_max_tokens": 4096,
                "cursor": "expand-query-current",
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );

    let command = service.command();
    assert_eq!(
        command.query().retrieval_scope(),
        &SessionRetrievalScope::Session(SessionId::new("session-exact").unwrap())
    );
    assert_eq!(command.query().provider(), Some("claude"));
    assert_eq!(command.query().query(), "decision");
    assert_eq!(command.query().grain(), RetrievalGrainV1::Occurrence);
    assert_eq!(command.query().limit(), 3);
    assert_eq!(command.query().context_budget().max_tokens, 4096);
    assert_eq!(command.query().cursor(), Some("expand-query-current"));
    assert_eq!(service.calls(), 1);
    assert_eq!(response["status"], "ok");
    assert_eq!(response["needs_synthesis"], true);
    assert_eq!(
        response["context_blocks"][0]["content"],
        "canonical context only"
    );
    assert_eq!(response["next_cursor"], "expand-query-next");
    assert_eq!(response["source_coverage"][0]["source_id"], "claude");
}

#[tokio::test]
async fn expand_query_translates_node_ids_through_summary_expansion() {
    let service = RecordingService::new(complete("unused", "assistant", None));
    service.set_expand_outcome(LcmExpandServiceOutcome::Complete {
        expansion: LcmExpandResponse {
            kind: "summary_node".to_string(),
            content: "canonical summary context".to_string(),
            content_range: LcmContentRange {
                offset: 0,
                limit: 4096,
                returned_chars: 25,
                total_chars: 25,
                truncated: false,
            },
            raw_message: None,
            summary_node: None,
            summary_sources: Vec::new(),
            payload_ref: None,
            from_current_session: Some(true),
            externalized_note: None,
            source_pagination: None,
        },
        temporal: temporal(None),
        grain: RetrievalGrainV1::Summary,
        state: HydrationStateV1::Available,
        retrieval: LcmRetrievalOutcome::complete(LcmDataFreshness::Fresh),
    });
    let response = payload(
        handle_lcm_expand_query(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "prompt": "What did we decide?",
                "node_ids": ["summary-1", "summary-2"],
                "max_results": 1,
                "context_max_tokens": 4096,
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );

    assert_eq!(service.calls(), 0);
    assert_eq!(service.expand_calls(), 1);
    assert!(matches!(
        service.expand_command().target(),
        LcmExpandTarget::SummaryNode { node_id } if node_id == "summary-1"
    ));
    assert_eq!(response["status"], "partial");
    assert_eq!(response["omitted"], 1);
    assert_eq!(response["node_ids"], json!(["summary-1"]));
    assert_eq!(
        response["context_blocks"][0]["content"],
        "canonical summary context"
    );
}

#[tokio::test]
async fn expand_query_omits_typed_unavailable_summary_sources() {
    let service = RecordingService::new(complete("unused", "assistant", None));
    let source = |store_id, state, content: &str| crate::sessions::lcm::LcmExpandedSummarySource {
        source_ref: LcmSourceRef::RawMessage { store_id },
        state,
        content: content.to_string(),
        content_range: (state == HydrationStateV1::Available).then_some(LcmContentRange {
            offset: 0,
            limit: 4096,
            returned_chars: content.chars().count() as u64,
            total_chars: content.chars().count() as u64,
            truncated: false,
        }),
        content_truncated: false,
        raw_message: None,
        summary_node: None,
    };
    service.set_expand_outcome(LcmExpandServiceOutcome::Partial {
        expansion: Some(LcmExpandResponse {
            kind: "summary_node".to_string(),
            content: "canonical summary".to_string(),
            content_range: LcmContentRange {
                offset: 0,
                limit: 4096,
                returned_chars: 17,
                total_chars: 17,
                truncated: false,
            },
            raw_message: None,
            summary_node: None,
            summary_sources: vec![
                source(1, HydrationStateV1::Available, "visible source"),
                source(2, HydrationStateV1::Redacted, ""),
                source(3, HydrationStateV1::Unauthorized, ""),
                source(4, HydrationStateV1::Deleted, ""),
            ],
            payload_ref: None,
            from_current_session: None,
            externalized_note: None,
            source_pagination: None,
        }),
        temporal: temporal(None),
        grain: RetrievalGrainV1::Summary,
        state: Some(HydrationStateV1::Available),
        retrieval: LcmRetrievalOutcome::partial(LcmDataFreshness::Fresh, 3),
    });

    let direct = payload(
        handle_lcm_expand(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "target": {"kind": "summary_node", "node_id": "summary-1"},
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );
    assert_eq!(direct["status"], "partial", "{direct}");
    assert_eq!(direct["omitted"], 3, "{direct}");
    assert_eq!(
        direct["expansion"]["summary_sources"][1]["state"],
        "redacted"
    );
    assert_eq!(
        direct["expansion"]["summary_sources"][2]["state"],
        "unauthorized"
    );
    assert_eq!(
        direct["expansion"]["summary_sources"][3]["state"],
        "deleted"
    );

    let response = payload(
        handle_lcm_expand_query(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "prompt": "Recover visible context",
                "node_ids": ["summary-1"],
                "max_results": 4,
                "context_max_tokens": 4096,
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );

    assert_eq!(response["status"], "partial", "{response}");
    assert_eq!(response["omitted"], 3, "{response}");
    assert!(
        response["context_blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["content"] == "visible source"),
        "{response}"
    );
    assert!(
        response["context_blocks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| !block["content"].as_str().unwrap_or_default().is_empty()),
        "{response}"
    );
    assert_eq!(
        response["context_pagination"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["state"].as_str())
            .collect::<Vec<_>>(),
        vec!["available", "redacted", "unauthorized", "deleted"]
    );
}

#[tokio::test]
async fn expand_query_forwards_single_node_cursor_to_canonical_expansion() {
    let service = RecordingService::new(complete("unused", "assistant", None));
    service.set_expand_outcome(LcmExpandServiceOutcome::Complete {
        expansion: LcmExpandResponse {
            kind: "summary_node".to_string(),
            content: "continued context".to_string(),
            content_range: LcmContentRange {
                offset: 0,
                limit: 4096,
                returned_chars: 17,
                total_chars: 17,
                truncated: false,
            },
            raw_message: None,
            summary_node: None,
            summary_sources: Vec::new(),
            payload_ref: None,
            from_current_session: Some(true),
            externalized_note: None,
            source_pagination: None,
        },
        temporal: temporal(None),
        grain: RetrievalGrainV1::Summary,
        state: HydrationStateV1::Available,
        retrieval: LcmRetrievalOutcome::complete(LcmDataFreshness::Fresh),
    });

    handle_lcm_expand_query(
        LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
        json!({
            "provider": "claude",
            "session_id": "session-exact",
            "prompt": "Continue",
            "node_ids": ["summary-1"],
            "cursor": "expand-query-node-current",
            "format": "json"
        }),
    )
    .await
    .unwrap();

    assert_eq!(
        service.expand_command().cursor(),
        Some("expand-query-node-current")
    );
}

#[test]
fn expand_query_response_bounds_oversized_prompt_and_query_before_synthesis() {
    let prompt = "p".repeat(3_000);
    let query = "q".repeat(2_000);
    let (response, _) = expand_query_response_from_sources(
        &prompt,
        Some(&query),
        128,
        128,
        vec![("raw_message", None, "bounded context".to_string())],
    );

    assert_eq!(response.prompt.chars().count(), 2_048);
    assert_eq!(response.query.as_deref().unwrap().chars().count(), 1_024);
    let synthesis = response.synthesis_prompt.expect("response needs synthesis");
    assert!(synthesis.user.contains(&response.prompt));
    assert!(!synthesis.user.contains(&prompt));
}
