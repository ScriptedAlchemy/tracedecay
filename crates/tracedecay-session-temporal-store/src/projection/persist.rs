use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Deserialize;
use serde_json::{Value, json};
use tracedecay_domain::{
    AnchorProvenanceRelation, CanonicalObservationEnvelopeV1, CanonicalObservationIdV1,
    CopyProofV1, DurableObservationV1,
    LogicalCopyRecordV1, MessageOccurrenceRecordV1, RetrievalAnchorRecord, SessionAuthorityClassV1,
    SessionId, TemporalAssertionKindV1, TemporalAssertionRecordV1, TemporalValidityV1, UtcMicros,
    derive_exact_observation_anchor_id,
};
use tracedecay_lcm::retrieval_content::{derived_text_for_index, projected_content_hash};
use tracedecay_runtime_core::db::engine::{
    IntoValue, Value as EngineValue, params, params_from_iter,
};
use tracedecay_store::{
    SessionMessageProjection, SessionStoreError, SessionStoreResult,
    SessionTemporalProjectionBatchReceiptV1, SessionTemporalProjectionBatchV1,
};
use tracedecay_temporal_query::execution::ExecutionControl;

use tracedecay_store::derive_canonical_projection;

use super::super::query::{
    PERSIST_OPERATION, encode_watermarks, frontier_i64, generation_i64, missing_observation,
    now_micros, read_generation, read_observation, read_observations, storage, storage_message,
};
use super::super::rebuild::checkpoint_relation_rebuild_control;
use super::materialize::*;
use super::receipts::*;

pub(crate) fn observation_envelope_from_payload(
    payload: &Value,
) -> Result<CanonicalObservationEnvelopeV1, serde_json::Error> {
    CanonicalObservationEnvelopeV1::deserialize(payload)
}

fn observation_envelope(
    observation: &tracedecay_domain::DurableObservationV1,
) -> SessionStoreResult<CanonicalObservationEnvelopeV1> {
    observation_envelope_from_payload(observation.payload())
        .map_err(|error| storage(PERSIST_OPERATION, error))
}

#[tracing::instrument(
    name = "session_temporal.persist.projection_batch",
    level = "trace",
    skip_all
)]
/// Persists one projected batch inside the caller's transaction and returns
/// its receipt with the committed item count the inserted receipt row
/// carries. `reads` is a begin segment's preloaded rows; when present the
/// session-scoped receipt reads resolve in memory while everything the item
/// itself writes stays on the live connection.
pub async fn persist_session_temporal_projection_batch_in_transaction(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    control: &ExecutionControl,
    reads: Option<&crate::refresh::BeginSegmentReads>,
) -> SessionStoreResult<(SessionTemporalProjectionBatchReceiptV1, u64)> {
    checkpoint_relation_rebuild_control(control)?;
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
    let session_key = batch.session_id().as_str();
    if let Some((receipt, committed_item_count)) = match reads {
        Some(reads) => resolve_receipt_segment(
            batch,
            batch_digest.as_str(),
            reads.receipt_rows(session_key),
        ),
        None => read_projection_receipt(conn, batch, batch_digest.as_str()).await,
    }? {
        return Ok((receipt, committed_item_count));
    }
    match reads {
        Some(reads) => contiguous_checkpoint_segment(batch, reads.receipt_rows(session_key)),
        None => require_contiguous_checkpoint(conn, batch).await,
    }?;

    persist_occurrences(conn, batch, control).await?;
    for copy in batch.copies() {
        checkpoint_relation_rebuild_control(control)?;
        validate_copy(conn, batch, copy).await?;
    }
    for assertion in batch.assertions() {
        checkpoint_relation_rebuild_control(control)?;
        persist_assertion(conn, batch, assertion).await?;
    }
    rebuild_current_occurrences(conn, batch).await?;
    let terminal = batch.source_through() == batch.watermarks().source_frontier()
        && batch.projection_through() == batch.watermarks().projection_frontier();
    if terminal {
        extend_assertion_derivatives(conn, batch, control).await?;
        super::derived::extend_derived_evidence(conn, batch, control).await?;
    }

    let committed_at = now_micros(PERSIST_OPERATION)?;
    // Intermediate receipts are exact batch journals through `batch_digest`.
    // Only the terminal receipt claims whole-generation coverage: the base
    // generation's coverage plus the rows this candidate added and minus the
    // versions it superseded. Activation recomputes that delta before the CAS.
    // The coverage fold reads the candidate rows this item just persisted,
    // so it runs on the live transaction even when a segment preload exists.
    let coverage = if terminal {
        candidate_projection_coverage(conn, batch.session_id(), batch.generation(), control, None)
            .await?
    } else {
        empty_projection_coverage()
    };
    let counts = match reads {
        Some(reads) => {
            let base = reads.projection.base_coverage(
                session_key,
                generation_i64(batch.generation(), PERSIST_OPERATION)?,
            )?;
            Some(progress_counts_segment(
                batch,
                reads.receipt_rows(session_key),
                &base,
            )?)
        }
        None => None,
    };
    let committed_item_count = insert_projection_receipt(
        conn,
        batch,
        batch_digest.as_str(),
        &coverage,
        committed_at.0,
        counts,
    )
    .await?;
    Ok((
        SessionTemporalProjectionBatchReceiptV1::applied(
            batch,
            batch_digest,
            batch.occurrences().len(),
            batch.copies().len(),
            batch.assertions().len(),
            committed_at,
        )?,
        committed_item_count,
    ))
}

struct CanonicalOccurrenceProjection {
    source_sequence: u64,
    observation: DurableObservationV1,
    envelope: CanonicalObservationEnvelopeV1,
    outputs: Vec<SessionMessageProjection>,
}

