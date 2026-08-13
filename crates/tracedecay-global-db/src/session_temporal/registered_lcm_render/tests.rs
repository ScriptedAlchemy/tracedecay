use tempfile::tempdir;
use tracedecay_domain::RetrievalAnchorId;

use super::*;
use crate::tests::harness::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

/// Mirrors the canonical fixture's summary lineage as the relation graph
/// publishes it: the parent's sources are `[summary-child, raw 12]` and the
/// child's are `[raw 11]`, with every raw source carried as the anchor the
/// graph actually stores.
fn canonical_fixture_relations() -> Vec<SummaryRelationRead> {
    let anchor = |id: &str| RetrievalAnchorId::new(id).expect("retrieval anchor");
    vec![
        SummaryRelationRead {
            summary_id: "summary-parent".to_owned(),
            sources: vec![
                GraphSummarySourceRef::Summary {
                    summary_id: "summary-child".to_owned(),
                },
                GraphSummarySourceRef::Anchor {
                    anchor_id: anchor("anchor-message-b"),
                },
            ],
            predecessor_summary_id: None,
            successor_summary_ids: Vec::new(),
        },
        SummaryRelationRead {
            summary_id: "summary-child".to_owned(),
            sources: vec![GraphSummarySourceRef::Anchor {
                anchor_id: anchor("anchor-message-a"),
            }],
            predecessor_summary_id: None,
            successor_summary_ids: Vec::new(),
        },
    ]
}

async fn seeded_render_fixture(directory: &std::path::Path) -> HostAdmissionTestRuntimeV1 {
    let runtime = HostAdmissionTestRuntimeV1::profile(directory)
        .await
        .expect("registered profile runtime");
    runtime
        .seed_lcm_render_fixture_for_test(HostAdmissionScope::Profile)
        .await
        .expect("canonical LCM render fixture");
    runtime
}

async fn mutate_fixture(runtime: &HostAdmissionTestRuntimeV1, sql: &str) {
    runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered session database")
        .writer_connection()
        .expect("registered writer connection")
        .execute_batch(sql)
        .await
        .expect("fixture mutation");
}

