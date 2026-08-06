//! Generation-bound temporal session retrieval.

use std::collections::BTreeSet;

use serde::de::DeserializeOwned;
use tracedecay_domain::{
    LogicalCopyRecordV1, MessageOccurrenceRecordV1, RetrievalAnchorId, SessionCursorKeyIdV1,
    SessionCursorVersionV1, SessionId, SessionProjectionGenerationV1, SessionSummaryIdV1,
    SessionSummaryRecordV1, SignedCursorKeyRefV1, SummaryPublicationMetadataV1,
    SummarySourceHorizonV1, TemporalAssertionRecordV1, TemporalCoverageCountsV1, TemporalModeV1,
    UtcMicros,
};
use tracedecay_runtime_core::db::engine::{Row, params};
use tracedecay_store::{
    MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE, SessionFrozenWatermarksV1, SessionRetrievalPageV1,
    SessionStoreError, SessionStoreResult, SessionTemporalCapabilitiesV1,
    SessionTemporalCapabilityV1, SessionTemporalRetrievalRequestV1,
    SessionTemporalSnapshotRequestV1, SessionTemporalSnapshotV1,
};
use tracedecay_temporal_query::ports::{ExecutionControl, TemporalPortError};

use super::query::{now_micros, storage, storage_message};
use super::relations::{SessionRelationError, SummarySourceRef};
use super::store::execution_control_graph_cancellation;
use crate::RegisteredGlobalDb;

const EXPAND_OPERATION: &str = "retrieve session temporal page";
const FREEZE_OPERATION: &str = "freeze session temporal snapshot";
const MAX_SUMMARY_TOPOLOGY_RELATIONS: usize = MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE * 3;

struct SummarySeed {
    summary_id: SessionSummaryIdV1,
    summary_anchor_id: RetrievalAnchorId,
    source_horizon: SummarySourceHorizonV1,
    created_at: UtcMicros,
    publication: Option<SummaryPublicationMetadataV1>,
}

