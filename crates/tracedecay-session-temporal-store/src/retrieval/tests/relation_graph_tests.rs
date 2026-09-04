use tracedecay_domain::{
    CopyProofV1, MessageOccurrenceIdV1, ObservationId, ProjectId, UserProfileId,
};
use tracedecay_temporal_query::candidates::CandidateClause;

use super::*;
use crate::relations::{
    LogicalCopyRelation, SessionRelationProjection, SessionRelationScope, SummaryRelationNode,
    SummarySourceRef,
};
use crate::sql::TemporalSqlRead;

fn profile_scope() -> SessionRelationScope {
    SessionRelationScope::profile_sessions(UserProfileId::new("profile.fixture").expect("profile"))
}

fn relation_projection(
    session_id: &str,
    summaries: Vec<SummaryRelationNode>,
    logical_copies: Vec<LogicalCopyRelation>,
) -> SessionRelationProjection {
    SessionRelationProjection {
        scope: profile_scope(),
        session_id: SessionId::new(session_id).expect("session"),
        generation: 1,
        summaries,
        logical_copies,
        thread_hierarchy: Vec::new(),
        agent_hierarchy: Vec::new(),
        parent_session_id: None,
        workflow_agents: Vec::new(),
    }
}

async fn records_from_projection(
    read: &RegisteredTemporalRead,
    snapshot: &TemporalExecutionSnapshot,
    candidate: RankingCandidate,
    request: &PageRequest,
    projection: SessionRelationProjection,
) -> Result<Vec<TemporalRecord>, TemporalPortError> {
    let scope = projection.scope.clone();
    let store = crate::relations::memory_relation_store();
    store.replace(&projection).expect("relation projection");
    let candidates = [candidate];
    let relations = load_record_relations(
        &store,
        &scope,
        snapshot.retrieval_scope(),
        snapshot,
        &candidates,
        0,
        request,
    )?;
    let query = build_record_query_with_relations(
        snapshot.retrieval_scope(),
        snapshot,
        &candidates,
        0,
        &RecordCursor {
            candidate: 0,
            kind: 0,
            session_id: String::new(),
            stable_id: String::new(),
        },
        request.page_item_limit().saturating_add(1),
        request,
        &relations,
    )?;
    let mut rows = tracedecay_runtime_core::db::engine::QueryExecutor::query(
        &read.read,
        &query.sql,
        query.params,
    )
    .await
    .map_err(|error| read_error(RECORD_OPERATION, error))?;
    let mut records = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| read_error(RECORD_OPERATION, error))?
    {
        records.push(temporal_record_from_row(&row)?);
    }
    Ok(records)
}

async fn summary_search_ids(
    read: &RegisteredTemporalRead,
    session_id: &str,
    provider: &str,
    generation: u64,
    mode: TemporalModeV1,
    channel: CandidateChannel,
    value: &str,
) -> Vec<String> {
    let snapshot = scoped_snapshot_for_session(session_id, generation, Some(provider), mode);
    let request = record_request();
    let mut rows = query_candidate_clause(
        &TemporalSqlRead::registered(&read.read),
        snapshot.retrieval_scope(),
        snapshot.request(),
        generation,
        &CandidateClause {
            channel,
            value: value.to_string(),
            exact: channel == CandidateChannel::Anchor,
        },
        &CandidateCursor {
            clause: 0,
            knowledge_at: i64::MAX,
            session_id: String::new(),
            stable_id: String::new(),
        },
        8,
        &request,
        None,
    )
    .await
    .expect("summary candidate query");
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.expect("summary candidate row") {
        ids.push(row.get(0).expect("summary candidate id"));
    }
    ids
}

async fn summary_expansion_ids(
    runtime: &HostAdmissionTestRuntimeV1,
    session_id: &str,
    mode: TemporalModeV1,
) -> Vec<String> {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let access = crate::SessionTemporalAccess::new(database);
    let session_id = SessionId::new(session_id).expect("session");
    let snapshot = access
        .freeze_session_temporal_snapshot_result(
            tracedecay_store::SessionTemporalSnapshotRequestV1::new(session_id.clone()),
        )
        .await
        .expect("frozen temporal snapshot");
    access
        .retrieve_session_temporal_page_result(
            tracedecay_store::SessionTemporalRetrievalRequestV1::new(
                session_id,
                mode,
                RetrievalGrainV1::Summary,
                snapshot,
                8,
                None,
                ExecutionControl::default(),
            )
            .expect("summary retrieval request"),
        )
        .await
        .expect("summary expansion")
        .summaries()
        .iter()
        .map(|summary| summary.summary_id().as_str().to_string())
        .collect()
}