/// Loads every observation the batch's occurrences derive from in one paged
/// `read_observations` pass instead of one query per source group. The map
/// holds exactly the rows the equivalent per-id reads would return; a missing
/// id surfaces through the same `missing_observation` error at the same point
/// in the caller's iteration order.
async fn load_output_counts(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &str,
    observation_ids: &[String],
) -> SessionStoreResult<HashMap<(String, i64), usize>> {
    let mut counts = HashMap::new();
    if observation_ids.is_empty() {
        return Ok(counts);
    }
    let ids_json = serde_json::to_string(observation_ids)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let mut rows = conn
        .query(
            "SELECT observation_id, observation_sequence, output_count
             FROM session_temporal_observation_effects
             WHERE session_id = ?1 AND output_count > 0
               AND observation_id IN (SELECT value FROM json_each(?2))",
            params![session_id, ids_json.as_str()],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    {
        let observation_id: String = row
            .get(0)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let sequence: i64 = row
            .get(1)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let count: i64 = row
            .get(2)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        counts
            .entry((observation_id, sequence))
            .or_insert(usize::try_from(count).map_err(|error| storage(PERSIST_OPERATION, error))?);
    }
    Ok(counts)
}

/// Loads the anchor rows joined through `observation_retrieval_anchors` for
/// every source observation in one query. Each source's expected anchor is
/// still selected by exact `anchor_id` equality at apply time, so the row set
/// each occurrence observes is identical to the per-occurrence join.
async fn load_source_anchor_jsons(
    conn: &impl crate::handle::SessionTemporalExec,
    observation_ids: &[String],
) -> SessionStoreResult<HashMap<String, Vec<(String, String)>>> {
    let mut anchors: HashMap<String, Vec<(String, String)>> = HashMap::new();
    if observation_ids.is_empty() {
        return Ok(anchors);
    }
    let ids_json = serde_json::to_string(observation_ids)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let mut rows = conn
        .query(
            "SELECT link.observation_id, link.anchor_id, anchor.anchor_json
             FROM observation_retrieval_anchors AS link
             JOIN retrieval_anchors AS anchor ON anchor.anchor_id = link.anchor_id
             WHERE link.observation_id IN (SELECT value FROM json_each(?1))",
            params![ids_json.as_str()],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    {
        let observation_id: String = row
            .get(0)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let anchor_id: String = row
            .get(1)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let anchor_json: String = row
            .get(2)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        anchors
            .entry(observation_id)
            .or_default()
            .push((anchor_id, anchor_json));
    }
    Ok(anchors)
}

fn canonical_occurrence_projection(
    batch: &SessionTemporalProjectionBatchV1,
    source_sequence: u64,
    observation: DurableObservationV1,
    output_count: usize,
) -> SessionStoreResult<CanonicalOccurrenceProjection> {
    if source_sequence > batch.watermarks().source_frontier() {
        return Err(SessionStoreError::FrozenWatermarkMismatch);
    }
    let projection = derive_canonical_projection(&observation)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let envelope = observation_envelope(&observation)?;
    let outputs = projection
        .message()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    if outputs.len() != output_count {
        return Err(storage_message(
            PERSIST_OPERATION,
            "canonical observation effect output authority disagrees with its projection",
        ));
    }
    Ok(CanonicalOccurrenceProjection {
        source_sequence,
        observation,
        envelope,
        outputs,
    })
}

#[tracing::instrument(
    name = "session_temporal.persist.occurrences",
    level = "trace",
    skip_all
)]
pub(super) async fn persist_occurrences(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    control: &ExecutionControl,
) -> SessionStoreResult<()> {
    let mut occurrences_by_source = BTreeMap::<_, Vec<_>>::new();
    let mut source_ids = Vec::new();
    let mut seen_sources = BTreeSet::new();
    for occurrence in batch.occurrences() {
        occurrences_by_source
            .entry(occurrence.source_observation_id.as_str())
            .or_default()
            .push(occurrence);
        if seen_sources.insert(occurrence.source_observation_id.as_str()) {
            source_ids.push(occurrence.source_observation_id.clone());
        }
    }
    let observations = read_observations(conn, source_ids.as_slice()).await?;
    let source_id_strings = source_ids
        .iter()
        .map(CanonicalObservationIdV1::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    let session_key = batch.session_id().as_str();
    let output_counts =
        load_output_counts(conn, session_key, source_id_strings.as_slice()).await?;
    let anchors = load_source_anchor_jsons(conn, source_id_strings.as_slice()).await?;
    let mut memo = EnsureMemo::default();
    let mut pending = Vec::new();
    for occurrences in occurrences_by_source.into_values() {
        checkpoint_relation_rebuild_control(control)?;
        let first = occurrences.first().ok_or_else(|| {
            storage_message(
                PERSIST_OPERATION,
                "canonical occurrence source group is unexpectedly empty",
            )
        })?;
        let (source_sequence, observation) = observations
            .get(first.source_observation_id.as_str())
            .cloned()
            .ok_or_else(|| missing_observation(&first.source_observation_id))?;
        let output_count = output_counts
            .get(&(
                first.source_observation_id.as_str().to_string(),
                frontier_i64(source_sequence, PERSIST_OPERATION)?,
            ))
            .copied()
            .ok_or_else(|| {
                storage_message(
                    PERSIST_OPERATION,
                    "canonical observation has no atomically recorded temporal effect",
                )
            })?;
        let canonical =
            canonical_occurrence_projection(batch, source_sequence, observation, output_count)?;
        let expected_anchor = derive_exact_observation_anchor_id(
            canonical.observation.scope(),
            canonical.observation.observation_id(),
        )
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let anchor_json = anchors
            .get(first.source_observation_id.as_str())
            .and_then(|rows| {
                rows.iter()
                    .find(|(anchor_id, _)| anchor_id == expected_anchor.as_str())
            })
            .map(|(_, anchor_json)| anchor_json.as_str())
            .ok_or_else(|| {
                storage_message(
                    PERSIST_OPERATION,
                    "canonical observation retrieval anchor is missing",
                )
            })?;
        for occurrence in occurrences {
            checkpoint_relation_rebuild_control(control)?;
            let ordinal = usize::try_from(occurrence.projection_output_ordinal.value())
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let output = canonical.outputs.get(ordinal).ok_or_else(|| {
                storage_message(
                    PERSIST_OPERATION,
                    "canonical observation has no atomically recorded temporal effect output",
                )
            })?;
            pending.push(
                prepare_occurrence(
                    conn,
                    batch,
                    occurrence,
                    &canonical,
                    output,
                    anchor_json,
                    &mut memo,
                )
                .await?,
            );
        }
    }
    insert_prepared_occurrences(conn, batch, &pending).await
}

/// Tracks the minimum bound values already issued to the `ensure_*` writers
/// for each entity within one projection batch. Each `ensure_*` statement is a
/// monotone MIN-merge keyed on `(session_id, entity, generation)`, so a repeat
/// call whose values cannot lower any bound already issued is a no-op and can
/// be skipped without changing the committed rows.
#[derive(Default)]
struct EnsureMemo {
    threads: BTreeMap<String, (String, i64)>,
    turns: BTreeMap<String, (i64, String, i64)>,
    agents: BTreeMap<String, i64>,
}

impl EnsureMemo {
    fn thread_needs(&mut self, thread_id: &str, grouping: &str, created_at: i64) -> bool {
        match self.threads.get_mut(thread_id) {
            None => {
                self.threads
                    .insert(thread_id.to_string(), (grouping.to_string(), created_at));
                true
            }
            Some(min) => {
                let needs = grouping < min.0.as_str() || created_at < min.1;
                if grouping < min.0.as_str() {
                    min.0 = grouping.to_string();
                }
                if created_at < min.1 {
                    min.1 = created_at;
                }
                needs
            }
        }
    }

    fn turn_needs(
        &mut self,
        turn_id: &str,
        ordinal: i64,
        grouping: &str,
        created_at: i64,
    ) -> bool {
        match self.turns.get_mut(turn_id) {
            None => {
                self.turns.insert(
                    turn_id.to_string(),
                    (ordinal, grouping.to_string(), created_at),
                );
                true
            }
            Some(min) => {
                let needs = ordinal < min.0 || grouping < min.1.as_str() || created_at < min.2;
                if ordinal < min.0 {
                    min.0 = ordinal;
                }
                if grouping < min.1.as_str() {
                    min.1 = grouping.to_string();
                }
                if created_at < min.2 {
                    min.2 = created_at;
                }
                needs
            }
        }
    }

    fn agent_needs(&mut self, agent_id: &str, created_at: i64) -> bool {
        match self.agents.get_mut(agent_id) {
            None => {
                self.agents.insert(agent_id.to_string(), created_at);
                true
            }
            Some(min) => {
                let needs = created_at < *min;
                if needs {
                    *min = created_at;
                }
                needs
            }
        }
    }
}

/// One fully validated occurrence row awaiting the batch's single insert.
/// Validation (canonical equality, privacy/derivation checks, and the
/// memo-deduped `ensure_*` writes) already ran; `values` holds the 25 bound
/// columns in `session_occurrences` column order.
struct PreparedOccurrence<'a> {
    occurrence: &'a MessageOccurrenceRecordV1,
    source_provider: String,
    text: String,
    values: Vec<EngineValue>,
    turn_member: Option<(String, i64)>,
}

