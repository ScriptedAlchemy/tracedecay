use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use libsql::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    AnchorProvenanceRelationV2, CanonicalObservationEnvelopeV1, CopyProofV1, LogicalCopyRecordV1,
    MessageId, MessageOccurrenceIdV1, MessageOccurrenceRecordV1, RetrievalAnchorRecord,
    SessionAuthorityClassV1, SessionId, TemporalAssertionKindV1, TemporalAssertionRecordV1,
    TemporalCoverageCountsV1, TemporalValidityV1, UtcMicros, derive_exact_observation_anchor_id,
};
use tracedecay_store::{
    MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS, ObservationProjection, ProjectionStoreError,
    ProjectionStoreResult, SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1,
    SessionRefreshProgressV1, SessionStoreError, SessionStoreResult, SessionTemporalDigestV1,
    SessionTemporalProjectionBatchReceiptV1, SessionTemporalProjectionBatchV1,
};

use super::super::GlobalDb;
use super::super::observation_projection::derive_projection;
use super::query::{
    PERSIST_OPERATION, encode_watermarks, frontier_i64, generation_i64, now_micros,
    read_generation, read_observation, storage, storage_message,
};
use super::refresh::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};

const DISCOVER_REFRESH: &str = "discover session temporal refresh";
const MATERIALIZE_REFRESH: &str = "materialize session temporal refresh";

#[derive(Default)]
pub(super) struct ParentMessageResolver {
    occurrences: BTreeMap<String, BTreeSet<String>>,
}

impl ParentMessageResolver {
    pub(super) fn register(&mut self, message_id: &str, occurrence_id: &str) {
        self.occurrences
            .entry(message_id.to_owned())
            .or_default()
            .insert(occurrence_id.to_owned());
    }

    pub(super) fn reject_ambiguity(&self, operation: &'static str) -> SessionStoreResult<()> {
        if let Some((message_id, occurrences)) = self
            .occurrences
            .iter()
            .find(|(_, occurrences)| occurrences.len() > 1)
        {
            return Err(storage_message(
                operation,
                format!(
                    "session-scoped message id {message_id} resolves to {} occurrences",
                    occurrences.len()
                ),
            ));
        }
        Ok(())
    }

    pub(super) fn resolve(&self, message_id: &str) -> Option<&str> {
        self.occurrences
            .get(message_id)
            .and_then(|occurrences| occurrences.first())
            .map(String::as_str)
    }
}

pub(super) async fn canonical_parent_message_resolver(
    conn: &Connection,
    session_id: &str,
    source_frontier: u64,
    operation: &'static str,
) -> SessionStoreResult<ParentMessageResolver> {
    let mut resolver = ParentMessageResolver::default();
    let mut rows = conn
        .query(
            "SELECT observation_json
             FROM observations
             WHERE sequence <= ?1
             ORDER BY sequence, observation_id",
            params![frontier_i64(source_frontier, operation)?],
        )
        .await
        .map_err(|error| storage(operation, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(operation, error))?
    {
        let encoded: String = row.get(0).map_err(|error| storage(operation, error))?;
        let observation: tracedecay_domain::DurableObservationV1 =
            serde_json::from_str(&encoded).map_err(|error| storage(operation, error))?;
        let projection =
            derive_projection(&observation).map_err(|error| storage(operation, error))?;
        for output in projection
            .messages()
            .filter(|output| output.session().session_id == session_id)
        {
            let occurrence_id = MessageOccurrenceIdV1::derive(
                observation.observation_id(),
                tracedecay_domain::ProjectionOutputOrdinalV1::new(output.output_ordinal()),
            );
            resolver.register(&output.message().message_id, occurrence_id.as_str());
        }
    }
    resolver.reject_ambiguity(operation)?;
    Ok(resolver)
}

impl GlobalDb {
    pub(crate) async fn pending_session_temporal_refresh_requests_result(
        &self,
        limit: usize,
    ) -> SessionStoreResult<Vec<SessionRefreshBeginOrJoinRequestV1>> {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
        let limit = i64::try_from(limit).map_err(|error| storage(DISCOVER_REFRESH, error))?;
        let mut rows = snapshot
            .query(
                "WITH active AS (
                    SELECT session_id, frozen_watermarks_json
                    FROM session_temporal_generations
                    WHERE state = 'active'
                 ),
                 running AS (
                    SELECT session_id
                    FROM session_refresh_operations
                    WHERE state = 'running'
                 )
                 SELECT effect.session_id,
                        MAX(effect.observation_sequence),
                        COALESCE(
                            CAST(json_extract(
                                active.frozen_watermarks_json,
                                '$.projection_frontier'
                            ) AS INTEGER),
                            0
                        )
                 FROM session_temporal_observation_effects AS effect
                 LEFT JOIN active ON active.session_id = effect.session_id
                 LEFT JOIN running ON running.session_id = effect.session_id
                 WHERE running.session_id IS NULL
                 GROUP BY effect.session_id
                 HAVING MAX(CASE
                     WHEN effect.output_count > 0 THEN effect.observation_sequence
                     ELSE NULL
                 END) > COALESCE(
                    CAST(json_extract(
                        active.frozen_watermarks_json,
                        '$.projection_frontier'
                    ) AS INTEGER),
                    0
                )
                 ORDER BY effect.session_id
                 LIMIT ?1",
                params![limit],
            )
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
        let mut requests = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(DISCOVER_REFRESH, error))?
        {
            let session_id = SessionId::new(
                row.get::<String>(0)
                    .map_err(|error| storage(DISCOVER_REFRESH, error))?,
            )
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
            let observed_through = u64::try_from(
                row.get::<i64>(1)
                    .map_err(|error| storage(DISCOVER_REFRESH, error))?,
            )
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
            let committed_through = u64::try_from(
                row.get::<i64>(2)
                    .map_err(|error| storage(DISCOVER_REFRESH, error))?,
            )
            .map_err(|error| storage(DISCOVER_REFRESH, error))?;
            requests.push(SessionRefreshBeginOrJoinRequestV1::new(
                session_id,
                SessionRefreshFrontierV1::new(observed_through, committed_through)?,
            ));
        }
        Ok(requests)
    }

    pub(crate) async fn materialize_session_temporal_refresh_batch_result(
        &self,
        recovery: &SessionRefreshRecoveryV1,
    ) -> SessionStoreResult<Option<(SessionRefreshProgressV1, SessionTemporalProjectionBatchV1)>>
    {
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        materialize_session_temporal_refresh_batch_in_transaction(&snapshot, recovery).await
    }

    pub(crate) async fn persist_session_temporal_projection_batch_result(
        &self,
        batch: SessionTemporalProjectionBatchV1,
    ) -> SessionStoreResult<SessionTemporalProjectionBatchReceiptV1> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let receipt =
            persist_session_temporal_projection_batch_in_transaction(&transaction, &batch).await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        Ok(receipt)
    }
}

