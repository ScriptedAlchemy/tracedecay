use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_domain::{
    AgentInstanceId, CopyProofV1, MessageId, MessageOccurrenceIdV1, RetrievalAnchorId, SessionId,
    SessionProjectionGenerationV1, TemporalValidityV1, ThreadId, UtcMicros,
};
use tracedecay_graph_db::{GraphCancellation, GraphWatermark};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{SessionStoreError, SessionStoreResult};

use super::operations::CanonicalPublicationManifest;
use super::query::{generation_i64, storage, storage_message};
use super::relations::{
    AgentHierarchyRelation, LogicalCopyRelation, SessionRelationError, SessionRelationProjection,
    SessionRelationScope, SummaryRelationNode, SummaryRelationRead, SummarySourceRef,
    ThreadHierarchyRelation, WorkflowAgentMembership,
};
use crate::handle::{
    SessionTemporalAccess, SessionTemporalExec, SessionTemporalRegisteredDb,
    SessionTemporalWriteTxn,
};

const RECONSTRUCT_OPERATION: &str = "reconstruct native session relation projection";
const DEFAULT_MAX_ENTITIES: usize = 100_000;
const DEFAULT_MAX_RELATIONS: usize = 100_000;
// Concurrent publishers may supersede a generation between the loser's
// reconstruction and its receipt acknowledgement; each retry re-reads the
// refreshed generation, so the bound only limits back-to-back supersessions.
const APPLY_PUBLICATION_RACE_ATTEMPTS: usize = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionRelationRecoveryPage {
    pub recovered: usize,
    pub processed: usize,
    pub has_more: bool,
    pub next_retry_at_unix_seconds: Option<i64>,
}

struct PendingRelationRecovery {
    session_id: String,
    generation: i64,
    scope_kind: String,
    scope_id: String,
    projection_json: Option<String>,
    failure_count: i64,
}

struct CanonicalOccurrence {
    occurrence_id: MessageOccurrenceIdV1,
    retrieval_anchor_id: RetrievalAnchorId,
    copied_from_anchor_ids: Vec<RetrievalAnchorId>,
    thread_id: Option<ThreadId>,
    message_id: Option<MessageId>,
    agent_id: Option<AgentInstanceId>,
    parent_message_id: Option<MessageId>,
    parent_agent_id: Option<AgentInstanceId>,
    parent_session_id: Option<SessionId>,
    ordinal: u32,
    knowledge_at: UtcMicros,
    valid_time: TemporalValidityV1,
}

