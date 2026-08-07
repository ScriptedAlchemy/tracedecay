use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_domain::{
    AgentInstanceId, AnchorProvenanceRelationV2, CanonicalObservationEnvelopeV1, CopyProofV1,
    DurableObservationV1, MessageId, MessageOccurrenceIdV1, RetrievalAnchorId,
    RetrievalAnchorRecord, SessionId, SessionProjectionGenerationV1, TemporalValidityV1, ThreadId,
    UtcMicros,
};
use tracedecay_graph_db::{GraphCancellation, GraphWatermark};
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};
use tracedecay_store::SessionStoreResult;

use super::operations::CanonicalPublicationManifest;
use super::query::{generation_i64, storage, storage_message};
use super::relations::{
    AgentHierarchyRelation, LogicalCopyRelation, SessionRelationError, SessionRelationProjection,
    SessionRelationScope, SummaryRelationNode, SummaryRelationRead, SummarySourceRef,
    ThreadHierarchyRelation, WorkflowAgentMembership,
};
use crate::RegisteredGlobalDb;

const RECONSTRUCT_OPERATION: &str = "reconstruct native session relation projection";
const DEFAULT_MAX_ENTITIES: usize = 100_000;
const DEFAULT_MAX_RELATIONS: usize = 100_000;
// Concurrent publishers may supersede a generation between the loser's
// reconstruction and its receipt acknowledgement; each retry re-reads the
// refreshed generation, so the bound only limits back-to-back supersessions.
const APPLY_PUBLICATION_RACE_ATTEMPTS: usize = 3;

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

