use tracedecay_domain::{ProjectId, RetrievalAnchorId, SessionId};
use tracedecay_runtime_core::db::engine::{Executor, TestConnection};
use tracedecay_temporal_query::hydration::HydrationError;
use tracedecay_temporal_query::ports::{ExecutionControl, TemporalPortError};

use super::{TemporalSqlRead, summary_has_provider_evidence};
use crate::session_temporal::relations::{
    SessionRelationProjection, SessionRelationScope, SummaryRelationNode, SummarySourceRef,
};

fn project() -> ProjectId {
    ProjectId::new("project-hydration").expect("project")
}

fn session() -> SessionId {
    SessionId::new("session-hydration").expect("session")
}

fn projection(anchor_id: &str) -> SessionRelationProjection {
    SessionRelationProjection {
        scope: SessionRelationScope::project_sessions(project()),
        session_id: session(),
        generation: 1,
        summaries: vec![SummaryRelationNode {
            summary_id: "summary-root".to_string(),
            sources: vec![SummarySourceRef::Anchor {
                anchor_id: RetrievalAnchorId::new(anchor_id).expect("anchor"),
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

async fn canonical_source_connection(path: &std::path::Path) -> TestConnection {
    let connection = TestConnection::open(path);
    connection
        .execute_batch(
            "CREATE TABLE session_occurrences (
                 session_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 retrieval_anchor_id TEXT NOT NULL,
                 source_observation_id TEXT NOT NULL,
                 source_provider TEXT NOT NULL
             );
             INSERT INTO session_occurrences VALUES (
                 'session-hydration', 1, 'anchor-source', 'observation-source', 'claude'
             );",
        )
        .await
        .expect("canonical source fixture");
    connection
}

#[tokio::test]
async fn provider_evidence_follows_grafeo_sources_without_a_sql_relation_table() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let connection = canonical_source_connection(&directory.path().join("sources.db")).await;
    let relations = crate::session_temporal::relations::memory_relation_store();
    relations
        .replace(&projection("anchor-source"))
        .expect("relation projection");

    let matched = summary_has_provider_evidence(
        &TemporalSqlRead::engine_connection(&connection),
        &relations,
        &SessionRelationScope::project_sessions(project()),
        &session(),
        1,
        "summary-root",
        "claude",
        &ExecutionControl::default(),
    )
    .await
    .expect("provider evidence");

    assert!(matched);
}

#[tokio::test]
async fn missing_relation_projection_is_hydration_unavailable_not_provider_denial() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let connection = canonical_source_connection(&directory.path().join("unavailable.db")).await;
    let relations = crate::session_temporal::relations::memory_relation_store();

    let error = summary_has_provider_evidence(
        &TemporalSqlRead::engine_connection(&connection),
        &relations,
        &SessionRelationScope::project_sessions(project()),
        &session(),
        1,
        "summary-root",
        "claude",
        &ExecutionControl::default(),
    )
    .await
    .expect_err("missing graph projection must not become a provider mismatch");

    assert_eq!(error, HydrationError::Unavailable);
}

#[tokio::test]
async fn cancelled_provider_evidence_traversal_preserves_control_error() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let connection = canonical_source_connection(&directory.path().join("cancelled.db")).await;
    let relations = crate::session_temporal::relations::memory_relation_store();
    relations
        .replace(&projection("anchor-source"))
        .expect("relation projection");
    let control = ExecutionControl::default();
    control.cancel();

    let error = summary_has_provider_evidence(
        &TemporalSqlRead::engine_connection(&connection),
        &relations,
        &SessionRelationScope::project_sessions(project()),
        &session(),
        1,
        "summary-root",
        "claude",
        &control,
    )
    .await
    .expect_err("cancelled traversal");

    assert_eq!(
        error,
        HydrationError::Interrupted(TemporalPortError::Cancelled)
    );
}