impl<D: SessionTemporalRegisteredDb + Sync> SessionTemporalAccess<'_, D> {
    pub async fn active_session_summary_relations(
        &self,
        session_id: &SessionId,
        summary_ids: &[String],
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> SessionStoreResult<(SessionProjectionGenerationV1, Vec<SummaryRelationRead>)> {
        let generation = self.active_relation_generation(session_id).await?;
        let (scope, store) = self
            .session_relation_store()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let relations = store
            .summary_relations(
                &scope,
                session_id,
                generation.value(),
                summary_ids,
                max_relations,
                cancellation,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        Ok((generation, relations))
    }

    #[tracing::instrument(
        name = "session_temporal.persist.relation_projection",
        level = "trace",
        skip_all
    )]
    pub async fn apply_active_session_relation_projection(
        &self,
        session_id: &SessionId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> SessionStoreResult<GraphWatermark> {
        crate::support::record_snapshot_admissions(1);
        let (scope, _) = self
            .session_relation_store()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let mut last_race = None;
        for _ in 0..APPLY_PUBLICATION_RACE_ATTEMPTS {
            let snapshot = self
                .read_snapshot()
                .await
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
            let generation = active_generation(&snapshot, session_id).await?;
            // Publication freezes the complete retained graph in its effect journal.
            // Availability can change without removing historical graph members, so
            // replay that authority rather than deriving membership from live roots.
            let mut rows = snapshot
                .query(
                    "SELECT projection_json FROM session_relation_effect_journal
                     WHERE session_id = ?1 AND generation = ?2",
                    params![
                        session_id.as_str(),
                        generation_i64(generation, RECONSTRUCT_OPERATION)?
                    ],
                )
                .await
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
            let projection = if let Some(row) = rows
                .next()
                .await
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
            {
                let encoded: String = row
                    .get(0)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
                serde_json::from_str::<SessionRelationProjection>(&encoded)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
            } else {
                seed_session_relation_projection(
                    self.inner(),
                    &snapshot,
                    session_id,
                    Arc::clone(&cancellation),
                )
                .await?
            };
            drop(rows);
            if projection.scope != scope
                || projection.session_id != *session_id
                || projection.generation != generation.value()
            {
                return Err(SessionStoreError::ReceiptIdentityMismatch {
                    context: "active relation projection identity",
                });
            }
            enforce_projection_bounds(&projection, DEFAULT_MAX_ENTITIES, DEFAULT_MAX_RELATIONS)?;
            super::relations::validate_projection(&projection)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
            drop(snapshot);
            let error = match super::relation_receipts::apply_relation_projection(
                self.inner(),
                &projection,
                Arc::clone(&cancellation),
            )
            .await
            {
                Ok(watermark) => return Ok(watermark),
                Err(error) => error,
            };
            // Concurrent publishers race this post-commit apply. Retry only
            // when the durable publication state provably moved past this
            // attempt, a newer generation superseded the one just
            // reconstructed, or a peer settled this generation's receipt
            // first. The retry re-reads the refreshed generation; a genuine
            // mismatch re-surfaces as soon as the state stops moving.
            let refreshed = self.active_relation_generation(session_id).await?;
            if refreshed == generation
                && !super::relation_receipts::relation_receipt_applied(
                    self.inner(),
                    session_id,
                    generation,
                )
                .await?
            {
                return Err(error);
            }
            last_race = Some(error);
        }
        Err(last_race.unwrap_or_else(|| {
            storage_message(
                RECONSTRUCT_OPERATION,
                "active session relation projection kept racing past its retry budget",
            )
        }))
    }

    #[tracing::instrument(
        name = "session_temporal.persist.recover_relations",
        level = "trace",
        skip_all
    )]
    pub async fn recover_pending_session_relation_projections(
        &self,
        limit: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> SessionStoreResult<usize> {
        Ok(self
            .recover_pending_session_relation_projection_page(limit, cancellation)
            .await?
            .recovered)
    }

    #[tracing::instrument(
        name = "session_temporal.persist.recover_relation_page",
        level = "trace",
        skip_all
    )]
    pub async fn recover_pending_session_relation_projection_page(
        &self,
        limit: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> SessionStoreResult<SessionRelationRecoveryPage> {
        if limit == 0 {
            return Err(storage(
                RECONSTRUCT_OPERATION,
                SessionRelationError::BudgetExhausted,
            ));
        }
        require_not_cancelled(&cancellation)?;
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        crate::support::record_snapshot_admissions(1);
        let (scope, _) = self
            .session_relation_store()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let mut rows = snapshot
            .query(
                "SELECT receipt.session_id, receipt.generation, receipt.scope_kind,
                        receipt.scope_id, journal.projection_json,
                        receipt.recovery_failure_count
                 FROM session_relation_receipts AS receipt
                 LEFT JOIN session_relation_effect_journal AS journal
                   ON journal.session_id = receipt.session_id
                  AND journal.generation = receipt.generation
                 WHERE receipt.state = 'pending'
                   AND receipt.recovery_state IN ('pending', 'retryable')
                   AND receipt.recovery_next_attempt_at <= unixepoch()
                 ORDER BY receipt.created_at, receipt.session_id, receipt.generation
                 LIMIT ?1",
                params![bounded_query_limit(limit)?],
            )
            .await
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let mut pending = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
        {
            require_not_cancelled(&cancellation)?;
            pending.push(PendingRelationRecovery {
                session_id: row
                    .get(0)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
                generation: row
                    .get(1)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
                scope_kind: row
                    .get(2)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
                scope_id: row
                    .get(3)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
                projection_json: row
                    .get(4)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
                failure_count: row
                    .get(5)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            });
        }
        drop(rows);
        drop(snapshot);
        let expected_kind = match scope {
            SessionRelationScope::ProjectSessions { .. } => "project_sessions",
            SessionRelationScope::ProfileSessions { .. } => "profile_sessions",
        };
        let has_more = pending.len() > limit;
        pending.truncate(limit);
        let mut recovered = 0_usize;
        let mut processed = 0_usize;
        for candidate in &pending {
            require_not_cancelled(&cancellation)?;
            processed = processed.saturating_add(1);
            let session_id = match SessionId::new(candidate.session_id.clone()) {
                Ok(session_id) => session_id,
                Err(_) => {
                    self.settle_relation_recovery_failure(
                        candidate,
                        "invalid_session_identity",
                        true,
                    )
                    .await?;
                    continue;
                }
            };
            let generation = match u64::try_from(candidate.generation)
                .ok()
                .and_then(|generation| SessionProjectionGenerationV1::new(generation).ok())
            {
                Some(generation) => generation,
                None => {
                    self.settle_relation_recovery_failure(candidate, "invalid_generation", true)
                        .await?;
                    continue;
                }
            };
            if candidate.scope_kind != expected_kind || candidate.scope_id != scope.identity() {
                self.settle_relation_recovery_failure(candidate, "scope_mismatch", true)
                    .await?;
                continue;
            }
            let Some(projection_json) = candidate.projection_json.as_deref() else {
                self.settle_relation_recovery_failure(candidate, "journal_missing", true)
                    .await?;
                continue;
            };
            let projection =
                match serde_json::from_str::<SessionRelationProjection>(projection_json) {
                    Ok(projection) => projection,
                    Err(_) => {
                        self.settle_relation_recovery_failure(candidate, "journal_malformed", true)
                            .await?;
                        continue;
                    }
                };
            if projection.session_id != session_id || projection.generation != generation.value() {
                self.settle_relation_recovery_failure(candidate, "journal_identity_mismatch", true)
                    .await?;
                continue;
            }
            if projection.scope != scope {
                self.settle_relation_recovery_failure(candidate, "journal_scope_mismatch", true)
                    .await?;
                continue;
            }
            match super::relation_receipts::apply_relation_projection(
                self.inner(),
                &projection,
                Arc::clone(&cancellation),
            )
            .await
            {
                Ok(_) => recovered = recovered.saturating_add(1),
                Err(SessionStoreError::Cancelled) => return Err(SessionStoreError::Cancelled),
                Err(SessionStoreError::DeadlineExceeded) => {
                    return Err(SessionStoreError::DeadlineExceeded);
                }
                Err(error) => {
                    let (failure_code, permanent) = relation_apply_failure_disposition(&error);
                    self.settle_relation_recovery_failure(candidate, failure_code, permanent)
                        .await?;
                }
            }
        }
        crate::support::record_output_sessions(u64::try_from(recovered).unwrap_or(u64::MAX));
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let mut retry_rows = snapshot
            .query(
                "SELECT MIN(recovery_next_attempt_at)
                 FROM session_relation_receipts
                 WHERE state = 'pending'
                   AND recovery_state IN ('pending', 'retryable')",
                (),
            )
            .await
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let next_retry_at_unix_seconds = retry_rows
            .next()
            .await
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
            .ok_or_else(|| {
                storage_message(
                    RECONSTRUCT_OPERATION,
                    "relation retry query returned no aggregate row",
                )
            })?
            .get(0)
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        Ok(SessionRelationRecoveryPage {
            recovered,
            processed,
            has_more,
            next_retry_at_unix_seconds,
        })
    }

    async fn settle_relation_recovery_failure(
        &self,
        candidate: &PendingRelationRecovery,
        failure_code: &'static str,
        permanent: bool,
    ) -> SessionStoreResult<()> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let next_count = candidate.failure_count.saturating_add(1);
        let delay_seconds = 1_i64
            .checked_shl(u32::try_from(next_count.min(8)).unwrap_or(8))
            .unwrap_or(256);
        let changed = transaction
            .execute(
                "UPDATE session_relation_receipts
                 SET recovery_state = ?4,
                     recovery_failure_code = ?5,
                     recovery_failure_count = ?6,
                     recovery_next_attempt_at = CASE
                         WHEN ?4 = 'retryable' THEN unixepoch() + ?7 ELSE 0 END
                 WHERE session_id = ?1 AND generation = ?2
                   AND state = 'pending' AND recovery_failure_count = ?3",
                params![
                    candidate.session_id.as_str(),
                    candidate.generation,
                    candidate.failure_count,
                    if permanent { "permanent" } else { "retryable" },
                    failure_code,
                    next_count,
                    delay_seconds,
                ],
            )
            .await
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        if changed != 1 {
            return Err(storage_message(
                RECONSTRUCT_OPERATION,
                "relation recovery disposition changed concurrently",
            ));
        }
        transaction
            .commit()
            .await
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        Ok(())
    }

    async fn active_relation_generation(
        &self,
        session_id: &SessionId,
    ) -> SessionStoreResult<SessionProjectionGenerationV1> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        active_generation(&snapshot, session_id).await
    }
}