fn bind(value: impl IntoValue) -> SessionStoreResult<EngineValue> {
    value
        .into_value()
        .map_err(|error| storage(PERSIST_OPERATION, error))
}

async fn prepare_occurrence<'a>(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    occurrence: &'a MessageOccurrenceRecordV1,
    canonical: &CanonicalOccurrenceProjection,
    output: &SessionMessageProjection,
    anchor_json: &str,
    memo: &mut EnsureMemo,
) -> SessionStoreResult<PreparedOccurrence<'a>> {
    let envelope = &canonical.envelope;
    if output.session().session_id != occurrence.session_id.as_str() {
        return Err(storage_message(
            PERSIST_OPERATION,
            format!(
                "observation {} has no matching message output {} for session {}",
                occurrence.source_observation_id.as_str(),
                occurrence.projection_output_ordinal.value(),
                occurrence.session_id.as_str()
            ),
        ));
    }
    let (expected, copied_from_anchor_ids) = canonical_occurrence_from_anchor(
        &canonical.observation,
        envelope,
        output,
        anchor_json,
    )?;
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
        let grouping =
            serde_json::to_string(grouping).map_err(|error| storage(PERSIST_OPERATION, error))?;
        if memo.thread_needs(thread_id.as_str(), &grouping, occurrence.knowledge_at.0) {
            ensure_thread(
                conn,
                batch.session_id().as_str(),
                generation,
                thread_id.as_str(),
                &grouping,
                occurrence.knowledge_at.0,
            )
            .await?;
        }
    }
    if let (Some(turn_id), Some(grouping)) = (&occurrence.turn_id, &occurrence.turn_grouping) {
        let grouping =
            serde_json::to_string(grouping).map_err(|error| storage(PERSIST_OPERATION, error))?;
        let ordinal = i64::from(occurrence.projection_output_ordinal.value());
        if memo.turn_needs(
            turn_id.as_str(),
            ordinal,
            &grouping,
            occurrence.knowledge_at.0,
        ) {
            ensure_turn(
                conn,
                batch.session_id().as_str(),
                generation,
                turn_id.as_str(),
                &grouping,
                ordinal,
                occurrence.knowledge_at.0,
            )
            .await?;
        }
    }
    if let Some(agent_id) = &occurrence.agent_id
        && memo.agent_needs(agent_id.as_str(), occurrence.knowledge_at.0)
    {
        ensure_agent(
            conn,
            batch.session_id().as_str(),
            generation,
            agent_id.as_str(),
            occurrence.knowledge_at.0,
        )
        .await?;
    }
    if let Some(parent_agent_id) = envelope.relations().parent_agent_id()
        && memo.agent_needs(parent_agent_id.as_str(), occurrence.knowledge_at.0)
    {
        ensure_agent(
            conn,
            batch.session_id().as_str(),
            generation,
            parent_agent_id.as_str(),
            occurrence.knowledge_at.0,
        )
        .await?;
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
    let copied_from_anchor_ids = serde_json::to_string(&copied_from_anchor_ids)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let sanitized_content = output.message().text.as_str();
    let sanitized_content_digest = projected_content_hash(sanitized_content);
    let sanitized_content_bytes = i64::try_from(sanitized_content.len())
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    // Index/FTS store the derived budget, not a second full copy of the body.
    // Canonical text stays on the observation/LCM payload.
    let index_text = derived_text_for_index(sanitized_content);
    let relations = envelope.relations();
    Ok(PreparedOccurrence {
        occurrence,
        source_provider: output.message().provider.as_str().to_owned(),
        text: output.message().text.as_str().to_owned(),
        values: vec![
            bind(batch.session_id().as_str())?,
            bind(generation)?,
            bind(occurrence.occurrence_id.as_str())?,
            bind(occurrence.source_observation_id.as_str())?,
            bind(i64::try_from(canonical.source_sequence)
                .map_err(|error| storage(PERSIST_OPERATION, error))?)?,
            bind(output.message().provider.as_str())?,
            bind(i64::from(occurrence.projection_output_ordinal.value()))?,
            bind(occurrence.retrieval_anchor_id.as_str())?,
            bind(occurrence
                .thread_id
                .as_ref()
                .map(tracedecay_domain::ThreadId::as_str))?,
            bind(thread_grouping.as_deref())?,
            bind(occurrence
                .turn_id
                .as_ref()
                .map(tracedecay_domain::TurnId::as_str))?,
            bind(turn_grouping.as_deref())?,
            bind(occurrence
                .message_id
                .as_ref()
                .map(tracedecay_domain::MessageId::as_str))?,
            bind(occurrence
                .agent_id
                .as_ref()
                .map(tracedecay_domain::AgentInstanceId::as_str))?,
            bind(relations
                .parent_message_id()
                .map(tracedecay_domain::ObservationId::as_str))?,
            bind(relations
                .parent_agent_id()
                .map(tracedecay_domain::ObservationId::as_str))?,
            bind(relations.parent_session_id().map(SessionId::as_str))?,
            bind(copied_from_anchor_ids.as_str())?,
            bind(role.as_str())?,
            bind(occurrence.knowledge_at.0)?,
            bind(valid_time.as_str())?,
            bind(evidence.as_str())?,
            bind(sanitized_content_digest.as_str())?,
            bind(sanitized_content_bytes)?,
            bind(index_text.as_str())?,
        ],
        turn_member: occurrence.turn_id.as_ref().map(|turn_id| {
            (
                turn_id.as_str().to_owned(),
                i64::from(occurrence.projection_output_ordinal.value()),
            )
        }),
    })
}

