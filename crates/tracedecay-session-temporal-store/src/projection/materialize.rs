use std::collections::{BTreeMap, BTreeSet, HashMap, hash_map::Entry};

use serde_json::json;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, CopyProofV1, LogicalCopyRecordV1, MessageId,
    MessageOccurrenceIdV1, MessageOccurrenceRecordV1, RetrievalAnchorRecord, SessionId,
    TemporalAssertionKindV1, TemporalAssertionRecordV1, TemporalCoverageCountsV1, UtcMicros,
};
use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{
    MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS, SessionRefreshFrontierV1,
    SessionRefreshProgressV1, SessionStoreError, SessionStoreResult,
    SessionTemporalProjectionBatchV1,
};
use tracedecay_temporal_query::ports::ExecutionControl;

use crate::support::derive_projection;

use super::super::query::{
    PERSIST_OPERATION, frontier_i64, generation_i64, missing_observation, now_micros,
    read_observation, read_observations, storage, storage_message,
};
use super::super::rebuild::checkpoint_relation_rebuild_control;
use super::super::refresh::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};
use super::MATERIALIZE_REFRESH;
use super::persist::*;
use super::receipts::*;

const PARENT_RESOLVER_PAGE_SIZE: i64 = 512;
const PARENT_RESOLVER_PAGE_MAX_BYTES: i64 = 32 * 1024 * 1024;

pub(super) async fn materialize_session_temporal_refresh_batch_in_transaction(
    conn: &impl crate::handle::SessionTemporalQuery,
    recovery: &SessionRefreshRecoveryV1,
    baseline_copy_count: u64,
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
                baseline_copy_count,
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
        if matches!(
            recovery.restart_state(),
            SessionRefreshRestartStateV1::BeginProjection
        ) {
            let batch = SessionTemporalProjectionBatchV1::new(
                recovery.session_id().clone(),
                recovery.candidate_generation(),
                recovery.frozen_watermarks().clone(),
                vec![],
                vec![],
                vec![],
            )?
            .with_checkpoint(batch_ordinal, target_through, target_through)?;
            let progress = SessionRefreshProgressV1::new(
                recovery.operation_id().clone(),
                recovery.session_id().clone(),
                SessionRefreshFrontierV1::new(target_through, target_through)?,
                previous_coverage,
                batch_ordinal.saturating_add(1),
                previous_records,
                now_micros(MATERIALIZE_REFRESH)?,
            )
            .with_source_coverage(recovery.source_coverage(target_through)?);
            return Ok(Some((progress, batch)));
        }
        return Ok(None);
    }

    let (all_occurrences, occurrence_work) =
        materialize_effect_occurrences(conn, &effects, item_count).await?;
    record_occurrence_materialization_work(occurrence_work);
    let mut parents = candidate_parent_message_resolver(
        conn,
        recovery.session_id(),
        recovery.candidate_generation(),
        &all_occurrences,
    )
    .await?;
    let mut selected_effects = 0usize;
    let mut selected_occurrences = 0usize;
    let mut selected_items = 0usize;
    let mut copies = Vec::new();
    let mut assertions = Vec::new();
    for (_, _, output_count) in &effects {
        let end = selected_occurrences.saturating_add(*output_count);
        let effect_occurrences =
            all_occurrences
                .get(selected_occurrences..end)
                .ok_or_else(|| {
                    storage_message(
                        MATERIALIZE_REFRESH,
                        "observation effect output count does not match canonical projection",
                    )
                })?;
        let (effect_copies, effect_assertions, relation_work) =
            derive_retained_projection_relations(
                conn,
                recovery.session_id(),
                effect_occurrences,
                &parents,
            )
            .await?;
        record_relation_derivation_work(relation_work);
        let effect_items = effect_occurrences
            .len()
            .saturating_add(effect_copies.len())
            .saturating_add(effect_assertions.len());
        if selected_items.saturating_add(effect_items) > MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS
        {
            if selected_effects == 0 {
                return Err(SessionStoreError::BatchLimitExceeded {
                    field: "session temporal derived observation effect records",
                    count: effect_items,
                    max: MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS,
                });
            }
            has_more = true;
            break;
        }
        selected_items += effect_items;
        selected_occurrences = end;
        selected_effects += 1;
        copies.extend(effect_copies);
        assertions.extend(effect_assertions);
        for occurrence in effect_occurrences {
            if let Some(message_id) = occurrence.message_id.as_ref() {
                parents.register(message_id.as_str(), occurrence.occurrence_id.as_str());
            }
        }
    }
    if selected_effects == 0 {
        return Err(SessionStoreError::BatchLimitExceeded {
            field: "session temporal derived observation effect records",
            count: MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS.saturating_add(1),
            max: MAX_SESSION_TEMPORAL_PROJECTION_BATCH_ITEMS,
        });
    }
    let occurrences = all_occurrences
        .get(..selected_occurrences)
        .ok_or_else(|| {
            storage_message(
                MATERIALIZE_REFRESH,
                "selected observation effects exceed materialized occurrences",
            )
        })?
        .to_vec();
    if selected_effects < effects.len() {
        effects.truncate(selected_effects);
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
    )
    .with_source_coverage(recovery.source_coverage(source_through)?);
    Ok(Some((progress, batch)))
}