#[tracing::instrument(
    name = "session_temporal.persist.seed_relation",
    level = "trace",
    skip_all
)]
pub async fn seed_session_relation_projection(
    database: &impl SessionTemporalRegisteredDb,
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<SessionRelationProjection> {
    let (scope, store) = database
        .session_relation_store()
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    let Some(generation) = optional_active_generation(conn, session_id).await? else {
        return reconstruct_session_context(&scope, session_id, 1, conn, cancellation).await;
    };
    match store.load_projection(
        &scope,
        session_id,
        generation.value(),
        DEFAULT_MAX_ENTITIES,
        DEFAULT_MAX_RELATIONS,
        Arc::clone(&cancellation),
    ) {
        Ok(projection) => Ok(projection),
        Err(SessionRelationError::NotFound) => {
            reconstruct_session_relation_projection(
                conn,
                &scope,
                session_id,
                generation,
                DEFAULT_MAX_ENTITIES,
                DEFAULT_MAX_RELATIONS,
                cancellation,
            )
            .await
        }
        Err(error) => Err(storage(RECONSTRUCT_OPERATION, error)),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn reconstruct_session_relation_projection(
    conn: &impl crate::handle::SessionTemporalQuery,
    scope: &SessionRelationScope,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    max_entities: usize,
    max_relations: usize,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<SessionRelationProjection> {
    if max_entities == 0 || max_relations == 0 {
        return Err(storage(
            RECONSTRUCT_OPERATION,
            SessionRelationError::BudgetExhausted,
        ));
    }
    require_not_cancelled(&cancellation)?;
    let summaries =
        reconstruct_summaries(conn, session_id, generation, max_relations, &cancellation).await?;
    let occurrences =
        reconstruct_occurrences(conn, session_id, generation, max_entities, &cancellation).await?;
    let (logical_copies, thread_hierarchy, agent_hierarchy, observed_parent) =
        occurrence_relations(&occurrences)?.into_parts();
    let (parent_session_id, workflow_agents) =
        reconstruct_session_metadata(conn, session_id, observed_parent, &cancellation).await?;
    let projection = SessionRelationProjection {
        scope: scope.clone(),
        session_id: session_id.clone(),
        generation: generation.value(),
        summaries,
        logical_copies,
        thread_hierarchy,
        agent_hierarchy,
        parent_session_id,
        workflow_agents,
    };
    enforce_projection_bounds(&projection, max_entities, max_relations)?;
    super::relations::validate_projection(&projection)
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    Ok(projection)
}

async fn reconstruct_session_context(
    scope: &SessionRelationScope,
    session_id: &SessionId,
    generation: u64,
    conn: &impl crate::handle::SessionTemporalQuery,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<SessionRelationProjection> {
    let (parent_session_id, workflow_agents) =
        reconstruct_session_metadata(conn, session_id, None, &cancellation).await?;
    Ok(SessionRelationProjection {
        scope: scope.clone(),
        session_id: session_id.clone(),
        generation,
        summaries: Vec::new(),
        logical_copies: Vec::new(),
        thread_hierarchy: Vec::new(),
        agent_hierarchy: Vec::new(),
        parent_session_id,
        workflow_agents,
    })
}

async fn reconstruct_summaries(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    max_relations: usize,
    cancellation: &Arc<dyn GraphCancellation>,
) -> SessionStoreResult<Vec<SummaryRelationNode>> {
    // Availability selects retrieval roots, not the immutable graph's membership:
    // a live successor still names its stale predecessor and that node's sources.
    // Bound the closure itself, retain absent members through the LEFT JOIN so
    // decoding fails closed, and leave content availability to the read authority.
    let mut rows = conn
        .query(
            "WITH RECURSIVE lineage(summary_id) AS (
                 SELECT summary_id FROM session_summary_availability
                 WHERE session_id = ?1 AND generation = ?2
                   AND availability = 'available'
                 UNION
                 SELECT json_extract(node.publication_json, '$.predecessor_summary_id')
                 FROM lineage
                 JOIN session_summary_nodes AS node
                   ON node.session_id = ?1 AND node.summary_id = lineage.summary_id
                 WHERE json_extract(node.publication_json, '$.predecessor_summary_id') IS NOT NULL
                 UNION
                 SELECT json_extract(source.value, '$.id')
                 FROM lineage
                 JOIN session_summary_nodes AS node
                   ON node.session_id = ?1 AND node.summary_id = lineage.summary_id
                 JOIN json_each(node.publication_json, '$.canonical_sources') AS source
                 WHERE json_extract(source.value, '$.kind') = 'summary'
                 LIMIT ?3
             )
             SELECT lineage.summary_id, node.publication_json
             FROM lineage
             LEFT JOIN session_summary_availability AS availability
               ON availability.session_id = ?1 AND availability.generation = ?2
              AND availability.summary_id = lineage.summary_id
             LEFT JOIN session_summary_nodes AS node
               ON node.summary_id = availability.summary_id
              AND node.session_id = availability.session_id
             ORDER BY node.created_at, lineage.summary_id",
            params![
                session_id.as_str(),
                generation_i64(generation, RECONSTRUCT_OPERATION)?,
                bounded_query_limit(max_relations)?
            ],
        )
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    let mut summaries = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
    {
        require_not_cancelled(cancellation)?;
        if summaries.len() == max_relations {
            return Err(storage(
                RECONSTRUCT_OPERATION,
                SessionRelationError::BudgetExhausted,
            ));
        }
        let summary_id: String = row
            .get(0)
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let encoded: String = row
            .get(1)
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let manifest: CanonicalPublicationManifest = serde_json::from_str(&encoded)
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let sources = manifest
            .canonical_sources
            .into_iter()
            .map(|source| match source.kind.as_str() {
                "summary" => Ok(SummarySourceRef::Summary {
                    summary_id: source.id,
                }),
                "anchor" => RetrievalAnchorId::new(source.id)
                    .map(|anchor_id| SummarySourceRef::Anchor { anchor_id })
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error)),
                _ => Err(storage_message(
                    RECONSTRUCT_OPERATION,
                    "canonical summary manifest has an invalid source kind",
                )),
            })
            .collect::<SessionStoreResult<Vec<_>>>()?;
        summaries.push(SummaryRelationNode {
            summary_id,
            sources,
            predecessor_summary_id: manifest.predecessor_summary_id,
        });
    }
    summaries.sort_by(|left, right| left.summary_id.cmp(&right.summary_id));
    Ok(summaries)
}

/// The occurrence columns relations derive from; the parent and copied-from
/// facts were copied from the canonical observation and anchor on persist.
const RELATION_OCCURRENCE_COLUMNS: &str = "occurrence.occurrence_id,
    occurrence.retrieval_anchor_id, occurrence.copied_from_anchor_ids_json,
    occurrence.message_id, occurrence.agent_id, occurrence.projection_output_ordinal,
    occurrence.knowledge_at, occurrence.valid_time_json, occurrence.thread_id,
    occurrence.parent_message_id, occurrence.parent_agent_id, occurrence.parent_session_id";

async fn reconstruct_occurrences(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    max_entities: usize,
    cancellation: &Arc<dyn GraphCancellation>,
) -> SessionStoreResult<Vec<CanonicalOccurrence>> {
    query_occurrences(
        conn,
        &format!(
            "SELECT {RELATION_OCCURRENCE_COLUMNS}
             FROM session_occurrences AS occurrence
             WHERE occurrence.session_id = ?1 AND +occurrence.generation <= ?2
             ORDER BY occurrence.projection_output_ordinal, occurrence.occurrence_id
             LIMIT ?3"
        ),
        session_id,
        generation,
        max_entities,
        cancellation,
    )
    .await
}

async fn query_occurrences(
    conn: &impl crate::handle::SessionTemporalQuery,
    sql: &str,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    max_entities: usize,
    cancellation: &Arc<dyn GraphCancellation>,
) -> SessionStoreResult<Vec<CanonicalOccurrence>> {
    let mut rows = conn
        .query(
            sql,
            params![
                session_id.as_str(),
                generation_i64(generation, RECONSTRUCT_OPERATION)?,
                bounded_query_limit(max_entities)?
            ],
        )
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    let mut occurrences = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
    {
        require_not_cancelled(cancellation)?;
        if occurrences.len() == max_entities {
            return Err(storage(
                RECONSTRUCT_OPERATION,
                SessionRelationError::BudgetExhausted,
            ));
        }
        occurrences.push(decode_occurrence(&row)?);
    }
    Ok(occurrences)
}

fn decode_occurrence(
    row: &tracedecay_runtime_core::db::engine::Row,
) -> SessionStoreResult<CanonicalOccurrence> {
    let text = |index| {
        row.get::<String>(index)
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))
    };
    let optional_text = |index| {
        row.get::<Option<String>>(index)
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))
    };
    let copied_from_anchor_ids: Vec<RetrievalAnchorId> =
        serde_json::from_str(&text(2)?).map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    Ok(CanonicalOccurrence {
        occurrence_id: MessageOccurrenceIdV1::new(text(0)?)
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        retrieval_anchor_id: RetrievalAnchorId::new(text(1)?)
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        copied_from_anchor_ids,
        message_id: optional_text(3)?
            .map(MessageId::new)
            .transpose()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        agent_id: optional_text(4)?
            .map(AgentInstanceId::new)
            .transpose()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        ordinal: u32::try_from(
            row.get::<i64>(5)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        )
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        knowledge_at: UtcMicros(
            row.get(6)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        ),
        valid_time: serde_json::from_str(&text(7)?)
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        thread_id: optional_text(8)?
            .map(ThreadId::new)
            .transpose()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        parent_message_id: optional_text(9)?
            .map(MessageId::new)
            .transpose()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        parent_agent_id: optional_text(10)?
            .map(AgentInstanceId::new)
            .transpose()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        parent_session_id: optional_text(11)?
            .map(SessionId::new)
            .transpose()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
    })
}

