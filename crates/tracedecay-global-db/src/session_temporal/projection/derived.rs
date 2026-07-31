use crate::db::engine::{Executor, params};
use tracedecay_domain::{
    DerivedEvidenceKindV1, DerivedEvidenceOccurrenceRefV1, MessageId, MessageOccurrenceIdV1,
    RetrievalAnchorId, SESSION_DERIVED_SPAN_MAX_MEMBERS_V1, SessionDerivedEvidencePolicyV1,
    SessionDerivedEvidenceRecordV1, SessionId, ThreadId, UtcMicros,
    derive_session_evidence_from_occurrences,
};
use tracedecay_store::{SessionStoreResult, SessionTemporalProjectionBatchV1};

use super::super::query::{PERSIST_OPERATION, generation_i64, storage, storage_message};

pub(super) async fn rebuild_derived_evidence(
    conn: &impl Executor,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    let session_id = batch.session_id().as_str();
    conn.execute(
        "DELETE FROM session_derived_evidence_members
         WHERE session_id = ?1 AND generation = ?2",
        params![session_id, generation],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    conn.execute(
        "DELETE FROM session_derived_evidence
         WHERE session_id = ?1 AND generation = ?2",
        params![session_id, generation],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;

    let occurrences = load_occurrence_refs(conn, batch.session_id(), generation).await?;
    if occurrences.is_empty() {
        return Ok(());
    }
    let policy = SessionDerivedEvidencePolicyV1 {
        span_max_members: SESSION_DERIVED_SPAN_MAX_MEMBERS_V1,
    };
    let derived =
        derive_session_evidence_from_occurrences(batch.session_id(), &occurrences, &policy)?;
    for record in derived {
        persist_derived_record(conn, batch.session_id(), generation, &record).await?;
    }
    Ok(())
}

async fn load_occurrence_refs(
    conn: &impl Executor,
    session_id: &SessionId,
    generation: i64,
) -> SessionStoreResult<Vec<DerivedEvidenceOccurrenceRefV1>> {
    let mut rows = conn
        .query(
            "SELECT occurrence.occurrence_id,
                    occurrence.retrieval_anchor_id,
                    occurrence.thread_id,
                    occurrence.message_id,
                    occurrence.knowledge_at,
                    effect.observation_sequence,
                    occurrence.projection_output_ordinal
             FROM session_occurrences AS occurrence
             JOIN session_temporal_observation_effects AS effect
               ON effect.observation_id = occurrence.source_observation_id
              AND effect.session_id = occurrence.session_id
             WHERE occurrence.session_id = ?1 AND occurrence.generation = ?2
             ORDER BY effect.observation_sequence ASC,
                      occurrence.projection_output_ordinal ASC,
                      occurrence.occurrence_id ASC",
            params![session_id.as_str(), generation],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let mut occurrences = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    {
        let occurrence_id = row
            .get::<String>(0)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let retrieval_anchor_id = row
            .get::<String>(1)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let thread_id = row
            .get::<Option<String>>(2)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let message_id = row
            .get::<Option<String>>(3)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let knowledge_at = row
            .get::<i64>(4)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let observation_sequence = row
            .get::<i64>(5)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let projection_output_ordinal = row
            .get::<i64>(6)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        occurrences.push(DerivedEvidenceOccurrenceRefV1 {
            occurrence_id: MessageOccurrenceIdV1::new(occurrence_id)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            retrieval_anchor_id: RetrievalAnchorId::new(retrieval_anchor_id)
                .map_err(|error| storage_message(PERSIST_OPERATION, error.to_string()))?,
            thread_id: thread_id
                .map(|value| {
                    ThreadId::new(value)
                        .map_err(|error| storage_message(PERSIST_OPERATION, error.to_string()))
                })
                .transpose()?,
            message_id: message_id
                .map(|value| {
                    MessageId::new(value)
                        .map_err(|error| storage_message(PERSIST_OPERATION, error.to_string()))
                })
                .transpose()?,
            knowledge_at: UtcMicros(knowledge_at),
            observation_sequence: u64::try_from(observation_sequence)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            projection_output_ordinal: u32::try_from(projection_output_ordinal)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
        });
    }
    Ok(occurrences)
}

async fn persist_derived_record(
    conn: &impl Executor,
    session_id: &SessionId,
    generation: i64,
    record: &SessionDerivedEvidenceRecordV1,
) -> SessionStoreResult<()> {
    ensure_derived_anchor(conn, record).await?;
    let evidence_json =
        serde_json::to_string(record).map_err(|error| storage(PERSIST_OPERATION, error))?;
    conn.execute(
        "INSERT INTO session_derived_evidence (
            session_id, generation, evidence_kind, evidence_id,
            retrieval_anchor_id, thread_id,
            first_occurrence_id, last_occurrence_id,
            algorithm_version, configuration_digest,
            member_count, member_digest, evidence_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            session_id.as_str(),
            generation,
            record.evidence_kind().as_str(),
            record.evidence_id().as_str(),
            record.retrieval_anchor_id().as_str(),
            record.thread_id().map(ThreadId::as_str),
            record.first_occurrence_id().as_str(),
            record.last_occurrence_id().as_str(),
            record.algorithm_version(),
            record.configuration_digest().as_str(),
            i64::from(record.member_count()),
            record.member_digest().as_str(),
            evidence_json.as_str(),
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    for member in record.members() {
        conn.execute(
            "INSERT INTO session_derived_evidence_members (
                session_id, generation, evidence_kind, evidence_id,
                ordinal, occurrence_id, member_role
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id.as_str(),
                generation,
                record.evidence_kind().as_str(),
                record.evidence_id().as_str(),
                i64::from(member.ordinal),
                member.occurrence_id.as_str(),
                member.member_role.as_str(),
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    }
    Ok(())
}

async fn ensure_derived_anchor(
    conn: &impl Executor,
    record: &SessionDerivedEvidenceRecordV1,
) -> SessionStoreResult<()> {
    let mut owner_rows = conn
        .query(
            "SELECT anchor.owner_json
             FROM session_occurrences AS occurrence
             JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = occurrence.retrieval_anchor_id
             WHERE occurrence.occurrence_id = ?1
             LIMIT 1",
            params![record.first_occurrence_id().as_str()],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let owner_json = match owner_rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    {
        Some(row) => row
            .get::<String>(0)
            .map_err(|error| storage(PERSIST_OPERATION, error))?,
        None => {
            return Err(storage_message(
                PERSIST_OPERATION,
                "derived evidence member occurrence is missing a retrieval anchor",
            ));
        }
    };
    let entity_kind = match record.evidence_kind() {
        DerivedEvidenceKindV1::Span => "evidence_span",
        DerivedEvidenceKindV1::Burst => "evidence_burst",
    };
    let anchor_json = serde_json::json!({
        "kind": "session_derived_evidence",
        "evidence_kind": record.evidence_kind().as_str(),
        "evidence_id": record.evidence_id().as_str(),
        "entity_kind": entity_kind,
        "member_count": record.member_count(),
        "member_digest": record.member_digest().as_str(),
        "authority": "derived_projection",
    })
    .to_string();
    conn.execute(
        "INSERT OR IGNORE INTO retrieval_anchors (
            anchor_id, anchor_json, owner_json, projection_generation
         ) VALUES (?1, ?2, ?3, ?4)",
        params![
            record.retrieval_anchor_id().as_str(),
            anchor_json.as_str(),
            owner_json.as_str(),
            "session-derived-evidence.v1",
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok(())
}
