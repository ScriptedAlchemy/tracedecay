use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectId, ProviderId, RetentionClass, RetrievalAnchorId, RetrievalGrainV1,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, TemporalModeV1,
};
use tracedecay_runtime_core::db::engine::{Executor, TestConnection};
use tracedecay_temporal_query::candidates::CandidateChannel;
use tracedecay_temporal_query::ports::{
    BindingDigest, ExecutionControl, KernelVersions, TemporalCandidateFilterV1,
    TemporalExecutionSnapshot, TemporalPortError, TemporalSessionScopeFilterV1,
    TemporalSnapshotRequest, TemporalWatermarks,
};
use tracedecay_temporal_query::ranking::RankingCandidate;
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

use super::GlobalDbTemporalReadPort;
use crate::session_temporal::relations::{
    SessionRelationProjection, SessionRelationScope, SummaryRelationNode, SummarySourceRef,
    WorkflowAgentMembership,
};

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn project() -> ProjectId {
    ProjectId::new("project-retrieval").expect("project")
}

fn session() -> SessionId {
    SessionId::new("session-retrieval").expect("session")
}

fn snapshot(control: ExecutionControl) -> TemporalExecutionSnapshot {
    TemporalExecutionSnapshot::new_authorized(
        TemporalSnapshotRequest::new(
            session(),
            digest('1'),
            digest('2'),
            digest('3'),
            TemporalModeV1::Current,
            RetrievalGrainV1::Session,
        )
        .expect("request")
        .with_execution_control(control),
        TemporalWatermarks {
            generation: 1,
            source: 0,
            projection: 0,
            index: 0,
            summary: 0,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest: BindingDigest::new("configuration", digest('4'))
                .expect("configuration"),
        },
        None,
        ValidatedAuthorization::Authorized,
    )
    .expect("snapshot")
}

fn candidate() -> RankingCandidate {
    RankingCandidate {
        stable_id: "summary:summary-root".to_string(),
        anchor_id: RetrievalAnchorId::new("summary-anchor").expect("summary anchor"),
        retriever_record_id: "summary-root".to_string(),
        channel: CandidateChannel::Summary,
        raw_score: 1_000,
        knowledge_at_micros: 1,
        logical_message: None,
        turn: None,
        session: Some(session().to_string()),
        source: Some("claude".to_string()),
        evidence_role: None,
        exact_ranges: Vec::new(),
    }
}

fn projection() -> SessionRelationProjection {
    SessionRelationProjection {
        scope: SessionRelationScope::project_sessions(project()),
        session_id: session(),
        generation: 1,
        summaries: vec![SummaryRelationNode {
            summary_id: "summary-root".to_string(),
            sources: vec![SummarySourceRef::Anchor {
                anchor_id: RetrievalAnchorId::new("source-anchor").expect("source anchor"),
            }],
            predecessor_summary_id: None,
        }],
        logical_copies: Vec::new(),
        thread_hierarchy: Vec::new(),
        agent_hierarchy: Vec::new(),
        parent_session_id: None,
        workflow_agents: Vec::new(),
    }
}

fn canonical_observation() -> DurableObservationV1 {
    let provider = ProviderId::new("claude").expect("provider");
    let session_id = session();
    let source = ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone())
        .expect("source");
    let range = ObservationSourceRangeV1::new(1, 2).expect("range");
    let record_id = ObservationId::new("observation-source").expect("record");
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        record_id.clone(),
        CanonicalObservationRelationsV1::new(session_id),
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::User,
            content: serde_json::json!({"text": "canonical source"}),
            model: None,
            timestamp: Some(1),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
    )
    .expect("envelope");
    let payload = serde_json::to_value(envelope).expect("payload");
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt-source").expect("receipt id"),
            tracedecay_domain::ComponentVersion::new("retrieval-graph-test.v1").expect("component"),
        )
        .expect("receipt ref"),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).expect("payload ref")),
    )
    .expect("receipt");
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        ObservationScopeV1::Project {
            project_id: project(),
        },
        ObservationSourceGenerationV1::new(1).expect("source generation"),
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        record_id,
    )
    .expect("identity");
    DurableObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.retrieval-graph-test").expect("retention"),
        payload,
    )
    .expect("observation")
}

async fn canonical_source_connection(path: &std::path::Path) -> TestConnection {
    let connection = TestConnection::open(path);
    connection
        .execute_batch(
            "CREATE TABLE observations (
                 observation_id TEXT PRIMARY KEY,
                 observation_json TEXT NOT NULL
             );
             CREATE TABLE session_occurrences (
                 session_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 retrieval_anchor_id TEXT NOT NULL,
                 source_observation_id TEXT NOT NULL,
                 role TEXT NOT NULL
             );",
        )
        .await
        .expect("canonical source schema");
    let encoded = serde_json::to_string(&canonical_observation()).expect("observation json");
    connection
        .execute(
            "INSERT INTO observations VALUES ('observation-source', ?1)",
            [encoded],
        )
        .await
        .expect("canonical observation");
    connection
        .execute(
            "INSERT INTO session_occurrences VALUES (
                 'session-retrieval', 1, 'source-anchor', 'observation-source', 'user'
             )",
            (),
        )
        .await
        .expect("canonical occurrence");
    connection
}