/// Relation precedence: occurrences order session-wide by
/// `(knowledge_at, ordinal, occurrence_id)`, the temporal index order.
fn precedence_key(occurrence: &CanonicalOccurrence) -> (UtcMicros, u32, &str) {
    (
        occurrence.knowledge_at,
        occurrence.ordinal,
        occurrence.occurrence_id.as_str(),
    )
}

/// Relations of a set of occurrences, keyed as the projection dedupes them.
///
/// Every relation looks only at occurrences that precede its subject (an
/// explicit copy also at ones sharing its instant), so relations of the
/// occurrences appended after a settled set's last instant extend that set's
/// relations without changing any of them.
#[derive(Default)]
struct OccurrenceRelations {
    copies: BTreeMap<(String, String), LogicalCopyRelation>,
    threads: BTreeMap<(String, String), ThreadHierarchyRelation>,
    agents: BTreeMap<(String, String), AgentHierarchyRelation>,
    parent_session_id: Option<SessionId>,
}

impl OccurrenceRelations {
    fn from_projection(projection: &SessionRelationProjection) -> Self {
        Self {
            copies: projection
                .logical_copies
                .iter()
                .map(|copy| {
                    (
                        (
                            copy.occurrence_id.as_str().to_owned(),
                            copy.copied_from_occurrence_id.as_str().to_owned(),
                        ),
                        copy.clone(),
                    )
                })
                .collect(),
            threads: projection
                .thread_hierarchy
                .iter()
                .map(|thread| {
                    (
                        (
                            thread.parent_thread_id.as_str().to_owned(),
                            thread.child_thread_id.as_str().to_owned(),
                        ),
                        thread.clone(),
                    )
                })
                .collect(),
            agents: projection
                .agent_hierarchy
                .iter()
                .map(|agent| {
                    (
                        (
                            agent.parent_agent_id.as_str().to_owned(),
                            agent.child_agent_id.as_str().to_owned(),
                        ),
                        agent.clone(),
                    )
                })
                .collect(),
            parent_session_id: projection.parent_session_id.clone(),
        }
    }

