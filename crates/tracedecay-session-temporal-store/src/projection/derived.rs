use tracedecay_domain::{
    DerivedEvidenceKindV1, DerivedEvidenceOccurrenceRefV1, DerivedEvidenceTailV1, MessageId,
    MessageOccurrenceIdV1, RetrievalAnchorId, SESSION_DERIVED_SPAN_MAX_MEMBERS_V1,
    SessionDerivedEvidencePolicyV1, SessionDerivedEvidenceRecordV1, SessionId, ThreadId, UtcMicros,
    extend_session_evidence,
};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{SessionStoreResult, SessionTemporalProjectionBatchV1};
use tracedecay_temporal_query::execution::ExecutionControl;

use super::super::query::{PERSIST_OPERATION, generation_i64, storage, storage_message};
use super::super::rebuild::checkpoint_relation_rebuild_control;

/// Keep one occurrence-ref read under the exact-SQL materialization ceiling
/// (10_000 rows / 64 MiB).
const OCCURRENCE_REF_PAGE_ROWS: i64 = 512;

const OCCURRENCE_REF_COLUMNS: &str = "occurrence_id, retrieval_anchor_id, thread_id, message_id,
     knowledge_at, source_sequence, projection_output_ordinal";

/// Extends the base generation's spans and bursts with the occurrences this
/// candidate introduced. Derived order is the effect sequence, and a
/// candidate only projects effects past its base frontier, so only the base's
/// last run can grow.
#[tracing::instrument(
    name = "session_temporal.projection.extend_derived",
    level = "trace",
    skip_all
)]
pub(super) async fn extend_derived_evidence(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    control: &ExecutionControl,
) -> SessionStoreResult<()> {
    checkpoint_relation_rebuild_control(control)?;
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    let session_id = batch.session_id();
    let occurrences =
        load_introduced_occurrence_refs(conn, session_id, generation, control).await?;

    let Some(first) = occurrences.first() else {
        return Ok(());
    };
    let tail = load_tail(conn, session_id, generation, first).await?;
    let policy = SessionDerivedEvidencePolicyV1 {
        span_max_members: SESSION_DERIVED_SPAN_MAX_MEMBERS_V1,
    };
    let extension = extend_session_evidence(session_id, tail.as_ref(), &occurrences, &policy)?;

    for record in &extension.records {
        checkpoint_relation_rebuild_control(control)?;
        persist_derived_record(conn, session_id, generation, record).await?;
    }
    for (kind, first_occurrence_id, member) in &extension.members {
        checkpoint_relation_rebuild_control(control)?;
        conn.execute(
            "INSERT OR REPLACE INTO session_derived_evidence_members (
                session_id, generation, evidence_kind, first_occurrence_id,
                ordinal, occurrence_id, member_role
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id.as_str(),
                generation,
                kind.as_str(),
                first_occurrence_id.as_str(),
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

async fn load_introduced_occurrence_refs(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &SessionId,
    generation: i64,
    control: &ExecutionControl,
) -> SessionStoreResult<Vec<DerivedEvidenceOccurrenceRefV1>> {
    let first_page = format!(
        "SELECT {OCCURRENCE_REF_COLUMNS}
         FROM session_occurrences INDEXED BY idx_session_occurrences_introduced
         WHERE session_id = ?1 AND generation = ?2
         ORDER BY source_sequence, projection_output_ordinal, occurrence_id
         LIMIT ?3"
    );
    let next_page = format!(
        "SELECT {OCCURRENCE_REF_COLUMNS}
         FROM session_occurrences INDEXED BY idx_session_occurrences_introduced
         WHERE session_id = ?1 AND generation = ?2
           AND (source_sequence, projection_output_ordinal, occurrence_id) > (?4, ?5, ?6)
         ORDER BY source_sequence, projection_output_ordinal, occurrence_id
         LIMIT ?3"
    );
    let mut occurrences: Vec<DerivedEvidenceOccurrenceRefV1> = Vec::new();
    loop {
        checkpoint_relation_rebuild_control(control)?;
        let mut rows = match occurrences.last() {
            None => conn
                .query(
                    &first_page,
                    params![session_id.as_str(), generation, OCCURRENCE_REF_PAGE_ROWS],
                )
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            Some(last) => conn
                .query(
                    &next_page,
                    params![
                        session_id.as_str(),
                        generation,
                        OCCURRENCE_REF_PAGE_ROWS,
                        i64::try_from(last.observation_sequence)
                            .map_err(|error| storage(PERSIST_OPERATION, error))?,
                        i64::from(last.projection_output_ordinal),
                        last.occurrence_id.as_str(),
                    ],
                )
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
        };
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?
        {
            checkpoint_relation_rebuild_control(control)?;
            page_rows += 1;
            occurrences.push(decode_occurrence_ref(&row)?);
        }
        if page_rows < OCCURRENCE_REF_PAGE_ROWS {
            return Ok(occurrences);
        }
    }
}

fn decode_occurrence_ref(
    row: &tracedecay_runtime_core::db::engine::Row,
) -> SessionStoreResult<DerivedEvidenceOccurrenceRefV1> {
    let text = |index| {
        row.get::<String>(index)
            .map_err(|error| storage(PERSIST_OPERATION, error))
    };
    let optional_text = |index| {
        row.get::<Option<String>>(index)
            .map_err(|error| storage(PERSIST_OPERATION, error))
    };
    let integer = |index| {
        row.get::<i64>(index)
            .map_err(|error| storage(PERSIST_OPERATION, error))
    };
    Ok(DerivedEvidenceOccurrenceRefV1 {
        occurrence_id: MessageOccurrenceIdV1::new(text(0)?)
            .map_err(|error| storage(PERSIST_OPERATION, error))?,
        retrieval_anchor_id: RetrievalAnchorId::new(text(1)?)
            .map_err(|error| storage_message(PERSIST_OPERATION, error.to_string()))?,
        thread_id: optional_text(2)?
            .map(|value| {
                ThreadId::new(value)
                    .map_err(|error| storage_message(PERSIST_OPERATION, error.to_string()))
            })
            .transpose()?,
        message_id: optional_text(3)?
            .map(|value| {
                MessageId::new(value)
                    .map_err(|error| storage_message(PERSIST_OPERATION, error.to_string()))
            })
            .transpose()?,
        knowledge_at: UtcMicros(integer(4)?),
        observation_sequence: u64::try_from(integer(5)?)
            .map_err(|error| storage(PERSIST_OPERATION, error))?,
        projection_output_ordinal: u32::try_from(integer(6)?)
            .map_err(|error| storage(PERSIST_OPERATION, error))?,
    })
}

/// Reads the burst and span that end at the base generation's last
/// occurrence, or `None` when the base holds no occurrence.
async fn load_tail(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &SessionId,
    generation: i64,
    first_introduced: &DerivedEvidenceOccurrenceRefV1,
) -> SessionStoreResult<Option<DerivedEvidenceTailV1>> {
    let mut rows = conn
        .query(
            &format!(
                "SELECT {OCCURRENCE_REF_COLUMNS}
                 FROM session_occurrences INDEXED BY idx_session_occurrences_source_order
                 WHERE session_id = ?1 AND generation < ?2
                 ORDER BY source_sequence DESC, projection_output_ordinal DESC,
                          occurrence_id DESC
                 LIMIT 1"
            ),
            params![session_id.as_str(), generation],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    else {
        return Ok(None);
    };
    let last = decode_occurrence_ref(&row)?;
    drop(rows);
    if (last.observation_sequence, last.projection_output_ordinal)
        > (
            first_introduced.observation_sequence,
            first_introduced.projection_output_ordinal,
        )
    {
        return Err(storage_message(
            PERSIST_OPERATION,
            "candidate occurrences precede the base generation's derived evidence order",
        ));
    }
    let burst = load_tail_record(
        conn,
        session_id,
        generation,
        DerivedEvidenceKindV1::Burst,
        &last.occurrence_id,
    )
    .await?;
    let span = load_tail_record(
        conn,
        session_id,
        generation,
        DerivedEvidenceKindV1::Span,
        &last.occurrence_id,
    )
    .await?;
    Ok(Some(DerivedEvidenceTailV1 { burst, span }))
}

async fn load_tail_record(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &SessionId,
    generation: i64,
    kind: DerivedEvidenceKindV1,
    last_occurrence_id: &MessageOccurrenceIdV1,
) -> SessionStoreResult<SessionDerivedEvidenceRecordV1> {
    let mut rows = conn
        .query(
            "SELECT evidence_json
             FROM session_derived_evidence
             WHERE session_id = ?1 AND evidence_kind = ?3
               AND last_occurrence_id = ?4 AND generation < ?2
             ORDER BY generation DESC
             LIMIT 1",
            params![
                session_id.as_str(),
                generation,
                kind.as_str(),
                last_occurrence_id.as_str()
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
                "base generation's last occurrence has no derived evidence",
            )
        })?
        .get(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    serde_json::from_str(&encoded).map_err(|error| storage(PERSIST_OPERATION, error))
}

async fn persist_derived_record(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &SessionId,
    generation: i64,
    record: &SessionDerivedEvidenceRecordV1,
) -> SessionStoreResult<()> {
    ensure_derived_anchor(conn, session_id, generation, record).await?;
    let evidence_json =
        serde_json::to_string(record).map_err(|error| storage(PERSIST_OPERATION, error))?;
    conn.execute(
        "INSERT OR REPLACE INTO session_derived_evidence (
            session_id, generation, evidence_kind, first_occurrence_id, evidence_id,
            retrieval_anchor_id, thread_id, last_occurrence_id,
            algorithm_version, configuration_digest,
            member_count, member_digest, evidence_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            session_id.as_str(),
            generation,
            record.evidence_kind().as_str(),
            record.first_occurrence_id().as_str(),
            record.evidence_id().as_str(),
            record.retrieval_anchor_id().as_str(),
            record.thread_id().map(ThreadId::as_str),
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
    Ok(())
}

const DERIVED_ANCHOR_OWNER_SQL: &str = "SELECT anchor.owner_json
     FROM session_occurrences AS occurrence
     JOIN retrieval_anchors AS anchor
       ON anchor.anchor_id = occurrence.retrieval_anchor_id
     WHERE occurrence.session_id = ?1
       AND occurrence.occurrence_id = ?3
       AND +occurrence.generation <= ?2";

async fn ensure_derived_anchor(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &SessionId,
    generation: i64,
    record: &SessionDerivedEvidenceRecordV1,
) -> SessionStoreResult<()> {
    let mut owner_rows = conn
        .query(
            DERIVED_ANCHOR_OWNER_SQL,
            params![
                session_id.as_str(),
                generation,
                record.first_occurrence_id().as_str()
            ],
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

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_runtime_core::db::engine::Executor;

    #[tokio::test]
    async fn introduced_occurrence_refs_span_more_than_one_exact_sql_page() {
        let dir = tempfile::TempDir::new().expect("occurrence dir");
        let conn = tracedecay_runtime_core::db::engine::TestConnection::open(
            &dir.path().join("occurrences.db"),
        );
        Executor::execute_batch(
            &conn,
            "CREATE TABLE session_occurrences (
                session_id TEXT, generation INTEGER, occurrence_id TEXT,
                retrieval_anchor_id TEXT, thread_id TEXT, message_id TEXT,
                knowledge_at INTEGER, source_sequence INTEGER,
                projection_output_ordinal INTEGER
             );
             CREATE INDEX idx_session_occurrences_introduced
                ON session_occurrences(session_id, generation);",
        )
        .await
        .expect("schema");
        let total = OCCURRENCE_REF_PAGE_ROWS + 3;
        for (generation, offset) in [(1, 0), (2, 1_000)] {
            for index in 0..total {
                Executor::execute(
                    &conn,
                    "INSERT INTO session_occurrences (
                        session_id, generation, occurrence_id, retrieval_anchor_id,
                        knowledge_at, source_sequence, projection_output_ordinal
                     ) VALUES ('page-session', ?1, ?2, 'anchor.page', ?3, ?3, 0)",
                    params![
                        generation,
                        format!("sha256:{:064x}", offset + index),
                        offset + index + 1
                    ],
                )
                .await
                .expect("occurrence");
            }
        }

        let session_id = SessionId::new("page-session").expect("session");
        let refs =
            load_introduced_occurrence_refs(&conn, &session_id, 2, &ExecutionControl::default())
                .await
                .expect("paged refs");

        assert_eq!(refs.len(), usize::try_from(total).expect("total"));
        assert_eq!(refs[0].observation_sequence, 1_001);
        assert_eq!(
            refs.last().expect("last").observation_sequence,
            u64::try_from(1_000 + total).expect("last sequence")
        );
    }
}