async fn canonical_session_connection(path: &std::path::Path) -> TestConnection {
    let connection = TestConnection::open(path);
    connection
        .execute_batch(
            "CREATE TABLE sessions (
                 session_id TEXT PRIMARY KEY,
                 provider TEXT NOT NULL,
                 project_key TEXT,
                 project_path TEXT
             );
             INSERT INTO sessions VALUES (
                 'session-retrieval', 'claude', 'project-retrieval', '/project-retrieval'
             );",
        )
        .await
        .expect("canonical session fixture");
    connection
}

#[tokio::test]
async fn session_context_filters_read_parent_and_workflow_only_from_grafeo() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let connection = canonical_session_connection(&directory.path().join("session.db")).await;
    let relations = crate::session_temporal::relations::memory_relation_store();
    let mut graph_projection = projection();
    graph_projection.parent_session_id =
        Some(SessionId::new("graph-parent").expect("parent session"));
    graph_projection.workflow_agents = vec![WorkflowAgentMembership {
        run_id: "run-graph".to_string(),
        agent_label: "worker".to_string(),
    }];
    relations
        .replace(&graph_projection)
        .expect("relation projection");
    let scope = SessionRelationScope::project_sessions(project());
    let adapter = GlobalDbTemporalReadPort::new_with_relations(&connection, &scope, relations);
    let snapshot = snapshot(ExecutionControl::default());
    let filter = TemporalCandidateFilterV1 {
        parent_session_id: Some("graph-parent".to_string()),
        session_scope: TemporalSessionScopeFilterV1::SubagentsOnly,
        workflow_run: Some("run-graph".to_string()),
        workflow_agent: Some("worker".to_string()),
        ..TemporalCandidateFilterV1::default()
    };

    assert!(
        adapter
            .session_matches_filter(&snapshot, session().as_str(), "claude", &filter)
            .await
            .expect("graph session context")
    );
}

#[tokio::test]
async fn summary_semantic_filter_hydrates_only_grafeo_source_anchors() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let connection = canonical_source_connection(&directory.path().join("sources.db")).await;
    let relations = crate::session_temporal::relations::memory_relation_store();
    relations
        .replace(&projection())
        .expect("relation projection");
    let scope = SessionRelationScope::project_sessions(project());
    let adapter = GlobalDbTemporalReadPort::new_with_relations(&connection, &scope, relations);
    let filter = TemporalCandidateFilterV1 {
        source: Some("claude".to_string()),
        ..TemporalCandidateFilterV1::default()
    };

    assert!(
        adapter
            .candidate_observations_match(
                &candidate(),
                &filter,
                &snapshot(ExecutionControl::default())
            )
            .await
            .expect("summary source eligibility")
    );
}

#[tokio::test]
async fn missing_summary_relation_projection_is_a_typed_read_failure() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let connection = canonical_source_connection(&directory.path().join("unavailable.db")).await;
    let relations = crate::session_temporal::relations::memory_relation_store();
    let scope = SessionRelationScope::project_sessions(project());
    let adapter = GlobalDbTemporalReadPort::new_with_relations(&connection, &scope, relations);

    let error = adapter
        .candidate_observations_match(
            &candidate(),
            &TemporalCandidateFilterV1 {
                source: Some("claude".to_string()),
                ..TemporalCandidateFilterV1::default()
            },
            &snapshot(ExecutionControl::default()),
        )
        .await
        .expect_err("missing graph projection");

    assert!(
        matches!(error, TemporalPortError::Read { operation, .. } if operation == super::CANDIDATE_OPERATION)
    );
}

#[tokio::test]
async fn cancelled_summary_relation_read_preserves_temporal_cancellation() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let connection = canonical_source_connection(&directory.path().join("cancelled.db")).await;
    let relations = crate::session_temporal::relations::memory_relation_store();
    relations
        .replace(&projection())
        .expect("relation projection");
    let scope = SessionRelationScope::project_sessions(project());
    let adapter = GlobalDbTemporalReadPort::new_with_relations(&connection, &scope, relations);
    let control = ExecutionControl::default();
    control.cancel();

    let error = adapter
        .candidate_observations_match(
            &candidate(),
            &TemporalCandidateFilterV1 {
                source: Some("claude".to_string()),
                ..TemporalCandidateFilterV1::default()
            },
            &snapshot(control),
        )
        .await
        .expect_err("cancelled relation read");

    assert_eq!(error, TemporalPortError::Cancelled);
}
