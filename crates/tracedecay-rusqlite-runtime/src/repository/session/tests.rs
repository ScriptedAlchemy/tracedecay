use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use rusqlite::hooks::{AuthAction, Authorization};
use serde_json::json;
use tracedecay_domain::{
    CanonicalObservationIdV1, CopyProofV1, LogicalCopyRecordV1, MessageOccurrenceIdV1,
    MessageOccurrenceRecordV1, ObservationId, ProjectionOutputOrdinalV1, RetrievalAnchorId,
    SessionId, SessionProjectionGenerationV1, SessionSummaryIdV1, SessionSummaryRecordV1,
    SummarySourceHorizonV1, TemporalValidityV1, UtcMicros,
};
use tracedecay_store::{
    SessionFrozenWatermarksV1, SessionSummaryPublicationRequestV1, SessionTemporalCapabilitiesV1,
    SessionTemporalCapabilityV1, SessionTemporalSnapshotV1,
};

use super::*;

const OBSERVATION_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn install_insert_prepare_counter(connection: &Connection) -> Arc<Mutex<BTreeMap<String, usize>>> {
    let counts = Arc::new(Mutex::new(BTreeMap::new()));
    let tracked = Arc::clone(&counts);
    connection
        .authorizer(Some(move |context: rusqlite::hooks::AuthContext<'_>| {
            if let AuthAction::Insert { table_name } = context.action {
                *tracked
                    .lock()
                    .unwrap()
                    .entry(table_name.to_owned())
                    .or_default() += 1;
            }
            Authorization::Allow
        }))
        .unwrap();
    counts
}

fn projection_schema(connection: &Connection) {
    connection
        .execute_batch(
            "
                CREATE TABLE session_temporal_generations (
                    session_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    frozen_watermarks_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY(session_id, generation)
                );
                CREATE TABLE session_temporal_projection_receipts (
                    session_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    batch_ordinal INTEGER NOT NULL,
                    batch_digest TEXT NOT NULL,
                    frozen_watermarks_json TEXT NOT NULL,
                    source_through INTEGER NOT NULL,
                    projection_through INTEGER NOT NULL,
                    occurrence_count INTEGER NOT NULL,
                    occurrence_digest TEXT NOT NULL,
                    dimension_count INTEGER NOT NULL,
                    dimension_digest TEXT NOT NULL,
                    copy_count INTEGER NOT NULL,
                    copy_digest TEXT NOT NULL,
                    assertion_count INTEGER NOT NULL,
                    assertion_digest TEXT NOT NULL,
                    supersession_count INTEGER NOT NULL,
                    supersession_digest TEXT NOT NULL,
                    current_count INTEGER NOT NULL,
                    current_digest TEXT NOT NULL,
                    fts_count INTEGER NOT NULL,
                    fts_digest TEXT NOT NULL,
                    committed_at INTEGER NOT NULL,
                    PRIMARY KEY(session_id, generation, batch_ordinal)
                );
                CREATE TABLE session_threads (
                    session_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    thread_id TEXT NOT NULL,
                    grouping_provenance TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY(session_id, generation, thread_id)
                );
                CREATE TABLE session_turns (
                    session_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    turn_id TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    grouping_provenance TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY(session_id, generation, turn_id)
                );
                CREATE TABLE session_agents (
                    session_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    agent_id TEXT NOT NULL,
                    agent_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY(session_id, generation, agent_id)
                );
                CREATE TABLE session_occurrences (
                    session_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    occurrence_id TEXT NOT NULL,
                    source_observation_id TEXT NOT NULL,
                    projection_output_ordinal INTEGER NOT NULL,
                    retrieval_anchor_id TEXT NOT NULL,
                    thread_id TEXT,
                    thread_grouping_json TEXT,
                    turn_id TEXT,
                    turn_grouping_json TEXT,
                    message_id TEXT,
                    agent_id TEXT,
                    role TEXT NOT NULL,
                    knowledge_at INTEGER NOT NULL,
                    valid_time_json TEXT NOT NULL,
                    evidence_json TEXT NOT NULL,
                    snippet_text TEXT NOT NULL,
                    index_text TEXT NOT NULL,
                    PRIMARY KEY(session_id, generation, occurrence_id)
                );
                CREATE TABLE session_logical_copy_edges (
                    session_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    occurrence_id TEXT NOT NULL,
                    copied_from_occurrence_id TEXT NOT NULL,
                    proof_json TEXT NOT NULL,
                    knowledge_at INTEGER NOT NULL,
                    valid_time_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY(session_id, generation, occurrence_id, copied_from_occurrence_id)
                );
                CREATE TABLE session_assertions (
                    session_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    assertion_id TEXT NOT NULL,
                    assertion_kind TEXT NOT NULL,
                    subject_anchor_id TEXT NOT NULL,
                    object_anchor_id TEXT NOT NULL,
                    knowledge_at INTEGER NOT NULL,
                    valid_time_json TEXT NOT NULL,
                    evidence_json TEXT NOT NULL,
                    PRIMARY KEY(session_id, generation, assertion_id)
                );
                ",
        )
        .unwrap();
}