async fn current_summary_records(
    read: &RegisteredTemporalRead,
    session_id: &str,
    provider: &str,
    generation: u64,
    summary_id: &str,
    anchor_id: &str,
    projection: SessionRelationProjection,
) -> Vec<TemporalRecord> {
    let mut candidate = candidate_for_anchor(anchor_id);
    candidate.channel = CandidateChannel::Summary;
    candidate.retriever_record_id = summary_id.to_string();
    candidate.session = Some(session_id.to_string());
    candidate.source = Some(provider.to_string());
    candidate.participant_generation = generation;
    records_from_projection(
        read,
        &scoped_snapshot_for_session(
            session_id,
            generation,
            Some(provider),
            TemporalModeV1::Current,
        ),
        candidate,
        &record_request(),
        projection,
    )
    .await
    .expect("current summary records")
}

async fn seed_production_summary(
    runtime: &HostAdmissionTestRuntimeV1,
    provider: &str,
    session_id: &str,
) -> tracedecay_lcm::LcmSummaryNode {
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    assert!(
        database
            .upsert_session(&tracedecay_store::SessionRecord {
                provider: provider.to_string(),
                session_id: session_id.to_string(),
                project_key: "user".to_string(),
                project_path: "/dirty-summary".to_string(),
                title: Some("dirty summary retrieval fixture".to_string()),
                started_at: Some(1),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    let storage_root = database.db_path().parent().expect("session storage root");
    for ordinal in 1..=4 {
        database
            .lcm_ingest_raw_message(
                storage_root,
                &tracedecay_store::SessionMessageRecord {
                    provider: provider.to_string(),
                    message_id: format!("dirty-summary-message-{ordinal}"),
                    session_id: session_id.to_string(),
                    role: "assistant".to_string(),
                    timestamp: Some(ordinal),
                    ordinal,
                    text: format!("retained summary source {ordinal}"),
                    kind: Some("message".to_string()),
                    model: None,
                    tool_names: None,
                    source_path: None,
                    source_offset: None,
                    metadata_json: None,
                },
            )
            .await
            .expect("production raw message ingest");
    }
    database
        .lcm_protect_session_raw_messages(provider, session_id)
        .await
        .expect("production raw protection");
    let response = database
        .lcm_compress_guarded(
            tracedecay_lcm::LcmCompressionRequest {
                provider: provider.to_string(),
                session_id: session_id.to_string(),
                messages: Vec::new(),
                current_tokens: Some(1_000),
                focus_topic: None,
                ignore_session_patterns: Vec::new(),
                stateless_session_patterns: Vec::new(),
                ignore_message_patterns: Vec::new(),
                expected_current_frontier_store_id: None,
                threshold_tokens: None,
                max_assembly_tokens: None,
                leaf_chunk_tokens: Some(1),
                max_source_messages: Some(8),
                summary_fan_in: None,
                incremental_max_depth: None,
                fresh_tail_count: Some(1),
                dynamic_leaf_chunk_enabled: None,
                dynamic_leaf_chunk_max: None,
                context_length: None,
                reserve_tokens_floor: None,
                summarizer: tracedecay_lcm::LcmSummarizerMode::Fake {
                    summary_text: "production dirty summary sentinel".to_string(),
                },
            },
            &ExecutionControl::default(),
            || Ok(()),
        )
        .await
        .expect("production summary publication");
    assert_eq!(response.summary_nodes.len(), 1);
    response
        .summary_nodes
        .into_iter()
        .next()
        .expect("published summary")
}

#[tokio::test]
async fn copy_lineage_comes_from_grafeo_without_a_sql_relation_table() {
    let directory = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_cross_session_record_fixture_for_test().await;
    let target_value = digest('a');
    let graph_source_value = digest('b');
    let content_digest = "c".repeat(64);
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    Executor::execute_batch(
        &database
            .writer_connection()
            .expect("registered profile writer"),
        &format!(
            "INSERT INTO retrieval_anchors (
                 anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES
                 ('graph-target-anchor', '{{}}', '{{}}', 'fixture'),
                 ('graph-source-anchor', '{{}}', '{{}}', 'fixture');
             INSERT INTO session_occurrences (
                 session_id, generation, occurrence_id, source_observation_id,
                 source_provider, projection_output_ordinal, retrieval_anchor_id,
                 role, knowledge_at, valid_time_json, evidence_json,
                 sanitized_content_digest, sanitized_content_bytes,
                 snippet_text, index_text
             ) VALUES
                 ('session-b', 1, '{target_value}', 'observation-shared',
                  'fixture-provider', 2, 'graph-target-anchor', 'user', 6,
                  '{{\"kind\":\"unknown\"}}',
                  '{{\"authority\":\"canonical_observation\",\"evidence_class\":\"observed\",
                    \"source_anchor_id\":\"graph-target-anchor\",
                    \"sanitization_receipt\":{{
                      \"receipt_id\":\"receipt-1\",\"sanitizer_version\":\"fixture\"
                    }}}}',
                  '{content_digest}', 6, 'target', 'target'),
                 ('session-b', 1, '{graph_source_value}', 'observation-shared',
                  'fixture-provider', 3, 'graph-source-anchor', 'user', 5,
                  '{{\"kind\":\"unknown\"}}',
                  '{{\"authority\":\"canonical_observation\",\"evidence_class\":\"observed\",
                    \"source_anchor_id\":\"graph-source-anchor\",
                    \"sanitization_receipt\":{{
                      \"receipt_id\":\"receipt-1\",\"sanitizer_version\":\"fixture\"
                    }}}}',
                  '{content_digest}', 5, 'graph', 'graph');"
        ),
    )
    .await
    .expect("occurrence fixture");
    let read = runtime.retrieval_read_for_test().await;
    let target = MessageOccurrenceIdV1::new(target_value).expect("target");
    let graph_source = MessageOccurrenceIdV1::new(graph_source_value).expect("graph source");
    let mut candidate = candidate_for_anchor("graph-target-anchor");
    candidate.retriever_record_id = target.to_string();
    candidate.session = Some("session-b".to_string());
    let records = records_from_projection(
        &read,
        &root_snapshot_with_mode(1, None, TemporalModeV1::Forensic),
        candidate,
        &record_request(),
        relation_projection(
            "session-b",
            Vec::new(),
            vec![LogicalCopyRelation {
                occurrence_id: target.clone(),
                copied_from_occurrence_id: graph_source.clone(),
                proof: CopyProofV1::ProviderLinkage {
                    source_occurrence_id: graph_source.clone(),
                    provider_record_id: ObservationId::new("graph-proof").expect("proof"),
                },
                knowledge_at: UtcMicros(5),
                valid_time: TemporalValidityV1::Unknown,
            }],
        ),
    )
    .await
    .expect("record relations");
    let copy = records
        .iter()
        .find_map(|record| match record {
            TemporalRecord::Copy(copy) => Some(copy),
            _ => None,
        })
        .expect("graph copy");

    assert_eq!(copy.occurrence_id, target);
    assert_eq!(copy.copied_from_occurrence_id, graph_source);
}