impl RegisteredGlobalDb {
    pub async fn freeze_session_temporal_snapshot_result(
        &self,
        request: SessionTemporalSnapshotRequestV1,
    ) -> SessionStoreResult<SessionTemporalSnapshotV1> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(FREEZE_OPERATION, error))?;
        let mut rows = snapshot
            .query(
                "SELECT generation, frozen_watermarks_json
                 FROM session_temporal_generations
                 WHERE session_id = ?1 AND state = 'active'
                 LIMIT 2",
                params![request.session_id().as_str()],
            )
            .await
            .map_err(|error| storage(FREEZE_OPERATION, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| storage(FREEZE_OPERATION, error))?
            .ok_or_else(|| {
                storage_message(
                    FREEZE_OPERATION,
                    "active session temporal generation is missing",
                )
            })?;
        // Row values are statement-cursor backed: own them before any
        // further Rows::next(), which advances the shared statement and would
        // make later get()/String reads observe NullValue.
        let generation = decode_generation_i64(
            row.get(0)
                .map_err(|error| storage(FREEZE_OPERATION, error))?,
            FREEZE_OPERATION,
        )?;
        let encoded: Option<String> = row
            .get(1)
            .map_err(|error| storage(FREEZE_OPERATION, error))?;
        if rows
            .next()
            .await
            .map_err(|error| storage(FREEZE_OPERATION, error))?
            .is_some()
        {
            return Err(storage_message(
                FREEZE_OPERATION,
                "active session temporal generation is not unique",
            ));
        }
        let encoded = encoded.ok_or_else(|| {
            storage_message(
                FREEZE_OPERATION,
                "active generation is missing frozen watermarks",
            )
        })?;
        // Durable JSON may pin the candidate-begin active_generation. The
        // generation column is authoritative after activation (same rule as
        // freeze_participants / activation receipts).
        let watermarks = decode_frozen_watermarks(&encoded, generation)?;
        let frozen_at = now_micros(FREEZE_OPERATION)?;
        Ok(SessionTemporalSnapshotV1::new(
            request.session_id().clone(),
            frozen_at,
            watermarks,
            SessionTemporalCapabilitiesV1::new([
                SessionTemporalCapabilityV1::FrozenWatermarks,
                SessionTemporalCapabilityV1::GenerationRebuild,
            ]),
        ))
    }

    pub async fn retrieve_session_temporal_page_result(
        &self,
        request: SessionTemporalRetrievalRequestV1,
    ) -> SessionStoreResult<SessionRetrievalPageV1> {
        let generation = i64::try_from(request.snapshot().watermarks().active_generation().value())
            .map_err(|error| storage(EXPAND_OPERATION, error))?;
        let read = self
            .read_snapshot()
            .await
            .map_err(|error| storage(EXPAND_OPERATION, error))?;
        validate_frozen_snapshot(&read, request.snapshot()).await?;
        if request.grain() == tracedecay_domain::RetrievalGrainV1::Summary {
            return retrieve_summary_page(self, &read, &request, generation).await;
        }
        let (after_knowledge, after_occurrence) = if let Some(after) = request.after_occurrence_id()
        {
            let mut cursor_rows = read
                .query(
                    "SELECT knowledge_at
                         FROM session_occurrences
                         WHERE session_id = ?1
                           AND generation = ?2
                           AND occurrence_id = ?3
                         LIMIT 2",
                    params![request.session_id().as_str(), generation, after.as_str()],
                )
                .await
                .map_err(|error| storage(EXPAND_OPERATION, error))?;
            let row = cursor_rows
                .next()
                .await
                .map_err(|error| storage(EXPAND_OPERATION, error))?
                .ok_or_else(|| {
                    storage_message(
                        EXPAND_OPERATION,
                        "temporal page cursor is outside the frozen session generation",
                    )
                })?;
            let knowledge_at = row
                .get::<i64>(0)
                .map_err(|error| storage(EXPAND_OPERATION, error))?;
            if cursor_rows
                .next()
                .await
                .map_err(|error| storage(EXPAND_OPERATION, error))?
                .is_some()
            {
                return Err(storage_message(
                    EXPAND_OPERATION,
                    "temporal page cursor is not unique",
                ));
            }
            (knowledge_at, after.as_str().to_string())
        } else {
            (i64::MIN, String::new())
        };
        let cutoff = match request.temporal_mode() {
            TemporalModeV1::AsOf { cutoff } => cutoff.0,
            _ => i64::MAX,
        };
        let fetch_limit = i64::try_from(request.page_size().saturating_add(1))
            .map_err(|error| storage(EXPAND_OPERATION, error))?;
        let mut rows = read
            .query(
                "SELECT occurrence_id, source_observation_id,
                        projection_output_ordinal, retrieval_anchor_id,
                        thread_id, thread_grouping_json,
                        turn_id, turn_grouping_json, message_id, agent_id,
                        role, knowledge_at, valid_time_json, evidence_json
                 FROM session_occurrences AS occurrence
                 WHERE occurrence.session_id = ?1
                   AND occurrence.generation = ?2
                   AND (
                       occurrence.knowledge_at > ?3
                       OR (
                           occurrence.knowledge_at = ?3
                           AND occurrence.occurrence_id > ?4
                       )
                   )
                   AND (
                       ?5 <> 'as_of'
                       OR (
                           occurrence.knowledge_at <= ?6
                           AND json_extract(occurrence.valid_time_json, '$.kind') = 'known'
                           AND json_extract(occurrence.valid_time_json, '$.valid_at') <= ?6
                       )
                   )
                   AND (
                       ?7 IN ('occurrence', 'logical_message', 'session')
                       OR (?7 = 'turn' AND occurrence.turn_id IS NOT NULL)
                       OR (?7 = 'thread' AND occurrence.thread_id IS NOT NULL)
                       OR (?7 = 'agent' AND occurrence.agent_id IS NOT NULL)
                       OR (
                           ?7 IN ('evidence_span', 'evidence_burst')
                           AND EXISTS (
                               SELECT 1
                               FROM session_derived_evidence_members AS member
                               WHERE member.session_id = occurrence.session_id
                                 AND member.generation = occurrence.generation
                                 AND member.occurrence_id = occurrence.occurrence_id
                                 AND member.evidence_kind = CASE ?7
                                     WHEN 'evidence_span' THEN 'span'
                                     ELSE 'burst'
                                 END
                           )
                       )
                   )
                 ORDER BY occurrence.knowledge_at, occurrence.occurrence_id
                 LIMIT ?8",
                params![
                    request.session_id().as_str(),
                    generation,
                    after_knowledge,
                    after_occurrence,
                    request.temporal_mode().as_str(),
                    cutoff,
                    request.grain().as_str(),
                    fetch_limit,
                ],
            )
            .await
            .map_err(|error| storage(EXPAND_OPERATION, error))?;
        let mut occurrences = Vec::with_capacity(request.page_size());
        let mut occurrence_anchors = Vec::with_capacity(request.page_size());
        let mut has_more = false;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(EXPAND_OPERATION, error))?
        {
            if occurrences.len() == request.page_size() {
                has_more = true;
                break;
            }
            let occurrence = occurrence_from_row(&row, request.session_id())?;
            occurrence_anchors.push((
                occurrence.occurrence_id.clone(),
                occurrence.retrieval_anchor_id.clone(),
            ));
            occurrences.push(occurrence);
        }
        drop(rows);

        let mut remaining =
            MAX_SESSION_TEMPORAL_RETRIEVAL_PAGE_SIZE.saturating_sub(occurrences.len());
        let copies = if remaining == 0 {
            Vec::new()
        } else {
            relation_logical_copies(self, &request, &occurrence_anchors, remaining)?
        };
        remaining = remaining.saturating_sub(copies.len());

        let mut assertions = Vec::new();
        let mut seen_assertions = BTreeSet::new();
        if remaining != 0 {
            for (_, anchor_id) in &occurrence_anchors {
                let mut assertion_rows = read
                    .query(
                        "SELECT assertion_id, assertion_kind,
                                subject_anchor_id, object_anchor_id,
                                knowledge_at, valid_time_json, evidence_json
                         FROM session_assertions AS assertion
                         WHERE assertion.session_id = ?1
                           AND assertion.generation = ?2
                           AND (
                               assertion.subject_anchor_id = ?3
                               OR assertion.object_anchor_id = ?3
                           )
                           AND (
                               ?4 <> 'as_of'
                               OR (
                                   assertion.knowledge_at <= ?5
                                   AND json_extract(
                                       assertion.valid_time_json, '$.kind'
                                   ) = 'known'
                                   AND json_extract(
                                       assertion.valid_time_json, '$.valid_at'
                                   ) <= ?5
                               )
                           )
                           AND (
                               ?4 <> 'current'
                               OR NOT EXISTS (
                                   SELECT 1
                                   FROM session_assertion_supersession AS supersession
                                   WHERE supersession.session_id = assertion.session_id
                                     AND supersession.generation = assertion.generation
                                     AND supersession.superseded_assertion_id =
                                         assertion.assertion_id
                               )
                           )
                         ORDER BY assertion.knowledge_at, assertion.assertion_id",
                        params![
                            request.session_id().as_str(),
                            generation,
                            anchor_id.as_str(),
                            request.temporal_mode().as_str(),
                            cutoff
                        ],
                    )
                    .await
                    .map_err(|error| storage(EXPAND_OPERATION, error))?;
                while remaining != 0
                    && let Some(row) = assertion_rows
                        .next()
                        .await
                        .map_err(|error| storage(EXPAND_OPERATION, error))?
                {
                    let assertion_id = row
                        .get::<String>(0)
                        .map_err(|error| storage(EXPAND_OPERATION, error))?;
                    if seen_assertions.insert(assertion_id.clone()) {
                        assertions.push(assertion_from_row(&row, assertion_id)?);
                        remaining -= 1;
                    }
                }
                if remaining == 0 {
                    break;
                }
            }
        }

        let next_after_occurrence_id = has_more
            .then(|| occurrences.last().map(|item| item.occurrence_id.clone()))
            .flatten();
        SessionRetrievalPageV1::new(
            request.snapshot().clone(),
            occurrences,
            copies,
            assertions,
            Vec::new(),
            TemporalCoverageCountsV1 {
                visible: u64::try_from(occurrence_anchors.len()).unwrap_or(u64::MAX),
                hidden: 0,
                unknown: 0,
                redacted: 0,
            },
            next_after_occurrence_id,
        )
    }
}