/// Registers a second session so a raw row can be re-owned away from
/// `session-a` without tripping the raw-message session foreign key.
async fn seed_foreign_session(runtime: &HostAdmissionTestRuntimeV1) {
    let session = tracedecay_sessions::runtime::SessionRecord {
        provider: "codex".to_owned(),
        session_id: "session-b".to_owned(),
        project_key: "project-a".to_owned(),
        project_path: "/project-a".to_owned(),
        title: Some("Foreign session".to_owned()),
        started_at: Some(10),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    assert!(
        runtime
            .upsert_session_for_test(HostAdmissionScope::Profile, &session)
            .await
            .expect("foreign session upsert"),
        "foreign session must register"
    );
}

fn summary_expand_request(node_id: &str) -> LcmExpandRequest {
    LcmExpandRequest {
        provider: "codex".to_owned(),
        session_id: "session-a".to_owned(),
        target: LcmExpandTarget::SummaryNode {
            node_id: node_id.to_owned(),
        },
        content_slice: None,
        source_offset: 0,
        source_limit: None,
    }
}

fn summary_describe_request(node_id: &str) -> LcmDescribeRequest {
    LcmDescribeRequest {
        provider: "codex".to_owned(),
        session_id: "session-a".to_owned(),
        target: LcmDescribeTarget::SummaryNode {
            node_id: node_id.to_owned(),
        },
    }
}

/// The retention drop pass deletes projection-durable raw rows that summary
/// lineage still names, so before this the registered render path aborted every
/// summary older than the drop window with an ownership error. Rendering must
/// survive and report the dropped source as retention-expired.
#[tokio::test]
async fn registered_render_survives_retention_dropped_raw_sources() {
    let directory = tempdir().expect("temporary session store");
    let runtime = seeded_render_fixture(directory.path()).await;
    mutate_fixture(&runtime, "DELETE FROM lcm_raw_messages").await;
    let snapshot = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered session database")
        .read_snapshot()
        .await
        .expect("registered read snapshot");
    let relations = canonical_fixture_relations();

    let expansion = expand(
        &snapshot,
        summary_expand_request("summary-parent"),
        "canonical parent summary",
        &relations,
    )
    .await
    .expect("a retention-dropped raw source must not abort the expansion");

    assert_eq!(expansion.kind, "summary_node");
    assert_eq!(expansion.summary_sources.len(), 2);
    assert_eq!(
        expansion.summary_sources[0].source_ref,
        LcmSourceRef::SummaryNode {
            node_id: "summary-child".to_owned()
        }
    );
    let dropped = &expansion.summary_sources[1];
    assert_eq!(
        dropped.source_ref,
        LcmSourceRef::RawMessage { store_id: 12 }
    );
    assert_eq!(dropped.state, HydrationStateV1::RetentionExpired);
    assert!(dropped.content.is_empty());
    assert!(dropped.raw_message.is_none());
    assert!(
        dropped.raw_message_metadata.is_none(),
        "a dropped raw row must not surface metadata it no longer has"
    );

    let described = describe(
        &snapshot,
        summary_describe_request("summary-parent"),
        &relations,
    )
    .await
    .expect("a retention-dropped raw source must not abort the description");
    let summary_node = described.summary_node.expect("summary metadata");
    assert_eq!(summary_node.source_count, 2);
    assert_eq!(summary_node.children.len(), 2);
    let child = &summary_node.children[1];
    assert_eq!(child.source_kind, "raw_message");
    assert_eq!(child.store_id, Some(12));
    assert_eq!(
        (child.role.as_deref(), child.storage_kind),
        (None, None),
        "raw metadata is read straight off the dropped row, so it must be absent"
    );

    // The child summary reaches its own dropped source the same way.
    let child_expansion = expand(
        &snapshot,
        summary_expand_request("summary-child"),
        "canonical child summary",
        &relations,
    )
    .await
    .expect("the child summary must expand too");
    assert_eq!(child_expansion.summary_sources.len(), 1);
    assert_eq!(
        child_expansion.summary_sources[0].state,
        HydrationStateV1::RetentionExpired
    );
}

/// Absence is retention; presence under another session is still a disclosure
/// boundary. A raw row that exists but belongs elsewhere must keep failing
/// closed rather than riding the retention path.
#[tokio::test]
async fn registered_render_still_refuses_a_foreign_session_summary_source() {
    let directory = tempdir().expect("temporary session store");
    let runtime = seeded_render_fixture(directory.path()).await;
    seed_foreign_session(&runtime).await;
    mutate_fixture(
        &runtime,
        "UPDATE lcm_raw_messages SET session_id = 'session-b' WHERE store_id = 11",
    )
    .await;
    let snapshot = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered session database")
        .read_snapshot()
        .await
        .expect("registered read snapshot");
    let relations = canonical_fixture_relations();

    let error = expand(
        &snapshot,
        summary_expand_request("summary-child"),
        "canonical child summary",
        &relations,
    )
    .await
    .expect_err("a present but foreign raw source must not be disclosed");
    assert!(
        matches!(error, LcmError::SummarySourceNotOwnedBySession),
        "expected an ownership refusal, got: {error:?}"
    );

    let error = describe(
        &snapshot,
        summary_describe_request("summary-child"),
        &relations,
    )
    .await
    .expect_err("describe must refuse the same foreign raw source");
    assert!(
        matches!(error, LcmError::SummarySourceNotOwnedBySession),
        "expected an ownership refusal, got: {error:?}"
    );
}

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

    // Canonical content that cannot be verified against the stored payload
    // hash must be refused outright: fabricating a full raw message from
    // metadata plus unverifiable content is no longer a journey.
    let error = expand(
        &snapshot,
        LcmExpandRequest {
            provider: "codex".to_string(),
            session_id: "session-a".to_string(),
            target: LcmExpandTarget::RawMessage { store_id: 12 },
            content_slice: None,
            source_offset: 0,
            source_limit: None,
        },
        "",
        &[],
    )
    .await
    .expect_err("unverifiable canonical content must not fabricate a raw message");

    assert!(
        matches!(error, LcmError::PayloadIntegrityMismatch),
        "expected a payload-integrity refusal, got: {error:?}"
    );
}
