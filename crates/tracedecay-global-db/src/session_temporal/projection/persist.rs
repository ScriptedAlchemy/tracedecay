use serde_json::{Value, json};
use tracedecay_domain::{
    AnchorProvenanceRelationV2, CanonicalObservationEnvelopeV1, CopyProofV1, LogicalCopyRecordV1,
    MessageOccurrenceIdV1, MessageOccurrenceRecordV1, RetrievalAnchorRecord,
    SessionAuthorityClassV1, SessionId, TemporalAssertionKindV1, TemporalAssertionRecordV1,
    TemporalValidityV1, UtcMicros, derive_exact_observation_anchor_id,
};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::{
    SessionStoreError, SessionStoreResult, SessionTemporalProjectionBatchReceiptV1,
    SessionTemporalProjectionBatchV1,
};

use crate::observation_projection::derive_projection;

use super::super::query::{
    PERSIST_OPERATION, encode_watermarks, frontier_i64, generation_i64, now_micros,
    read_generation, read_observation, storage, storage_message,
};
use super::MATERIALIZE_REFRESH;
use super::materialize::*;
use super::receipts::*;

pub async fn session_temporal_projection_record_count(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    generation: tracedecay_domain::SessionProjectionGenerationV1,
) -> SessionStoreResult<u64> {
    let mut rows = conn
        .query(
            "SELECT
                (SELECT COUNT(*) FROM session_occurrences
                 WHERE session_id = ?1 AND generation = ?2)
              + (SELECT COUNT(*) FROM session_logical_copy_edges
                 WHERE session_id = ?1 AND generation = ?2)
              + (SELECT COUNT(*) FROM session_assertions
                 WHERE session_id = ?1 AND generation = ?2)",
            params![
                session_id.as_str(),
                generation_i64(generation, MATERIALIZE_REFRESH)?,
            ],
        )
        .await
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
    let count = rows
        .next()
        .await
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?
        .ok_or_else(|| {
            storage_message(
                MATERIALIZE_REFRESH,
                "projection record count returned no row",
            )
        })?
        .get::<i64>(0)
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
    u64::try_from(count).map_err(|error| storage(MATERIALIZE_REFRESH, error))
}

pub async fn persist_session_temporal_projection_batch_in_transaction(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<SessionTemporalProjectionBatchReceiptV1> {
    let generation = read_generation(
        conn,
        batch.session_id(),
        batch.generation(),
        PERSIST_OPERATION,
    )
    .await?
    .ok_or(SessionStoreError::MissingGeneration {
        generation: batch.generation(),
    })?;
    if generation.state != "building" {
        return Err(storage_message(
            PERSIST_OPERATION,
            format!(
                "projection batch cannot write generation in state {}",
                generation.state
            ),
        ));
    }
    if generation.frozen_watermarks_json
        != encode_watermarks(batch.watermarks(), PERSIST_OPERATION)?
    {
        return Err(SessionStoreError::FrozenWatermarkMismatch);
    }

    let batch_digest = canonical_batch_digest(batch)?;
    if let Some(receipt) = read_projection_receipt(conn, batch, batch_digest.as_str()).await? {
        return Ok(receipt);
    }
    require_contiguous_checkpoint(conn, batch).await?;

    for occurrence in batch.occurrences() {
        persist_occurrence(conn, batch, occurrence).await?;
    }
    for copy in batch.copies() {
        persist_copy(conn, batch, copy).await?;
    }
    for assertion in batch.assertions() {
        persist_assertion(conn, batch, assertion).await?;
    }
    rebuild_current_occurrences(conn, batch).await?;
    rebuild_assertion_derivatives(conn, batch).await?;
    super::derived::rebuild_derived_evidence(conn, batch).await?;

    let committed_at = now_micros(PERSIST_OPERATION)?;
    let coverage = projection_coverage(conn, batch).await?;
    insert_projection_receipt(
        conn,
        batch,
        batch_digest.as_str(),
        &coverage,
        committed_at.0,
    )
    .await?;
    SessionTemporalProjectionBatchReceiptV1::applied(
        batch,
        batch_digest,
        batch.occurrences().len(),
        batch.copies().len(),
        batch.assertions().len(),
        committed_at,
    )
}

pub async fn seed_active_projection_in_transaction(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    const COPIES: &[&str] = &[
        "INSERT INTO session_turns (
            session_id, generation, turn_id, ordinal, grouping_provenance, created_at
         )
         SELECT session_id, ?2, turn_id, ordinal, grouping_provenance, created_at
         FROM session_turns WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_threads (
            session_id, generation, thread_id, grouping_provenance, created_at
         )
         SELECT session_id, ?2, thread_id, grouping_provenance, created_at
         FROM session_threads WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_agents (
            session_id, generation, agent_id, agent_json, created_at
         )
         SELECT session_id, ?2, agent_id, agent_json, created_at
         FROM session_agents WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id,
            projection_output_ordinal, retrieval_anchor_id, thread_id,
            thread_grouping_json, turn_id, turn_grouping_json, message_id,
            agent_id, role, knowledge_at, valid_time_json, evidence_json,
            snippet_text, index_text
         )
         SELECT session_id, ?2, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, thread_id,
                thread_grouping_json, turn_id, turn_grouping_json, message_id,
                agent_id, role, knowledge_at, valid_time_json, evidence_json,
                snippet_text, index_text
         FROM session_occurrences WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_logical_copy_edges (
            session_id, generation, occurrence_id, copied_from_occurrence_id,
            proof_json, knowledge_at, valid_time_json, created_at
         )
         SELECT session_id, ?2, occurrence_id, copied_from_occurrence_id,
                proof_json, knowledge_at, valid_time_json, created_at
         FROM session_logical_copy_edges WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_turn_members (
            session_id, generation, turn_id, occurrence_id, ordinal
         )
         SELECT session_id, ?2, turn_id, occurrence_id, ordinal
         FROM session_turn_members WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_thread_hierarchy_edges (
            session_id, generation, parent_thread_id, child_thread_id, ordinal
         )
         SELECT session_id, ?2, parent_thread_id, child_thread_id, ordinal
         FROM session_thread_hierarchy_edges
         WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_agent_hierarchy_edges (
            session_id, generation, parent_agent_id, child_agent_id, ordinal
         )
         SELECT session_id, ?2, parent_agent_id, child_agent_id, ordinal
         FROM session_agent_hierarchy_edges
         WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_assertions (
            session_id, generation, assertion_id, assertion_kind,
            subject_anchor_id, object_anchor_id, knowledge_at,
            valid_time_json, evidence_json
         )
         SELECT session_id, ?2, assertion_id, assertion_kind,
                subject_anchor_id, object_anchor_id, knowledge_at,
                valid_time_json, evidence_json
         FROM session_assertions WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_assertion_supersession (
            session_id, generation, superseded_assertion_id,
            superseding_assertion_id, created_at
         )
         SELECT session_id, ?2, superseded_assertion_id,
                superseding_assertion_id, created_at
         FROM session_assertion_supersession
         WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_current_entities (
            session_id, generation, entity_kind, entity_id,
            current_assertion_id, current_occurrence_id, coverage_json
         )
         SELECT session_id, ?2, entity_kind, entity_id,
                current_assertion_id, current_occurrence_id, coverage_json
         FROM session_current_entities WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_derived_evidence (
            session_id, generation, evidence_kind, evidence_id,
            retrieval_anchor_id, thread_id,
            first_occurrence_id, last_occurrence_id,
            algorithm_version, configuration_digest,
            member_count, member_digest, evidence_json
         )
         SELECT session_id, ?2, evidence_kind, evidence_id,
                retrieval_anchor_id, thread_id,
                first_occurrence_id, last_occurrence_id,
                algorithm_version, configuration_digest,
                member_count, member_digest, evidence_json
         FROM session_derived_evidence WHERE session_id = ?1 AND generation = ?3",
        "INSERT INTO session_derived_evidence_members (
            session_id, generation, evidence_kind, evidence_id,
            ordinal, occurrence_id, member_role
         )
         SELECT session_id, ?2, evidence_kind, evidence_id,
                ordinal, occurrence_id, member_role
         FROM session_derived_evidence_members
         WHERE session_id = ?1 AND generation = ?3",
    ];
    if batch.batch_ordinal() != 0 || batch.watermarks().active_generation() == batch.generation() {
        return Ok(());
    }
    let session_id = batch.session_id().as_str();
    let candidate = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    let active = generation_i64(batch.watermarks().active_generation(), PERSIST_OPERATION)?;
    for sql in COPIES {
        conn.execute(sql, params![session_id, candidate, active])
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    }
    Ok(())
}