#[tokio::test]
async fn summary_sources_and_predecessor_come_from_grafeo() {
    let directory = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .expect("registered profile runtime");
    runtime
        .seed_historical_summary_successor_fixture_for_test()
        .await;
    let read = runtime.retrieval_read_for_test().await;
    let mut candidate = candidate_for_anchor("historical-summary-anchor");
    candidate.channel = CandidateChannel::Summary;
    candidate.retriever_record_id = "historical-summary".to_string();
    let records = records_from_projection(
        &read,
        &scoped_snapshot_with_mode(
            1,
            None,
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(6),
            },
        ),
        candidate,
        &record_request(),
        relation_projection(
            "session-snapshot",
            vec![
                SummaryRelationNode {
                    summary_id: "graph-predecessor".to_string(),
                    sources: Vec::new(),
                    predecessor_summary_id: None,
                },
                SummaryRelationNode {
                    summary_id: "historical-summary".to_string(),
                    sources: vec![SummarySourceRef::Anchor {
                        anchor_id: RetrievalAnchorId::new("shared-summary-source")
                            .expect("source anchor"),
                    }],
                    predecessor_summary_id: Some("graph-predecessor".to_string()),
                },
            ],
            Vec::new(),
        ),
    )
    .await
    .expect("summary relations");
    let summary = records
        .iter()
        .find_map(|record| match record {
            TemporalRecord::Summary(summary) => Some(summary),
            _ => None,
        })
        .expect("summary");
    let source = records
        .iter()
        .find_map(|record| match record {
            TemporalRecord::SummarySource(source) => Some(source),
            _ => None,
        })
        .expect("summary source");

    assert_eq!(
        summary
            .predecessor_summary_id()
            .map(tracedecay_domain::SessionSummaryIdV1::as_str),
        Some("graph-predecessor")
    );
    assert_eq!(
        source.state,
        SummarySourceState::Covered {
            knowledge_at: UtcMicros(5),
            valid_time: TemporalValidityV1::Known {
                valid_at: UtcMicros(5),
            },
        }
    );
}