const OCCURRENCE_ROW_COLUMNS: usize = 25;
const OCCURRENCE_INSERT_CHUNK_ROWS: usize = 200;

/// Inserts the batch's prepared occurrences as multi-row
/// `INSERT OR IGNORE … RETURNING` chunks, then runs the exact-row check on
/// the conflicting ids in one query and the `session_turn_members` inserts
/// for the newly inserted rows in one statement — the same committed rows
/// as the previous per-occurrence statement sequence, in the same item
/// order, inside the same transaction.
async fn insert_prepared_occurrences(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    pending: &[PreparedOccurrence<'_>],
) -> SessionStoreResult<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    let mut inserted = HashSet::<String>::new();
    for chunk in pending.chunks(OCCURRENCE_INSERT_CHUNK_ROWS) {
        let mut sql = String::from(
            "INSERT OR IGNORE INTO session_occurrences (
            session_id, generation, occurrence_id, source_observation_id, source_sequence,
            source_provider, projection_output_ordinal, retrieval_anchor_id,
            thread_id, thread_grouping_json, turn_id, turn_grouping_json,
            message_id, agent_id, parent_message_id, parent_agent_id, parent_session_id,
            copied_from_anchor_ids_json, role, knowledge_at, valid_time_json,
            evidence_json, sanitized_content_digest, sanitized_content_bytes,
            index_text
         ) VALUES ",
        );
        for row_index in 0..chunk.len() {
            if row_index > 0 {
                sql.push(',');
            }
            sql.push('(');
            for column_index in 0..OCCURRENCE_ROW_COLUMNS {
                if column_index > 0 {
                    sql.push(',');
                }
                sql.push('?');
            }
            sql.push(')');
        }
        sql.push_str(" RETURNING occurrence_id");
        let values = chunk
            .iter()
            .flat_map(|row| row.values.iter().cloned())
            .collect::<Vec<EngineValue>>();
        let mut rows = conn
            .query(sql.as_str(), params_from_iter(values))
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?
        {
            let occurrence_id: String = row
                .get(0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            inserted.insert(occurrence_id);
        }
    }

    // Rows later in the batch repeating an id an earlier row already
    // attempted are conflicts regardless of what the insert returned — the
    // earlier row claimed the id.
    let mut attempted = HashSet::<&str>::new();
    let was_inserted = pending
        .iter()
        .map(|row| {
            attempted.insert(row.occurrence.occurrence_id.as_str())
                && inserted.contains(row.occurrence.occurrence_id.as_str())
        })
        .collect::<Vec<_>>();
    let conflicts = pending
        .iter()
        .zip(was_inserted.iter())
        .filter(|(_, inserted)| !**inserted)
        .map(|(row, _)| row)
        .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        // Occurrences are append-only and keyed per session, so a row the base
        // or an earlier insert already holds must match exactly. Compare every
        // conflicting id in one query using the same projection of fields the
        // per-row check reads.
        let conflict_ids = conflicts
            .iter()
            .map(|row| row.occurrence.occurrence_id.as_str().to_string())
            .collect::<Vec<_>>();
        let ids_json = serde_json::to_string(&conflict_ids)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let mut rows = conn
            .query(
                "SELECT occurrence_id, json_object(
                    'source_observation_id', source_observation_id,
                    'source_provider', source_provider,
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
                    'sanitized_content_digest', sanitized_content_digest,
                    'sanitized_content_bytes', sanitized_content_bytes,
                    'snippet_text', snippet_text,
                    'index_text', index_text
                 )
                 FROM session_occurrences
                 WHERE session_id = ?1 AND +generation <= ?2
                   AND occurrence_id IN (SELECT value FROM json_each(?3))",
                params![
                    batch.session_id().as_str(),
                    generation,
                    ids_json.as_str()
                ],
            )
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        let mut stored = HashMap::<String, String>::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?
        {
            let occurrence_id: String = row
                .get(0)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let encoded: String = row
                .get(1)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            stored.insert(occurrence_id, encoded);
        }
        for conflict in conflicts {
            let encoded = stored
                .get(conflict.occurrence.occurrence_id.as_str())
                .ok_or_else(|| {
                    storage_message(
                        PERSIST_OPERATION,
                        "occurrence insert was ignored without a row this generation reads",
                    )
                })?;
            require_occurrence_row(
                encoded,
                conflict.occurrence,
                conflict.source_provider.as_str(),
                conflict.text.as_str(),
            )?;
        }
    }

    let member_rows = pending
        .iter()
        .zip(was_inserted.iter())
        .filter(|(_, inserted)| **inserted)
        .map(|(row, _)| row)
        .filter_map(|row| {
            row.turn_member
                .as_ref()
                .map(|(turn_id, ordinal)| (turn_id, &row.occurrence.occurrence_id, ordinal))
        })
        .collect::<Vec<_>>();
    for chunk in member_rows.chunks(OCCURRENCE_INSERT_CHUNK_ROWS) {
        let mut sql = String::from(
            "INSERT OR IGNORE INTO session_turn_members (
                session_id, generation, turn_id, occurrence_id, ordinal
             ) VALUES ",
        );
        let mut values = Vec::new();
        for (row_index, (turn_id, occurrence_id, ordinal)) in chunk.iter().enumerate() {
            if row_index > 0 {
                sql.push(',');
            }
            sql.push_str("(?, ?, ?, ?, ?)");
            values.push(bind(batch.session_id().as_str())?);
            values.push(bind(generation)?);
            values.push(bind(turn_id.as_str())?);
            values.push(bind(occurrence_id.as_str())?);
            values.push(bind(**ordinal)?);
        }
        conn.execute(sql.as_str(), params_from_iter(values))
            .await
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    }
    Ok(())
}