fn summary_schema(connection: &Connection) {
    connection
        .execute_batch(
            "
                CREATE TABLE session_summary_nodes (
                    summary_id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    summary_anchor_id TEXT NOT NULL,
                    summary_text TEXT NOT NULL,
                    index_text TEXT NOT NULL,
                    source_horizon_json TEXT NOT NULL,
                    publication_json TEXT,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE session_summary_sources (
                    summary_id TEXT NOT NULL,
                    source_ordinal INTEGER NOT NULL,
                    source_kind TEXT NOT NULL,
                    source_anchor_id TEXT,
                    source_summary_id TEXT,
                    PRIMARY KEY(summary_id, source_ordinal)
                );
                CREATE TABLE session_summary_successors (
                    predecessor_summary_id TEXT NOT NULL,
                    successor_summary_id TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY(predecessor_summary_id, successor_summary_id)
                );
                ",
        )
        .unwrap();
}

fn session_id() -> SessionId {
    SessionId::new("session.prepare-cache").unwrap()
}

fn generation(value: u64) -> SessionProjectionGenerationV1 {
    SessionProjectionGenerationV1::new(value).unwrap()
}

fn occurrence_id(ordinal: u32) -> MessageOccurrenceIdV1 {
    MessageOccurrenceIdV1::derive(
        &CanonicalObservationIdV1::new(OBSERVATION_DIGEST).unwrap(),
        ProjectionOutputOrdinalV1::new(ordinal),
    )
}

fn occurrence(session_id: &SessionId, ordinal: u32) -> MessageOccurrenceRecordV1 {
    serde_json::from_value(json!({
        "occurrence_id": occurrence_id(ordinal),
        "source_observation_id": OBSERVATION_DIGEST,
        "projection_output_ordinal": ordinal,
        "retrieval_anchor_id": format!("anchor.occurrence.{ordinal}"),
        "session_id": session_id,
        "thread_id": "thread.prepare-cache",
        "thread_grouping": {"kind": "provider_native"},
        "turn_id": "turn.prepare-cache",
        "turn_grouping": {"kind": "provider_native"},
        "message_id": format!("message.prepare-cache.{ordinal}"),
        "agent_id": "agent.prepare-cache",
        "role": "user",
        "knowledge_at": 50,
        "valid_time": {"kind": "known", "valid_at": 40},
        "evidence": {
            "authority": "provider_native",
            "evidence_class": "provider_declared",
            "source_anchor_id": "anchor.evidence",
            "sanitization_receipt": {
                "receipt_id": "receipt.prepare-cache",
                "sanitizer_version": "sanitizer.prepare-cache"
            }
        }
    }))
    .unwrap()
}

fn projection_batch() -> SessionTemporalProjectionBatchV1 {
    let session_id = session_id();
    SessionTemporalProjectionBatchV1::new(
        session_id.clone(),
        generation(8),
        SessionFrozenWatermarksV1::new(generation(7), 51, 47, 43),
        (0..3)
            .map(|ordinal| occurrence(&session_id, ordinal))
            .collect(),
        (1..3)
            .map(|ordinal| LogicalCopyRecordV1 {
                occurrence_id: occurrence_id(ordinal),
                copied_from_occurrence_id: occurrence_id(0),
                proof: CopyProofV1::ProviderLinkage {
                    source_occurrence_id: occurrence_id(0),
                    provider_record_id: ObservationId::new(format!(
                        "provider.copy.prepare-cache.{ordinal}"
                    ))
                    .unwrap(),
                },
                knowledge_at: UtcMicros(50),
                valid_time: TemporalValidityV1::Unknown,
            })
            .collect(),
        (1..3)
            .map(|ordinal| {
                serde_json::from_value(json!({
                    "assertion_id": format!("assertion.prepare-cache.{ordinal}"),
                    "kind": "supports",
                    "subject_anchor_id": format!("anchor.occurrence.{ordinal}"),
                    "object_anchor_id": "anchor.occurrence.0",
                    "knowledge_at": 50,
                    "valid_time": {"kind": "known", "valid_at": 40},
                    "evidence": {
                        "authority": "provider_native",
                        "evidence_class": "provider_declared",
                        "source_anchor_id": "anchor.evidence",
                        "sanitization_receipt": {
                            "receipt_id": "receipt.prepare-cache",
                            "sanitizer_version": "sanitizer.prepare-cache"
                        }
                    }
                }))
                .unwrap()
            })
            .collect(),
    )
    .unwrap()
}

fn summary_request() -> SessionSummaryPublicationRequestV1 {
    let session_id = session_id();
    let watermarks = SessionFrozenWatermarksV1::new(generation(7), 51, 47, 43);
    let snapshot = SessionTemporalSnapshotV1::new(
        session_id.clone(),
        UtcMicros(99),
        watermarks,
        SessionTemporalCapabilitiesV1::new([
            SessionTemporalCapabilityV1::ImmutableSummaryPublication,
        ]),
    );
    let summary = SessionSummaryRecordV1::new(
        SessionSummaryIdV1::new("summary.prepare-cache").unwrap(),
        session_id,
        RetrievalAnchorId::new("anchor.summary.prepare-cache").unwrap(),
        (0..3)
            .map(|ordinal| {
                RetrievalAnchorId::new(format!("anchor.summary-source.{ordinal}")).unwrap()
            })
            .collect(),
        SummarySourceHorizonV1 {
            knowledge_through: UtcMicros(50),
            valid_through: Some(UtcMicros(40)),
        },
        UtcMicros(60),
    )
    .unwrap();
    SessionSummaryPublicationRequestV1::new(summary, snapshot).unwrap()
}

#[test]
fn projection_batch_prepares_each_insert_once() {
    let mut connection = Connection::open_in_memory().unwrap();
    projection_schema(&connection);
    let batch = projection_batch();
    connection
        .execute(
            "INSERT INTO session_temporal_generations (
                    session_id, generation, state, frozen_watermarks_json, created_at
                 ) VALUES (?1, ?2, 'building', ?3, 0)",
            params![
                batch.session_id().as_str(),
                u64_to_i64(batch.generation().value(), "session generation").unwrap(),
                encode_watermarks(batch.watermarks()).unwrap(),
            ],
        )
        .unwrap();
    let prepares = install_insert_prepare_counter(&connection);

    let savepoint = connection.savepoint().unwrap();
    SessionExecutor
        .execute_projection_write(&savepoint, &batch)
        .unwrap();
    savepoint.commit().unwrap();

    let prepares = prepares.lock().unwrap();
    for table in [
        "session_threads",
        "session_turns",
        "session_agents",
        "session_occurrences",
        "session_logical_copy_edges",
        "session_assertions",
        "session_temporal_projection_receipts",
    ] {
        assert_eq!(
            prepares.get(table),
            Some(&1),
            "{table} must be prepared once for the whole batch"
        );
    }
}

#[test]
fn summary_sources_prepare_once_for_the_whole_publication() {
    let mut connection = Connection::open_in_memory().unwrap();
    summary_schema(&connection);
    let request = summary_request();
    let prepares = install_insert_prepare_counter(&connection);

    let savepoint = connection.savepoint().unwrap();
    SessionExecutor
        .execute_summary_write(&savepoint, &request)
        .unwrap();
    savepoint.commit().unwrap();

    assert_eq!(
        prepares
            .lock()
            .unwrap()
            .get("session_summary_sources")
            .copied(),
        Some(1),
        "summary source statement must be prepared once for the publication"
    );
}
