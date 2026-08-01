use std::path::Path;
use std::sync::Mutex;

use tempfile::TempDir;
use tracedecay_domain::{
    HydrationStateV1, RetrievalAnchorId, RetrievalGrainV1, SessionId, SessionSourceCoverageV1,
    SessionSourceFrontierV1, SessionSourceIdV1, SessionTemporalCoverageRequestV1,
    TemporalCoverageCountsV1, TemporalModeV1, UtcMicros,
};

use super::super::super::*;
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
async fn describe_maps_summary_target_to_typed_service_and_adds_temporal_metadata() {
    let service = RecordingService::new(complete("unused", "assistant", None));
    service.set_describe_outcome(LcmDescribeServiceOutcome::Complete {
        description: LcmDescribeResponse {
            target: "summary_node".to_string(),
            provider: "claude".to_string(),
            session_id: "session-exact".to_string(),
            raw_message_count: 2,
            summary_node_count: 1,
            external_payload_count: 0,
            first_store_id: Some(1),
            last_store_id: Some(2),
            raw_messages: Vec::new(),
            summary_nodes: Vec::new(),
            summary_node: None,
            external_payload: None,
        },
        temporal: temporal(None),
        grain: RetrievalGrainV1::Summary,
        state: HydrationStateV1::Available,
        lineage: Vec::new(),
        retrieval: LcmRetrievalOutcome::complete(LcmDataFreshness::Fresh),
    });
    let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));

    let response = payload(
        handle_lcm_describe(
            context,
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

    let command = service.describe_command();
    assert_eq!(command.provider(), "claude");
    assert_eq!(command.session_id().as_str(), "session-exact");
    assert_eq!(command.grain(), RetrievalGrainV1::Summary);
    assert!(matches!(
        command.target(),
        LcmDescribeTarget::SummaryNode { node_id } if node_id == "summary-1"
    ));
    assert_eq!(response["description"]["raw_message_count"], 2);
    assert_eq!(response["description"]["summary_node_count"], 1);
    assert_eq!(response["grain"], "summary");
    assert_eq!(response["state"], "available");
    assert_eq!(response["anchors"][0], "anchor.compatibility.1");
    assert_eq!(response["watermarks"]["generation"], 9);
    assert_eq!(response["coverage"]["visible"], 1);
    assert_eq!(response["source_coverage"][0]["source_id"], "claude");
    assert_eq!(
        response["source_coverage"][0]["reason"]["kind"],
        "caught_up"
    );
    assert!(response["lineage"].is_array());
}

#[tokio::test]
async fn expand_maps_raw_alias_and_preserves_bounded_legacy_expansion() {
    let service = RecordingService::new(complete("unused", "assistant", None));
    service.set_expand_outcome(LcmExpandServiceOutcome::Complete {
        expansion: LcmExpandResponse {
            kind: "raw_message".to_string(),
            content: "😀界".to_string(),
            content_range: LcmContentRange {
                offset: 1,
                limit: 2,
                returned_chars: 2,
                total_chars: 5,
                truncated: true,
            },
            raw_message: None,
            summary_node: None,
            summary_sources: Vec::new(),
            payload_ref: None,
            from_current_session: Some(false),
            externalized_note: None,
            source_pagination: None,
        },
        temporal: temporal(Some("opaque-next")),
        grain: RetrievalGrainV1::Occurrence,
        state: HydrationStateV1::Available,
        retrieval: LcmRetrievalOutcome::complete(LcmDataFreshness::Fresh),
    });
    let context = LcmHandlerContext::user(Path::new("/missing"), None, Some(&service));

    let response = payload(
        handle_lcm_expand(
            context,
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "target": {"kind": "raw_message", "store_id": 41},
                "content_offset": 1,
                "content_limit": 2,
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );

    let command = service.expand_command();
    assert_eq!(command.provider(), "claude");
    assert_eq!(command.session_id().as_str(), "session-exact");
    assert_eq!(command.grain(), RetrievalGrainV1::Occurrence);
    assert_eq!(command.content_slice().offset, 1);
    assert_eq!(command.content_slice().limit, 2);
    assert_eq!(command.source_offset(), 0);
    assert_eq!(command.source_limit(), None);
    assert_eq!(command.cursor(), None);
    assert!(matches!(
        command.target(),
        LcmExpandTarget::RawMessage { store_id: 41 }
    ));
    assert_eq!(response["expansion"]["content"], "😀界");
    assert_eq!(response["expansion"]["from_current_session"], false);
    assert_eq!(response["grain"], "occurrence");
    assert_eq!(response["state"], "available");
    assert_eq!(response["next_cursor"], "opaque-next");
    assert_eq!(response["source_coverage"][0]["source_id"], "claude");
}

#[tokio::test]
async fn stale_describe_and_expand_render_typed_freshness_in_json_and_markdown() {
    let service = RecordingService::new(complete("unused", "assistant", None));
    let retrieval = LcmRetrievalOutcome::stale(LcmDataFreshness::Stored { generation_lag: 7 });
    service.set_describe_outcome(LcmDescribeServiceOutcome::Stale {
        temporal: temporal(None),
        retrieval,
    });
    service.set_expand_outcome(LcmExpandServiceOutcome::Stale {
        temporal: temporal(None),
        retrieval,
    });

    let describe = payload(
        handle_lcm_describe(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({"provider": "claude", "session_id": "session-exact", "format": "json"}),
        )
        .await
        .unwrap(),
    );
    let expand = payload(
        handle_lcm_expand(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "target": {"kind": "raw_message", "store_id": 41},
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );
    for response in [&describe, &expand] {
        assert_eq!(response["status"], "stale");
        assert_eq!(response["retrieval"]["outcome"], "stale");
        assert_eq!(response["retrieval"]["freshness"]["state"], "stored");
        assert_eq!(response["retrieval"]["freshness"]["generation_lag"], 7);
        assert!(response.get("error").is_none(), "{response}");
    }

    let markdown = response_text(
        handle_lcm_describe(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({"provider": "claude", "session_id": "session-exact", "format": "markdown"}),
        )
        .await
        .unwrap(),
    );
    assert!(markdown.contains("**status:** stale"), "{markdown}");
    assert!(markdown.contains("**generation lag:** 7"), "{markdown}");
    assert!(
        !markdown.contains("temporal_store_unavailable"),
        "{markdown}"
    );
}

#[tokio::test]
async fn zero_item_partial_describe_and_expand_render_omissions_without_deletion() {
    let service = RecordingService::new(complete("unused", "assistant", None));
    let retrieval =
        LcmRetrievalOutcome::partial(LcmDataFreshness::Partial { generation_lag: 3 }, 5);
    service.set_describe_outcome(LcmDescribeServiceOutcome::Partial {
        description: None,
        temporal: temporal(None),
        grain: RetrievalGrainV1::Summary,
        state: None,
        lineage: Vec::new(),
        retrieval,
    });
    service.set_expand_outcome(LcmExpandServiceOutcome::Partial {
        expansion: None,
        temporal: temporal(None),
        grain: RetrievalGrainV1::Summary,
        state: None,
        retrieval,
    });

    let describe = payload(
        handle_lcm_describe(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({"provider": "claude", "session_id": "session-exact", "format": "json"}),
        )
        .await
        .unwrap(),
    );
    let expand = payload(
        handle_lcm_expand(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "target": {"kind": "raw_message", "store_id": 41},
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );
    for response in [&describe, &expand] {
        assert_eq!(response["status"], "partial");
        assert_eq!(response["omitted"], 5);
        assert_eq!(response["retrieval"]["outcome"], "partial");
        assert_eq!(response["retrieval"]["freshness"]["state"], "partial");
        assert_eq!(response["retrieval"]["freshness"]["generation_lag"], 3);
        assert!(response.get("error").is_none(), "{response}");
    }
    assert!(describe["description"].is_null());
    assert!(expand["expansion"].is_null());

    let markdown = response_text(
        handle_lcm_expand(
            LcmHandlerContext::user(Path::new("/missing"), None, Some(&service)),
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "target": {"kind": "raw_message", "store_id": 41},
                "format": "markdown"
            }),
        )
        .await
        .unwrap(),
    );
    assert!(markdown.contains("**status:** partial"), "{markdown}");
    assert!(markdown.contains("**omitted:** 5"), "{markdown}");
    assert!(
        !markdown.contains("temporal_store_unavailable"),
        "{markdown}"
    );
    assert!(!markdown.contains("**status:** deleted"), "{markdown}");
}

#[tokio::test]
async fn describe_and_expand_without_service_never_probe_legacy_storage() {
    let missing_path = Path::new("/definitely/missing/tracedecay-lcm-authority.db");

    let describe = payload(
        handle_lcm_describe(
            LcmHandlerContext::user(missing_path, None, None),
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );
    assert_eq!(describe["status"], "unavailable");
    assert_eq!(
        describe["error"]["code"],
        "lcm_retrieval_service_unavailable"
    );
    assert_eq!(describe["description"], json!([]));

    let expand = payload(
        handle_lcm_expand(
            LcmHandlerContext::user(missing_path, None, None),
            json!({
                "provider": "claude",
                "session_id": "session-exact",
                "target": {"kind": "raw_message", "store_id": 41},
                "format": "json"
            }),
        )
        .await
        .unwrap(),
    );
    assert_eq!(expand["status"], "unavailable");
    assert_eq!(expand["error"]["code"], "lcm_retrieval_service_unavailable");
    assert_eq!(expand["expansion"], json!([]));
    assert!(!missing_path.exists());
}
