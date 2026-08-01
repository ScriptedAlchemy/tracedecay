use std::path::Path;

use tempfile::TempDir;
use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalModeV1, UtcMicros};

use super::super::super::*;
use super::super::test_support::*;
use super::*;
use crate::application::session::{SessionDataFreshness, SessionRetrievalScope};
use crate::mcp::tools::handlers::session::message_search::{
    SessionRetrievalPageView, SessionRetrievalServiceOutcome,
};

#[tokio::test]
async fn load_maps_exact_forensic_occurrence_and_preserves_legacy_keys() {
    let service = RecordingService::new(complete("a😀界bc", "assistant", Some("opaque-next")));
    let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
    let response = handle_lcm_load_session(
        context,
        json!({
            "provider": "claude",
            "session_id": "session-exact",
            "roles": ["assistant"],
            "start_time": 10,
            "end_time": 30,
            "cursor": "opaque-current",
            "limit": 7,
            "content_offset": 1,
            "content_limit": 2,
            "format": "json"
        }),
    )
    .await
    .unwrap();

    let command = service.command();
    assert_eq!(
        command.query().retrieval_scope(),
        &SessionRetrievalScope::Session(SessionId::new("session-exact").unwrap())
    );
    assert_eq!(command.query().provider(), Some("claude"));
    assert_eq!(command.query().query(), "");
    assert_eq!(command.query().cursor(), Some("opaque-current"));
    assert_eq!(command.query().temporal_mode(), TemporalModeV1::Forensic);
    assert_eq!(command.query().grain(), RetrievalGrainV1::Occurrence);
    assert_eq!(command.query().limit(), 7);
    assert_eq!(command.filters().roles, ["assistant"]);
    assert_eq!(command.filters().time_range.start_time, Some(10));
    assert_eq!(command.filters().time_range.end_time, Some(30));

    let response = payload(response);
    assert_eq!(response["messages"][0]["content"], "😀界");
    assert_eq!(response["messages"][0]["content_range"]["offset"], 1);
    assert_eq!(
        response["messages"][0]["content_range"]["returned_chars"],
        2
    );
    assert_eq!(response["messages"][0]["content_range"]["total_chars"], 5);
    assert_eq!(response["next_cursor"], "opaque-next");
    assert!(response["anchors"].is_array());
    assert!(response["watermarks"].is_object());
    assert_eq!(response["watermarks"]["generation"], 9);
    assert!(response["coverage"].is_object());
    assert!(response["explanations"].is_array());
}