pub(super) async fn materialize_effect_occurrences(
    conn: &impl crate::handle::SessionTemporalQuery,
    effects: &[(tracedecay_domain::CanonicalObservationIdV1, u64, usize)],
    item_count: usize,
) -> SessionStoreResult<(
    Vec<MessageOccurrenceRecordV1>,
    OccurrenceMaterializationWork,
)> {
    let mut occurrences = Vec::with_capacity(item_count);
    let mut work = OccurrenceMaterializationWork::default();
    // One chunked prefetch for the whole effect set instead of one probe per
    // effect. The loop below still walks `effects` in order, so both the emitted
    // occurrence order and the first error raised are unchanged.
    let observation_ids = effects
        .iter()
        .map(|(observation_id, _, _)| observation_id.clone())
        .collect::<Vec<_>>();
    let observations = read_observations(conn, &observation_ids).await?;
    for (observation_id, _, output_count) in effects {
        let (_, observation) = observations
            .get(observation_id.as_str())
            .ok_or_else(|| missing_observation(observation_id))?;
        // One derivation per observation, reused across all of its outputs.
        let projection =
            derive_projection(observation).map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        let outputs = projection.messages().collect::<Vec<_>>();
        if outputs.len() != *output_count {
            return Err(storage_message(
                MATERIALIZE_REFRESH,
                "canonical observation effect output authority disagrees with its projection",
            ));
        }
        let envelope: CanonicalObservationEnvelopeV1 =
            observation_envelope_from_payload(observation.payload())
                .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        work.envelope_parses = work.envelope_parses.saturating_add(1);
        for output in outputs {
            occurrences.push(canonical_occurrence(conn, observation, &envelope, output).await?);
        }
    }
    Ok((occurrences, work))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct OccurrenceMaterializationWork {
    pub(super) envelope_parses: u64,
}

#[inline(always)]
fn record_occurrence_materialization_work(work: OccurrenceMaterializationWork) {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_temporal.occurrence_materialization.envelope_parses")
        .inc(work.envelope_parses);
    #[cfg(not(feature = "hotpath"))]
    let _ = work;
}

pub(super) fn derived_temporal_assertion_id(
    subject_anchor_id: &tracedecay_domain::RetrievalAnchorId,
    kind: TemporalAssertionKindV1,
    object_anchor_id: &tracedecay_domain::RetrievalAnchorId,
) -> String {
    digest_bytes(
        format!(
            "session-temporal-assertion-v1\0{}\0{}\0{}",
            subject_anchor_id.as_str(),
            kind.as_str(),
            object_anchor_id.as_str()
        )
        .as_bytes(),
    )
}

/// Prefer `ProviderLinkage` when the parent message id is the source observation's
/// stable provider record id; otherwise emit `ParentMessageLinkage`.
pub(super) async fn canonical_parent_copy_proof(
    conn: &impl crate::handle::SessionTemporalQuery,
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
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?
        else {
            let parent_message = MessageId::new(parent_message_id.to_owned())
                .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
            return Ok(CopyProofV1::ParentMessageLinkage {
                source_occurrence_id: parent_occurrence_id.clone(),
                parent_message_id: parent_message,
            });
        };
        tracedecay_domain::CanonicalObservationIdV1::new(
            row.get::<String>(0)
                .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
        )
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?
    };
    let (_, observation) = read_observation(conn, &observation_id).await?;
    let envelope: CanonicalObservationEnvelopeV1 =
        observation_envelope_from_payload(observation.payload())
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

pub(super) async fn canonical_copy_proof_for_retained(
    conn: &impl crate::handle::SessionTemporalQuery,
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
pub(super) async fn derive_retained_projection_relations(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    occurrences: &[MessageOccurrenceRecordV1],
    parents: &ParentMessageResolver,
) -> SessionStoreResult<(
    Vec<LogicalCopyRecordV1>,
    Vec<TemporalAssertionRecordV1>,
    RelationDerivationWork,
)> {
    let mut copies = BTreeMap::<(String, String), LogicalCopyRecordV1>::new();
    let mut assertions = BTreeMap::<String, TemporalAssertionRecordV1>::new();
    let mut seen_copy_keys = BTreeSet::new();
    let mut seen_assertion_anchors = BTreeSet::new();

    // Occurrences routinely share a source observation, so the chunked prefetch
    // both collapses the repeats and drops the per-occurrence round trip. The
    // loop order, and therefore the first error raised, is unchanged.
    let observation_ids = occurrences
        .iter()
        .map(|occurrence| occurrence.source_observation_id.clone())
        .collect::<Vec<_>>();
    let observations = read_observations(conn, &observation_ids).await?;
    let mut envelopes = HashMap::with_capacity(observations.len());
    let mut work = RelationDerivationWork::default();

    for occurrence in occurrences {
        let envelope = match envelopes.entry(occurrence.source_observation_id.as_str().to_owned()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let (_, observation) = observations
                    .get(occurrence.source_observation_id.as_str())
                    .ok_or_else(|| missing_observation(&occurrence.source_observation_id))?;
                let envelope: CanonicalObservationEnvelopeV1 =
                    observation_envelope_from_payload(observation.payload())
                        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
                work.envelope_parses = work.envelope_parses.saturating_add(1);
                entry.insert(envelope)
            }
        };
        // A parent-message relation is conversation threading, not evidence of a
        // copy: Claude's `parentUuid`, and every other producer of
        // `parent_message_id`, points at the message this one *replies to*. Only
        // a re-emission of the same logical message is a logical copy, so the
        // derived copy edge is restricted to occurrences whose own logical
        // message id is the parent link. Treating every reply as a copy makes
        // non-forensic resolution collapse the reply into its parent, erasing it
        // from every current-mode retrieval surface while forensic reads (e.g.
        // `lcm_load_session`) still show it.
        if let Some(parent_message_id) = envelope.relations().parent_message_id().filter(|parent| {
            occurrence
                .message_id
                .as_ref()
                .is_some_and(|message_id| message_id.as_str() == parent.as_str())
        }) && let Some(parent_occurrence_id) = parents.resolve(parent_message_id.as_str())
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

        if !seen_assertion_anchors.insert(occurrence.retrieval_anchor_id.as_str()) {
            continue;
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
            let assertion_id = derived_temporal_assertion_id(
                &occurrence.retrieval_anchor_id,
                kind,
                lineage.anchor_id(),
            );
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
        work,
    ))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct RelationDerivationWork {
    pub(super) envelope_parses: u64,
}

#[inline(always)]
fn record_relation_derivation_work(work: RelationDerivationWork) {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_temporal.relation_derivation.envelope_parses")
        .inc(work.envelope_parses);
    #[cfg(not(feature = "hotpath"))]
    let _ = work;
}

#[hotpath::measure(future = true, label = "session_temporal.projection.candidate_parent")]
async fn candidate_parent_message_resolver(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    generation: tracedecay_domain::SessionProjectionGenerationV1,
    occurrences: &[MessageOccurrenceRecordV1],
) -> SessionStoreResult<ParentMessageResolver> {
    let message_ids = occurrences
        .iter()
        .filter_map(|occurrence| occurrence.message_id.as_ref())
        .map(|message_id| message_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut resolver = ParentMessageResolver::default();
    if message_ids.is_empty() {
        return Ok(resolver);
    }
    let encoded_ids =
        serde_json::to_string(&message_ids).map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
    record_parent_resolver_probe();
    let mut rows = conn
        .query(
            "SELECT requested.value,
                    (
                        SELECT occurrence.occurrence_id
                        FROM session_occurrences AS occurrence
                        JOIN session_temporal_observation_effects AS effect
                          ON effect.observation_id = occurrence.source_observation_id
                         AND effect.session_id = occurrence.session_id
                        WHERE occurrence.session_id = ?1
                          AND occurrence.generation = ?2
                          AND occurrence.message_id = requested.value
                        ORDER BY effect.observation_sequence,
                                 occurrence.projection_output_ordinal,
                                 occurrence.occurrence_id
                        LIMIT 1
                    )
             FROM json_each(?3) AS requested
             ORDER BY requested.value",
            params![
                session_id.as_str(),
                generation_i64(generation, MATERIALIZE_REFRESH)?,
                encoded_ids.as_str(),
            ],
        )
        .await
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(MATERIALIZE_REFRESH, error))?
    {
        let message_id: String = row
            .get(0)
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        let occurrence_id: Option<String> = row
            .get(1)
            .map_err(|error| storage(MATERIALIZE_REFRESH, error))?;
        let Some(occurrence_id) = occurrence_id else {
            continue;
        };
        record_parent_resolver_row(
            u64::try_from(message_id.len().saturating_add(occurrence_id.len()))
                .map_err(|error| storage(MATERIALIZE_REFRESH, error))?,
        );
        resolver.register(&message_id, &occurrence_id);
    }
    Ok(resolver)
}

#[hotpath::measure(future = true, label = "session_temporal.projection.parent_resolver")]
pub async fn canonical_parent_message_resolver(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &str,
    source_frontier: u64,
    operation: &'static str,
    control: Option<&ExecutionControl>,
    retain_canonical_outputs: bool,
) -> SessionStoreResult<ParentMessageResolver> {
    let mut resolver = ParentMessageResolver::default();
    let frontier = frontier_i64(source_frontier, operation)?;
    let mut after_sequence = 0_i64;
    loop {
        if let Some(control) = control {
            checkpoint_relation_rebuild_control(control)?;
        }
        record_parent_resolver_probe();
        let mut rows = conn
            .query(
                "WITH page AS (
                     SELECT effect.observation_sequence AS sequence,
                            observation.observation_json
                     FROM session_temporal_observation_effects AS effect
                     JOIN observations AS observation
                       ON observation.observation_id = effect.observation_id
                     WHERE effect.session_id = ?1
                       AND effect.observation_sequence > ?2
                       AND effect.observation_sequence <= ?3
                       AND effect.output_count > 0
                     ORDER BY effect.observation_sequence
                     LIMIT ?4
                 ),
                 bounded AS (
                     SELECT sequence, observation_json,
                            ROW_NUMBER() OVER (ORDER BY sequence) AS page_row,
                            SUM(length(CAST(observation_json AS BLOB)))
                                OVER (ORDER BY sequence) AS cumulative_bytes
                     FROM page
                 )
                 SELECT sequence, observation_json
                 FROM bounded
                 WHERE cumulative_bytes <= ?5 OR page_row = 1
                 ORDER BY sequence",
                params![
                    session_id,
                    after_sequence,
                    frontier,
                    PARENT_RESOLVER_PAGE_SIZE,
                    PARENT_RESOLVER_PAGE_MAX_BYTES
                ],
            )
            .await
            .map_err(|error| storage(operation, error))?;
        let mut page_count = 0usize;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(operation, error))?
        {
            if let Some(control) = control {
                checkpoint_relation_rebuild_control(control)?;
            }
            let sequence: i64 = row.get(0).map_err(|error| storage(operation, error))?;
            if sequence <= after_sequence {
                return Err(storage_message(
                    operation,
                    "parent resolver observation page did not advance",
                ));
            }
            let encoded: String = row.get(1).map_err(|error| storage(operation, error))?;
            record_parent_resolver_row(
                u64::try_from(encoded.len()).map_err(|error| storage(operation, error))?,
            );
            let observation: tracedecay_domain::DurableObservationV1 =
                serde_json::from_str(&encoded).map_err(|error| storage(operation, error))?;
            let envelope = if retain_canonical_outputs {
                Some(
                    observation_envelope_from_payload(observation.payload())
                        .map_err(|error| storage(operation, error))?,
                )
            } else {
                None
            };
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
                if let Some(envelope) = envelope.as_ref() {
                    resolver.canonical_outputs.push((
                        occurrence_id.as_str().to_owned(),
                        envelope
                            .relations()
                            .parent_message_id()
                            .filter(|parent| parent.as_str() == output.message().message_id)
                            .map(|parent| parent.as_str().to_owned()),
                    ));
                }
            }
            after_sequence = sequence;
            page_count += 1;
        }
        drop(rows);
        if page_count == 0 {
            break;
        }
    }
    resolver.reject_ambiguity(operation)?;
    Ok(resolver)
}