fn relation_logical_copies(
    db: &RegisteredGlobalDb,
    request: &SessionTemporalRetrievalRequestV1,
    occurrence_anchors: &[(
        tracedecay_domain::MessageOccurrenceIdV1,
        tracedecay_domain::RetrievalAnchorId,
    )],
    max_relations: usize,
) -> SessionStoreResult<Vec<LogicalCopyRecordV1>> {
    let control = request.execution_control();
    checkpoint_execution_control(control)?;
    let (project_id, relation_store) = db
        .session_relation_store()
        .map_err(|error| storage(EXPAND_OPERATION, error))?;
    let occurrence_ids = occurrence_anchors
        .iter()
        .map(|(occurrence_id, _)| occurrence_id.clone())
        .collect::<Vec<_>>();
    let cancellation = execution_control_graph_cancellation(control);
    checkpoint_execution_control(control)?;
    let copies = relation_store.logical_copies(
        project_id,
        request.session_id(),
        request.snapshot().watermarks().active_generation().value(),
        &occurrence_ids,
        max_relations,
        cancellation,
    );
    checkpoint_execution_control(control)?;
    copies.map_err(map_session_relation_error).map(|batches| {
        batches
            .into_iter()
            .flatten()
            .map(|copy| LogicalCopyRecordV1 {
                occurrence_id: copy.occurrence_id,
                copied_from_occurrence_id: copy.copied_from_occurrence_id,
                proof: copy.proof,
                knowledge_at: copy.knowledge_at,
                valid_time: copy.valid_time,
            })
            .collect()
    })
}