/// Also returns the anchors the occurrence's anchor record copied from, the
/// lineage its relations derive from.
pub(super) async fn canonical_occurrence(
    conn: &impl crate::handle::SessionTemporalQuery,
    observation: &tracedecay_domain::DurableObservationV1,
    envelope: &CanonicalObservationEnvelopeV1,
    output: &SessionMessageProjection,
) -> SessionStoreResult<(
    MessageOccurrenceRecordV1,
    Vec<tracedecay_domain::RetrievalAnchorId>,
)> {
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
    canonical_occurrence_from_anchor(observation, envelope, output, &anchor_json)
}

/// The shared canonical-occurrence derivation over an already-loaded anchor
/// record — the batch preload and the single-shot query path produce the same
/// `anchor_json` row before this runs.
fn canonical_occurrence_from_anchor(
    observation: &tracedecay_domain::DurableObservationV1,
    envelope: &CanonicalObservationEnvelopeV1,
    output: &SessionMessageProjection,
    anchor_json: &str,
) -> SessionStoreResult<(
    MessageOccurrenceRecordV1,
    Vec<tracedecay_domain::RetrievalAnchorId>,
)> {
    let output_ordinal = output.output_ordinal();
    let expected_anchor =
        derive_exact_observation_anchor_id(observation.scope(), observation.observation_id())
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let anchor: RetrievalAnchorRecord =
        serde_json::from_str(anchor_json).map_err(|error| storage(PERSIST_OPERATION, error))?;
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
        .or_else(|| {
            // Host-native timestamps arrive in whatever unit the host emits;
            // the summary-publication side already normalizes through the
            // same helper, and comparing valid times across the two paths is
            // only sound if both land in canonical micros.
            envelope
                .evidence()
                .native_timestamp()
                .map(|native| UtcMicros(super::super::operations::normalize_timestamp(native)))
        });
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
    let copied_from = anchor
        .source_anchors()
        .iter()
        .filter(|source| source.relation() == AnchorProvenanceRelation::CopiedFrom)
        .map(|source| source.anchor_id().clone())
        .collect();
    Ok((record, copied_from))
}