#[inline(always)]
fn record_parent_resolver_probe() {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("session_temporal.parent_resolver.query_probes").inc(1_u64);
}

#[inline(always)]
fn record_parent_resolver_row(bytes: u64) {
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("session_temporal.parent_resolver.rows").inc(1_u64);
        hotpath::gauge!("session_temporal.parent_resolver.row_payload_bytes").inc(bytes);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = bytes;
}

impl ParentMessageResolver {
    pub(crate) fn register(&mut self, message_id: &str, occurrence_id: &str) {
        self.occurrences
            .entry(message_id.to_owned())
            .and_modify(|resolution| {
                resolution.occurrences.insert(occurrence_id.to_owned());
            })
            .or_insert_with(|| ParentMessageResolution {
                selected: occurrence_id.to_owned(),
                occurrences: BTreeSet::from([occurrence_id.to_owned()]),
            });
    }

    pub(crate) fn reject_ambiguity(&self, operation: &'static str) -> SessionStoreResult<()> {
        if let Some((message_id, resolution)) = self
            .occurrences
            .iter()
            .find(|(_, resolution)| resolution.occurrences.len() > 1)
        {
            return Err(storage_message(
                operation,
                format!(
                    "session-scoped message id {message_id} resolves to {} occurrences",
                    resolution.occurrences.len()
                ),
            ));
        }
        Ok(())
    }

    pub(crate) fn resolve(&self, message_id: &str) -> Option<&str> {
        self.occurrences
            .get(message_id)
            .map(|resolution| resolution.selected.as_str())
    }

    pub(crate) fn canonical_outputs(&self) -> &[(String, Option<String>)] {
        &self.canonical_outputs
    }
}

#[derive(Debug, Default)]
pub struct ParentMessageResolver {
    occurrences: BTreeMap<String, ParentMessageResolution>,
    canonical_outputs: Vec<(String, Option<String>)>,
}

#[derive(Debug)]
struct ParentMessageResolution {
    selected: String,
    occurrences: BTreeSet<String>,
}
