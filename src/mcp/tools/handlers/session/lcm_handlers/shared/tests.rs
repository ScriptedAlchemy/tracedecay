use std::path::Path;
use std::sync::Mutex;

use tempfile::TempDir;
use tracedecay_domain::{
    HydrationStateV1, RetrievalAnchorId, RetrievalGrainV1, SessionId, SessionSourceCoverageV1,
    SessionSourceFrontierV1, SessionSourceIdV1, SessionTemporalCoverageRequestV1,
    TemporalCoverageCountsV1, TemporalModeV1, UtcMicros,
};

use super::super::super::*;
use super::super::retrieval::{handle_lcm_grep, handle_lcm_load_session};
use super::super::test_support::*;
use super::*;
use crate::application::session::{SessionDataFreshness, SessionRetrievalScope};
use crate::mcp::tools::handlers::session::message_search::{
    LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
    LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
    SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalPageView,
    SessionRetrievalServiceFuture, SessionRetrievalServiceOutcome, SessionRetrievalServicePort,
    SessionRetrievalUnavailable, SessionTemporalMetadataView, SessionTemporalWatermarksView,
};
use crate::sessions::lcm::{LcmContentRange, LcmDescribeResponse, LcmExpandResponse};

#[tokio::test]
async fn unsupported_filters_are_typed_and_never_call_the_service() {
    for args in [
        json!({"query": "x", "branch": "main", "include_summaries": false, "format": "json"}),
        json!({"query": "x", "sort": "recency", "include_summaries": false, "format": "json"}),
    ] {
        let service = RecordingService::new(complete("unused", "user", None));
        let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
        let response = payload(handle_lcm_grep(context, args).await.unwrap());
        assert_eq!(response["status"], "unsupported_filter");
        assert!(
            response["error"]["code"]
                .as_str()
                .unwrap()
                .starts_with("lcm_")
        );
        assert_eq!(service.calls(), 0);
    }
}

#[tokio::test]
async fn malformed_unsupported_filters_are_rejected_without_broadening() {
    for (args, field) in [
        (
            json!({"query": "x", "include_summaries": "yes", "format": "json"}),
            "include_summaries",
        ),
        (
            json!({"query": "x", "source": 7, "format": "json"}),
            "source",
        ),
        (
            json!({"query": "x", "sort": false, "format": "json"}),
            "sort",
        ),
        (
            json!({"query": "x", "provider": 7, "format": "json"}),
            "provider",
        ),
        (json!({"query": "x", "role": 7, "format": "json"}), "role"),
        (
            json!({"query": "x", "temporal_mode": false, "format": "json"}),
            "temporal_mode",
        ),
        (
            json!({"query": "x", "branch": 7, "format": "json"}),
            "branch",
        ),
    ] {
        let service = RecordingService::new(complete("unused", "user", None));
        let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
        let error = handle_lcm_grep(context, args).await.unwrap_err();
        assert!(error.to_string().contains(field), "{error}");
        assert_eq!(service.calls(), 0);
    }
}

#[tokio::test]
async fn cursor_failures_and_legacy_numeric_cursor_are_typed_without_db_fallback() {
    let denied = RecordingService::new(SessionRetrievalServiceOutcome::Denied);
    let denied_context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&denied));
    let denied_response = payload(
        handle_lcm_grep(
            denied_context,
            json!({
                "query": "tampered",
                "cursor": "tampered.cursor",
                "include_summaries": false,
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );
    assert_eq!(denied_response["status"], "denied");

    let drifted = RecordingService::new(SessionRetrievalServiceOutcome::WrongScope);
    let drifted_context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&drifted));
    let drifted_response = payload(
        handle_lcm_grep(
            drifted_context,
            json!({
                "query": "drifted",
                "cursor": "opaque.other-root",
                "include_summaries": false,
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );
    assert_eq!(drifted_response["status"], "wrong_scope");

    let service = RecordingService::new(complete("compat", "assistant", Some("opaque-next")));
    let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));
    let error = handle_lcm_load_session(
        context,
        json!({
            "session_id": "session-exact",
            "after_store_id": 7,
            "format": "json"
        }),
    )
    .await
    .expect_err("legacy offset pagination must be rejected");
    assert!(
        error
            .to_string()
            .contains("after_store_id is no longer supported")
    );
    assert_eq!(service.calls(), 0);

    let missing_path = Path::new("/definitely/missing/tracedecay-sessions.db");
    let missing_context = LcmHandlerContext::user(missing_path, None, None);
    let missing = payload(
        handle_lcm_load_session(
            missing_context,
            json!({"session_id": "session-exact", "format": "json"}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(missing["status"], "unavailable");
    assert!(!missing_path.exists());
}