async fn materialize_session_temporal_refresh_batch_in_transaction(
    conn: &Connection,
    recovery: &SessionRefreshRecoveryV1,
) -> SessionStoreResult<Option<(SessionRefreshProgressV1, SessionTemporalProjectionBatchV1)>> {
    let (
        batch_ordinal,
        committed_through,
        previous_records,
        previous_coverage,
        previous_updated_at,
    ) = match recovery.restart_state() {
        SessionRefreshRestartStateV1::BeginProjection => {
            let baseline_records = session_temporal_projection_record_count(
                conn,
                recovery.session_id(),
                recovery.frozen_watermarks().active_generation(),
            )
            .await?;
            (
                0,
                recovery.source_frontier(),
                baseline_records,
                TemporalCoverageCountsV1 {
                    visible: baseline_records,
                    hidden: 0,
                    unknown: 0,
                    redacted: 0,
                },
                None,
            )
        }
        SessionRefreshRestartStateV1::ResumeProjection { next_batch_ordinal } => {
            let progress =
                recovery
                    .progress()
                    .ok_or(SessionStoreError::InvalidStateTransition {
                        context: "refresh recovery progress",
                    })?;
            (
                next_batch_ordinal,
                progress.frontier().committed_through(),
                progress.committed_records(),
                *progress.coverage(),
                Some(progress.updated_at()),
            )
        }
        SessionRefreshRestartStateV1::ReadyToComplete => return Ok(None),
    };
    let target_through = recovery.target_frontier().observed_through();
    let query_limit = i64::try_from(MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS.saturating_add(1))
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
    let mut rows = conn
        .query(
            "SELECT observation_id, observation_sequence, output_count
             FROM session_temporal_observation_effects
             WHERE session_id = ?1
               AND observation_sequence > ?2
               AND observation_sequence <= ?3
               AND output_count > 0
             ORDER BY observation_sequence, observation_id
             LIMIT ?4",
            params![
                recovery.session_id().as_str(),
                frontier_i64(committed_through, MATERIALIZE_REFRESH)?,
                frontier_i64(target_through, MATERIALIZE_REFRESH)?,
                query_limit,
            ],
        )
        .await
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
    let mut effects = Vec::new();
    let mut item_count = 0usize;
    let mut has_more = false;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?
    {
        let observation_id = tracedecay_domain::CanonicalObservationIdV1::new(
            row.get::<String>(0)
                .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
        )
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        let sequence = u64::try_from(
            row.get::<i64>(1)
                .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
        )
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        let output_count = usize::try_from(
            row.get::<i64>(2)
                .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
        )
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        if item_count.saturating_add(output_count) > MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS {
            if effects.is_empty() {
                return Err(SessionStoreError::BatchLimitExceeded {
                    field: "session temporal observation effect outputs",
                    count: output_count,
                    max: MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS,
                });
            }
            has_more = true;
            break;
        }
        item_count += output_count;
        effects.push((observation_id, sequence, output_count));
    }
    drop(rows);
    if effects.is_empty() {
        return Ok(None);
    }

    let mut low = 1usize;
    let mut high = effects.len();
    let mut selected = None;
    let mut single_effect_count = None;
    while low <= high {
        let prefix_len = low + (high - low) / 2;
        let prefix_item_count = effects[..prefix_len]
            .iter()
            .fold(0usize, |count, (_, _, outputs)| {
                count.saturating_add(*outputs)
            });
        let occurrences =
            materialize_effect_occurrences(conn, &effects[..prefix_len], prefix_item_count).await?;
        let (copies, assertions) = derive_retained_projection_relations(
            conn,
            recovery.session_id(),
            target_through,
            &occurrences,
        )
        .await?;
        let derived_count = occurrences
            .len()
            .saturating_add(copies.len())
            .saturating_add(assertions.len());
        if derived_count <= MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS {
            selected = Some((prefix_len, occurrences, copies, assertions));
            low = prefix_len.saturating_add(1);
        } else {
            if prefix_len == 1 {
                single_effect_count = Some(derived_count);
            }
            high = prefix_len.saturating_sub(1);
        }
    }
    let Some((prefix_len, occurrences, copies, assertions)) = selected else {
        return Err(SessionStoreError::BatchLimitExceeded {
            field: "session temporal derived observation effect records",
            count: single_effect_count
                .unwrap_or(MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS.saturating_add(1)),
            max: MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS,
        });
    };
    if prefix_len < effects.len() {
        effects.truncate(prefix_len);
        has_more = true;
    }
    let source_through = if has_more {
        effects.last().map(|(_, sequence, _)| *sequence).ok_or(
            SessionStoreError::InvalidStateTransition {
                context: "refresh projection source checkpoint",
            },
        )?
    } else {
        target_through
    };
    let batch = SessionTemporalProjectionBatchV1::new(
        recovery.session_id().clone(),
        recovery.candidate_generation(),
        recovery.frozen_watermarks().clone(),
        occurrences,
        copies,
        assertions,
    )?
    .with_checkpoint(batch_ordinal, source_through, source_through)?;
    let batch_records =
        u64::try_from(batch.item_count()).map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
    let committed_records = previous_records
        .checked_add(batch_records)
        .ok_or_else(|| storage_message(MATERIALIZE_REFRESH, "refresh record count overflow"))?;
    let coverage = TemporalCoverageCountsV1 {
        visible: previous_coverage
            .visible
            .checked_add(batch_records)
            .ok_or_else(|| storage_message(MATERIALIZE_REFRESH, "refresh coverage overflow"))?,
        hidden: previous_coverage.hidden,
        unknown: previous_coverage.unknown,
        redacted: previous_coverage.redacted,
    };
    let mut updated_at = now_micros(MATERIALIZE_REFRESH)?;
    if let Some(previous_updated_at) = previous_updated_at
        && updated_at <= previous_updated_at
    {
        updated_at = UtcMicros(previous_updated_at.0.saturating_add(1));
    }
    let progress = SessionRefreshProgressV1::new(
        recovery.operation_id().clone(),
        recovery.session_id().clone(),
        SessionRefreshFrontierV1::new(target_through, source_through)?,
        coverage,
        batch_ordinal.saturating_add(1),
        committed_records,
        updated_at,
    );
    Ok(Some((progress, batch)))
}

async fn materialize_effect_occurrences(
    conn: &Connection,
    effects: &[(tracedecay_domain::CanonicalObservationIdV1, u64, usize)],
    item_count: usize,
) -> SessionStoreResult<Vec<MessageOccurrenceRecordV1>> {
    let mut occurrences = Vec::with_capacity(item_count);
    for (observation_id, _, output_count) in effects {
        let (_, observation) = read_observation(conn, observation_id).await?;
        for output_ordinal in 0..*output_count {
            occurrences.push(
                canonical_occurrence(
                    conn,
                    &observation,
                    u32::try_from(output_ordinal)
                        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
                )
                .await?,
            );
        }
    }
    Ok(occurrences)
}

fn derived_temporal_assertion_id(
    occurrence_id: &MessageOccurrenceIdV1,
    kind: TemporalAssertionKindV1,
    object_anchor_id: &tracedecay_domain::RetrievalAnchorId,
) -> String {
    digest_bytes(
        format!(
            "session-temporal-assertion-v1\0{}\0{}\0{}",
            occurrence_id.as_str(),
            kind.as_str(),
            object_anchor_id.as_str()
        )
        .as_bytes(),
    )
}

/// Prefer ProviderLinkage when the parent message id is the source observation's
/// stable provider record id; otherwise emit ParentMessageLinkage.
async fn canonical_parent_copy_proof(
    conn: &Connection,
    session_id: &SessionId,
    parent_occurrence_id: &MessageOccurrenceIdV1,
    parent_message_id: &str,
    parent_source_observation_id: Option<&tracedecay_domain::CanonicalObservationIdV1>,
) -> SessionStoreResult<CopyProofV1> {
    let observation_id = if let Some(observation_id) = parent_source_observation_id {
        observation_id.clone()
    } else {
        let mut rows = conn
            .query(
                "SELECT source_observation_id
                 FROM session_occurrences
                 WHERE session_id = ?1 AND occurrence_id = ?2
                 ORDER BY generation DESC
                 LIMIT 1",
                params![session_id.as_str(), parent_occurrence_id.as_str()],
            )
            .await
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        match rows
            .next()
            .await
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?
        {
            Some(row) => tracedecay_domain::CanonicalObservationIdV1::new(
                row.get::<String>(0)
                    .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
            )
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
            None => {
                let parent_message = MessageId::new(parent_message_id.to_owned())
                    .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
                return Ok(CopyProofV1::ParentMessageLinkage {
                    source_occurrence_id: parent_occurrence_id.clone(),
                    parent_message_id: parent_message,
                });
            }
        }
    };
    let (_, observation) = read_observation(conn, &observation_id).await?;
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone())
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
    let stable = envelope.stable_record_id();
    if stable.as_str() == parent_message_id {
        Ok(CopyProofV1::ProviderLinkage {
            source_occurrence_id: parent_occurrence_id.clone(),
            provider_record_id: stable.clone(),
        })
    } else {
        let parent_message = MessageId::new(parent_message_id.to_owned())
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        Ok(CopyProofV1::ParentMessageLinkage {
            source_occurrence_id: parent_occurrence_id.clone(),
            parent_message_id: parent_message,
        })
    }
}