#[tokio::test]
async fn provider_filter_uses_grafeo_retained_summary_anchors() {
    let directory = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_provider_summary_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;
    let projection = || {
        relation_projection(
            "session-snapshot",
            vec![SummaryRelationNode {
                summary_id: "summary-provider".to_string(),
                sources: vec![SummarySourceRef::Anchor {
                    anchor_id: RetrievalAnchorId::new("source-claude").expect("source anchor"),
                }],
                predecessor_summary_id: None,
            }],
            Vec::new(),
        )
    };
    let candidate = || {
        let mut candidate = candidate_for_anchor("anchor-summary-provider");
        candidate.channel = CandidateChannel::Summary;
        candidate.retriever_record_id = "summary-provider".to_string();
        candidate
    };

    let claude = records_from_projection(
        &read,
        &scoped_snapshot(1, Some("claude")),
        candidate(),
        &record_request(),
        projection(),
    )
    .await
    .expect("claude records");
    assert!(
        claude
            .iter()
            .any(|record| matches!(record, TemporalRecord::Summary(_)))
    );
    let codex = records_from_projection(
        &read,
        &scoped_snapshot(1, Some("codex")),
        candidate(),
        &record_request(),
        projection(),
    )
    .await
    .expect("codex records");
    assert!(
        codex
            .iter()
            .all(|record| !matches!(record, TemporalRecord::Summary(_)))
    );
}

