use tempfile::tempdir;

use super::*;
use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

#[tokio::test]
async fn registered_metadata_rendering_matches_the_canonical_fixture() {
    let directory = tempdir().expect("temporary session store");
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .expect("registered profile runtime");
    runtime
        .seed_lcm_render_fixture_for_test(HostAdmissionScope::Profile)
        .await
        .expect("canonical LCM render fixture");

    let describe_requests = [
        LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "session-a".to_string(),
            target: LcmDescribeTarget::Session,
        },
        LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "session-a".to_string(),
            target: LcmDescribeTarget::SummaryNode {
                node_id: "summary-parent".to_string(),
            },
        },
        LcmDescribeRequest {
            provider: "codex".to_string(),
            session_id: "session-a".to_string(),
            target: LcmDescribeTarget::ExternalPayload {
                payload_ref: "payload-a".to_string(),
            },
        },
    ];
    let expand_requests = [
        LcmExpandRequest {
            provider: "codex".to_string(),
            session_id: "session-a".to_string(),
            target: LcmExpandTarget::RawMessage { store_id: 11 },
            content_slice: Some(LcmContentSlice {
                offset: 2,
                limit: 7,
            }),
            source_offset: 0,
            source_limit: None,
        },
        LcmExpandRequest {
            provider: "codex".to_string(),
            session_id: "session-a".to_string(),
            target: LcmExpandTarget::SummaryNode {
                node_id: "summary-parent".to_string(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 1,
                limit: 9,
            }),
            source_offset: 0,
            source_limit: Some(2),
        },
        LcmExpandRequest {
            provider: "codex".to_string(),
            session_id: "session-a".to_string(),
            target: LcmExpandTarget::ExternalPayload {
                payload_ref: "payload-a".to_string(),
            },
            content_slice: Some(LcmContentSlice {
                offset: 3,
                limit: 8,
            }),
            source_offset: 0,
            source_limit: None,
        },
    ];

    let session = runtime
        .lcm_describe_for_test(describe_requests[0].clone())
        .await
        .expect("registered session describe");
    assert_eq!(session.target, "session");
    assert_eq!(session.raw_message_count, 2);
    assert_eq!(session.summary_node_count, 2);
    assert_eq!(session.external_payload_count, 1);
    assert_eq!(
        (session.first_store_id, session.last_store_id),
        (Some(11), Some(12))
    );

    let summary = runtime
        .lcm_describe_for_test(describe_requests[1].clone())
        .await
        .expect("registered summary describe");
    let summary_node = summary.summary_node.expect("summary metadata");
    assert_eq!(summary_node.node_id, "summary-parent");
    assert_eq!(summary_node.children.len(), 2);

    let payload = runtime
        .lcm_describe_for_test(describe_requests[2].clone())
        .await
        .expect("registered payload describe");
    let external = payload.external_payload.expect("external payload metadata");
    assert_eq!(external.payload_ref, "payload-a");
    assert_eq!(external.content_preview, "canonical external payload");

    let expected = [
        ("raw_message", "nonical", 0usize),
        ("summary_node", "anonical ", 2usize),
        ("external_payload", "onical e", 0usize),
    ];
    for (request, (kind, content, source_count)) in expand_requests.into_iter().zip(expected) {
        let expansion = runtime
            .lcm_expand_for_test(request)
            .await
            .expect("registered expansion");
        assert_eq!(expansion.kind, kind);
        assert_eq!(expansion.content, content);
        assert_eq!(expansion.summary_sources.len(), source_count);
    }
}

#[tokio::test]
async fn registered_metadata_rows_do_not_fabricate_full_raw_messages() {
    let directory = tempdir().expect("temporary session store");
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .expect("registered profile runtime");
    runtime
        .seed_lcm_render_fixture_for_test(HostAdmissionScope::Profile)
        .await
        .expect("canonical LCM render fixture");
    let snapshot = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered session database")
        .read_snapshot()
        .await
        .expect("registered read snapshot");

    let expansion = expand(
        &snapshot,
        LcmExpandRequest {
            provider: "codex".to_string(),
            session_id: "session-a".to_string(),
            target: LcmExpandTarget::SummaryNode {
                node_id: "summary-parent".to_string(),
            },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        },
        "canonical parent summary",
    )
    .await
    .expect("metadata-only summary expansion");
    let raw_source = expansion
        .summary_sources
        .iter()
        .find(|source| matches!(source.source_ref, LcmSourceRef::RawMessage { .. }))
        .expect("raw summary source");
    let raw_source = serde_json::to_value(raw_source).expect("serializable raw source");

    assert!(
        raw_source["raw_message"].is_null(),
        "metadata-only read fabricated a full raw message: {raw_source}"
    );
    assert_eq!(
        raw_source["raw_message_metadata"]["message_id"],
        "message-b"
    );
}