    /// Adds `occurrence`'s relations. `parent` is the latest occurrence
    /// preceding it whose message is its parent message; `copied_from` pairs
    /// each copied-from anchor with the latest occurrence of that anchor at
    /// or before its instant.
    fn add(
        &mut self,
        occurrence: &CanonicalOccurrence,
        parent: Option<&CanonicalOccurrence>,
        copied_from: &[(&RetrievalAnchorId, &CanonicalOccurrence)],
    ) -> SessionStoreResult<()> {
        // A parent-message link normally means "reply to", which is thread
        // topology rather than evidence that the reply copied its parent. Only
        // a re-emission whose own logical message identity equals the parent
        // identity is admitted as a logical copy.
        if let (Some(message), Some(parent_message), Some(source)) = (
            &occurrence.message_id,
            &occurrence.parent_message_id,
            parent,
        ) && message == parent_message
        {
            self.insert_copy(LogicalCopyRelation {
                occurrence_id: occurrence.occurrence_id.clone(),
                copied_from_occurrence_id: source.occurrence_id.clone(),
                proof: CopyProofV1::ParentMessageLinkage {
                    source_occurrence_id: source.occurrence_id.clone(),
                    parent_message_id: parent_message.clone(),
                },
                knowledge_at: occurrence.knowledge_at,
                valid_time: occurrence.valid_time,
            });
        }
        // The copier's anchor record itself asserts the lineage, so a source
        // projected in the same knowledge instant stays admissible; only
        // strictly-future occurrences are refused.
        for (source_anchor, source) in copied_from {
            self.insert_copy(LogicalCopyRelation {
                occurrence_id: occurrence.occurrence_id.clone(),
                copied_from_occurrence_id: source.occurrence_id.clone(),
                proof: CopyProofV1::ExplicitAnchorAssertion {
                    source_occurrence_id: source.occurrence_id.clone(),
                    assertion_anchor_id: (*source_anchor).clone(),
                },
                knowledge_at: occurrence.knowledge_at,
                valid_time: occurrence.valid_time,
            });
        }
        if let (Some(child_thread), Some(parent_occurrence)) = (&occurrence.thread_id, parent)
            && occurrence.parent_message_id.is_some()
            && let Some(parent_thread) = &parent_occurrence.thread_id
            && parent_thread != child_thread
        {
            self.threads
                .entry((
                    parent_thread.as_str().to_owned(),
                    child_thread.as_str().to_owned(),
                ))
                .and_modify(|relation| relation.ordinal = relation.ordinal.min(occurrence.ordinal))
                .or_insert_with(|| ThreadHierarchyRelation {
                    parent_thread_id: parent_thread.clone(),
                    child_thread_id: child_thread.clone(),
                    ordinal: occurrence.ordinal,
                });
        }
        if let (Some(parent), Some(child)) = (&occurrence.parent_agent_id, &occurrence.agent_id) {
            self.agents
                .entry((parent.as_str().to_owned(), child.as_str().to_owned()))
                .and_modify(|relation| relation.ordinal = relation.ordinal.min(occurrence.ordinal))
                .or_insert_with(|| AgentHierarchyRelation {
                    parent_agent_id: parent.clone(),
                    child_agent_id: child.clone(),
                    ordinal: occurrence.ordinal,
                });
        }
        if let Some(parent) = &occurrence.parent_session_id {
            match &self.parent_session_id {
                Some(existing) if existing != parent => {
                    return Err(storage_message(
                        RECONSTRUCT_OPERATION,
                        "canonical observations disagree on parent session identity",
                    ));
                }
                Some(_) => {}
                None => self.parent_session_id = Some(parent.clone()),
            }
        }
        Ok(())
    }

    fn insert_copy(&mut self, relation: LogicalCopyRelation) {
        self.copies.insert(
            (
                relation.occurrence_id.as_str().to_owned(),
                relation.copied_from_occurrence_id.as_str().to_owned(),
            ),
            relation,
        );
    }

    #[allow(clippy::type_complexity)]
    fn into_parts(
        self,
    ) -> (
        Vec<LogicalCopyRelation>,
        Vec<ThreadHierarchyRelation>,
        Vec<AgentHierarchyRelation>,
        Option<SessionId>,
    ) {
        (
            self.copies.into_values().collect(),
            self.threads.into_values().collect(),
            self.agents.into_values().collect(),
            self.parent_session_id,
        )
    }
}

fn occurrence_relations(
    occurrences: &[CanonicalOccurrence],
) -> SessionStoreResult<OccurrenceRelations> {
    let message_occurrences = occurrences
        .iter()
        .filter_map(|occurrence| {
            occurrence
                .message_id
                .as_ref()
                .map(|message| (message.as_str(), occurrence))
        })
        .fold(
            BTreeMap::<_, Vec<_>>::new(),
            |mut index, (message, occurrence)| {
                index.entry(message).or_default().push(occurrence);
                index
            },
        );
    let anchor_occurrences =
        occurrences
            .iter()
            .fold(BTreeMap::<_, Vec<_>>::new(), |mut index, occurrence| {
                index
                    .entry(occurrence.retrieval_anchor_id.as_str())
                    .or_default()
                    .push(occurrence);
                index
            });
    let mut relations = OccurrenceRelations::default();
    for occurrence in occurrences {
        let parent = occurrence.parent_message_id.as_ref().and_then(|parent| {
            message_occurrences
                .get(parent.as_str())
                .into_iter()
                .flatten()
                .filter(|candidate| precedence_key(candidate) < precedence_key(occurrence))
                .max_by_key(|candidate| precedence_key(candidate))
                .copied()
        });
        let copied_from = occurrence
            .copied_from_anchor_ids
            .iter()
            .filter_map(|anchor| {
                anchor_occurrences
                    .get(anchor.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|candidate| {
                        candidate.occurrence_id != occurrence.occurrence_id
                            && (candidate.knowledge_at, candidate.ordinal)
                                <= (occurrence.knowledge_at, occurrence.ordinal)
                    })
                    .max_by_key(|candidate| precedence_key(candidate))
                    .map(|source| (anchor, *source))
            })
            .collect::<Vec<_>>();
        relations.add(occurrence, parent, &copied_from)?;
    }
    Ok(relations)
}