async fn canonical_copy_proof_for_retained(
    conn: &Connection,
    batch: &SessionTemporalProjectionBatchV1,
    copy: &LogicalCopyRecordV1,
) -> SessionStoreResult<CopyProofV1> {
    let (_, source, _) =
        occurrence_observation_and_anchor(conn, batch, &copy.copied_from_occurrence_id).await?;
    let source_message_id = source
        .relations()
        .message_id()
        .unwrap_or_else(|| source.stable_record_id());
    if source.stable_record_id().as_str() == source_message_id.as_str() {
        Ok(CopyProofV1::ProviderLinkage {
            source_occurrence_id: copy.copied_from_occurrence_id.clone(),
            provider_record_id: source.stable_record_id().clone(),
        })
    } else {
        Ok(CopyProofV1::ParentMessageLinkage {
            source_occurrence_id: copy.copied_from_occurrence_id.clone(),
            parent_message_id: MessageId::new(source_message_id.as_str().to_owned())
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
        })
    }
}

/// Derive retained parent-message copies and typed assertion edges from canonical
/// observation envelopes and retrieval-anchor lineage already stored for the batch.
/// `CopiedFrom` is deliberately not auto-emitted: explicit typed copy records remain
/// the authority until the domain/store copy-bitemporality contract exposes a
/// canonical derivation identity for copied evidence.
async fn derive_retained_projection_relations(
    conn: &Connection,
    session_id: &SessionId,
    source_frontier: u64,
    occurrences: &[MessageOccurrenceRecordV1],
) -> SessionStoreResult<(Vec<LogicalCopyRecordV1>, Vec<TemporalAssertionRecordV1>)> {
    let parents = canonical_parent_message_resolver(
        conn,
        session_id.as_str(),
        source_frontier,
        MATERIALIZE_REFRESH,
    )
    .await?;

    let mut copies = BTreeMap::<(String, String), LogicalCopyRecordV1>::new();
    let mut assertions = BTreeMap::<String, TemporalAssertionRecordV1>::new();
    let mut seen_copy_keys = BTreeSet::new();

    for occurrence in occurrences {
        let (_, observation) = read_observation(conn, &occurrence.source_observation_id).await?;
        let envelope: CanonicalObservationEnvelopeV1 =
            serde_json::from_value(observation.payload().clone())
                .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        if let Some(parent_message_id) = envelope.relations().parent_message_id()
            && let Some(parent_occurrence_id) = parents.resolve(parent_message_id.as_str())
        {
            let key = (
                occurrence.occurrence_id.as_str().to_owned(),
                parent_occurrence_id.to_owned(),
            );
            if seen_copy_keys.insert(key.clone()) {
                let parent_occurrence = MessageOccurrenceIdV1::new(parent_occurrence_id.to_owned())
                    .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
                let parent_source = occurrences
                    .iter()
                    .find(|candidate| candidate.occurrence_id.as_str() == parent_occurrence_id)
                    .map(|candidate| candidate.source_observation_id.clone());
                let proof = canonical_parent_copy_proof(
                    conn,
                    session_id,
                    &parent_occurrence,
                    parent_message_id.as_str(),
                    parent_source.as_ref(),
                )
                .await?;
                copies.insert(
                    key,
                    LogicalCopyRecordV1 {
                        occurrence_id: occurrence.occurrence_id.clone(),
                        copied_from_occurrence_id: parent_occurrence,
                        proof,
                        knowledge_at: occurrence.knowledge_at,
                        valid_time: occurrence.valid_time,
                    },
                );
            }
        }

        let mut anchor_rows = conn
            .query(
                "SELECT anchor_json FROM retrieval_anchors WHERE anchor_id = ?1",
                params![occurrence.retrieval_anchor_id.as_str()],
            )
            .await
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        let anchor_json: Option<String> = match anchor_rows
            .next()
            .await
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?
        {
            Some(row) => Some(
                row.get(0)
                    .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
            ),
            None => None,
        };
        drop(anchor_rows);
        let Some(anchor_json) = anchor_json else {
            continue;
        };
        let anchor: RetrievalAnchorRecord = serde_json::from_str(&anchor_json)
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        for lineage in anchor.source_anchors() {
            // CopiedFrom edges are retained-evidence validated on persist, but
            // activation frontier parity currently expects only parent-message
            // copies; do not auto-emit CopiedFrom here.
            let Some(kind) = assertion_kind_for_relation(lineage.relation()) else {
                continue;
            };
            let assertion_id =
                derived_temporal_assertion_id(&occurrence.occurrence_id, kind, lineage.anchor_id());
            let assertion: TemporalAssertionRecordV1 = serde_json::from_value(json!({
                "assertion_id": assertion_id,
                "kind": kind.as_str(),
                "subject_anchor_id": occurrence.retrieval_anchor_id,
                "object_anchor_id": lineage.anchor_id(),
                "knowledge_at": occurrence.knowledge_at,
                "valid_time": occurrence.valid_time,
                "evidence": {
                    "authority": "explicit_anchor_assertion",
                    "evidence_class": occurrence.evidence.evidence_class,
                    "source_anchor_id": occurrence.retrieval_anchor_id,
                    "sanitization_receipt": occurrence.evidence.sanitization_receipt,
                }
            }))
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
            assertions.insert(assertion.assertion_id.as_str().to_owned(), assertion);
        }
    }

    Ok((
        copies.into_values().collect(),
        assertions.into_values().collect(),
    ))
}

pub(super) async fn session_temporal_projection_record_count(
    conn: &Connection,
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

pub(super) async fn persist_session_temporal_projection_batch_in_transaction(
    conn: &Connection,
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

pub(super) async fn seed_active_projection_in_transaction(
    conn: &Connection,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    if batch.batch_ordinal() != 0 || batch.watermarks().active_generation() == batch.generation() {
        return Ok(());
    }
    let session_id = batch.session_id().as_str();
    let candidate = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    let active = generation_i64(batch.watermarks().active_generation(), PERSIST_OPERATION)?;
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
    ];
    for sql in COPIES {
        conn.execute(sql, params![session_id, candidate, active])
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
struct ProjectionCoverage {
    occurrences: (usize, String),
    dimensions: (usize, String),
    copies: (usize, String),
    assertions: (usize, String),
    supersession: (usize, String),
    current: (usize, String),
    fts: (usize, String),
}

pub(super) async fn validate_final_projection_receipt(
    conn: &Connection,
    session_id: &tracedecay_domain::SessionId,
    generation: tracedecay_domain::SessionProjectionGenerationV1,
    watermarks: &tracedecay_store::SessionFrozenWatermarksV1,
) -> SessionStoreResult<()> {
    let generation_i64 = generation_i64(generation, super::query::ACTIVATE_OPERATION)?;
    let mut rows = conn
        .query(
            "SELECT COUNT(*), MIN(batch_ordinal), MAX(batch_ordinal)
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2",
            params![session_id.as_str(), generation_i64],
        )
        .await
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                super::query::ACTIVATE_OPERATION,
                "projection receipt aggregate returned no row",
            )
        })?;
    let count: i64 = row
        .get(0)
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
    let minimum: Option<i64> = row
        .get(1)
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
    let maximum: Option<i64> = row
        .get(2)
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
    drop(rows);
    if count <= 0 || minimum != Some(0) || maximum != Some(count - 1) {
        return Err(storage_message(
            super::query::ACTIVATE_OPERATION,
            "candidate projection receipts are missing or noncontiguous",
        ));
    }
    let mut rows = conn
        .query(
            "SELECT source_through, projection_through,
                    occurrence_count, occurrence_digest,
                    dimension_count, dimension_digest,
                    copy_count, copy_digest,
                    assertion_count, assertion_digest,
                    supersession_count, supersession_digest,
                    current_count, current_digest,
                    fts_count, fts_digest
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2
             ORDER BY batch_ordinal DESC LIMIT 1",
            params![session_id.as_str(), generation_i64],
        )
        .await
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                super::query::ACTIVATE_OPERATION,
                "candidate final projection receipt is missing",
            )
        })?;
    let source_through: i64 = row
        .get(0)
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
    let projection_through: i64 = row
        .get(1)
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
    if u64::try_from(source_through).ok() != Some(watermarks.source_frontier())
        || u64::try_from(projection_through).ok() != Some(watermarks.projection_frontier())
    {
        return Err(storage_message(
            super::query::ACTIVATE_OPERATION,
            "final projection receipt does not cover the frozen frontiers",
        ));
    }
    let count = |index| -> SessionStoreResult<usize> {
        let value = row
            .get::<i64>(index)
            .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
        usize::try_from(value).map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))
    };
    let digest = |index| {
        row.get::<String>(index)
            .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))
    };
    let expected = ProjectionCoverage {
        occurrences: (count(2)?, digest(3)?),
        dimensions: (count(4)?, digest(5)?),
        copies: (count(6)?, digest(7)?),
        assertions: (count(8)?, digest(9)?),
        supersession: (count(10)?, digest(11)?),
        current: (count(12)?, digest(13)?),
        fts: (count(14)?, digest(15)?),
    };
    let batch = SessionTemporalProjectionBatchV1::new(
        session_id.clone(),
        generation,
        watermarks.clone(),
        vec![],
        vec![],
        vec![],
    )?;
    let actual = projection_coverage(conn, &batch).await?;
    if actual != expected {
        return Err(storage_message(
            super::query::ACTIVATE_OPERATION,
            "candidate projection rows do not match the immutable final receipt",
        ));
    }
    validate_canonical_assertion_completeness(
        conn,
        session_id,
        generation_i64,
        watermarks.source_frontier(),
    )
    .await?;
    Ok(())
}