pub(super) async fn persist_occurrence(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
    occurrence: &MessageOccurrenceRecordV1,
) -> SessionStoreResult<bool> {
    let (source_sequence, observation) =
        read_observation(conn, &occurrence.source_observation_id).await?;
    if source_sequence > batch.watermarks().source_frontier() {
        return Err(SessionStoreError::FrozenWatermarkMismatch);
    }
    let mut authority_rows = conn
        .query(
            "SELECT 1 FROM session_temporal_observation_effects
             WHERE observation_id = ?1 AND observation_sequence = ?2
               AND session_id = ?3 AND output_count > ?4",
            params![
                occurrence.source_observation_id.as_str(),
                frontier_i64(source_sequence, PERSIST_OPERATION)?,
                batch.session_id().as_str(),
                i64::from(occurrence.projection_output_ordinal.value()),
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    if authority_rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .is_none()
    {
        return Err(storage_message(
            PERSIST_OPERATION,
            "canonical observation has no atomically recorded temporal effect",
        ));
    }
    let projection =
        derive_projection(&observation).map_err(|error| storage(PERSIST_OPERATION, error))?;
    let output = projection
        .messages()
        .find(|output| {
            output.output_ordinal() == occurrence.projection_output_ordinal.value()
                && output.session().session_id == occurrence.session_id.as_str()
        })
        .ok_or_else(|| {
            storage_message(
                PERSIST_OPERATION,
                format!(
                    "observation {} has no matching message output {} for session {}",
                    occurrence.source_observation_id.as_str(),
                    occurrence.projection_output_ordinal.value(),
                    occurrence.session_id.as_str()
                ),
            )
        })?;
    let expected =
        canonical_occurrence(conn, &observation, &projection, output.output_ordinal()).await?;
    if occurrence != &expected {
        return Err(storage_message(
            PERSIST_OPERATION,
            "occurrence does not equal its canonical observation, anchor, and receipt projection",
        ));
    }
    let role = output.message().role.clone();

    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    if let (Some(thread_id), Some(grouping)) = (&occurrence.thread_id, &occurrence.thread_grouping)
    {
        ensure_thread(
            conn,
            batch.session_id().as_str(),
            generation,
            thread_id.as_str(),
            &serde_json::to_string(grouping).map_err(|error| storage(PERSIST_OPERATION, error))?,
            occurrence.knowledge_at.0,
        )
        .await?;
    }
    if let (Some(turn_id), Some(grouping)) = (&occurrence.turn_id, &occurrence.turn_grouping) {
        ensure_turn(
            conn,
            batch.session_id().as_str(),
            generation,
            turn_id.as_str(),
            &serde_json::to_string(grouping).map_err(|error| storage(PERSIST_OPERATION, error))?,
            i64::from(occurrence.projection_output_ordinal.value()),
            occurrence.knowledge_at.0,
        )
        .await?;
    }
    if let Some(agent_id) = &occurrence.agent_id {
        ensure_agent(
            conn,
            batch.session_id().as_str(),
            generation,
            agent_id.as_str(),
            occurrence.knowledge_at.0,
        )
        .await?;
    }
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone())
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    if let (Some(parent_agent_id), Some(agent_id)) = (
        envelope.relations().parent_agent_id(),
        envelope.relations().agent_id(),
    ) {
        ensure_agent(
            conn,
            batch.session_id().as_str(),
            generation,
            parent_agent_id.as_str(),
            occurrence.knowledge_at.0,
        )
        .await?;
        conn.execute(
            "INSERT OR IGNORE INTO session_agent_hierarchy_edges (
                session_id, generation, parent_agent_id, child_agent_id, ordinal
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                batch.session_id().as_str(),
                generation,
                parent_agent_id.as_str(),
                agent_id.as_str(),
                i64::from(occurrence.projection_output_ordinal.value()),
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    }

    let thread_grouping = occurrence
        .thread_grouping
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let turn_grouping = occurrence
        .turn_grouping
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let valid_time = serde_json::to_string(&occurrence.valid_time)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let evidence = serde_json::to_string(&occurrence.evidence)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO session_occurrences (
                session_id, generation, occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id,
                thread_id, thread_grouping_json, turn_id, turn_grouping_json,
                message_id, agent_id, role, knowledge_at, valid_time_json,
                evidence_json, snippet_text, index_text
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18
             )",
            params![
                batch.session_id().as_str(),
                generation,
                occurrence.occurrence_id.as_str(),
                occurrence.source_observation_id.as_str(),
                i64::from(occurrence.projection_output_ordinal.value()),
                occurrence.retrieval_anchor_id.as_str(),
                occurrence
                    .thread_id
                    .as_ref()
                    .map(tracedecay_domain::ThreadId::as_str),
                thread_grouping,
                occurrence
                    .turn_id
                    .as_ref()
                    .map(tracedecay_domain::TurnId::as_str),
                turn_grouping,
                occurrence
                    .message_id
                    .as_ref()
                    .map(tracedecay_domain::MessageId::as_str),
                occurrence
                    .agent_id
                    .as_ref()
                    .map(tracedecay_domain::AgentInstanceId::as_str),
                role,
                occurrence.knowledge_at.0,
                valid_time,
                evidence,
                output.message().text.as_str(),
                output.message().text.as_str(),
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        == 1;
    if !inserted {
        require_exact_occurrence(conn, batch, occurrence, output.message().text.as_str()).await?;
    }
    if let Some(turn_id) = &occurrence.turn_id {
        conn.execute(
            "INSERT OR IGNORE INTO session_turn_members (
                session_id, generation, turn_id, occurrence_id, ordinal
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                batch.session_id().as_str(),
                generation,
                turn_id.as_str(),
                occurrence.occurrence_id.as_str(),
                i64::from(occurrence.projection_output_ordinal.value()),
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    }
    Ok(inserted)
}

/// Derives the canonical occurrence for one already-derived projection output.
///
/// The projection is threaded in by the caller rather than re-derived here: a
/// single observation can produce many outputs, and re-deriving its projection
/// per ordinal repeated the whole canonical-JSON plus SHA-256 sweep for a value
/// the caller already holds.
pub(super) async fn canonical_occurrence(
    conn: &impl QueryExecutor,
    observation: &tracedecay_domain::DurableObservationV1,
    projection: &tracedecay_store::ObservationProjection,
    output_ordinal: u32,
) -> SessionStoreResult<MessageOccurrenceRecordV1> {
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone())
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let output = projection
        .messages()
        .find(|candidate| candidate.output_ordinal() == output_ordinal)
        .ok_or_else(|| storage_message(PERSIST_OPERATION, "canonical output ordinal is missing"))?;
    let expected_anchor =
        derive_exact_observation_anchor_id(observation.scope(), observation.observation_id())
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let mut rows = conn
        .query(
            "SELECT anchor.anchor_json
             FROM observation_retrieval_anchors AS link
             JOIN retrieval_anchors AS anchor ON anchor.anchor_id = link.anchor_id
             WHERE link.observation_id = ?1 AND link.anchor_id = ?2",
            params![
                observation.observation_id().as_str(),
                expected_anchor.as_str()
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let anchor_json: String = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PERSIST_OPERATION,
                "canonical observation retrieval anchor is missing",
            )
        })?
        .get(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let anchor: RetrievalAnchorRecord =
        serde_json::from_str(&anchor_json).map_err(|error| storage(PERSIST_OPERATION, error))?;
    if anchor.anchor_id() != &expected_anchor
        || !anchor
            .source_observations()
            .contains(observation.observation_id())
    {
        return Err(storage_message(
            PERSIST_OPERATION,
            "canonical observation retrieval anchor has invalid retained provenance",
        ));
    }
    let valid_at = anchor
        .occurred_at()
        .map(|interval| interval.start)
        .or_else(|| envelope.evidence().native_timestamp().map(UtcMicros));
    let valid_time = valid_at.map_or_else(
        || json!({"kind": "unknown"}),
        |valid_at| json!({"kind": "known", "valid_at": valid_at}),
    );
    let grouping = || json!({"kind": "provider_native"});
    let relations = envelope.relations();
    let record = serde_json::from_value(json!({
        "occurrence_id": tracedecay_domain::MessageOccurrenceIdV1::derive(
            observation.observation_id(),
            tracedecay_domain::ProjectionOutputOrdinalV1::new(output_ordinal),
        ),
        "source_observation_id": observation.observation_id(),
        "projection_output_ordinal": output_ordinal,
        "retrieval_anchor_id": expected_anchor,
        "session_id": relations.session_id(),
        "thread_id": relations.thread_id().map(tracedecay_domain::ObservationId::as_str),
        "thread_grouping": relations.thread_id().map(|_| grouping()),
        "turn_id": relations.turn_id().map(tracedecay_domain::ObservationId::as_str),
        "turn_grouping": relations.turn_id().map(|_| grouping()),
        "message_id": output.message().message_id,
        "agent_id": relations.agent_id().map(tracedecay_domain::ObservationId::as_str),
        "role": output.message().role,
        "knowledge_at": anchor.ingested_at(),
        "valid_time": valid_time,
        "evidence": {
            "authority": "canonical_observation",
            "evidence_class": anchor.evidence_class(),
            "source_anchor_id": anchor.anchor_id(),
            "sanitization_receipt": observation.receipt().receipt(),
        },
    }))
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok(record)
}

pub(super) async fn ensure_thread(
    conn: &impl Executor,
    session_id: &str,
    generation: i64,
    thread_id: &str,
    grouping: &str,
    created_at: i64,
) -> SessionStoreResult<()> {
    conn.execute(
        "INSERT INTO session_threads (
                session_id, generation, thread_id, grouping_provenance, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id, generation, thread_id) DO UPDATE SET
                grouping_provenance = MIN(
                    session_threads.grouping_provenance,
                    excluded.grouping_provenance
                ),
                created_at = MIN(session_threads.created_at, excluded.created_at)",
        params![session_id, generation, thread_id, grouping, created_at],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok(())
}

pub(super) async fn ensure_turn(
    conn: &impl Executor,
    session_id: &str,
    generation: i64,
    turn_id: &str,
    grouping: &str,
    ordinal: i64,
    created_at: i64,
) -> SessionStoreResult<()> {
    conn.execute(
        "INSERT INTO session_turns (
                session_id, generation, turn_id, ordinal, grouping_provenance, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(session_id, generation, turn_id) DO UPDATE SET
                ordinal = MIN(session_turns.ordinal, excluded.ordinal),
                grouping_provenance = MIN(
                    session_turns.grouping_provenance,
                    excluded.grouping_provenance
                ),
                created_at = MIN(session_turns.created_at, excluded.created_at)",
        params![
            session_id, generation, turn_id, ordinal, grouping, created_at
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok(())
}

pub(super) async fn ensure_agent(
    conn: &impl Executor,
    session_id: &str,
    generation: i64,
    agent_id: &str,
    created_at: i64,
) -> SessionStoreResult<()> {
    let encoded = serde_json::to_string(&json!({ "agent_id": agent_id }))
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    conn.execute(
        "INSERT INTO session_agents (
                session_id, generation, agent_id, agent_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id, generation, agent_id) DO UPDATE SET
                agent_json = MIN(session_agents.agent_json, excluded.agent_json),
                created_at = MIN(session_agents.created_at, excluded.created_at)",
        params![
            session_id,
            generation,
            agent_id,
            encoded.as_str(),
            created_at
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok(())
}

pub(super) async fn require_exact_occurrence(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
    occurrence: &MessageOccurrenceRecordV1,
    text: &str,
) -> SessionStoreResult<()> {
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    let mut rows = conn
        .query(
            "SELECT json_object(
                'source_observation_id', source_observation_id,
                'projection_output_ordinal', projection_output_ordinal,
                'retrieval_anchor_id', retrieval_anchor_id,
                'thread_id', thread_id,
                'thread_grouping_json', json(thread_grouping_json),
                'turn_id', turn_id,
                'turn_grouping_json', json(turn_grouping_json),
                'message_id', message_id,
                'agent_id', agent_id,
                'role', role,
                'knowledge_at', knowledge_at,
                'valid_time_json', json(valid_time_json),
                'evidence_json', json(evidence_json),
                'snippet_text', snippet_text,
                'index_text', index_text
             )
             FROM session_occurrences
             WHERE session_id = ?1 AND generation = ?2 AND occurrence_id = ?3",
            params![
                batch.session_id().as_str(),
                generation,
                occurrence.occurrence_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let encoded: String = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PERSIST_OPERATION,
                "occurrence insert was ignored without an existing row",
            )
        })?
        .get(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let actual: Value =
        serde_json::from_str(&encoded).map_err(|error| storage(PERSIST_OPERATION, error))?;
    let role =
        serde_json::to_value(occurrence.role).map_err(|error| storage(PERSIST_OPERATION, error))?;
    let expected = json!({
        "source_observation_id": occurrence.source_observation_id.as_str(),
        "projection_output_ordinal": occurrence.projection_output_ordinal.value(),
        "retrieval_anchor_id": occurrence.retrieval_anchor_id.as_str(),
        "thread_id": occurrence.thread_id.as_ref().map(tracedecay_domain::ThreadId::as_str),
        "thread_grouping_json": occurrence.thread_grouping,
        "turn_id": occurrence.turn_id.as_ref().map(tracedecay_domain::TurnId::as_str),
        "turn_grouping_json": occurrence.turn_grouping,
        "message_id": occurrence.message_id.as_ref().map(tracedecay_domain::MessageId::as_str),
        "agent_id": occurrence.agent_id.as_ref().map(tracedecay_domain::AgentInstanceId::as_str),
        "role": role,
        "knowledge_at": occurrence.knowledge_at.0,
        "valid_time_json": occurrence.valid_time,
        "evidence_json": occurrence.evidence,
        "snippet_text": text,
        "index_text": text,
    });
    if actual != expected {
        return Err(storage_message(
            PERSIST_OPERATION,
            format!(
                "occurrence {} conflicts with an existing immutable row",
                occurrence.occurrence_id.as_str()
            ),
        ));
    }
    Ok(())
}

pub(super) async fn persist_copy(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
    copy: &LogicalCopyRecordV1,
) -> SessionStoreResult<bool> {
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    validate_copy_proof(conn, batch, copy).await?;
    let mut created_at = None;
    let mut target_knowledge_at = None;
    let mut target_valid_time = None;
    for occurrence_id in [&copy.occurrence_id, &copy.copied_from_occurrence_id] {
        let mut rows = conn
            .query(
                "SELECT knowledge_at, valid_time_json FROM session_occurrences
                 WHERE session_id = ?1 AND generation = ?2 AND occurrence_id = ?3",
                params![
                    batch.session_id().as_str(),
                    generation,
                    occurrence_id.as_str()
                ],
            )
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?
            .ok_or_else(|| {
                storage_message(
                    PERSIST_OPERATION,
                    "logical copy endpoint is outside the owning session generation",
                )
            })?;
        if occurrence_id == &copy.occurrence_id {
            created_at = Some(
                row.get::<i64>(0)
                    .map_err(|error| storage(PERSIST_OPERATION, error))?,
            );
            target_knowledge_at = Some(
                row.get::<i64>(0)
                    .map_err(|error| storage(PERSIST_OPERATION, error))?,
            );
            target_valid_time = Some(
                row.get::<String>(1)
                    .map_err(|error| storage(PERSIST_OPERATION, error))?,
            );
        }
    }
    let created_at = created_at.ok_or_else(|| {
        storage_message(
            PERSIST_OPERATION,
            "logical copy target timestamp is missing",
        )
    })?;
    let target_knowledge_at = target_knowledge_at.ok_or_else(|| {
        storage_message(
            PERSIST_OPERATION,
            "logical copy target knowledge_at is missing",
        )
    })?;
    let target_valid_time = target_valid_time.ok_or_else(|| {
        storage_message(
            PERSIST_OPERATION,
            "logical copy target valid_time is missing",
        )
    })?;
    let expected_valid_time = serde_json::to_string(&copy.valid_time)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    if copy.knowledge_at.0 != target_knowledge_at || expected_valid_time != target_valid_time {
        return Err(storage_message(
            PERSIST_OPERATION,
            "logical copy bitemporal fields must match the target occurrence",
        ));
    }
    let proof =
        serde_json::to_string(&copy.proof).map_err(|error| storage(PERSIST_OPERATION, error))?;
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO session_logical_copy_edges (
                session_id, generation, occurrence_id, copied_from_occurrence_id,
                proof_json, knowledge_at, valid_time_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                batch.session_id().as_str(),
                generation,
                copy.occurrence_id.as_str(),
                copy.copied_from_occurrence_id.as_str(),
                proof.as_str(),
                copy.knowledge_at.0,
                expected_valid_time.as_str(),
                created_at,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        == 1;
    if !inserted {
        require_edge_json(
            conn,
            "SELECT json_object(
                'proof', json(proof_json),
                'knowledge_at', knowledge_at,
                'valid_time', json(valid_time_json)
             )
             FROM session_logical_copy_edges
             WHERE session_id = ?1 AND generation = ?2
               AND occurrence_id = ?3 AND copied_from_occurrence_id = ?4",
            batch,
            copy.occurrence_id.as_str(),
            copy.copied_from_occurrence_id.as_str(),
            &serde_json::to_string(&json!({
                "proof": copy.proof,
                "knowledge_at": copy.knowledge_at.0,
                "valid_time": copy.valid_time,
            }))
            .map_err(|error| storage(PERSIST_OPERATION, error))?,
            "logical copy",
        )
        .await?;
    }
    Ok(inserted)
}

pub(super) async fn occurrence_observation_and_anchor(
    conn: &impl QueryExecutor,
    batch: &SessionTemporalProjectionBatchV1,
    occurrence_id: &tracedecay_domain::MessageOccurrenceIdV1,
) -> SessionStoreResult<(
    tracedecay_domain::DurableObservationV1,
    CanonicalObservationEnvelopeV1,
    String,
)> {
    let mut rows = conn
        .query(
            "SELECT source_observation_id, retrieval_anchor_id
             FROM session_occurrences
             WHERE session_id = ?1 AND generation = ?2 AND occurrence_id = ?3",
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
                occurrence_id.as_str()
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PERSIST_OPERATION,
                "copy proof occurrence is not retained in the owning generation",
            )
        })?;
    let observation_id = row
        .get::<String>(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let anchor_id = row
        .get::<String>(1)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let observation_id = tracedecay_domain::CanonicalObservationIdV1::new(observation_id)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let (_, observation) = read_observation(conn, &observation_id).await?;
    let envelope = serde_json::from_value(observation.payload().clone())
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok((observation, envelope, anchor_id))
}

pub(super) async fn validate_copy_proof(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
    copy: &LogicalCopyRecordV1,
) -> SessionStoreResult<()> {
    let (_, target, target_anchor_id) =
        occurrence_observation_and_anchor(conn, batch, &copy.occurrence_id).await?;
    let (_, source, source_anchor_id) =
        occurrence_observation_and_anchor(conn, batch, &copy.copied_from_occurrence_id).await?;
    let source_message_id = source
        .relations()
        .message_id()
        .unwrap_or_else(|| source.stable_record_id());
    let provider_or_parent_valid =
        target.relations().parent_message_id() == Some(source_message_id);
    let valid = match &copy.proof {
        CopyProofV1::ProviderLinkage {
            provider_record_id, ..
        } => provider_or_parent_valid && provider_record_id == source.stable_record_id(),
        CopyProofV1::ParentMessageLinkage {
            parent_message_id, ..
        } => provider_or_parent_valid && parent_message_id.as_str() == source_message_id.as_str(),
        CopyProofV1::ExplicitAnchorAssertion {
            assertion_anchor_id,
            ..
        } => {
            let mut rows = conn
                .query(
                    "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = ?1",
                    params![target_anchor_id],
                )
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let anchor_json = rows
                .next()
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?
                .map(|row| row.get::<String>(0))
                .transpose()
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            anchor_json
                .and_then(|encoded| serde_json::from_str::<RetrievalAnchorRecord>(&encoded).ok())
                .is_some_and(|anchor| {
                    assertion_anchor_id.as_str() == source_anchor_id
                        && anchor.source_anchors().iter().any(|lineage| {
                            lineage.relation() == AnchorProvenanceRelationV2::CopiedFrom
                                && lineage.anchor_id() == assertion_anchor_id
                        })
                })
        }
    };
    if !valid {
        return Err(storage_message(
            PERSIST_OPERATION,
            "copy proof is not supported by retained provider, parent-message, or CopiedFrom anchor evidence",
        ));
    }
    if !matches!(copy.proof, CopyProofV1::ExplicitAnchorAssertion { .. }) {
        let canonical = canonical_copy_proof_for_retained(conn, batch, copy).await?;
        if copy.proof != canonical {
            return Err(storage_message(
                PERSIST_OPERATION,
                "copy proof representation is not the canonical form for retained evidence",
            ));
        }
    }
    Ok(())
}

pub(super) async fn persist_assertion(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
    assertion: &TemporalAssertionRecordV1,
) -> SessionStoreResult<bool> {
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    validate_assertion(conn, batch, assertion).await?;
    let valid_time = serde_json::to_string(&assertion.valid_time)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let evidence = serde_json::to_string(&assertion.evidence)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO session_assertions (
                session_id, generation, assertion_id, assertion_kind,
                subject_anchor_id, object_anchor_id, knowledge_at,
                valid_time_json, evidence_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                batch.session_id().as_str(),
                generation,
                assertion.assertion_id.as_str(),
                assertion.kind.as_str(),
                assertion.subject_anchor_id.as_str(),
                assertion.object_anchor_id.as_str(),
                assertion.knowledge_at.0,
                valid_time,
                evidence,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        == 1;
    if !inserted {
        let expected = json!({
            "assertion_kind": assertion.kind.as_str(),
            "subject_anchor_id": assertion.subject_anchor_id.as_str(),
            "object_anchor_id": assertion.object_anchor_id.as_str(),
            "knowledge_at": assertion.knowledge_at.0,
            "valid_time_json": assertion.valid_time,
            "evidence_json": assertion.evidence,
        });
        let mut rows = conn
            .query(
                "SELECT json_object(
                    'assertion_kind', assertion_kind,
                    'subject_anchor_id', subject_anchor_id,
                    'object_anchor_id', object_anchor_id,
                    'knowledge_at', knowledge_at,
                    'valid_time_json', json(valid_time_json),
                    'evidence_json', json(evidence_json)
                 )
                 FROM session_assertions
                 WHERE session_id = ?1 AND generation = ?2 AND assertion_id = ?3",
                params![
                    batch.session_id().as_str(),
                    generation,
                    assertion.assertion_id.as_str()
                ],
            )
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let encoded: String = rows
            .next()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?
            .ok_or_else(|| {
                storage_message(
                    PERSIST_OPERATION,
                    "assertion insert was ignored without an existing row",
                )
            })?
            .get(0)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let actual: Value =
            serde_json::from_str(&encoded).map_err(|error| storage(PERSIST_OPERATION, error))?;
        if actual != expected {
            return Err(storage_message(
                PERSIST_OPERATION,
                format!(
                    "assertion {} conflicts with an existing immutable row",
                    assertion.assertion_id.as_str()
                ),
            ));
        }
    }
    Ok(inserted)
}

pub(super) async fn validate_assertion(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
    assertion: &TemporalAssertionRecordV1,
) -> SessionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT occurrence.source_observation_id, anchor.anchor_json
             FROM session_occurrences AS occurrence
             JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = occurrence.retrieval_anchor_id
             WHERE occurrence.session_id = ?1 AND occurrence.generation = ?2
               AND occurrence.retrieval_anchor_id = ?3",
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
                assertion.subject_anchor_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let subject = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PERSIST_OPERATION,
                "assertion subject anchor is not retained in the owning generation",
            )
        })?;
    let observation_id: String = subject
        .get(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let anchor_json: String = subject
        .get(1)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    drop(rows);
    let mut object_rows = conn
        .query(
            "SELECT occurrence.source_observation_id, anchor.anchor_json
             FROM session_occurrences AS occurrence
             JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = occurrence.retrieval_anchor_id
             WHERE occurrence.session_id = ?1 AND occurrence.generation = ?2
               AND occurrence.retrieval_anchor_id = ?3",
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
                assertion.object_anchor_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let object = object_rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PERSIST_OPERATION,
                "assertion object anchor is not retained in the owning generation",
            )
        })?;
    let object_observation_id = object
        .get::<String>(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let object_anchor_json = object
        .get::<String>(1)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    drop(object_rows);
    let observation_id = tracedecay_domain::CanonicalObservationIdV1::new(observation_id)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let (_, observation) = read_observation(conn, &observation_id).await?;
    let object_observation_id =
        tracedecay_domain::CanonicalObservationIdV1::new(object_observation_id)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let (_, object_observation) = read_observation(conn, &object_observation_id).await?;
    let subject_envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone())
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let object_envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(object_observation.payload().clone())
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let anchor: RetrievalAnchorRecord =
        serde_json::from_str(&anchor_json).map_err(|error| storage(PERSIST_OPERATION, error))?;
    let object_anchor: RetrievalAnchorRecord = serde_json::from_str(&object_anchor_json)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let valid_time = anchor
        .occurred_at()
        .map_or(TemporalValidityV1::Unknown, |interval| {
            TemporalValidityV1::Known {
                valid_at: interval.start,
            }
        });
    let semantic_valid = anchor.source_anchors().iter().any(|lineage| {
        assertion_kind_for_relation(lineage.relation()) == Some(assertion.kind)
            && lineage.anchor_id() == &assertion.object_anchor_id
            && lineage.owner() == anchor.owner()
    });
    let canonical_binding = anchor.owner() == observation.scope()
        && object_anchor.owner() == object_observation.scope()
        && anchor.owner() == object_anchor.owner()
        && anchor.source_observations().contains(&observation_id)
        && object_anchor
            .source_observations()
            .contains(&object_observation_id)
        && subject_envelope.relations().session_id() == batch.session_id()
        && object_envelope.relations().session_id() == batch.session_id();
    let mut subject_occurrence_rows = conn
        .query(
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = ?1 AND generation = ?2 AND retrieval_anchor_id = ?3
             ORDER BY occurrence_id
             LIMIT 2",
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
                assertion.subject_anchor_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let subject_occurrence_id = subject_occurrence_rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PERSIST_OPERATION,
                "assertion subject occurrence is not retained in the owning generation",
            )
        })?
        .get::<String>(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    if subject_occurrence_rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .is_some()
    {
        return Err(storage_message(
            PERSIST_OPERATION,
            "assertion subject anchor resolves to ambiguous occurrences",
        ));
    }
    drop(subject_occurrence_rows);
    let subject_occurrence_id = MessageOccurrenceIdV1::new(subject_occurrence_id)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let expected_assertion_id = derived_temporal_assertion_id(
        &subject_occurrence_id,
        assertion.kind,
        &assertion.object_anchor_id,
    );
    if !semantic_valid
        || !canonical_binding
        || assertion.assertion_id.as_str() != expected_assertion_id
        || assertion.knowledge_at != anchor.ingested_at()
        || assertion.valid_time != valid_time
        || assertion.evidence.authority != SessionAuthorityClassV1::ExplicitAnchorAssertion
        || assertion.evidence.evidence_class != anchor.evidence_class()
        || assertion.evidence.source_anchor_id != assertion.subject_anchor_id
        || &assertion.evidence.sanitization_receipt != observation.receipt().receipt()
    {
        return Err(storage_message(
            PERSIST_OPERATION,
            "assertion temporal or authority evidence is not canonical",
        ));
    }
    Ok(())
}