/// Relations the occurrences a candidate introduced add to its base, or
/// `None` when one of them sorts before the base's last instant and could
/// change a settled occurrence's relations.
async fn introduced_occurrence_relations(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    max_entities: usize,
    cancellation: &Arc<dyn GraphCancellation>,
) -> SessionStoreResult<Option<OccurrenceRelations>> {
    let introduced = query_occurrences(
        conn,
        &format!(
            "SELECT {RELATION_OCCURRENCE_COLUMNS}
             FROM session_occurrences AS occurrence
                  INDEXED BY idx_session_occurrences_introduced
             WHERE occurrence.session_id = ?1 AND occurrence.generation = ?2
             ORDER BY occurrence.projection_output_ordinal, occurrence.occurrence_id
             LIMIT ?3"
        ),
        session_id,
        generation,
        max_entities,
        cancellation,
    )
    .await?;
    let Some(first_instant) = introduced
        .iter()
        .map(|occurrence| (occurrence.knowledge_at, occurrence.ordinal))
        .min()
    else {
        return Ok(Some(OccurrenceRelations::default()));
    };
    let generation_value = generation_i64(generation, RECONSTRUCT_OPERATION)?;
    let mut rows = conn
        .query(
            "SELECT knowledge_at, projection_output_ordinal
             FROM session_occurrences INDEXED BY idx_session_occurrences_generation_order
             WHERE session_id = ?1 AND generation < ?2
             ORDER BY knowledge_at DESC, projection_output_ordinal DESC
             LIMIT 1",
            params![session_id.as_str(), generation_value],
        )
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
    {
        let settled_instant = (
            UtcMicros(
                row.get(0)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            ),
            u32::try_from(
                row.get::<i64>(1)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        );
        if settled_instant >= first_instant {
            record_relation_reconstruction();
            return Ok(None);
        }
    }
    drop(rows);
    let mut relations = OccurrenceRelations::default();
    for occurrence in &introduced {
        require_not_cancelled(cancellation)?;
        let parent = match &occurrence.parent_message_id {
            Some(parent_message_id) => {
                latest_occurrence(
                    conn,
                    &format!(
                        "SELECT {RELATION_OCCURRENCE_COLUMNS}
                         FROM session_occurrences AS occurrence
                         WHERE occurrence.session_id = ?1 AND occurrence.message_id = ?3
                           AND +occurrence.generation <= ?2
                           AND (occurrence.knowledge_at, occurrence.projection_output_ordinal,
                                occurrence.occurrence_id) < (?4, ?5, ?6)
                         ORDER BY occurrence.knowledge_at DESC,
                                  occurrence.projection_output_ordinal DESC,
                                  occurrence.occurrence_id DESC
                         LIMIT 1"
                    ),
                    session_id,
                    generation_value,
                    parent_message_id.as_str(),
                    occurrence,
                )
                .await?
            }
            None => None,
        };
        let mut sources = Vec::with_capacity(occurrence.copied_from_anchor_ids.len());
        for anchor in &occurrence.copied_from_anchor_ids {
            if let Some(source) = latest_occurrence(
                conn,
                &format!(
                    "SELECT {RELATION_OCCURRENCE_COLUMNS}
                     FROM session_occurrences AS occurrence
                     WHERE occurrence.session_id = ?1 AND occurrence.retrieval_anchor_id = ?3
                       AND +occurrence.generation <= ?2
                       AND occurrence.occurrence_id <> ?6
                       AND (occurrence.knowledge_at, occurrence.projection_output_ordinal)
                           <= (?4, ?5)
                     ORDER BY occurrence.knowledge_at DESC,
                              occurrence.projection_output_ordinal DESC,
                              occurrence.occurrence_id DESC
                     LIMIT 1"
                ),
                session_id,
                generation_value,
                anchor.as_str(),
                occurrence,
            )
            .await?
            {
                sources.push((anchor, source));
            }
        }
        let copied_from = sources
            .iter()
            .map(|(anchor, source)| (*anchor, source))
            .collect::<Vec<_>>();
        relations.add(occurrence, parent.as_ref(), &copied_from)?;
    }
    Ok(Some(relations))
}

async fn latest_occurrence(
    conn: &impl crate::handle::SessionTemporalQuery,
    sql: &str,
    session_id: &SessionId,
    generation: i64,
    key: &str,
    subject: &CanonicalOccurrence,
) -> SessionStoreResult<Option<CanonicalOccurrence>> {
    let mut rows = conn
        .query(
            sql,
            params![
                session_id.as_str(),
                generation,
                key,
                subject.knowledge_at.0,
                i64::from(subject.ordinal),
                subject.occurrence_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
        .map(|row| decode_occurrence(&row))
        .transpose()
}

#[inline(always)]
fn record_relation_reconstruction() {}

/// Logical copies a candidate generation introduced, or every copy of the
/// generation when an introduced occurrence precedes the base's last instant.
pub(crate) enum IntroducedCopies {
    Extend(Vec<LogicalCopyRelation>),
    Reconstructed(Vec<LogicalCopyRelation>),
}

pub(crate) async fn introduced_logical_copies(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<IntroducedCopies> {
    match introduced_occurrence_relations(
        conn,
        session_id,
        generation,
        DEFAULT_MAX_ENTITIES,
        &cancellation,
    )
    .await?
    {
        Some(relations) => Ok(IntroducedCopies::Extend(relations.into_parts().0)),
        None => {
            let occurrences = reconstruct_occurrences(
                conn,
                session_id,
                generation,
                DEFAULT_MAX_ENTITIES,
                &cancellation,
            )
            .await?;
            Ok(IntroducedCopies::Reconstructed(
                occurrence_relations(&occurrences)?.into_parts().0,
            ))
        }
    }
}

/// Relation projection of a candidate generation: its base's applied
/// projection extended by the relations of the occurrences it introduced,
/// with summaries and session context read at the candidate. A base without
/// an applied projection, or an introduced occurrence that sorts before the
/// base's last instant, reconstructs the whole session instead.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn candidate_session_relation_projection(
    conn: &impl crate::handle::SessionTemporalQuery,
    scope: &SessionRelationScope,
    store: &super::relations::SessionRelationGraphStore,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    max_entities: usize,
    max_relations: usize,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<SessionRelationProjection> {
    let generation_value = generation_i64(generation, RECONSTRUCT_OPERATION)?;
    let mut rows = conn
        .query(
            "SELECT generation FROM session_temporal_generations
             WHERE session_id = ?1 AND generation < ?2 AND state = 'active'",
            params![session_id.as_str(), generation_value],
        )
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    let base_generation = rows
        .next()
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
        .map(|row| {
            row.get::<i64>(0)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))
        })
        .transpose()?;
    drop(rows);
    let reconstruct = || {
        reconstruct_session_relation_projection(
            conn,
            scope,
            session_id,
            generation,
            max_entities,
            max_relations,
            Arc::clone(&cancellation),
        )
    };
    let Some(base_generation) = base_generation else {
        return reconstruct().await;
    };
    let base_generation =
        u64::try_from(base_generation).map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    let base = match store.load_projection(
        scope,
        session_id,
        base_generation,
        max_entities,
        max_relations,
        Arc::clone(&cancellation),
    ) {
        Ok(base) => base,
        Err(SessionRelationError::NotFound) => {
            record_relation_reconstruction();
            return reconstruct().await;
        }
        Err(error) => return Err(storage(RECONSTRUCT_OPERATION, error)),
    };
    let Some(introduced) =
        introduced_occurrence_relations(conn, session_id, generation, max_entities, &cancellation)
            .await?
    else {
        return reconstruct().await;
    };
    let mut relations = OccurrenceRelations::from_projection(&base);
    let (copies, threads, agents, observed_parent) = introduced.into_parts();
    for copy in copies {
        relations.insert_copy(copy);
    }
    for thread in threads {
        relations
            .threads
            .entry((
                thread.parent_thread_id.as_str().to_owned(),
                thread.child_thread_id.as_str().to_owned(),
            ))
            .and_modify(|relation| relation.ordinal = relation.ordinal.min(thread.ordinal))
            .or_insert(thread);
    }
    for agent in agents {
        relations
            .agents
            .entry((
                agent.parent_agent_id.as_str().to_owned(),
                agent.child_agent_id.as_str().to_owned(),
            ))
            .and_modify(|relation| relation.ordinal = relation.ordinal.min(agent.ordinal))
            .or_insert(agent);
    }
    let observed_parent = match (observed_parent, relations.parent_session_id.take()) {
        (Some(observed), Some(settled)) if observed != settled => {
            return Err(storage_message(
                RECONSTRUCT_OPERATION,
                "canonical observations disagree on parent session identity",
            ));
        }
        (observed, settled) => observed.or(settled),
    };
    let summaries =
        reconstruct_summaries(conn, session_id, generation, max_relations, &cancellation).await?;
    let (logical_copies, thread_hierarchy, agent_hierarchy, _) = relations.into_parts();
    let (parent_session_id, workflow_agents) =
        reconstruct_session_metadata(conn, session_id, observed_parent, &cancellation).await?;
    let projection = SessionRelationProjection {
        scope: scope.clone(),
        session_id: session_id.clone(),
        generation: generation.value(),
        summaries,
        logical_copies,
        thread_hierarchy,
        agent_hierarchy,
        parent_session_id,
        workflow_agents,
    };
    enforce_projection_bounds(&projection, max_entities, max_relations)?;
    super::relations::validate_projection(&projection)
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    Ok(projection)
}

async fn reconstruct_session_metadata(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    observed_parent: Option<SessionId>,
    cancellation: &Arc<dyn GraphCancellation>,
) -> SessionStoreResult<(Option<SessionId>, Vec<WorkflowAgentMembership>)> {
    require_not_cancelled(cancellation)?;
    let mut rows = conn
        .query(
            "SELECT DISTINCT parent_session_id
             FROM sessions
             WHERE session_id = ?1 AND parent_session_id IS NOT NULL
             ORDER BY parent_session_id",
            params![session_id.as_str()],
        )
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    let stored_parent = rows
        .next()
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
        .map(|row| {
            SessionId::new(
                row.get::<String>(0)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))
        })
        .transpose()?;
    if rows
        .next()
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
        .is_some()
    {
        return Err(storage_message(
            RECONSTRUCT_OPERATION,
            "session identity has more than one parent",
        ));
    }
    let parent = match (observed_parent, stored_parent) {
        (Some(observed), Some(stored)) if observed != stored => {
            return Err(storage_message(
                RECONSTRUCT_OPERATION,
                "canonical observation and session parent identities disagree",
            ));
        }
        (Some(parent), _) | (_, Some(parent)) => Some(parent),
        (None, None) => None,
    };
    let mut rows = conn
        .query(
            "SELECT DISTINCT agent.run_id, agent.agent_label
             FROM workflow_agents AS agent
             LEFT JOIN sessions AS session
               ON session.session_id = ?1
              AND agent.transcript_path = session.transcript_path
             WHERE agent.agent_session_id = ?1 OR session.session_id IS NOT NULL
             ORDER BY agent.run_id, agent.agent_label",
            params![session_id.as_str()],
        )
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    let mut memberships = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
    {
        require_not_cancelled(cancellation)?;
        memberships.push(WorkflowAgentMembership {
            run_id: row
                .get(0)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            agent_label: row
                .get(1)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        });
    }
    Ok((parent, memberships))
}

fn projection_budget(count: usize, add: usize) -> SessionStoreResult<usize> {
    count
        .checked_add(add)
        .ok_or_else(|| storage(RECONSTRUCT_OPERATION, SessionRelationError::BudgetExhausted))
}

fn enforce_projection_bounds(
    projection: &SessionRelationProjection,
    max_entities: usize,
    max_relations: usize,
) -> SessionStoreResult<()> {
    let mut summary_relations = 0usize;
    for summary in &projection.summaries {
        summary_relations = projection_budget(summary_relations, summary.sources.len())?;
        summary_relations = projection_budget(
            summary_relations,
            usize::from(summary.predecessor_summary_id.is_some()),
        )?;
    }
    let mut relation_count = summary_relations;
    for count in [
        projection.logical_copies.len(),
        projection.thread_hierarchy.len(),
        projection.agent_hierarchy.len(),
        usize::from(projection.parent_session_id.is_some()),
        projection.workflow_agents.len(),
    ] {
        relation_count = projection_budget(relation_count, count)?;
    }
    let entity_count = projection_budget(relation_count, relation_count)
        .and_then(|doubled| projection_budget(projection.summaries.len(), doubled))?;
    if relation_count > max_relations || entity_count > max_entities {
        return Err(storage(
            RECONSTRUCT_OPERATION,
            SessionRelationError::BudgetExhausted,
        ));
    }
    Ok(())
}

async fn optional_active_generation(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
) -> SessionStoreResult<Option<SessionProjectionGenerationV1>> {
    let mut rows = conn
        .query(
            "SELECT generation
             FROM session_temporal_generations
             WHERE session_id = ?1 AND state = 'active'
             ORDER BY generation",
            params![session_id.as_str()],
        )
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    let generation = rows
        .next()
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
        .map(|row| {
            let raw: i64 = row
                .get(0)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
            SessionProjectionGenerationV1::new(
                u64::try_from(raw).map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))
        })
        .transpose()?;
    if rows
        .next()
        .await
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
        .is_some()
    {
        return Err(storage_message(
            RECONSTRUCT_OPERATION,
            "session has more than one active relation generation receipt",
        ));
    }
    Ok(generation)
}