async fn validate_canonical_assertion_completeness(
    conn: &Connection,
    session_id: &tracedecay_domain::SessionId,
    generation: i64,
    source_frontier: u64,
) -> SessionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT observation.observation_json, anchor.anchor_json
             FROM observations AS observation
             JOIN observation_retrieval_anchors AS binding
               ON binding.observation_id = observation.observation_id
             JOIN retrieval_anchors AS anchor ON anchor.anchor_id = binding.anchor_id
             WHERE observation.sequence <= ?1
             ORDER BY observation.sequence",
            params![frontier_i64(
                source_frontier,
                super::query::ACTIVATE_OPERATION,
            )?],
        )
        .await
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
    let mut required = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?
    {
        let observation: tracedecay_domain::DurableObservationV1 = serde_json::from_str(
            &row.get::<String>(0)
                .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?,
        )
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
        let Ok(envelope) =
            serde_json::from_value::<CanonicalObservationEnvelopeV1>(observation.payload().clone())
        else {
            continue;
        };
        if envelope.relations().session_id() != session_id {
            continue;
        }
        let anchor: RetrievalAnchorRecord = serde_json::from_str(
            &row.get::<String>(1)
                .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?,
        )
        .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
        if anchor.owner() != observation.scope()
            || !anchor
                .source_observations()
                .contains(observation.observation_id())
        {
            return Err(storage_message(
                super::query::ACTIVATE_OPERATION,
                "canonical assertion lineage is not bound to its owning observation",
            ));
        }
        for lineage in anchor.source_anchors() {
            if let Some(kind) = assertion_kind_for_relation(lineage.relation()) {
                required.push((
                    observation.observation_id().as_str().to_owned(),
                    anchor.anchor_id().as_str().to_owned(),
                    lineage.anchor_id().as_str().to_owned(),
                    kind.as_str(),
                    observation
                        .receipt()
                        .receipt()
                        .receipt_id()
                        .as_str()
                        .to_owned(),
                ));
            }
        }
    }
    drop(rows);

    for (observation_id, subject_anchor_id, object_anchor_id, kind, receipt_id) in required {
        let mut matches = conn
            .query(
                "SELECT COUNT(*)
                 FROM session_assertions AS assertion
                 JOIN session_occurrences AS subject
                   ON subject.session_id = assertion.session_id
                  AND subject.generation = assertion.generation
                  AND subject.retrieval_anchor_id = assertion.subject_anchor_id
                  AND subject.source_observation_id = ?5
                 JOIN session_occurrences AS object
                   ON object.session_id = assertion.session_id
                  AND object.generation = assertion.generation
                  AND object.retrieval_anchor_id = assertion.object_anchor_id
                 WHERE assertion.session_id = ?1 AND assertion.generation = ?2
                   AND assertion.assertion_kind = ?3
                   AND assertion.subject_anchor_id = ?4
                   AND assertion.object_anchor_id = ?6
                   AND json_extract(assertion.evidence_json, '$.sanitization_receipt.receipt_id')
                       = ?7",
                params![
                    session_id.as_str(),
                    generation,
                    kind,
                    subject_anchor_id,
                    observation_id,
                    object_anchor_id,
                    receipt_id,
                ],
            )
            .await
            .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
        let count = matches
            .next()
            .await
            .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?
            .ok_or_else(|| {
                storage_message(
                    super::query::ACTIVATE_OPERATION,
                    "canonical assertion completeness aggregate returned no row",
                )
            })?
            .get::<i64>(0)
            .map_err(|error| storage(super::query::ACTIVATE_OPERATION, error))?;
        if count != 1 {
            return Err(storage_message(
                super::query::ACTIVATE_OPERATION,
                "candidate omits canonical typed assertion lineage through the frozen frontier",
            ));
        }
    }
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in hasher.finalize() {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

pub(in crate::global_db) async fn record_canonical_observation_effect(
    conn: &Connection,
    sequence: u64,
    observation: &tracedecay_domain::DurableObservationV1,
    effect: &ObservationProjection,
) -> ProjectionStoreResult<()> {
    let Ok(envelope) =
        serde_json::from_value::<CanonicalObservationEnvelopeV1>(observation.payload().clone())
    else {
        return Ok(());
    };
    let mut outputs = effect
        .messages()
        .map(|output| {
            json!({
                "anchor_id": output.provenance().retrieval_anchor_id().as_str(),
                "digest": output.output_digest().as_str(),
                "ordinal": output.output_ordinal(),
                "provider": output.message().provider,
                "message_id": output.message().message_id,
                "session_id": output.session().session_id,
            })
        })
        .collect::<Vec<_>>();
    outputs.sort_unstable_by_key(|value| value.to_string());
    let effect_digest = digest_bytes(
        &serde_json::to_vec(&json!({
            "observation_id": observation.observation_id().as_str(),
            "outputs": outputs,
            "session_id": envelope.relations().session_id().as_str(),
        }))
        .map_err(|_| {
            ProjectionStoreError::Contract(
                tracedecay_domain::ObservationContractError::CanonicalEncoding,
            )
        })?,
    );
    let sequence =
        i64::try_from(sequence).map_err(|_| ProjectionStoreError::SequenceOverflow(sequence))?;
    let output_count = i64::try_from(effect.output_count())
        .map_err(|_| ProjectionStoreError::SequenceOverflow(u64::MAX))?;
    conn.execute(
        "INSERT INTO session_temporal_observation_effects (
            observation_id, observation_sequence, session_id, receipt_id,
            effect_digest, output_count, recorded_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch() * 1000000)
         ON CONFLICT(observation_id) DO NOTHING",
        params![
            observation.observation_id().as_str(),
            sequence,
            envelope.relations().session_id().as_str(),
            observation.receipt().receipt().receipt_id().as_str(),
            effect_digest.as_str(),
            output_count,
        ],
    )
    .await
    .map_err(|error| ProjectionStoreError::Storage {
        operation: "record canonical temporal observation effect",
        source: Box::new(error),
    })?;
    let mut rows = conn
        .query(
            "SELECT observation_sequence, session_id, receipt_id, effect_digest, output_count
             FROM session_temporal_observation_effects WHERE observation_id = ?1",
            params![observation.observation_id().as_str()],
        )
        .await
        .map_err(|error| ProjectionStoreError::Storage {
            operation: "verify canonical temporal observation effect",
            source: Box::new(error),
        })?;
    let row = rows
        .next()
        .await
        .map_err(|error| ProjectionStoreError::Storage {
            operation: "verify canonical temporal observation effect",
            source: Box::new(error),
        })?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?;
    let actual = (
        row.get::<i64>(0)
            .map_err(|error| ProjectionStoreError::Storage {
                operation: "verify canonical temporal observation effect",
                source: Box::new(error),
            })?,
        row.get::<String>(1)
            .map_err(|error| ProjectionStoreError::Storage {
                operation: "verify canonical temporal observation effect",
                source: Box::new(error),
            })?,
        row.get::<String>(2)
            .map_err(|error| ProjectionStoreError::Storage {
                operation: "verify canonical temporal observation effect",
                source: Box::new(error),
            })?,
        row.get::<String>(3)
            .map_err(|error| ProjectionStoreError::Storage {
                operation: "verify canonical temporal observation effect",
                source: Box::new(error),
            })?,
        row.get::<i64>(4)
            .map_err(|error| ProjectionStoreError::Storage {
                operation: "verify canonical temporal observation effect",
                source: Box::new(error),
            })?,
    );
    let expected = (
        sequence,
        envelope.relations().session_id().as_str().to_owned(),
        observation
            .receipt()
            .receipt()
            .receipt_id()
            .as_str()
            .to_owned(),
        effect_digest,
        output_count,
    );
    if actual == expected {
        Ok(())
    } else {
        Err(ProjectionStoreError::ProvenanceCollision)
    }
}