fn checkpoint_execution_control(control: &ExecutionControl) -> SessionStoreResult<()> {
    control.checkpoint().map_err(map_execution_control_error)
}

fn map_execution_control_error(error: TemporalPortError) -> SessionStoreError {
    match error {
        TemporalPortError::Cancelled => SessionStoreError::Cancelled,
        TemporalPortError::DeadlineExceeded => SessionStoreError::DeadlineExceeded,
        TemporalPortError::BudgetExceeded { resource } => {
            SessionStoreError::BudgetExceeded { resource }
        }
        _ => SessionStoreError::InvalidStateTransition {
            context: "session retrieval execution control checkpoint",
        },
    }
}

fn map_session_relation_error(error: SessionRelationError) -> SessionStoreError {
    match error {
        SessionRelationError::Cancelled => SessionStoreError::Cancelled,
        SessionRelationError::BudgetExhausted => SessionStoreError::BudgetExceeded {
            resource: "session relation traversal",
        },
        error => storage(EXPAND_OPERATION, error),
    }
}

fn relation_summary_relations(
    db: &RegisteredGlobalDb,
    request: &SessionTemporalRetrievalRequestV1,
    summary_ids: &[String],
) -> SessionStoreResult<Vec<super::relations::SummaryRelationRead>> {
    if summary_ids.is_empty() {
        return Ok(Vec::new());
    }
    let control = request.execution_control();
    checkpoint_execution_control(control)?;
    let (project_id, relation_store) = db
        .session_relation_store()
        .map_err(|error| storage(EXPAND_OPERATION, error))?;
    let cancellation = execution_control_graph_cancellation(control);
    checkpoint_execution_control(control)?;
    let relations = relation_store.summary_relations(
        project_id,
        request.session_id(),
        request.snapshot().watermarks().active_generation().value(),
        summary_ids,
        MAX_SUMMARY_TOPOLOGY_RELATIONS,
        cancellation,
    );
    checkpoint_execution_control(control)?;
    relations.map_err(map_session_relation_error)
}

