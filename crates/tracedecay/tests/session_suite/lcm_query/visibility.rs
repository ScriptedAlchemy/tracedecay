//! One summary authority: every LCM read serves `session_summary_nodes`, and a
//! summary is visible iff its active-generation availability is `available`.

use super::*;

const SUMMARY_TEXT: &str = "visibility quartz summary of the alpha turn";

async fn active_generation(db: &HostAdmissionTestRuntimeV1, session_id: &str) -> i64 {
    let snapshot = db
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database")
        .read_snapshot()
        .await
        .expect("registered read snapshot");
    let mut rows = snapshot
        .query(
            "SELECT generation FROM session_temporal_generations
             WHERE session_id = ?1 AND state = 'active'",
            params![session_id],
        )
        .await
        .expect("active generation query");
    rows.next()
        .await
        .expect("active generation row")
        .expect("one active generation")
        .get(0)
        .expect("generation column")
}

async fn count_rows(db: &HostAdmissionTestRuntimeV1, sql: &str, value: &str) -> i64 {
    let snapshot = db
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database")
        .read_snapshot()
        .await
        .expect("registered read snapshot");
    let mut rows = snapshot
        .query(sql, params![value])
        .await
        .expect("count query");
    rows.next()
        .await
        .expect("count row")
        .expect("count present")
        .get(0)
        .expect("count column")
}

fn grep_request(session_id: &str) -> LcmGrepRequest {
    LcmGrepRequest {
        provider: "cursor".into(),
        query: "quartz".into(),
        scope: LcmScope::Session,
        session_id: Some(session_id.into()),
        include_summaries: true,
        limit: 10,
        sort: LcmGrepSort::Recency,
        source: None,
        role: None,
        start_time: None,
        end_time: None,
        git_filter: Default::default(),
    }
}

fn expand_request(session_id: &str, node_id: &str) -> LcmExpandRequest {
    LcmExpandRequest {
        provider: "cursor".into(),
        session_id: session_id.into(),
        target: LcmExpandTarget::SummaryNode {
            node_id: node_id.into(),
        },
        content_slice: None,
        source_offset: 0,
        source_limit: None,
    }
}

#[tokio::test]
async fn published_summary_is_served_from_the_canonical_row_alone() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let session_id = "session-visibility";
    let store_ids = insert_raw_messages(&db, "cursor", session_id, &["alpha".to_string()]).await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            session_id,
            SUMMARY_TEXT,
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should publish");

    // The LCM summary tables are gone; the canonical row is the only copy.
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE ?1",
            "lcm\\_summary\\_nodes%",
        )
        .await,
        0,
        "no LCM summary table or index may remain"
    );
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
            "lcm_summary_sources",
        )
        .await,
        0
    );
    // One stored body: publication_json never repeats summary_text.
    assert_eq!(
        count_rows(
            &db,
            "SELECT COUNT(*) FROM session_summary_nodes
             WHERE summary_text = ?1 AND instr(publication_json, summary_text) = 0",
            SUMMARY_TEXT,
        )
        .await,
        1
    );

    let hits = db
        .lcm_grep_for_test(grep_request(session_id))
        .await
        .expect("grep should succeed")
        .hits;
    assert!(
        hits.iter()
            .any(|hit| hit.kind == "summary_node"
                && hit.node_id.as_deref() == Some(&summary.node_id)),
        "grep must find the canonical summary: {hits:?}"
    );

    let description = db
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "cursor".into(),
            session_id: session_id.into(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .expect("describe should load");
    assert_eq!(description.summary_node_count, 1);
    assert_eq!(description.summary_nodes[0].node_id, summary.node_id);

    let expansion = db
        .lcm_expand_for_test(expand_request(session_id, &summary.node_id))
        .await
        .expect("expand should load");
    let node = expansion.summary_node.expect("expanded summary node");
    assert_eq!(node.node_id, summary.node_id);
    assert_eq!(expansion.content, SUMMARY_TEXT);
    assert_eq!(
        node.source_refs,
        vec![LcmSourceRef::RawMessage {
            store_id: store_ids[0],
        }]
    );

    let status = db
        .lcm_status_for_test("cursor", Some(session_id))
        .await
        .expect("status should load");
    assert_eq!(status.summary_node_count, 1);
}

#[tokio::test]
async fn unavailable_summary_never_surfaces_while_its_row_is_retained() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let session_id = "session-retired";
    let store_ids = insert_raw_messages(&db, "cursor", session_id, &["alpha".to_string()]).await;
    let summary = db
        .lcm_insert_summary_node(summary_draft(
            "cursor",
            session_id,
            SUMMARY_TEXT,
            vec![LcmSourceRef::RawMessage {
                store_id: store_ids[0],
            }],
        ))
        .await
        .expect("summary should publish");
    let generation = active_generation(&db, session_id).await;

    // Retirement is an availability state, never a delete.
    db.apply_lcm_lineage_fault_for_test(LcmLineageFaultForTest::SetAvailability {
        session_id: session_id.into(),
        generation,
        summary_id: summary.node_id.clone(),
        availability: "unavailable".into(),
        reason: Some("session_retired".into()),
    })
    .await
    .expect("availability should update");

    assert_eq!(
        db.session_summary_node_count_for_test(HostAdmissionScope::Profile, session_id)
            .await
            .expect("canonical row count"),
        1,
        "the immutable canonical row stays for audit"
    );
    let hits = db
        .lcm_grep_for_test(grep_request(session_id))
        .await
        .expect("grep should succeed")
        .hits;
    assert!(
        hits.iter().all(|hit| hit.kind != "summary_node"),
        "an unavailable summary must not grep: {hits:?}"
    );
    let description = db
        .lcm_describe_for_test(LcmDescribeRequest {
            provider: "cursor".into(),
            session_id: session_id.into(),
            target: LcmDescribeTarget::Session,
        })
        .await
        .expect("describe should load");
    assert_eq!(description.summary_node_count, 0);
    assert!(description.summary_nodes.is_empty());
    assert_eq!(
        db.lcm_expand_for_test(expand_request(session_id, &summary.node_id))
            .await
            .expect_err("an unavailable summary must not expand"),
        LcmError::SummaryNodeNotFound
    );
    assert_eq!(
        db.lcm_status_for_test("cursor", Some(session_id))
            .await
            .expect("status should load")
            .summary_node_count,
        0
    );
    let replay = db
        .lcm_session_replay_slice_for_test(&LcmSessionReplayRequest {
            provider: "cursor".into(),
            session_id: session_id.into(),
            head_limit: 1,
            tail_limit: 0,
            max_snippet_chars: 200,
            summary_limit: 5,
            max_summary_chars: 200,
        })
        .await
        .expect("replay slice should load");
    assert!(replay.summary_nodes.is_empty());
}