#[tokio::test]
async fn load_preserves_the_kernel_page_order_bound_to_its_cursor() {
    let first = result("kernel-first", "assistant");
    let mut second = result("kernel-second", "assistant");
    second.message.message_id = "message-2".to_string();
    second.message.timestamp = Some(30);
    second.message.ordinal = 4;
    let service = RecordingService::new(SessionRetrievalServiceOutcome::Complete {
        page: SessionRetrievalPageView {
            results: vec![first, second],
            temporal: temporal(Some("opaque-next")),
        },
        freshness: SessionDataFreshness::Fresh,
    });
    let response = payload(
        handle_lcm_load_session(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({
                "session_id": "session-exact",
                "cursor": "opaque-current",
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );

    assert_eq!(response["messages"][0]["content"], "kernel-first");
    assert_eq!(response["messages"][1]["content"], "kernel-second");
    assert_eq!(response["next_cursor"], "opaque-next");
}

#[tokio::test]
async fn grep_preserves_exact_phrase_cjk_emoji_and_maps_exact_session_filters() {
    let query = "\"exact phrase\" 精确 😀";
    let service = RecordingService::new(complete(query, "user", Some("grep-next")));
    let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
    let response = handle_lcm_grep(
        context,
        json!({
            "query": query,
            "provider": "claude",
            "scope": "session",
            "session_id": "session-exact",
            "relationship_scope": "parents_only",
            "message_type": "direct_user",
            "role": "user",
            "cursor": "grep-current",
            "include_summaries": false,
            "sort": "relevance",
            "format": "json"
        }),
    )
    .await
    .unwrap();

    let command = service.command();
    assert_eq!(command.query().query(), query);
    assert_eq!(command.query().cursor(), Some("grep-current"));
    assert_eq!(command.query().temporal_mode(), TemporalModeV1::Current);
    assert_eq!(command.query().grain(), RetrievalGrainV1::Occurrence);
    assert_eq!(
        command.query().retrieval_scope(),
        &SessionRetrievalScope::Session(SessionId::new("session-exact").unwrap())
    );
    assert_eq!(command.filters().scope, SessionSearchScope::ParentsOnly);
    assert_eq!(
        command.filters().message_type,
        SessionMessageType::DirectUser
    );
    assert_eq!(command.filters().roles, ["user"]);

    let response = payload(response);
    assert_eq!(response["hits"][0]["snippet"], query);
    assert_eq!(response["capped_sessions"], json!({}));
    assert_eq!(response["next_cursor"], "grep-next");
}

#[tokio::test]
async fn grep_binds_summary_source_as_of_and_renders_stable_summary_hits() {
    let service = RecordingService::new(SessionRetrievalServiceOutcome::Complete {
        page: SessionRetrievalPageView {
            results: vec![summary_result(
                "current canonical summary",
                "summary-successor",
            )],
            temporal: temporal(Some("summary-next")),
        },
        freshness: SessionDataFreshness::Fresh,
    });
    let response = payload(
        handle_lcm_grep(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({
                "query": "canonical summary",
                "provider": "claude",
                "scope": "session",
                "session_id": "session-exact",
                "include_summaries": true,
                "source": "claude",
                "temporal_mode": "as_of",
                "as_of_micros": 1234,
                "cursor": "summary-current",
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );

    let command = service.command();
    assert_eq!(
        command.query().temporal_mode(),
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(1234)
        }
    );
    assert_eq!(command.query().cursor(), Some("summary-current"));
    assert_eq!(command.filters().source.as_deref(), Some("claude"));
    assert!(command.filters().include_summaries);
    assert_eq!(
        command.query().semantic_filter().source.as_deref(),
        Some("claude")
    );
    assert!(command.query().semantic_filter().include_summaries);

    assert_eq!(response["hits"][0]["kind"], "summary_node");
    assert_eq!(response["hits"][0]["node_id"], "summary-successor");
    assert!(response["hits"][0]["message_id"].is_null());
    assert!(response["hits"][0]["role"].is_null());
    assert_eq!(response["next_cursor"], "summary-next");
    assert_eq!(response["anchors"][0], "anchor.compatibility.1");
}

#[tokio::test]
async fn grep_missing_profile_store_is_unavailable_without_db_fallback() {
    let temp = TempDir::new().unwrap();
    let missing_path = temp.path().join("sessions.db");
    let response = payload(
        handle_lcm_grep(
            LcmHandlerContext::user(&missing_path, None, None),
            json!({"query": "anything", "format": "json"}),
        )
        .await
        .unwrap(),
    );

    assert_eq!(response["status"], "unavailable");
    assert_eq!(
        response["error"]["code"],
        "lcm_retrieval_service_unavailable"
    );
    assert_eq!(response["hits"], json!([]));
    assert!(!missing_path.exists());
}

#[tokio::test]
async fn project_read_alias_without_service_never_probes_the_store_path() {
    let temp = TempDir::new().unwrap();
    let missing_path = temp.path().join("sessions.db");
    let response = payload(
        handle_lcm_load_session(
            LcmHandlerContext::project_for_test(temp.path(), &missing_path, None),
            json!({"session_id": "session-exact", "format": "json"}),
        )
        .await
        .unwrap(),
    );

    assert_eq!(response["status"], "unavailable");
    assert_eq!(
        response["error"]["code"],
        "lcm_retrieval_service_unavailable"
    );
    assert!(!missing_path.exists());
}