fn sorted_json<T: serde::Serialize>(values: &[T]) -> SessionStoreResult<Vec<String>> {
    let mut encoded = values
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    encoded.sort_unstable();
    Ok(encoded)
}

fn canonical_batch_digest(
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<SessionTemporalDigestV1> {
    let encoded = serde_json::to_vec(&json!({
        "assertions": sorted_json(batch.assertions())?,
        "copies": sorted_json(batch.copies())?,
        "generation": batch.generation().value(),
        "occurrences": sorted_json(batch.occurrences())?,
        "projection_through": batch.projection_through(),
        "session_id": batch.session_id().as_str(),
        "source_through": batch.source_through(),
        "watermarks": encode_watermarks(batch.watermarks(), PERSIST_OPERATION)?,
    }))
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    SessionTemporalDigestV1::new(digest_bytes(&encoded))
}

async fn read_projection_receipt(
    conn: &Connection,
    batch: &SessionTemporalProjectionBatchV1,
    batch_digest: &str,
) -> SessionStoreResult<Option<SessionTemporalProjectionBatchReceiptV1>> {
    let mut rows = conn
        .query(
            "SELECT batch_digest, frozen_watermarks_json, source_through,
                    projection_through, committed_at
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2 AND batch_ordinal = ?3",
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
                frontier_i64(batch.batch_ordinal(), PERSIST_OPERATION)?,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    else {
        let mut digest_rows = conn
            .query(
                "SELECT batch_ordinal FROM session_temporal_projection_receipts
                 WHERE session_id = ?1 AND generation = ?2 AND batch_digest = ?3",
                params![
                    batch.session_id().as_str(),
                    generation_i64(batch.generation(), PERSIST_OPERATION)?,
                    batch_digest,
                ],
            )
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        if digest_rows
            .next()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?
            .is_some()
        {
            return Err(storage_message(
                PERSIST_OPERATION,
                "projection batch digest is already bound to a different ordinal",
            ));
        }
        return Ok(None);
    };
    let actual_digest: String = row
        .get(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let actual_watermarks: String = row
        .get(1)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let actual_source: i64 = row
        .get(2)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let actual_projection: i64 = row
        .get(3)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let committed_at: i64 = row
        .get(4)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    if actual_digest != batch_digest
        || actual_watermarks != encode_watermarks(batch.watermarks(), PERSIST_OPERATION)?
        || u64::try_from(actual_source).ok() != Some(batch.source_through())
        || u64::try_from(actual_projection).ok() != Some(batch.projection_through())
    {
        return Err(storage_message(
            PERSIST_OPERATION,
            "projection batch ordinal conflicts with its immutable receipt",
        ));
    }
    let batch_digest = SessionTemporalDigestV1::new(actual_digest)?;
    let existing = SessionTemporalProjectionBatchReceiptV1::applied(
        batch,
        batch_digest.clone(),
        batch.occurrences().len(),
        batch.copies().len(),
        batch.assertions().len(),
        tracedecay_domain::UtcMicros(committed_at),
    )?;
    Ok(Some(SessionTemporalProjectionBatchReceiptV1::exact_replay(
        batch,
        batch_digest,
        &existing,
        tracedecay_domain::UtcMicros(committed_at),
    )?))
}

async fn require_contiguous_checkpoint(
    conn: &Connection,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT batch_ordinal, source_through, projection_through
             FROM session_temporal_projection_receipts
             WHERE session_id = ?1 AND generation = ?2
             ORDER BY batch_ordinal DESC LIMIT 1",
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let previous = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    match previous {
        None if batch.batch_ordinal() == 0 => Ok(()),
        Some(row) => {
            let ordinal: i64 = row
                .get(0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let source: i64 = row
                .get(1)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let projection: i64 = row
                .get(2)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let expected = u64::try_from(ordinal)
                .map_err(|error| storage(PERSIST_OPERATION, error))?
                .saturating_add(1);
            if batch.batch_ordinal() != expected
                || u64::try_from(source)
                    .ok()
                    .is_none_or(|value| value > batch.source_through())
                || u64::try_from(projection)
                    .ok()
                    .is_none_or(|value| value > batch.projection_through())
            {
                return Err(storage_message(
                    PERSIST_OPERATION,
                    "projection batch checkpoint is not contiguous and monotonic",
                ));
            }
            Ok(())
        }
        None => Err(storage_message(
            PERSIST_OPERATION,
            "projection batch checkpoint must start at ordinal zero",
        )),
    }
}

async fn digest_query_rows(
    conn: &Connection,
    sql: &str,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<(usize, String)> {
    let mut rows = conn
        .query(
            sql,
            params![
                batch.session_id().as_str(),
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let mut encoded = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    {
        encoded.push(
            row.get::<String>(0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
        );
    }
    let digest = digest_bytes(encoded.join("\n").as_bytes());
    Ok((encoded.len(), digest))
}

async fn projection_coverage(
    conn: &Connection,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<ProjectionCoverage> {
    let occurrences = digest_query_rows(
        conn,
        "SELECT json_array(occurrence_id, source_observation_id,
                projection_output_ordinal, retrieval_anchor_id, thread_id,
                thread_grouping_json, turn_id, turn_grouping_json, message_id,
                agent_id, role, knowledge_at, valid_time_json, evidence_json,
                snippet_text, index_text)
         FROM session_occurrences
         WHERE session_id = ?1 AND generation = ?2
         ORDER BY occurrence_id",
        batch,
    )
    .await?;
    let dimensions = digest_query_rows(
        conn,
        "SELECT encoded FROM (
            SELECT 'agent:' || json_array(agent_id, agent_json, created_at) AS encoded
            FROM session_agents WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'thread:' || json_array(thread_id, grouping_provenance, created_at)
            FROM session_threads WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'turn:' || json_array(turn_id, ordinal, grouping_provenance, created_at)
            FROM session_turns WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'member:' || json_array(turn_id, occurrence_id, ordinal)
            FROM session_turn_members WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'agent-edge:' || json_array(parent_agent_id, child_agent_id, ordinal)
            FROM session_agent_hierarchy_edges WHERE session_id = ?1 AND generation = ?2
            UNION ALL
            SELECT 'thread-edge:' || json_array(parent_thread_id, child_thread_id, ordinal)
            FROM session_thread_hierarchy_edges WHERE session_id = ?1 AND generation = ?2
         ) ORDER BY encoded",
        batch,
    )
    .await?;
    let copies = digest_query_rows(
        conn,
        "SELECT json_array(
            occurrence_id, copied_from_occurrence_id, proof_json,
            knowledge_at, valid_time_json, created_at
         )
         FROM session_logical_copy_edges
         WHERE session_id = ?1 AND generation = ?2
         ORDER BY occurrence_id, copied_from_occurrence_id",
        batch,
    )
    .await?;
    let assertions = digest_query_rows(
        conn,
        "SELECT json_array(assertion_id, assertion_kind, subject_anchor_id,
                object_anchor_id, knowledge_at, valid_time_json, evidence_json)
         FROM session_assertions
         WHERE session_id = ?1 AND generation = ?2 ORDER BY assertion_id",
        batch,
    )
    .await?;
    let supersession = digest_query_rows(
        conn,
        "SELECT json_array(superseded_assertion_id, superseding_assertion_id, created_at)
         FROM session_assertion_supersession
         WHERE session_id = ?1 AND generation = ?2
         ORDER BY superseded_assertion_id, superseding_assertion_id",
        batch,
    )
    .await?;
    let current = digest_query_rows(
        conn,
        "SELECT json_array(entity_kind, entity_id, current_assertion_id,
                current_occurrence_id, coverage_json)
         FROM session_current_entities
         WHERE session_id = ?1 AND generation = ?2 ORDER BY entity_kind, entity_id",
        batch,
    )
    .await?;
    let fts = digest_query_rows(
        conn,
        "SELECT json_array(occurrence.occurrence_id, fts.index_text, fts.snippet_text)
         FROM session_occurrences AS occurrence
         JOIN session_occurrences_fts AS fts ON fts.rowid = occurrence.rowid
         WHERE occurrence.session_id = ?1 AND occurrence.generation = ?2
         ORDER BY occurrence.occurrence_id",
        batch,
    )
    .await?;
    Ok(ProjectionCoverage {
        occurrences,
        dimensions,
        copies,
        assertions,
        supersession,
        current,
        fts,
    })
}

async fn insert_projection_receipt(
    conn: &Connection,
    batch: &SessionTemporalProjectionBatchV1,
    batch_digest: &str,
    coverage: &ProjectionCoverage,
    committed_at: i64,
) -> SessionStoreResult<()> {
    conn.execute(
        "INSERT INTO session_temporal_projection_receipts (
            session_id, generation, batch_ordinal, batch_digest,
            frozen_watermarks_json, source_through, projection_through,
            occurrence_count, occurrence_digest, dimension_count, dimension_digest,
            copy_count, copy_digest, assertion_count, assertion_digest,
            supersession_count, supersession_digest, current_count, current_digest,
            fts_count, fts_digest, committed_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
         )",
        params![
            batch.session_id().as_str(),
            generation_i64(batch.generation(), PERSIST_OPERATION)?,
            frontier_i64(batch.batch_ordinal(), PERSIST_OPERATION)?,
            batch_digest,
            encode_watermarks(batch.watermarks(), PERSIST_OPERATION)?,
            frontier_i64(batch.source_through(), PERSIST_OPERATION)?,
            frontier_i64(batch.projection_through(), PERSIST_OPERATION)?,
            i64::try_from(coverage.occurrences.0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.occurrences.1.as_str(),
            i64::try_from(coverage.dimensions.0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.dimensions.1.as_str(),
            i64::try_from(coverage.copies.0).map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.copies.1.as_str(),
            i64::try_from(coverage.assertions.0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.assertions.1.as_str(),
            i64::try_from(coverage.supersession.0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.supersession.1.as_str(),
            i64::try_from(coverage.current.0).map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.current.1.as_str(),
            i64::try_from(coverage.fts.0).map_err(|error| storage(PERSIST_OPERATION, error))?,
            coverage.fts.1.as_str(),
            committed_at,
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok(())
}

async fn persist_occurrence(
    conn: &Connection,
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
    let expected = canonical_occurrence(conn, &observation, output.output_ordinal()).await?;
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
                occurrence.thread_id.as_ref().map(|value| value.as_str()),
                thread_grouping,
                occurrence.turn_id.as_ref().map(|value| value.as_str()),
                turn_grouping,
                occurrence.message_id.as_ref().map(|value| value.as_str()),
                occurrence.agent_id.as_ref().map(|value| value.as_str()),
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

async fn canonical_occurrence(
    conn: &Connection,
    observation: &tracedecay_domain::DurableObservationV1,
    output_ordinal: u32,
) -> SessionStoreResult<MessageOccurrenceRecordV1> {
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone())
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let projection =
        derive_projection(observation).map_err(|error| storage(PERSIST_OPERATION, error))?;
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
    let valid_time = anchor.occurred_at().map_or_else(
        || json!({"kind": "unknown"}),
        |interval| json!({"kind": "known", "valid_at": interval.start}),
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
        "thread_id": relations.thread_id().map(|id| id.as_str()),
        "thread_grouping": relations.thread_id().map(|_| grouping()),
        "turn_id": relations.turn_id().map(|id| id.as_str()),
        "turn_grouping": relations.turn_id().map(|_| grouping()),
        "message_id": output.message().message_id,
        "agent_id": relations.agent_id().map(|id| id.as_str()),
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

async fn ensure_thread(
    conn: &Connection,
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

async fn ensure_turn(
    conn: &Connection,
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

async fn ensure_agent(
    conn: &Connection,
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

async fn require_exact_occurrence(
    conn: &Connection,
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
        "thread_id": occurrence.thread_id.as_ref().map(|value| value.as_str()),
        "thread_grouping_json": occurrence.thread_grouping,
        "turn_id": occurrence.turn_id.as_ref().map(|value| value.as_str()),
        "turn_grouping_json": occurrence.turn_grouping,
        "message_id": occurrence.message_id.as_ref().map(|value| value.as_str()),
        "agent_id": occurrence.agent_id.as_ref().map(|value| value.as_str()),
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

async fn persist_copy(
    conn: &Connection,
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

async fn occurrence_observation_and_anchor(
    conn: &Connection,
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

async fn validate_copy_proof(
    conn: &Connection,
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

async fn persist_assertion(
    conn: &Connection,
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

async fn validate_assertion(
    conn: &Connection,
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

const fn assertion_kind_for_relation(
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
async fn require_edge_json(
    conn: &Connection,
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

async fn rebuild_current_occurrences(
    conn: &Connection,
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

async fn rebuild_assertion_derivatives(
    conn: &Connection,
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;
    use tracedecay_domain::{
        AnchorProvenanceRelationV2, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
        CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
        CanonicalObservationRelationsV1, DurableObservationV1, ObservationId,
        ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
        ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
        ObservationSourceRangeV1, PayloadReferenceV1, ProjectionGenerationId, ProviderId,
        RetentionClass, RetrievalAnchorId, RetrievalAnchorRecordV2, SanitizationReceiptId,
        SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    };
    use tracedecay_store::{
        AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
        SessionRefreshCompletionRequestV1, SessionRefreshTerminalStateV1,
    };

    use super::*;
    use crate::store::GlobalDbObservationStore;

    fn fixture_session(value: &str) -> SessionId {
        SessionId::new(value).unwrap()
    }

    fn fixture_receipt(receipt_id: &str, payload: &Value) -> SanitizationReceiptV1 {
        SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new(receipt_id).unwrap(),
                tracedecay_domain::ComponentVersion::new("sanitizer.projector-test.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(payload).unwrap()),
        )
        .unwrap()
    }

    fn fixture_observation(
        session_id: &SessionId,
        ordinal: u64,
        lineage: Option<(AnchorProvenanceRelationV2, RetrievalAnchorId)>,
        include_parent: bool,
    ) -> (DurableObservationV1, AnchoredObservationWrite) {
        let provider = ProviderId::new(format!("projector-test-{ordinal}")).unwrap();
        let source =
            ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone())
                .unwrap();
        let range = ObservationSourceRangeV1::new(ordinal, ordinal + 1).unwrap();
        let record_id = ObservationId::new(format!("record.projector.{ordinal}")).unwrap();
        let mut relations = CanonicalObservationRelationsV1::new(session_id.clone())
            .with_thread_id(ObservationId::new("thread.projector").unwrap())
            .with_turn_id(ObservationId::new("turn.projector").unwrap())
            .with_message_id(ObservationId::new(format!("message.projector.{ordinal}")).unwrap())
            .with_agent_id(ObservationId::new("agent.projector").unwrap());
        if include_parent && ordinal > 0 {
            relations = relations.with_parent_message_id(
                ObservationId::new(format!("message.projector.{}", ordinal - 1)).unwrap(),
            );
        }
        let envelope = CanonicalObservationEnvelopeV1::new(
            provider,
            "message",
            record_id.clone(),
            relations,
            vec![CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!({"text": format!("projector {ordinal}")}),
                model: Some("model.projector".to_owned()),
                timestamp: Some(1_750_000_000 + i64::try_from(ordinal).unwrap()),
            }],
            CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
        )
        .unwrap();
        let payload = serde_json::to_value(envelope).unwrap();
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).unwrap(),
            range,
            ObservationOrderingDomainV1::SnapshotOrder,
            record_id,
        )
        .unwrap();
        let observation = DurableObservationV1::new(
            identity,
            fixture_receipt(&format!("receipt.projector.{ordinal}"), &payload),
            RetentionClass::new("retention.projector-test").unwrap(),
            payload,
        )
        .unwrap();
        let next_cursor = ObservationSourceCursorV1::for_ordering(
            observation.source().clone(),
            observation.scope().clone(),
            observation.identity().generation(),
            observation.identity().ordering_domain(),
            observation.identity().position().end(),
        )
        .unwrap();
        let write = ObservationWrite::new(observation.clone(), None, next_cursor).unwrap();
        let projection_generation =
            ProjectionGenerationId::new("projection.projector-test.v1").unwrap();
        let authorization = tracedecay_store::build_observation_resolution_authorization_v1(
            write.observation(),
            "projector-test",
        )
        .unwrap();
        let anchor = tracedecay_store::build_observation_retrieval_anchor_v2(
            write.observation(),
            projection_generation.clone(),
            UtcMicros(1),
            authorization,
        )
        .unwrap();
        let mut anchor_json = serde_json::to_value(anchor).unwrap();
        if let Some((relation, anchor_id)) = lineage {
            anchor_json["source_anchors"] = json!([{
                "relation": relation,
                "anchor_id": anchor_id,
                "owner": write.observation().scope(),
            }]);
        }
        let anchor: RetrievalAnchorRecordV2 = serde_json::from_value(anchor_json).unwrap();
        let anchored = AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap();
        (observation, anchored)
    }

    async fn persist_fixture(
        db: &GlobalDb,
        observation: DurableObservationV1,
        anchored: AnchoredObservationWrite,
    ) {
        let store = GlobalDbObservationStore::new(db);
        store.persist_observation(anchored).await.unwrap();
        store
            .project_observation(observation.observation_id())
            .await
            .unwrap();
    }

    async fn scalar(db: &GlobalDb, sql: &str) -> i64 {
        let mut rows = db.read_connection().query(sql, ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    #[tokio::test]
    async fn relation_batch_persists_restarts_and_completes_without_duplicates() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("global.db");
        let session_id = fixture_session("session.projector.relation-restart");
        let operation_id;
        {
            let db = GlobalDb::open_at(&path).await.unwrap();
            let (first, first_write) = fixture_observation(&session_id, 0, None, false);
            let first_anchor =
                derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
            persist_fixture(&db, first, first_write).await;
            let (second, second_write) = fixture_observation(
                &session_id,
                1,
                Some((AnchorProvenanceRelationV2::Supersedes, first_anchor)),
                true,
            );
            persist_fixture(&db, second, second_write).await;
            let begin = db
                .begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
                    session_id.clone(),
                    SessionRefreshFrontierV1::new(2, 0).unwrap(),
                ))
                .await
                .unwrap();
            operation_id = begin.operation_id().clone();
            let recovery = db
                .session_refresh_recovery_result(&session_id)
                .await
                .unwrap()
                .unwrap();
            let (progress, batch) = db
                .materialize_session_temporal_refresh_batch_result(&recovery)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(batch.occurrences().len(), 2);
            assert_eq!(batch.copies().len(), 1);
            assert_eq!(batch.assertions().len(), 1);
            assert_eq!(batch.item_count(), 4);
            assert_eq!(progress.committed_records(), 4);
            assert_eq!(progress.coverage().visible, 4);
            db.persist_session_refresh_projection_batch_result(progress, batch)
                .await
                .unwrap();
        }

        let db = GlobalDb::open_at(&path).await.unwrap();
        let recovery = db
            .session_refresh_recovery_result(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            recovery.restart_state(),
            SessionRefreshRestartStateV1::ReadyToComplete
        );
        assert!(
            db.materialize_session_temporal_refresh_batch_result(&recovery)
                .await
                .unwrap()
                .is_none()
        );
        let progress = recovery.progress().unwrap();
        let request = SessionRefreshCompletionRequestV1::new(
            operation_id,
            session_id,
            progress.frontier(),
            *progress.coverage(),
        )
        .unwrap();
        let receipt = db
            .complete_session_refresh_result(request.clone())
            .await
            .unwrap();
        assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
        assert_eq!(
            db.complete_session_refresh_result(request).await.unwrap(),
            receipt
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT COUNT(*) FROM session_temporal_projection_receipts"
            )
            .await,
            1
        );
        assert_eq!(
            scalar(&db, "SELECT COUNT(*) FROM session_occurrences").await,
            2
        );
        assert_eq!(
            scalar(&db, "SELECT COUNT(*) FROM session_logical_copy_edges").await,
            1
        );
        assert_eq!(
            scalar(&db, "SELECT COUNT(*) FROM session_assertions").await,
            1
        );
        assert_eq!(
            scalar(&db, "SELECT COUNT(*) FROM session_refresh_receipts").await,
            1
        );
    }

    #[tokio::test]
    async fn copied_from_lineage_is_not_auto_emitted_by_materializer() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("global.db");
        let db = GlobalDb::open_at(&path).await.unwrap();
        let session_id = fixture_session("session.projector.copied-from");
        let (first, first_write) = fixture_observation(&session_id, 0, None, false);
        let first_anchor =
            derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
        persist_fixture(&db, first, first_write).await;
        let (second, second_write) = fixture_observation(
            &session_id,
            1,
            Some((AnchorProvenanceRelationV2::CopiedFrom, first_anchor)),
            false,
        );
        persist_fixture(&db, second, second_write).await;
        db.begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(2, 0).unwrap(),
        ))
        .await
        .unwrap();
        let recovery = db
            .session_refresh_recovery_result(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (progress, batch) = db
            .materialize_session_temporal_refresh_batch_result(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(batch.occurrences().len(), 2);
        assert!(batch.copies().is_empty());
        assert!(batch.assertions().is_empty());
        assert_eq!(progress.committed_records(), batch.item_count() as u64);
    }

    #[tokio::test]
    async fn relation_derivation_backs_off_to_the_total_batch_limit() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("global.db");
        let db = GlobalDb::open_at(&path).await.unwrap();
        let session_id = fixture_session("session.projector.derived-limit");
        for ordinal in 0..501 {
            let (observation, write) = fixture_observation(&session_id, ordinal, None, ordinal > 0);
            persist_fixture(&db, observation, write).await;
        }
        db.begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(501, 0).unwrap(),
        ))
        .await
        .unwrap();
        let recovery = db
            .session_refresh_recovery_result(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (first_progress, first_batch) = db
            .materialize_session_temporal_refresh_batch_result(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_batch.occurrences().len(), 500);
        assert_eq!(first_batch.copies().len(), 499);
        assert_eq!(first_batch.item_count(), 999);
        assert_eq!(first_progress.frontier().committed_through(), 500);
        assert_eq!(first_progress.committed_records(), 999);
        db.persist_session_refresh_projection_batch_result(first_progress, first_batch)
            .await
            .unwrap();

        let recovery = db
            .session_refresh_recovery_result(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (second_progress, second_batch) = db
            .materialize_session_temporal_refresh_batch_result(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second_batch.occurrences().len(), 1);
        assert_eq!(second_batch.copies().len(), 1);
        assert_eq!(second_batch.item_count(), 2);
        assert_eq!(second_progress.frontier().committed_through(), 501);
        assert_eq!(second_progress.committed_records(), 1001);
        db.persist_session_refresh_projection_batch_result(second_progress, second_batch)
            .await
            .unwrap();
    }

    #[test]
    fn assertion_identity_includes_the_object_anchor() {
        let session_id = fixture_session("session.projector.assertion-identity");
        let (first, _) = fixture_observation(&session_id, 0, None, false);
        let (second, _) = fixture_observation(&session_id, 1, None, false);
        let occurrence_id = MessageOccurrenceIdV1::derive(
            first.observation_id(),
            tracedecay_domain::ProjectionOutputOrdinalV1::new(0),
        );
        let first_anchor =
            derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
        let second_anchor =
            derive_exact_observation_anchor_id(second.scope(), second.observation_id()).unwrap();
        let first_id = derived_temporal_assertion_id(
            &occurrence_id,
            TemporalAssertionKindV1::Supports,
            &first_anchor,
        );
        let second_id = derived_temporal_assertion_id(
            &occurrence_id,
            TemporalAssertionKindV1::Supports,
            &second_anchor,
        );
        assert_ne!(first_id, second_id);
        assert!(first_id.starts_with("sha256:"));
        assert_eq!(first_id.len(), 71);
    }

    #[tokio::test]
    async fn parent_resolver_rejects_ambiguous_session_message_ids() {
        let mut resolver = ParentMessageResolver::default();
        resolver.register("message.shared", "occurrence.a");
        resolver.register("message.shared", "occurrence.b");
        let error = resolver
            .reject_ambiguity("test parent ambiguity")
            .expect_err("duplicate message ids must be rejected");
        assert!(
            error
                .to_string()
                .contains("session-scoped message id message.shared"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn materialize_persists_copy_bitemporality_and_rejects_forged_assertion_ids() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("global.db");
        let db = GlobalDb::open_at(&path).await.unwrap();
        let session_id = fixture_session("session.projector.copy-bitemporal");
        let (first, first_write) = fixture_observation(&session_id, 0, None, false);
        let first_anchor =
            derive_exact_observation_anchor_id(first.scope(), first.observation_id()).unwrap();
        persist_fixture(&db, first, first_write).await;
        let (second, second_write) = fixture_observation(
            &session_id,
            1,
            Some((AnchorProvenanceRelationV2::Supersedes, first_anchor)),
            true,
        );
        persist_fixture(&db, second, second_write).await;
        db.begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(2, 0).unwrap(),
        ))
        .await
        .unwrap();
        let recovery = db
            .session_refresh_recovery_result(&session_id)
            .await
            .unwrap()
            .unwrap();
        let (progress, batch) = db
            .materialize_session_temporal_refresh_batch_result(&recovery)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(batch.copies().len(), 1);
        assert_eq!(
            batch.copies()[0].valid_time,
            batch.occurrences()[1].valid_time
        );
        assert_eq!(
            batch.copies()[0].knowledge_at,
            batch.occurrences()[1].knowledge_at
        );
        assert!(matches!(
            batch.copies()[0].proof,
            CopyProofV1::ParentMessageLinkage { .. }
        ));

        let mut forged = batch.assertions()[0].clone();
        forged.assertion_id =
            tracedecay_domain::TemporalAssertionIdV1::new("assertion.forged").unwrap();
        let forged_batch = SessionTemporalProjectionBatchV1::new(
            batch.session_id().clone(),
            batch.generation(),
            batch.watermarks().clone(),
            batch.occurrences().to_vec(),
            batch.copies().to_vec(),
            vec![forged],
        )
        .unwrap()
        .with_checkpoint(
            batch.batch_ordinal(),
            batch.source_through(),
            batch.projection_through(),
        )
        .unwrap();
        let forged_error = db
            .persist_session_refresh_projection_batch_result(progress.clone(), forged_batch)
            .await
            .expect_err("forged assertion ids must be rejected");
        assert!(
            forged_error
                .to_string()
                .contains("assertion temporal or authority evidence is not canonical"),
            "{forged_error}"
        );

        db.persist_session_refresh_projection_batch_result(progress, batch.clone())
            .await
            .unwrap();
        let mut rows = db
            .read_connection()
            .query(
                "SELECT knowledge_at, valid_time_json FROM session_logical_copy_edges
                 WHERE session_id = ?1",
                params![session_id.as_str()],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let knowledge_at: i64 = row.get(0).unwrap();
        let valid_time: String = row.get(1).unwrap();
        assert_eq!(knowledge_at, batch.copies()[0].knowledge_at.0);
        assert_eq!(
            serde_json::from_str::<TemporalValidityV1>(&valid_time).unwrap(),
            batch.copies()[0].valid_time
        );
    }

    #[tokio::test]
    async fn multi_batch_refresh_progress_survives_restart_under_guard() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("global.db");
        let session_id = fixture_session("session.projector.multi-batch-guard");
        let operation_id;
        {
            let db = GlobalDb::open_at(&path).await.unwrap();
            for ordinal in 0..3 {
                let (observation, write) =
                    fixture_observation(&session_id, ordinal, None, ordinal > 0);
                persist_fixture(&db, observation, write).await;
            }
            let begin = db
                .begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
                    session_id.clone(),
                    SessionRefreshFrontierV1::new(3, 0).unwrap(),
                ))
                .await
                .unwrap();
            operation_id = begin.operation_id().clone();
            let recovery = db
                .session_refresh_recovery_result(&session_id)
                .await
                .unwrap()
                .unwrap();
            let (progress, batch) = db
                .materialize_session_temporal_refresh_batch_result(&recovery)
                .await
                .unwrap()
                .unwrap();
            assert!(batch.item_count() > 0);
            assert!(progress.frontier().committed_through() > 0);
            db.persist_session_refresh_projection_batch_result(progress, batch)
                .await
                .unwrap();
        }

        let db = GlobalDb::open_at(&path).await.unwrap();
        let recovery = db
            .session_refresh_recovery_result(&session_id)
            .await
            .unwrap()
            .unwrap();
        match recovery.restart_state() {
            SessionRefreshRestartStateV1::ResumeProjection { .. }
            | SessionRefreshRestartStateV1::ReadyToComplete => {}
            other => panic!("unexpected restart state after first batch: {other:?}"),
        }
        if let Some((progress, batch)) = db
            .materialize_session_temporal_refresh_batch_result(&recovery)
            .await
            .unwrap()
        {
            db.persist_session_refresh_projection_batch_result(progress, batch)
                .await
                .unwrap();
        }
        let recovery = db
            .session_refresh_recovery_result(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            recovery.restart_state(),
            SessionRefreshRestartStateV1::ReadyToComplete
        );
        let progress = recovery.progress().unwrap();
        let receipt = db
            .complete_session_refresh_result(
                SessionRefreshCompletionRequestV1::new(
                    operation_id,
                    session_id,
                    progress.frontier(),
                    *progress.coverage(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
        assert_eq!(
            scalar(&db, "SELECT COUNT(*) FROM session_refresh_progress").await,
            progress.committed_batches() as i64
        );
    }
}