#[tokio::test]
async fn current_temporal_reads_hide_a_durably_dirty_summary_across_restart() {
    const PROVIDER: &str = "cursor";
    const SESSION_ID: &str = "dirty-summary-session";
    const SUMMARY_TEXT: &str = "production dirty summary sentinel";

    let directory = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .expect("registered profile runtime");
    let summary = seed_production_summary(&runtime, PROVIDER, SESSION_ID).await;
    let database = runtime
        .registered_database(HostAdmissionScope::Profile)
        .expect("registered profile database");
    let frozen = crate::SessionTemporalAccess::new(database)
        .freeze_session_temporal_snapshot_result(
            tracedecay_store::SessionTemporalSnapshotRequestV1::new(
                SessionId::new(SESSION_ID).expect("session"),
            ),
        )
        .await
        .expect("published summary generation");
    let generation = frozen.watermarks().active_generation().value();
    Executor::execute_batch(
        &database
            .writer_connection()
            .expect("registered profile writer"),
        "INSERT INTO lcm_summary_convergence_dirty_raw(
             provider, session_id, store_id, rewind_frontier_store_id
         )
         SELECT 'cursor', 'dirty-summary-session', MIN(store_id),
                MAX(0, MIN(store_id) - 1)
         FROM lcm_raw_messages
         WHERE provider = 'cursor' AND session_id = 'dirty-summary-session';",
    )
    .await
    .expect("durable partial invalidation marker");

    let read = runtime.retrieval_read_for_test().await;
    let anchor_id = read
        .text_column(
            "SELECT summary_anchor_id FROM session_summary_nodes WHERE summary_id = ?1",
            vec![SqlValue::Text(summary.node_id.clone())],
            0,
        )
        .await
        .into_iter()
        .next()
        .expect("published summary anchor");
    let (scope, relation_store) =
        crate::SessionTemporalRegisteredDb::session_relation_store(database)
            .expect("registered relation authority");
    let projection = relation_store
        .load_projection(
            &scope,
            &SessionId::new(SESSION_ID).expect("session"),
            generation,
            256,
            256,
            std::sync::Arc::new(tracedecay_graph_db::NeverCancelled),
        )
        .expect("published relation projection");
    assert_eq!(
        summary_search_ids(
            &read,
            SESSION_ID,
            PROVIDER,
            generation,
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(i64::MAX),
            },
            CandidateChannel::Summary,
            SUMMARY_TEXT,
        )
        .await,
        [summary.node_id.as_str()],
        "historical search remains available while current convergence is incomplete"
    );
    assert!(
        summary_search_ids(
            &read,
            SESSION_ID,
            PROVIDER,
            generation,
            TemporalModeV1::Current,
            CandidateChannel::Summary,
            SUMMARY_TEXT,
        )
        .await
        .is_empty(),
        "current summary search must fail closed during partial invalidation"
    );
    assert!(
        summary_search_ids(
            &read,
            SESSION_ID,
            PROVIDER,
            generation,
            TemporalModeV1::Current,
            CandidateChannel::Anchor,
            &anchor_id,
        )
        .await
        .is_empty(),
        "current anchor search must not bypass summary invalidation"
    );
    assert!(
        current_summary_records(
            &read,
            SESSION_ID,
            PROVIDER,
            generation,
            &summary.node_id,
            &anchor_id,
            projection,
        )
        .await
        .is_empty(),
        "a retained candidate must not materialize the dirty summary"
    );
    assert_eq!(
        summary_expansion_ids(
            &runtime,
            SESSION_ID,
            TemporalModeV1::AsOf {
                cutoff: UtcMicros(i64::MAX),
            },
        )
        .await,
        [summary.node_id.as_str()],
        "historical expansion remains available while current convergence is incomplete"
    );
    assert!(
        summary_expansion_ids(&runtime, SESSION_ID, TemporalModeV1::Current)
            .await
            .is_empty(),
        "current summary expansion must fail closed during partial invalidation"
    );
    drop(read);
    drop(relation_store);
    drop(runtime);

    let restarted = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .expect("restarted registered profile runtime");
    let restarted_read = restarted.retrieval_read_for_test().await;
    assert!(
        summary_search_ids(
            &restarted_read,
            SESSION_ID,
            PROVIDER,
            generation,
            TemporalModeV1::Current,
            CandidateChannel::Summary,
            SUMMARY_TEXT,
        )
        .await
        .is_empty(),
        "the durable marker must keep current search fail closed after restart"
    );
    let restarted_database = restarted
        .registered_database(HostAdmissionScope::Profile)
        .expect("restarted registered profile database");
    let (restarted_scope, restarted_relations) =
        crate::SessionTemporalRegisteredDb::session_relation_store(restarted_database)
            .expect("restarted relation authority");
    let restarted_projection = restarted_relations
        .load_projection(
            &restarted_scope,
            &SessionId::new(SESSION_ID).expect("session"),
            generation,
            256,
            256,
            std::sync::Arc::new(tracedecay_graph_db::NeverCancelled),
        )
        .expect("restarted relation projection");
    assert!(
        current_summary_records(
            &restarted_read,
            SESSION_ID,
            PROVIDER,
            generation,
            &summary.node_id,
            &anchor_id,
            restarted_projection,
        )
        .await
        .is_empty(),
        "the durable marker must keep current materialization fail closed after restart"
    );
    assert!(
        summary_expansion_ids(&restarted, SESSION_ID, TemporalModeV1::Current)
            .await
            .is_empty(),
        "the durable marker must keep current expansion fail closed after restart"
    );
}