pub(super) const fn assertion_kind_for_relation(
    relation: AnchorProvenanceRelationV2,
) -> Option<TemporalAssertionKindV1> {
    match relation {
        AnchorProvenanceRelationV2::Corrects => Some(TemporalAssertionKindV1::Corrects),
        AnchorProvenanceRelationV2::Contradicts => Some(TemporalAssertionKindV1::Contradicts),
        AnchorProvenanceRelationV2::Supersedes => Some(TemporalAssertionKindV1::Supersedes),
        AnchorProvenanceRelationV2::Supports => Some(TemporalAssertionKindV1::Supports),
        AnchorProvenanceRelationV2::CapturedFrom
        | AnchorProvenanceRelationV2::Produced
        | AnchorProvenanceRelationV2::Observed
        | AnchorProvenanceRelationV2::ExecutedIn
        | AnchorProvenanceRelationV2::Discussed
        | AnchorProvenanceRelationV2::CopiedFrom
        | AnchorProvenanceRelationV2::DerivedFrom => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn require_edge_json(
    conn: &impl Executor,
    sql: &str,
    batch: &SessionTemporalProjectionBatchV1,
    left: &str,
    right: &str,
    expected: &str,
    edge: &str,
) -> SessionStoreResult<()> {
    let mut rows = conn
        .query(
            sql,
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
                left,
                right
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let actual: String = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PERSIST_OPERATION,
                format!("{edge} insert was ignored without an existing row"),
            )
        })?
        .get(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    if serde_json::from_str::<Value>(&actual).map_err(|error| storage(PERSIST_OPERATION, error))?
        != serde_json::from_str::<Value>(expected)
            .map_err(|error| storage(PERSIST_OPERATION, error))?
    {
        return Err(storage_message(
            PERSIST_OPERATION,
            format!("{edge} conflicts with an existing immutable row"),
        ));
    }
    Ok(())
}

pub(super) async fn rebuild_current_occurrences(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    conn.execute(
        "DELETE FROM session_current_entities
         WHERE session_id = ?1 AND generation = ?2 AND entity_kind = 'occurrence_anchor'",
        params![batch.session_id().as_str(), generation],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    conn.execute(
        "WITH ranked AS (
            SELECT retrieval_anchor_id, occurrence_id,
                   COUNT(*) OVER (PARTITION BY retrieval_anchor_id) AS occurrence_count,
                   ROW_NUMBER() OVER (
                       PARTITION BY retrieval_anchor_id
                       ORDER BY
                           CASE json_extract(valid_time_json, '$.kind')
                               WHEN 'known' THEN 1 ELSE 0
                           END DESC,
                           json_extract(valid_time_json, '$.valid_at') DESC,
                           knowledge_at DESC,
                           occurrence_id DESC
                   ) AS precedence
            FROM session_occurrences
            WHERE session_id = ?1 AND generation = ?2
         )
         INSERT INTO session_current_entities (
            session_id, generation, entity_kind, entity_id,
            current_assertion_id, current_occurrence_id, coverage_json
         )
         SELECT ?1, ?2, 'occurrence_anchor', retrieval_anchor_id,
                NULL, occurrence_id,
                json_object('occurrence_count', occurrence_count)
         FROM ranked WHERE precedence = 1",
        params![batch.session_id().as_str(), generation],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok(())
}

pub(super) async fn rebuild_assertion_derivatives(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    conn.execute(
        "DELETE FROM session_assertion_supersession
         WHERE session_id = ?1 AND generation = ?2",
        params![batch.session_id().as_str(), generation],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    conn.execute(
        "DELETE FROM session_current_entities
         WHERE session_id = ?1 AND generation = ?2 AND entity_kind = 'assertion_anchor'",
        params![batch.session_id().as_str(), generation],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;

    conn.execute(
        "INSERT INTO session_assertion_supersession (
            session_id, generation, superseded_assertion_id,
            superseding_assertion_id, created_at
         )
         WITH RECURSIVE direct (
             superseded_assertion_id, superseding_assertion_id, created_at
         ) AS (
             SELECT prior.assertion_id, current.assertion_id, current.knowledge_at
             FROM session_assertions AS current
             JOIN session_assertions AS prior
               ON prior.session_id = current.session_id
              AND prior.generation = current.generation
              AND prior.subject_anchor_id = current.object_anchor_id
             WHERE current.session_id = ?1 AND current.generation = ?2
               AND current.assertion_kind IN (?3, ?4)
               AND prior.assertion_kind IN (?3, ?4)
               AND json_extract(current.valid_time_json, '$.kind') = 'known'
               AND json_extract(prior.valid_time_json, '$.kind') = 'known'
               AND (
                    json_extract(prior.valid_time_json, '$.valid_at')
                        < json_extract(current.valid_time_json, '$.valid_at')
                    OR (
                        json_extract(prior.valid_time_json, '$.valid_at')
                            = json_extract(current.valid_time_json, '$.valid_at')
                        AND (
                            prior.knowledge_at < current.knowledge_at
                            OR (
                                prior.knowledge_at = current.knowledge_at
                                AND prior.assertion_id < current.assertion_id
                            )
                        )
                    )
               )
         ),
         transitive (
             superseded_assertion_id, superseding_assertion_id, created_at
         ) AS (
             SELECT superseded_assertion_id, superseding_assertion_id, created_at
             FROM direct
             UNION
             SELECT transitive.superseded_assertion_id,
                    direct.superseding_assertion_id, direct.created_at
             FROM transitive
             JOIN direct
               ON direct.superseded_assertion_id =
                  transitive.superseding_assertion_id
         )
         SELECT ?1, ?2, superseded_assertion_id,
                superseding_assertion_id, created_at
         FROM transitive",
        params![
            batch.session_id().as_str(),
            generation,
            TemporalAssertionKindV1::Corrects.as_str(),
            TemporalAssertionKindV1::Supersedes.as_str(),
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    conn.execute(
        "WITH RECURSIVE chains (
             root_anchor_id, assertion_id, subject_anchor_id,
             valid_at, knowledge_at
         ) AS (
             SELECT object_anchor_id, assertion_id, subject_anchor_id,
                    json_extract(valid_time_json, '$.valid_at'), knowledge_at
             FROM session_assertions
             WHERE session_id = ?1 AND generation = ?2
               AND assertion_kind IN (?3, ?4)
               AND json_extract(valid_time_json, '$.kind') = 'known'
             UNION
             SELECT chains.root_anchor_id, successor.assertion_id,
                    successor.subject_anchor_id,
                    json_extract(successor.valid_time_json, '$.valid_at'),
                    successor.knowledge_at
             FROM chains
             JOIN session_assertions AS successor
               ON successor.session_id = ?1
              AND successor.generation = ?2
              AND successor.object_anchor_id = chains.subject_anchor_id
             WHERE successor.assertion_kind IN (?3, ?4)
               AND json_extract(successor.valid_time_json, '$.kind') = 'known'
               AND (
                    chains.valid_at
                        < json_extract(successor.valid_time_json, '$.valid_at')
                    OR (
                        chains.valid_at
                            = json_extract(successor.valid_time_json, '$.valid_at')
                        AND (
                            chains.knowledge_at < successor.knowledge_at
                            OR (
                                chains.knowledge_at = successor.knowledge_at
                                AND chains.assertion_id < successor.assertion_id
                            )
                        )
                    )
               )
         ),
         ranked AS (
            SELECT assertion_id, root_anchor_id,
                   COUNT(*) OVER (PARTITION BY root_anchor_id) AS assertion_count,
                   ROW_NUMBER() OVER (
                       PARTITION BY root_anchor_id
                       ORDER BY valid_at DESC, knowledge_at DESC, assertion_id DESC
                   ) AS precedence
            FROM chains
         )
         INSERT INTO session_current_entities (
            session_id, generation, entity_kind, entity_id,
            current_assertion_id, current_occurrence_id, coverage_json
         )
         SELECT ?1, ?2, 'assertion_anchor', root_anchor_id,
                assertion_id, NULL, json_object('assertion_count', assertion_count)
         FROM ranked WHERE precedence = 1",
        params![
            batch.session_id().as_str(),
            generation,
            TemporalAssertionKindV1::Corrects.as_str(),
            TemporalAssertionKindV1::Supersedes.as_str(),
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok(())
}