/// Versions a thread whose earliest grouping or creation time moved. The
/// candidate reads its own version first, then its base's; an unchanged
/// thread writes nothing.
pub(super) async fn ensure_thread(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &str,
    generation: i64,
    thread_id: &str,
    grouping: &str,
    created_at: i64,
) -> SessionStoreResult<()> {
    conn.execute(
        "INSERT INTO session_threads (
                session_id, generation, thread_id, grouping_provenance, created_at
             )
             SELECT ?1, ?2, ?3,
                    MIN(?4, COALESCE(prior.grouping_provenance, ?4)),
                    MIN(?5, COALESCE(prior.created_at, ?5))
             FROM (SELECT 1) LEFT JOIN (
                 SELECT grouping_provenance, created_at FROM session_threads
                 WHERE session_id = ?1 AND thread_id = ?3 AND +generation <= ?2
                 ORDER BY generation DESC LIMIT 1
             ) AS prior
             WHERE prior.created_at IS NULL
                OR ?4 < prior.grouping_provenance
                OR ?5 < prior.created_at
             ON CONFLICT(session_id, thread_id, generation) DO UPDATE SET
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
    conn: &impl crate::handle::SessionTemporalExec,
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
             )
             SELECT ?1, ?2, ?3,
                    MIN(?4, COALESCE(prior.ordinal, ?4)),
                    MIN(?5, COALESCE(prior.grouping_provenance, ?5)),
                    MIN(?6, COALESCE(prior.created_at, ?6))
             FROM (SELECT 1) LEFT JOIN (
                 SELECT ordinal, grouping_provenance, created_at FROM session_turns
                 WHERE session_id = ?1 AND turn_id = ?3 AND +generation <= ?2
                 ORDER BY generation DESC LIMIT 1
             ) AS prior
             WHERE prior.created_at IS NULL
                OR ?4 < prior.ordinal
                OR ?5 < prior.grouping_provenance
                OR ?6 < prior.created_at
             ON CONFLICT(session_id, turn_id, generation) DO UPDATE SET
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
    conn: &impl crate::handle::SessionTemporalExec,
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
             )
             SELECT ?1, ?2, ?3,
                    MIN(?4, COALESCE(prior.agent_json, ?4)),
                    MIN(?5, COALESCE(prior.created_at, ?5))
             FROM (SELECT 1) LEFT JOIN (
                 SELECT agent_json, created_at FROM session_agents
                 WHERE session_id = ?1 AND agent_id = ?3 AND +generation <= ?2
                 ORDER BY generation DESC LIMIT 1
             ) AS prior
             WHERE prior.created_at IS NULL
                OR ?4 < prior.agent_json
                OR ?5 < prior.created_at
             ON CONFLICT(session_id, agent_id, generation) DO UPDATE SET
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