impl RegisteredGlobalDb {
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
                scope,
                session_id,
                generation.value(),
                summary_ids,
                max_relations,
                cancellation,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        Ok((generation, relations))
    }

    pub async fn apply_active_session_relation_projection(
        &self,
        session_id: &SessionId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> SessionStoreResult<GraphWatermark> {
        let (scope, _) = self
            .session_relation_store()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let mut last_race = None;
        for _ in 0..APPLY_PUBLICATION_RACE_ATTEMPTS {
            let generation = self.active_relation_generation(session_id).await?;
            let snapshot = self
                .read_snapshot()
                .await
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
            let projection = reconstruct_session_relation_projection(
                &snapshot,
                scope,
                session_id,
                generation,
                DEFAULT_MAX_ENTITIES,
                DEFAULT_MAX_RELATIONS,
                Arc::clone(&cancellation),
            )
            .await?;
            drop(snapshot);
            let error = match super::relation_receipts::apply_relation_projection(
                self,
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
            // attempt — a newer generation superseded the one just
            // reconstructed, or a peer settled this generation's receipt
            // first. The retry re-reads the refreshed generation; a genuine
            // mismatch re-surfaces as soon as the state stops moving.
            let refreshed = self.active_relation_generation(session_id).await?;
            if refreshed == generation
                && !super::relation_receipts::relation_receipt_applied(self, session_id, generation)
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

    pub async fn recover_pending_session_relation_projections(
        &self,
        limit: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> SessionStoreResult<usize> {
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
        let (scope, _) = self
            .session_relation_store()
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let mut rows = snapshot
            .query(
                "SELECT receipt.session_id, receipt.generation, receipt.scope_kind,
                        receipt.scope_id, journal.projection_json
                 FROM session_relation_receipts AS receipt
                 LEFT JOIN session_relation_effect_journal AS journal
                   ON journal.session_id = receipt.session_id
                  AND journal.generation = receipt.generation
                 WHERE receipt.state = 'pending'
                 ORDER BY receipt.created_at, receipt.session_id, receipt.generation
                 LIMIT ?1",
                params![
                    i64::try_from(limit).map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
                ],
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
            let session_id = SessionId::new(
                row.get::<String>(0)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
            let generation = SessionProjectionGenerationV1::new(
                u64::try_from(
                    row.get::<i64>(1)
                        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
                )
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
            let scope_kind: String = row
                .get(2)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
            let scope_id: String = row
                .get(3)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
            let expected_kind = match scope {
                SessionRelationScope::ProjectSessions { .. } => "project_sessions",
                SessionRelationScope::ProfileSessions { .. } => "profile_sessions",
            };
            if scope_kind != expected_kind || scope_id != scope.identity() {
                return Err(storage_message(
                    RECONSTRUCT_OPERATION,
                    "pending relation receipt does not belong to the mounted session scope",
                ));
            }
            let projection_json = row
                .get::<Option<String>>(4)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
                .ok_or_else(|| {
                    storage_message(
                        RECONSTRUCT_OPERATION,
                        "pending relation receipt has no durable effect journal",
                    )
                })?;
            let projection: SessionRelationProjection = serde_json::from_str(&projection_json)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
            if projection.session_id != session_id || projection.generation != generation.value() {
                return Err(storage_message(
                    RECONSTRUCT_OPERATION,
                    "relation effect journal identity does not match its receipt",
                ));
            }
            if projection.scope != *scope {
                return Err(storage_message(
                    RECONSTRUCT_OPERATION,
                    "relation effect journal does not belong to the mounted session scope",
                ));
            }
            pending.push(projection);
        }
        drop(rows);
        drop(snapshot);
        for projection in &pending {
            require_not_cancelled(&cancellation)?;
            super::relation_receipts::apply_relation_projection(
                self,
                projection,
                Arc::clone(&cancellation),
            )
            .await?;
        }
        Ok(pending.len())
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

pub async fn seed_session_relation_projection(
    database: &RegisteredGlobalDb,
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<SessionRelationProjection> {
    let (scope, store) = database
        .session_relation_store()
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
    let Some(generation) = optional_active_generation(conn, session_id).await? else {
        return reconstruct_session_context(scope, session_id, 1, conn, cancellation).await;
    };
    match store.load_projection(
        scope,
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
                scope,
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
    conn: &impl QueryExecutor,
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
        occurrence_relations(&occurrences)?;
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

pub(crate) async fn reconstruct_logical_copy_relations(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    max_entities: usize,
    cancellation: Arc<dyn GraphCancellation>,
) -> SessionStoreResult<Vec<LogicalCopyRelation>> {
    if max_entities == 0 {
        return Err(storage(
            RECONSTRUCT_OPERATION,
            SessionRelationError::BudgetExhausted,
        ));
    }
    let occurrences =
        reconstruct_occurrences(conn, session_id, generation, max_entities, &cancellation).await?;
    let (logical_copies, _, _, _) = occurrence_relations(&occurrences)?;
    Ok(logical_copies)
}

async fn reconstruct_session_context(
    scope: &SessionRelationScope,
    session_id: &SessionId,
    generation: u64,
    conn: &impl QueryExecutor,
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
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    max_relations: usize,
    cancellation: &Arc<dyn GraphCancellation>,
) -> SessionStoreResult<Vec<SummaryRelationNode>> {
    let mut rows = conn
        .query(
            "SELECT node.summary_id, node.publication_json
             FROM session_summary_availability AS availability
             JOIN session_summary_nodes AS node
               ON node.summary_id = availability.summary_id
              AND node.session_id = availability.session_id
             WHERE availability.session_id = ?1
               AND availability.generation = ?2
             ORDER BY node.created_at, node.summary_id
             LIMIT ?3",
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

async fn reconstruct_occurrences(
    conn: &impl QueryExecutor,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    max_entities: usize,
    cancellation: &Arc<dyn GraphCancellation>,
) -> SessionStoreResult<Vec<CanonicalOccurrence>> {
    let mut rows = conn
        .query(
            "SELECT occurrence.occurrence_id, occurrence.retrieval_anchor_id,
                    anchor.anchor_json, occurrence.message_id,
                    occurrence.agent_id, occurrence.projection_output_ordinal,
                    occurrence.knowledge_at, occurrence.valid_time_json,
                    observation.observation_json, occurrence.thread_id
             FROM session_occurrences AS occurrence
             JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = occurrence.retrieval_anchor_id
             JOIN observations AS observation
               ON observation.observation_id = occurrence.source_observation_id
             WHERE occurrence.session_id = ?1 AND occurrence.generation = ?2
             ORDER BY occurrence.projection_output_ordinal, occurrence.occurrence_id
             LIMIT ?3",
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
        let observation: DurableObservationV1 = serde_json::from_str(
            &row.get::<String>(8)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        )
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let envelope: CanonicalObservationEnvelopeV1 =
            serde_json::from_value(observation.payload().clone())
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        let anchor: RetrievalAnchorRecord = serde_json::from_str(
            &row.get::<String>(2)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        )
        .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?;
        occurrences.push(CanonicalOccurrence {
            occurrence_id: MessageOccurrenceIdV1::new(
                row.get::<String>(0)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            retrieval_anchor_id: RetrievalAnchorId::new(
                row.get::<String>(1)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            copied_from_anchor_ids: anchor
                .source_anchors()
                .iter()
                .filter(|source| source.relation() == AnchorProvenanceRelationV2::CopiedFrom)
                .map(|source| source.anchor_id().clone())
                .collect(),
            thread_id: row
                .get::<Option<String>>(9)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
                .map(ThreadId::new)
                .transpose()
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            message_id: row
                .get::<Option<String>>(3)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
                .map(MessageId::new)
                .transpose()
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            agent_id: row
                .get::<Option<String>>(4)
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?
                .map(AgentInstanceId::new)
                .transpose()
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            parent_message_id: envelope
                .relations()
                .parent_message_id()
                .map(|id| MessageId::new(id.as_str()))
                .transpose()
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            parent_agent_id: envelope
                .relations()
                .parent_agent_id()
                .map(|id| AgentInstanceId::new(id.as_str()))
                .transpose()
                .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            parent_session_id: envelope.relations().parent_session_id().cloned(),
            ordinal: u32::try_from(
                row.get::<i64>(5)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            knowledge_at: UtcMicros(
                row.get(6)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            ),
            valid_time: serde_json::from_str(
                &row.get::<String>(7)
                    .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
            )
            .map_err(|error| storage(RECONSTRUCT_OPERATION, error))?,
        });
    }
    Ok(occurrences)
}

fn occurrence_relations(
    occurrences: &[CanonicalOccurrence],
) -> SessionStoreResult<(
    Vec<LogicalCopyRelation>,
    Vec<ThreadHierarchyRelation>,
    Vec<AgentHierarchyRelation>,
    Option<SessionId>,
)> {
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
    let mut copies = BTreeMap::new();
    let mut threads = BTreeMap::new();
    let mut agents = BTreeMap::new();
    let mut parent_session_id = None;
    // Projection output ordinals are scoped to one observation projection, so
    // session-wide precedence is the canonical (knowledge_at, ordinal,
    // occurrence_id) order used by the temporal indexes.
    let precedes = |candidate: &CanonicalOccurrence, occurrence: &CanonicalOccurrence| {
        (
            candidate.knowledge_at,
            candidate.ordinal,
            candidate.occurrence_id.as_str(),
        ) < (
            occurrence.knowledge_at,
            occurrence.ordinal,
            occurrence.occurrence_id.as_str(),
        )
    };
    for occurrence in occurrences {
        // A parent-message link normally means "reply to", which is thread
        // topology rather than evidence that the reply copied its parent. Only
        // a re-emission whose own logical message identity equals the parent
        // identity is admitted as a logical copy.
        if let (Some(message), Some(parent)) =
            (&occurrence.message_id, &occurrence.parent_message_id)
            && message == parent
            && let Some(source) = message_occurrences
                .get(parent.as_str())
                .into_iter()
                .flatten()
                .filter(|candidate| precedes(candidate, occurrence))
                .max_by_key(|candidate| {
                    (
                        candidate.knowledge_at,
                        candidate.ordinal,
                        candidate.occurrence_id.as_str(),
                    )
                })
        {
            let relation = LogicalCopyRelation {
                occurrence_id: occurrence.occurrence_id.clone(),
                copied_from_occurrence_id: source.occurrence_id.clone(),
                proof: CopyProofV1::ParentMessageLinkage {
                    source_occurrence_id: source.occurrence_id.clone(),
                    parent_message_id: parent.clone(),
                },
                knowledge_at: occurrence.knowledge_at,
                valid_time: occurrence.valid_time,
            };
            copies.insert(
                (
                    relation.occurrence_id.as_str().to_owned(),
                    relation.copied_from_occurrence_id.as_str().to_owned(),
                ),
                relation,
            );
        }
        for source_anchor in &occurrence.copied_from_anchor_ids {
            // The copier's anchor record itself asserts the lineage, so a
            // source projected in the same knowledge instant stays admissible;
            // only strictly-future occurrences are refused.
            let Some(source) = anchor_occurrences
                .get(source_anchor.as_str())
                .into_iter()
                .flatten()
                .filter(|candidate| {
                    candidate.occurrence_id != occurrence.occurrence_id
                        && (candidate.knowledge_at, candidate.ordinal)
                            <= (occurrence.knowledge_at, occurrence.ordinal)
                })
                .max_by_key(|candidate| {
                    (
                        candidate.knowledge_at,
                        candidate.ordinal,
                        candidate.occurrence_id.as_str(),
                    )
                })
            else {
                continue;
            };
            let relation = LogicalCopyRelation {
                occurrence_id: occurrence.occurrence_id.clone(),
                copied_from_occurrence_id: source.occurrence_id.clone(),
                proof: CopyProofV1::ExplicitAnchorAssertion {
                    source_occurrence_id: source.occurrence_id.clone(),
                    assertion_anchor_id: source_anchor.clone(),
                },
                knowledge_at: occurrence.knowledge_at,
                valid_time: occurrence.valid_time,
            };
            copies.insert(
                (
                    relation.occurrence_id.as_str().to_owned(),
                    relation.copied_from_occurrence_id.as_str().to_owned(),
                ),
                relation,
            );
        }
        if let (Some(parent_message), Some(child_thread)) =
            (&occurrence.parent_message_id, &occurrence.thread_id)
            && let Some(parent_occurrence) = message_occurrences
                .get(parent_message.as_str())
                .into_iter()
                .flatten()
                .filter(|candidate| precedes(candidate, occurrence))
                .max_by_key(|candidate| {
                    (
                        candidate.knowledge_at,
                        candidate.ordinal,
                        candidate.occurrence_id.as_str(),
                    )
                })
            && let Some(parent_thread) = &parent_occurrence.thread_id
            && parent_thread != child_thread
        {
            threads
                .entry((
                    parent_thread.as_str().to_owned(),
                    child_thread.as_str().to_owned(),
                ))
                .or_insert_with(|| ThreadHierarchyRelation {
                    parent_thread_id: parent_thread.clone(),
                    child_thread_id: child_thread.clone(),
                    ordinal: occurrence.ordinal,
                });
        }
        if let (Some(parent), Some(child)) = (&occurrence.parent_agent_id, &occurrence.agent_id) {
            agents
                .entry((parent.as_str().to_owned(), child.as_str().to_owned()))
                .or_insert_with(|| AgentHierarchyRelation {
                    parent_agent_id: parent.clone(),
                    child_agent_id: child.clone(),
                    ordinal: occurrence.ordinal,
                });
        }
        if let Some(parent) = &occurrence.parent_session_id {
            match &parent_session_id {
                Some(existing) if existing != parent => {
                    return Err(storage_message(
                        RECONSTRUCT_OPERATION,
                        "canonical observations disagree on parent session identity",
                    ));
                }
                Some(_) => {}
                None => parent_session_id = Some(parent.clone()),
            }
        }
    }
    Ok((
        copies.into_values().collect(),
        threads.into_values().collect(),
        agents.into_values().collect(),
        parent_session_id,
    ))
}

async fn reconstruct_session_metadata(
    conn: &impl QueryExecutor,
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

fn enforce_projection_bounds(
    projection: &SessionRelationProjection,
    max_entities: usize,
    max_relations: usize,
) -> SessionStoreResult<()> {
    let summary_relations = projection
        .summaries
        .iter()
        .try_fold(0usize, |count, summary| {
            count.checked_add(summary.sources.len()).and_then(|count| {
                count.checked_add(usize::from(summary.predecessor_summary_id.is_some()))
            })
        })
        .ok_or_else(|| storage(RECONSTRUCT_OPERATION, SessionRelationError::BudgetExhausted))?;
    let relation_count = [
        projection.logical_copies.len(),
        projection.thread_hierarchy.len(),
        projection.agent_hierarchy.len(),
        usize::from(projection.parent_session_id.is_some()),
        projection.workflow_agents.len(),
    ]
    .into_iter()
    .try_fold(summary_relations, usize::checked_add)
    .ok_or_else(|| storage(RECONSTRUCT_OPERATION, SessionRelationError::BudgetExhausted))?;
    let entity_count =
        projection
            .summaries
            .len()
            .checked_add(relation_count.checked_mul(2).ok_or_else(|| {
                storage(RECONSTRUCT_OPERATION, SessionRelationError::BudgetExhausted)
            })?)
            .ok_or_else(|| storage(RECONSTRUCT_OPERATION, SessionRelationError::BudgetExhausted))?;
    if relation_count > max_relations || entity_count > max_entities {
        return Err(storage(
            RECONSTRUCT_OPERATION,
            SessionRelationError::BudgetExhausted,
        ));
    }
    Ok(())
}

async fn optional_active_generation(
    conn: &impl QueryExecutor,
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
    conn: &impl QueryExecutor,
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
        Err(storage(
            RECONSTRUCT_OPERATION,
            SessionRelationError::Cancelled,
        ))
    } else {
        Ok(())
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
            occurrence_id: crate::session_temporal::relations::test_support::occurrence_id_for_test(
                occurrence_id,
            ),
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
    fn parent_message_reconstructs_distinct_thread_hierarchy() {
        let parent = occurrence(
            "occurrence.parent",
            "anchor.parent",
            "message.parent",
            "thread.parent",
            None,
            1,
        );
        let child = occurrence(
            "occurrence.child",
            "anchor.child",
            "message.child",
            "thread.child",
            Some("message.parent"),
            2,
        );

        let (_, threads, _, _) = occurrence_relations(&[parent, child]).unwrap();

        assert_eq!(
            threads,
            vec![ThreadHierarchyRelation {
                parent_thread_id: ThreadId::new("thread.parent").unwrap(),
                child_thread_id: ThreadId::new("thread.child").unwrap(),
                ordinal: 2,
            }]
        );
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

        let (_, threads, _, _) = occurrence_relations(&[root, child, grandchild]).unwrap();

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
        let parent = occurrence(
            "occurrence.parent",
            "anchor.parent",
            "message.parent",
            "thread.shared",
            None,
            1,
        );
        let child = occurrence(
            "occurrence.child",
            "anchor.child",
            "message.child",
            "thread.shared",
            Some("message.parent"),
            2,
        );

        let (_, threads, _, _) = occurrence_relations(&[parent, child]).unwrap();

        assert!(threads.is_empty());
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

        let (_, threads, _, _) = occurrence_relations(&[child, future_parent]).unwrap();

        assert!(threads.is_empty());
    }
}