async fn active_generation(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
) -> SessionStoreResult<SessionProjectionGenerationV1> {
    optional_active_generation(conn, session_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                RECONSTRUCT_OPERATION,
                "active session relation generation receipt is unavailable",
            )
        })
}

fn bounded_query_limit(limit: usize) -> SessionStoreResult<i64> {
    let bounded = limit
        .checked_add(1)
        .ok_or_else(|| storage(RECONSTRUCT_OPERATION, SessionRelationError::BudgetExhausted))?;
    i64::try_from(bounded).map_err(|error| storage(RECONSTRUCT_OPERATION, error))
}

fn require_not_cancelled(cancellation: &Arc<dyn GraphCancellation>) -> SessionStoreResult<()> {
    if cancellation.is_cancelled() {
        Err(SessionStoreError::Cancelled)
    } else {
        Ok(())
    }
}

fn relation_apply_failure_disposition(error: &SessionStoreError) -> (&'static str, bool) {
    match error {
        SessionStoreError::UnsupportedCapability { .. } => ("relation_apply_unsupported", true),
        SessionStoreError::BudgetExceeded { .. } => ("relation_apply_budget_exhausted", true),
        SessionStoreError::ReceiptIdentityMismatch { .. } => ("relation_receipt_mismatch", true),
        SessionStoreError::SessionMismatch { .. } => ("relation_scope_mismatch", true),
        SessionStoreError::Storage { source, .. } => {
            match source.downcast_ref::<SessionRelationError>() {
                Some(SessionRelationError::Invalid) => ("relation_apply_invalid", true),
                Some(SessionRelationError::Cycle) => ("relation_apply_cycle", true),
                Some(SessionRelationError::Conflict) => ("relation_apply_conflict", true),
                Some(SessionRelationError::ResetRequired) => {
                    ("relation_apply_reset_required", true)
                }
                Some(SessionRelationError::Corrupt) => ("relation_apply_corrupt", true),
                Some(SessionRelationError::BudgetExhausted) => {
                    ("relation_apply_budget_exhausted", true)
                }
                _ => ("relation_apply_failed", false),
            }
        }
        _ => ("relation_apply_failed", false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occurrence(
        occurrence_id: &str,
        anchor_id: &str,
        message_id: &str,
        thread_id: &str,
        parent_message_id: Option<&str>,
        ordinal: u32,
    ) -> CanonicalOccurrence {
        CanonicalOccurrence {
            occurrence_id: crate::relations::test_support::occurrence_id_for_test(occurrence_id),
            retrieval_anchor_id: RetrievalAnchorId::new(anchor_id).unwrap(),
            copied_from_anchor_ids: Vec::new(),
            thread_id: Some(ThreadId::new(thread_id).unwrap()),
            message_id: Some(MessageId::new(message_id).unwrap()),
            agent_id: None,
            parent_message_id: parent_message_id.map(|value| MessageId::new(value).unwrap()),
            parent_agent_id: None,
            parent_session_id: None,
            ordinal,
            knowledge_at: UtcMicros(i64::from(ordinal)),
            valid_time: TemporalValidityV1::Unknown,
        }
    }

    #[test]
    fn parent_messages_reconstruct_the_full_thread_chain() {
        let root = occurrence(
            "occurrence.root",
            "anchor.root",
            "message.root",
            "thread.root",
            None,
            1,
        );
        let child = occurrence(
            "occurrence.child",
            "anchor.child",
            "message.child",
            "thread.child",
            Some("message.root"),
            2,
        );
        let grandchild = occurrence(
            "occurrence.grandchild",
            "anchor.grandchild",
            "message.grandchild",
            "thread.grandchild",
            Some("message.child"),
            3,
        );

        let (_, threads, _, _) = occurrence_relations(&[root, child, grandchild])
            .unwrap()
            .into_parts();

        assert_eq!(
            threads,
            vec![
                ThreadHierarchyRelation {
                    parent_thread_id: ThreadId::new("thread.child").unwrap(),
                    child_thread_id: ThreadId::new("thread.grandchild").unwrap(),
                    ordinal: 3,
                },
                ThreadHierarchyRelation {
                    parent_thread_id: ThreadId::new("thread.root").unwrap(),
                    child_thread_id: ThreadId::new("thread.child").unwrap(),
                    ordinal: 2,
                },
            ]
        );
    }

    #[test]
    fn parent_message_in_the_same_thread_does_not_fabricate_hierarchy() {
        let parent = || {
            occurrence(
                "occurrence.parent",
                "anchor.parent",
                "message.parent",
                "thread.shared",
                None,
                1,
            )
        };
        let child = || {
            occurrence(
                "occurrence.child",
                "anchor.child",
                "message.child",
                "thread.shared",
                Some("message.parent"),
                2,
            )
        };

        let (_, threads, _, _) = occurrence_relations(&[parent(), child()])
            .unwrap()
            .into_parts();
        assert!(threads.is_empty());

        let other_thread = occurrence(
            "occurrence.other",
            "anchor.other",
            "message.other",
            "thread.other",
            Some("message.parent"),
            3,
        );
        let (_, threads, _, _) = occurrence_relations(&[parent(), child(), other_thread])
            .unwrap()
            .into_parts();
        assert_eq!(
            threads,
            vec![ThreadHierarchyRelation {
                parent_thread_id: ThreadId::new("thread.shared").unwrap(),
                child_thread_id: ThreadId::new("thread.other").unwrap(),
                ordinal: 3,
            }]
        );
    }

    #[test]
    fn parent_message_does_not_bind_to_a_future_occurrence() {
        let child = occurrence(
            "occurrence.child",
            "anchor.child",
            "message.child",
            "thread.child",
            Some("message.parent"),
            1,
        );
        let future_parent = occurrence(
            "occurrence.future-parent",
            "anchor.future-parent",
            "message.parent",
            "thread.future-parent",
            None,
            2,
        );

        let (_, threads, _, _) = occurrence_relations(&[child, future_parent])
            .unwrap()
            .into_parts();
        assert!(threads.is_empty());

        let earlier_parent = occurrence(
            "occurrence.earlier-parent",
            "anchor.earlier-parent",
            "message.parent",
            "thread.earlier-parent",
            None,
            1,
        );
        let later_child = occurrence(
            "occurrence.later-child",
            "anchor.later-child",
            "message.child",
            "thread.child",
            Some("message.parent"),
            2,
        );
        let (_, threads, _, _) = occurrence_relations(&[earlier_parent, later_child])
            .unwrap()
            .into_parts();
        assert_eq!(
            threads,
            vec![ThreadHierarchyRelation {
                parent_thread_id: ThreadId::new("thread.earlier-parent").unwrap(),
                child_thread_id: ThreadId::new("thread.child").unwrap(),
                ordinal: 2,
            }]
        );
    }
}