/// The exact-match half of the occurrence conflict path: `encoded` is the
/// `json_object` projection of the stored row (fetched by the caller, singly
/// or in the batched conflict query) and must equal the row the batch tried
/// to insert. Rows written before the index cap retained the full sanitized
/// body, so an uncapped body spelling is also accepted.
fn require_occurrence_row(
    encoded: &str,
    occurrence: &MessageOccurrenceRecordV1,
    source_provider: &str,
    text: &str,
) -> SessionStoreResult<()> {
    let actual: serde_json::Value =
        serde_json::from_str(encoded).map_err(|error| storage(PERSIST_OPERATION, error))?;
    let role =
        serde_json::to_value(occurrence.role).map_err(|error| storage(PERSIST_OPERATION, error))?;
    let expected = json!({
        "source_observation_id": occurrence.source_observation_id.as_str(),
        "source_provider": source_provider,
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
        "sanitized_content_digest": projected_content_hash(text),
        "sanitized_content_bytes": text.len(),
        "snippet_text": derived_text_for_index(text),
        "index_text": derived_text_for_index(text),
    });
    if actual == expected {
        return Ok(());
    }
    // Rows written before the index cap retained the full sanitized body.
    // Their receipts bind those bytes, so validate without rewriting them.
    let mut uncapped = expected;
    uncapped["snippet_text"] = Value::String(text.to_owned());
    uncapped["index_text"] = Value::String(text.to_owned());
    if actual != uncapped {
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

#[tracing::instrument(
    name = "session_temporal.projection.validate_copy",
    level = "trace",
    skip_all
)]
pub(super) async fn validate_copy(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    copy: &LogicalCopyRecordV1,
) -> SessionStoreResult<bool> {
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    validate_copy_proof(conn, batch, copy).await?;
    let mut rows = conn
        .query(
            "SELECT occurrence_id, knowledge_at, valid_time_json
             FROM session_occurrences
             WHERE session_id = ?1 AND +generation <= ?2
               AND occurrence_id IN (?3, ?4)",
            params![
                batch.session_id().as_str(),
                generation,
                copy.occurrence_id.as_str(),
                copy.copied_from_occurrence_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let mut seen_source = false;
    let mut target_knowledge_at = None;
    let mut target_valid_time = None;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
    {
        let occurrence_id: String = row
            .get(0)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
        if occurrence_id == copy.occurrence_id.as_str() {
            target_knowledge_at = Some(
                row.get::<i64>(1)
                    .map_err(|error| storage(PERSIST_OPERATION, error))?,
            );
            target_valid_time = Some(
                row.get::<String>(2)
                    .map_err(|error| storage(PERSIST_OPERATION, error))?,
            );
        }
        if occurrence_id == copy.copied_from_occurrence_id.as_str() {
            seen_source = true;
        }
    }
    let (Some(target_knowledge_at), Some(target_valid_time)) =
        (target_knowledge_at, target_valid_time)
    else {
        return Err(storage_message(
            PERSIST_OPERATION,
            "logical copy endpoint is outside the owning session generation",
        ));
    };
    if !seen_source {
        return Err(storage_message(
            PERSIST_OPERATION,
            "logical copy endpoint is outside the owning session generation",
        ));
    }
    let expected_valid_time = serde_json::to_string(&copy.valid_time)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    if copy.knowledge_at.0 != target_knowledge_at || expected_valid_time != target_valid_time {
        return Err(storage_message(
            PERSIST_OPERATION,
            "logical copy bitemporal fields must match the target occurrence",
        ));
    }
    Ok(true)
}

pub(super) async fn occurrence_observation_and_anchor(
    conn: &impl crate::handle::SessionTemporalQuery,
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
             WHERE session_id = ?1 AND occurrence_id = ?3 AND +generation <= ?2",
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
    let envelope = observation_envelope(&observation)?;
    Ok((observation, envelope, anchor_id))
}

pub(super) async fn validate_copy_proof(
    conn: &impl crate::handle::SessionTemporalExec,
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
            let mut source_rows = conn
                .query(
                    "SELECT occurrence_id
                     FROM session_occurrences
                     WHERE session_id = ?1
                       AND +generation <= ?2
                       AND retrieval_anchor_id = ?3
                       AND occurrence_id != ?4
                     ORDER BY knowledge_at DESC, occurrence_id DESC
                     LIMIT 1",
                    params![
                        batch.session_id().as_str(),
                        generation_i64(batch.generation(), PERSIST_OPERATION)?,
                        assertion_anchor_id.as_str(),
                        copy.occurrence_id.as_str(),
                    ],
                )
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let canonical_source = source_rows
                .next()
                .await
                .map_err(|error| storage(PERSIST_OPERATION, error))?
                .map(|row| row.get::<String>(0))
                .transpose()
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
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
                        && canonical_source.as_deref()
                            == Some(copy.copied_from_occurrence_id.as_str())
                        && anchor.source_anchors().iter().any(|lineage| {
                            lineage.relation() == AnchorProvenanceRelation::CopiedFrom
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

#[tracing::instrument(name = "session_temporal.persist.assertion", level = "trace", skip_all)]
pub(super) async fn persist_assertion(
    conn: &impl crate::handle::SessionTemporalExec,
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
                 WHERE session_id = ?1 AND assertion_id = ?3 AND +generation <= ?2",
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
                    "assertion insert was ignored without a row this generation reads",
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
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    assertion: &TemporalAssertionRecordV1,
) -> SessionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT DISTINCT occurrence.source_observation_id, anchor.anchor_json
             FROM session_occurrences AS occurrence
             JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = occurrence.retrieval_anchor_id
             WHERE occurrence.session_id = ?1 AND +occurrence.generation <= ?2
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
    if rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .is_some()
    {
        return Err(storage_message(
            PERSIST_OPERATION,
            "assertion subject anchor resolves to multiple source observations",
        ));
    }
    drop(rows);
    let mut object_rows = conn
        .query(
            "SELECT DISTINCT occurrence.source_observation_id, anchor.anchor_json
             FROM session_occurrences AS occurrence
             JOIN retrieval_anchors AS anchor
               ON anchor.anchor_id = occurrence.retrieval_anchor_id
             WHERE occurrence.session_id = ?1 AND +occurrence.generation <= ?2
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
    if object_rows
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .is_some()
    {
        return Err(storage_message(
            PERSIST_OPERATION,
            "assertion object anchor resolves to multiple source observations",
        ));
    }
    drop(object_rows);
    let observation_id = tracedecay_domain::CanonicalObservationIdV1::new(observation_id)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let (_, observation) = read_observation(conn, &observation_id).await?;
    let object_observation_id =
        tracedecay_domain::CanonicalObservationIdV1::new(object_observation_id)
            .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let (_, object_observation) = read_observation(conn, &object_observation_id).await?;
    let subject_envelope = observation_envelope(&observation)?;
    let object_envelope = observation_envelope(&object_observation)?;
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
    let expected_assertion_id = derived_temporal_assertion_id(
        &assertion.subject_anchor_id,
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
    relation: AnchorProvenanceRelation,
) -> Option<TemporalAssertionKindV1> {
    match relation {
        AnchorProvenanceRelation::Corrects => Some(TemporalAssertionKindV1::Corrects),
        AnchorProvenanceRelation::Contradicts => Some(TemporalAssertionKindV1::Contradicts),
        AnchorProvenanceRelation::Supersedes => Some(TemporalAssertionKindV1::Supersedes),
        AnchorProvenanceRelation::Supports => Some(TemporalAssertionKindV1::Supports),
        AnchorProvenanceRelation::CapturedFrom
        | AnchorProvenanceRelation::Produced
        | AnchorProvenanceRelation::Observed
        | AnchorProvenanceRelation::ExecutedIn
        | AnchorProvenanceRelation::Discussed
        | AnchorProvenanceRelation::CopiedFrom
        | AnchorProvenanceRelation::DerivedFrom => None,
    }
}

/// Inserts a new current-entity version for every winner that differs from
/// the version the candidate reads. `?1` session, `?2` generation, and the
/// `ranked` CTE yields `(entity_id, occurrence_id | assertion_id, count)`.
const CURRENT_ENTITY_VERSION_INSERT: &str = "
     INSERT INTO session_current_entities (
        session_id, generation, entity_kind, entity_id,
        current_assertion_id, current_occurrence_id, coverage_json
     )
     SELECT ?1, ?2, winner.entity_kind, winner.entity_id,
            winner.current_assertion_id, winner.current_occurrence_id, winner.coverage_json
     FROM winners AS winner
     WHERE NOT EXISTS (
         SELECT 1 FROM session_current_entities AS visible
         WHERE visible.session_id = ?1
           AND visible.entity_kind = winner.entity_kind
           AND visible.entity_id = winner.entity_id
           AND visible.generation = (
               SELECT MAX(version.generation) FROM session_current_entities AS version
               WHERE version.session_id = ?1
                 AND version.entity_kind = winner.entity_kind
                 AND version.entity_id = winner.entity_id
                 AND +version.generation <= ?2
           )
           AND visible.current_assertion_id IS winner.current_assertion_id
           AND visible.current_occurrence_id IS winner.current_occurrence_id
           AND visible.coverage_json = winner.coverage_json
     )
     ON CONFLICT(session_id, entity_kind, entity_id, generation) DO UPDATE SET
        current_assertion_id = excluded.current_assertion_id,
        current_occurrence_id = excluded.current_occurrence_id,
        coverage_json = excluded.coverage_json";

#[tracing::instrument(
    name = "session_temporal.projection.rebuild_occurrences",
    level = "trace",
    skip_all
)]
pub(super) async fn rebuild_current_occurrences(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
) -> SessionStoreResult<()> {
    let affected_anchors = batch
        .occurrences()
        .iter()
        .map(|occurrence| occurrence.retrieval_anchor_id.as_str())
        .collect::<BTreeSet<_>>();
    if affected_anchors.is_empty() {
        return Ok(());
    }
    let affected_anchors_json = serde_json::to_string(&affected_anchors)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    conn.execute(
        &format!(
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
                WHERE session_id = ?1 AND +generation <= ?2
                  AND retrieval_anchor_id IN (SELECT value FROM json_each(?3))
             ),
             winners AS (
                SELECT 'occurrence_anchor' AS entity_kind,
                       retrieval_anchor_id AS entity_id,
                       NULL AS current_assertion_id,
                       occurrence_id AS current_occurrence_id,
                       json_object('occurrence_count', occurrence_count) AS coverage_json
                FROM ranked WHERE precedence = 1
             )
             {CURRENT_ENTITY_VERSION_INSERT}"
        ),
        params![
            batch.session_id().as_str(),
            generation,
            affected_anchors_json.as_str()
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok(())
}

/// Supersession closures and assertion-anchor winners depend only on the
/// session's assertions, which are append-only, so both only grow and only
/// a candidate that added assertions can change them. Their direct edges
/// compare two assertions' own fields, so the closure over the visible
/// assertions contains every edge the base already holds.
#[tracing::instrument(
    name = "session_temporal.projection.rebuild_assertions",
    level = "trace",
    skip_all
)]
pub(super) async fn extend_assertion_derivatives(
    conn: &impl crate::handle::SessionTemporalExec,
    batch: &SessionTemporalProjectionBatchV1,
    control: &ExecutionControl,
) -> SessionStoreResult<()> {
    checkpoint_relation_rebuild_control(control)?;
    let generation = generation_i64(batch.generation(), PERSIST_OPERATION)?;
    let mut added = conn
        .query(
            "SELECT 1 FROM session_assertions
             WHERE session_id = ?1 AND generation = ?2 LIMIT 1",
            params![batch.session_id().as_str(), generation],
        )
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    if added
        .next()
        .await
        .map_err(|error| storage(PERSIST_OPERATION, error))?
        .is_none()
    {
        return Ok(());
    }
    drop(added);

    checkpoint_relation_rebuild_control(control)?;
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
              AND +prior.generation <= ?2
              AND prior.subject_anchor_id = current.object_anchor_id
             WHERE current.session_id = ?1 AND +current.generation <= ?2
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
         SELECT ?1, ?2, edge.superseded_assertion_id,
                edge.superseding_assertion_id, edge.created_at
         FROM transitive AS edge
         WHERE NOT EXISTS (
             SELECT 1 FROM session_assertion_supersession AS settled
             WHERE settled.session_id = ?1 AND +settled.generation <= ?2
               AND settled.superseded_assertion_id = edge.superseded_assertion_id
               AND settled.superseding_assertion_id = edge.superseding_assertion_id
         )",
        params![
            batch.session_id().as_str(),
            generation,
            TemporalAssertionKindV1::Corrects.as_str(),
            TemporalAssertionKindV1::Supersedes.as_str(),
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    checkpoint_relation_rebuild_control(control)?;
    conn.execute(
        &format!(
            "WITH RECURSIVE chains (
                 root_anchor_id, assertion_id, subject_anchor_id,
                 valid_at, knowledge_at
             ) AS (
                 SELECT object_anchor_id, assertion_id, subject_anchor_id,
                        json_extract(valid_time_json, '$.valid_at'), knowledge_at
                 FROM session_assertions
                 WHERE session_id = ?1 AND +generation <= ?2
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
                  AND +successor.generation <= ?2
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
             ),
             winners AS (
                SELECT 'assertion_anchor' AS entity_kind,
                       root_anchor_id AS entity_id,
                       assertion_id AS current_assertion_id,
                       NULL AS current_occurrence_id,
                       json_object('assertion_count', assertion_count) AS coverage_json
                FROM ranked WHERE precedence = 1
             )
             {CURRENT_ENTITY_VERSION_INSERT}"
        ),
        params![
            batch.session_id().as_str(),
            generation,
            TemporalAssertionKindV1::Corrects.as_str(),
            TemporalAssertionKindV1::Supersedes.as_str(),
        ],
    )
    .await
    .map_err(|error| storage(PERSIST_OPERATION, error))?;
    checkpoint_relation_rebuild_control(control)?;
    Ok(())
}