async fn summary_anchor_for(
    read: &tracedecay_runtime_core::db::engine::ReadSnapshot,
    session_id: &SessionId,
    summary_id: &str,
) -> SessionStoreResult<RetrievalAnchorId> {
    let mut rows = read
        .query(
            "SELECT summary_anchor_id
             FROM session_summary_nodes
             WHERE session_id = ?1 AND summary_id = ?2
             LIMIT 2",
            params![session_id.as_str(), summary_id],
        )
        .await
        .map_err(|error| storage(EXPAND_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(EXPAND_OPERATION, error))?
        .ok_or_else(|| map_session_relation_error(SessionRelationError::Corrupt))?;
    let anchor_id = decode_text(
        row.get::<String>(0)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
    )?;
    if rows
        .next()
        .await
        .map_err(|error| storage(EXPAND_OPERATION, error))?
        .is_some()
    {
        return Err(map_session_relation_error(SessionRelationError::Corrupt));
    }
    Ok(anchor_id)
}

async fn retrieve_summary_page(
    db: &RegisteredGlobalDb,
    read: &tracedecay_runtime_core::db::engine::ReadSnapshot,
    request: &SessionTemporalRetrievalRequestV1,
    generation: i64,
) -> SessionStoreResult<SessionRetrievalPageV1> {
    if request.after_occurrence_id().is_some() {
        return Err(storage_message(
            EXPAND_OPERATION,
            "summary continuation requires the canonical temporal cursor",
        ));
    }
    let cutoff = match request.temporal_mode() {
        TemporalModeV1::AsOf { cutoff } => cutoff.0,
        _ => i64::MAX,
    };
    let fetch_limit = i64::try_from(request.page_size().saturating_add(1))
        .map_err(|error| storage(EXPAND_OPERATION, error))?;
    let mut rows = read
        .query(
            "SELECT node.summary_id, node.summary_anchor_id,
                    node.source_horizon_json, node.created_at,
                    node.publication_json
             FROM session_summary_nodes AS node
             JOIN session_summary_availability AS availability
               ON availability.session_id = node.session_id
              AND availability.generation = ?2
              AND availability.summary_id = node.summary_id
              AND availability.availability = 'available'
             WHERE node.session_id = ?1
               AND (?3 <> 'as_of' OR node.created_at <= ?4)
             ORDER BY node.created_at, node.summary_id
             LIMIT ?5",
            params![
                request.session_id().as_str(),
                generation,
                request.temporal_mode().as_str(),
                cutoff,
                fetch_limit
            ],
        )
        .await
        .map_err(|error| storage(EXPAND_OPERATION, error))?;
    let mut summary_seeds = Vec::with_capacity(request.page_size());
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(EXPAND_OPERATION, error))?
    {
        if summary_seeds.len() == request.page_size() {
            return Err(storage_message(
                EXPAND_OPERATION,
                "summary page exceeds the transitional store cursor capacity",
            ));
        }
        let summary_id = decode_text::<SessionSummaryIdV1>(
            row.get::<String>(0)
                .map_err(|error| storage(EXPAND_OPERATION, error))?,
        )?;
        let summary_anchor_id = decode_text::<RetrievalAnchorId>(
            row.get::<String>(1)
                .map_err(|error| storage(EXPAND_OPERATION, error))?,
        )?;
        let source_horizon = decode_json_value::<SummarySourceHorizonV1>(parse_json_value(
            row.get::<String>(2)
                .map_err(|error| storage(EXPAND_OPERATION, error))?,
        )?)?;
        let created_at = UtcMicros(
            row.get::<i64>(3)
                .map_err(|error| storage(EXPAND_OPERATION, error))?,
        );
        let publication = row
            .get::<Option<String>>(4)
            .map_err(|error| storage(EXPAND_OPERATION, error))?;
        summary_seeds.push(SummarySeed {
            summary_id,
            summary_anchor_id,
            source_horizon,
            created_at,
            publication: publication
                .as_deref()
                .map(decode_summary_publication)
                .transpose()?,
        });
    }
    drop(rows);

    let summary_ids = summary_seeds
        .iter()
        .map(|seed| seed.summary_id.as_str().to_owned())
        .collect::<Vec<_>>();
    let relations = relation_summary_relations(db, request, &summary_ids)?;
    if relations.len() != summary_seeds.len() {
        return Err(map_session_relation_error(SessionRelationError::Corrupt));
    }
    let mut summaries = Vec::with_capacity(summary_seeds.len());
    for (seed, relation) in summary_seeds.into_iter().zip(relations) {
        if relation.summary_id != seed.summary_id.as_str() {
            return Err(map_session_relation_error(SessionRelationError::Corrupt));
        }
        let mut source_anchors = Vec::new();
        for source in relation.sources {
            match source {
                SummarySourceRef::Anchor { anchor_id } => source_anchors.push(anchor_id),
                SummarySourceRef::Summary { summary_id } => source_anchors
                    .push(summary_anchor_for(read, request.session_id(), &summary_id).await?),
            }
        }
        let mut summary = SessionSummaryRecordV1::new(
            seed.summary_id,
            request.session_id().clone(),
            seed.summary_anchor_id,
            source_anchors,
            seed.source_horizon,
            seed.created_at,
        )?;
        if let Some(predecessor) = relation.predecessor_summary_id {
            summary = summary.with_predecessor(decode_text(predecessor)?)?;
        }
        if let Some(publication) = seed.publication {
            summary = summary.with_publication(publication)?;
        }
        summaries.push(summary);
    }
    let visible = u64::try_from(summaries.len()).unwrap_or(u64::MAX);
    SessionRetrievalPageV1::new(
        request.snapshot().clone(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        summaries,
        TemporalCoverageCountsV1 {
            visible,
            hidden: 0,
            unknown: 0,
            redacted: 0,
        },
        None,
    )
}

async fn validate_frozen_snapshot(
    read: &tracedecay_runtime_core::db::engine::ReadSnapshot,
    snapshot: &SessionTemporalSnapshotV1,
) -> SessionStoreResult<()> {
    let generation = i64::try_from(snapshot.watermarks().active_generation().value())
        .map_err(|error| storage(EXPAND_OPERATION, error))?;
    let mut rows = read
        .query(
            "SELECT state, frozen_watermarks_json
             FROM session_temporal_generations
             WHERE session_id = ?1 AND generation = ?2
             LIMIT 2",
            params![snapshot.session_id().as_str(), generation],
        )
        .await
        .map_err(|error| storage(EXPAND_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(EXPAND_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(EXPAND_OPERATION, "frozen session generation is unavailable")
        })?;
    let state = row
        .get::<String>(0)
        .map_err(|error| storage(EXPAND_OPERATION, error))?;
    let encoded = row
        .get::<String>(1)
        .map_err(|error| storage(EXPAND_OPERATION, error))?;
    if rows
        .next()
        .await
        .map_err(|error| storage(EXPAND_OPERATION, error))?
        .is_some()
    {
        return Err(storage_message(
            EXPAND_OPERATION,
            "frozen session generation is not unique",
        ));
    }
    let watermarks = decode_frozen_watermarks(&encoded, snapshot.watermarks().active_generation())?;
    if state != "active"
        || !watermarks.has_same_frontiers_and_cursor(snapshot.watermarks())
        || watermarks.active_generation() != snapshot.watermarks().active_generation()
    {
        return Err(storage_message(
            EXPAND_OPERATION,
            "session temporal snapshot drifted from the frozen generation",
        ));
    }
    Ok(())
}

fn occurrence_from_row(
    row: &Row,
    session_id: &SessionId,
) -> SessionStoreResult<MessageOccurrenceRecordV1> {
    let thread_grouping = optional_json(
        row.get::<Option<String>>(5)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
    )?;
    let turn_grouping = optional_json(
        row.get::<Option<String>>(7)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
    )?;
    decode_json_value(serde_json::json!({
        "occurrence_id": row.get::<String>(0)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "source_observation_id": row.get::<String>(1)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "projection_output_ordinal": row.get::<i64>(2)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "retrieval_anchor_id": row.get::<String>(3)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "session_id": session_id,
        "thread_id": row.get::<Option<String>>(4)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "thread_grouping": thread_grouping,
        "turn_id": row.get::<Option<String>>(6)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "turn_grouping": turn_grouping,
        "message_id": row.get::<Option<String>>(8)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "agent_id": row.get::<Option<String>>(9)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "role": row.get::<String>(10)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "knowledge_at": row.get::<i64>(11)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "valid_time": parse_json_value(
            row.get::<String>(12)
                .map_err(|error| storage(EXPAND_OPERATION, error))?
        )?,
        "evidence": parse_json_value(
            row.get::<String>(13)
                .map_err(|error| storage(EXPAND_OPERATION, error))?
        )?,
    }))
}

fn assertion_from_row(
    row: &Row,
    assertion_id: String,
) -> SessionStoreResult<TemporalAssertionRecordV1> {
    decode_json_value(serde_json::json!({
        "assertion_id": assertion_id,
        "kind": row.get::<String>(1)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "subject_anchor_id": row.get::<String>(2)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "object_anchor_id": row.get::<String>(3)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "knowledge_at": row.get::<i64>(4)
            .map_err(|error| storage(EXPAND_OPERATION, error))?,
        "valid_time": parse_json_value(
            row.get::<String>(5)
                .map_err(|error| storage(EXPAND_OPERATION, error))?
        )?,
        "evidence": parse_json_value(
            row.get::<String>(6)
                .map_err(|error| storage(EXPAND_OPERATION, error))?
        )?,
    }))
}

fn optional_json(encoded: Option<String>) -> SessionStoreResult<serde_json::Value> {
    encoded.map_or(Ok(serde_json::Value::Null), parse_json_value)
}

fn parse_json_value(encoded: String) -> SessionStoreResult<serde_json::Value> {
    serde_json::from_str(&encoded).map_err(|error| storage(EXPAND_OPERATION, error))
}

fn decode_json_value<T: DeserializeOwned>(value: serde_json::Value) -> SessionStoreResult<T> {
    serde_json::from_value(value).map_err(|error| storage(EXPAND_OPERATION, error))
}

fn decode_text<T: DeserializeOwned>(value: String) -> SessionStoreResult<T> {
    decode_json_value(serde_json::Value::String(value))
}

fn decode_summary_publication(encoded: &str) -> SessionStoreResult<SummaryPublicationMetadataV1> {
    let value = parse_json_value(encoded.to_string())?;
    let value = if value.get("version").is_some() {
        let configuration_digest = value["configuration_digest"]
            .as_str()
            .map(|digest| {
                if digest.starts_with("sha256:") {
                    digest.to_owned()
                } else {
                    format!("sha256:{digest}")
                }
            })
            .ok_or_else(|| {
                storage_message(
                    EXPAND_OPERATION,
                    "summary publication configuration digest is unavailable",
                )
            })?;
        serde_json::json!({
            "model_route": value["model_route"],
            "configuration_digest": configuration_digest,
            "sanitization_receipt": {
                "receipt_id": value["sanitization_receipt"],
                "sanitizer_version": super::operations::SANITIZER_VERSION,
            },
        })
    } else {
        value
    };
    decode_json_value(value)
}

fn decode_generation_i64(
    value: i64,
    operation: &'static str,
) -> SessionStoreResult<SessionProjectionGenerationV1> {
    let generation = u64::try_from(value).map_err(|error| storage(operation, error))?;
    SessionProjectionGenerationV1::new(generation).map_err(SessionStoreError::from)
}

fn decode_frozen_watermarks(
    encoded: &str,
    active_generation: SessionProjectionGenerationV1,
) -> SessionStoreResult<SessionFrozenWatermarksV1> {
    let value: serde_json::Value =
        serde_json::from_str(encoded).map_err(|error| storage(FREEZE_OPERATION, error))?;
    let json_generation = value["active_generation"]
        .as_u64()
        .ok_or_else(|| storage_message(FREEZE_OPERATION, "active_generation is invalid"))?;
    if json_generation > active_generation.value() {
        return Err(storage_message(
            FREEZE_OPERATION,
            "frozen watermarks active_generation exceeds the active generation column",
        ));
    }
    let source = value["source_frontier"]
        .as_u64()
        .ok_or_else(|| storage_message(FREEZE_OPERATION, "source_frontier is invalid"))?;
    let projection = value["projection_frontier"]
        .as_u64()
        .ok_or_else(|| storage_message(FREEZE_OPERATION, "projection_frontier is invalid"))?;
    let summary = value["summary_frontier"]
        .as_u64()
        .ok_or_else(|| storage_message(FREEZE_OPERATION, "summary_frontier is invalid"))?;
    let mut watermarks =
        SessionFrozenWatermarksV1::new(active_generation, source, projection, summary);
    if let Some(cursor) = value.get("cursor_key").filter(|value| !value.is_null()) {
        let key_id = cursor["key_id"]
            .as_str()
            .ok_or_else(|| storage_message(FREEZE_OPERATION, "cursor key id is invalid"))?;
        let version = cursor["version"]
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| storage_message(FREEZE_OPERATION, "cursor key version is invalid"))?;
        watermarks = watermarks.with_cursor_key(SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new(key_id)?,
            version: SessionCursorVersionV1::new(version)?,
        });
    }
    Ok(watermarks)
}