#[tokio::test]
async fn anchor_lookups_that_resolve_a_summary_read_its_summary_relations() {
    let directory = tempdir().expect("temporary directory");
    let runtime = HostAdmissionTestRuntimeV1::profile(directory.path())
        .await
        .expect("registered profile runtime");
    runtime.seed_provider_summary_fixture_for_test().await;
    let read = runtime.retrieval_read_for_test().await;
    // A summary describe hydrates over the summary's own anchor, so its
    // candidate arrives on the anchor channel carrying the summary's evidence
    // role and summary identity — never an occurrence identity.
    let mut candidate = candidate_for_anchor("anchor-summary-provider");
    candidate.channel = CandidateChannel::Anchor;
    candidate.evidence_role = Some("summary".to_string());
    candidate.retriever_record_id = "summary-provider".to_string();
    let records = records_from_projection(
        &read,
        &scoped_snapshot(1, Some("claude")),
        candidate,
        &record_request(),
        relation_projection(
            "session-snapshot",
            vec![SummaryRelationNode {
                summary_id: "summary-provider".to_string(),
                sources: vec![SummarySourceRef::Anchor {
                    anchor_id: RetrievalAnchorId::new("source-claude").expect("source anchor"),
                }],
                predecessor_summary_id: None,
            }],
            Vec::new(),
        ),
    )
    .await
    .expect("anchor-channel summary relations");

    assert!(
        records
            .iter()
            .any(|record| matches!(record, TemporalRecord::Summary(_))),
        "an anchor lookup that resolves a summary node must produce its summary record"
    );
}

#[test]
fn current_relation_reads_reject_a_stale_graph_generation() {
    let store = crate::relations::memory_relation_store();
    let mut projection = relation_projection(
        "session-snapshot",
        vec![SummaryRelationNode {
            summary_id: "summary-current".to_string(),
            sources: Vec::new(),
            predecessor_summary_id: None,
        }],
        Vec::new(),
    );
    projection.generation = 2;
    store.replace(&projection).expect("relation projection");
    let watermarks = TemporalWatermarks {
        generation: 2,
        source: 2,
        projection: 2,
        index: 2,
        summary: 2,
    };
    let snapshot = scoped_snapshot(2, Some("claude"))
        .with_participant_manifest(
            TemporalParticipantManifest::new(vec![
                TemporalParticipantGeneration::new(
                    SessionId::new("session-snapshot").expect("participant session"),
                    "claude",
                    watermarks,
                    1,
                    &BindingDigest::new("configuration", digest('4')).expect("configuration"),
                    &BindingDigest::new("authorization", digest('2')).expect("authorization"),
                    TemporalParticipantAuthorization::Authorized,
                    TemporalSourceAccess::Available,
                )
                .expect("stale graph participant"),
            ])
            .expect("participant manifest"),
        )
        .expect("root snapshot");
    let mut candidate = candidate_for_anchor("anchor-summary-current");
    candidate.channel = CandidateChannel::Summary;
    candidate.retriever_record_id = "summary-current".to_string();
    candidate.source = Some("claude".to_string());

    let error = load_record_relations(
        &store,
        &projection.scope,
        snapshot.retrieval_scope(),
        &snapshot,
        &[candidate],
        0,
        &record_request(),
    )
    .expect_err("a stale graph watermark must not satisfy a current relation read");

    assert!(matches!(error, TemporalPortError::Read { .. }));
}

#[test]
fn relation_reads_do_not_alias_profile_scope_to_project_scope() {
    let store = crate::relations::memory_relation_store();
    store
        .replace(&relation_projection(
            "session-snapshot",
            Vec::new(),
            Vec::new(),
        ))
        .expect("profile projection");
    let snapshot = scoped_snapshot(1, None);
    let mut candidate = candidate_for_anchor("anchor");
    candidate.retriever_record_id = digest('d');
    let error = load_record_relations(
        &store,
        &SessionRelationScope::project_sessions(
            ProjectId::new("project.retrieval-relations").expect("project"),
        ),
        snapshot.retrieval_scope(),
        &snapshot,
        &[candidate],
        0,
        &record_request(),
    )
    .expect_err("project scope must not read profile relations");

    assert!(matches!(error, TemporalPortError::Read { .. }));
}

#[test]
fn cancelled_record_relation_read_preserves_temporal_cancellation() {
    let store = crate::relations::memory_relation_store();
    let projection = relation_projection("session-snapshot", Vec::new(), Vec::new());
    store.replace(&projection).expect("relation projection");
    let snapshot = scoped_snapshot(1, None);
    snapshot.request().execution_control().cancel();
    let mut candidate = candidate_for_anchor("anchor");
    candidate.retriever_record_id = digest('e');
    let error = load_record_relations(
        &store,
        &projection.scope,
        snapshot.retrieval_scope(),
        &snapshot,
        &[candidate],
        0,
        &record_request(),
    )
    .expect_err("cancelled relation read");

    assert_eq!(error, TemporalPortError::Cancelled);
}
