use std::collections::BTreeSet;

use tracedecay_domain::{DurableObservationV1, MessageOccurrenceIdV1, ProjectionOutputOrdinalV1};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::{
    SessionFrozenWatermarksV1, SessionGenerationActivationReceiptV1,
    SessionGenerationActivationRequestV1, SessionGenerationRebuildDispositionV1,
    SessionGenerationRebuildReceiptV1, SessionGenerationRebuildRequestV1, SessionStoreError,
    SessionStoreResult,
};

use super::super::RegisteredGlobalDb;
use super::super::observation_projection::derive_projection;
use super::projection::{canonical_parent_message_resolver, validate_final_projection_receipt};
use super::query::{
    ACTIVATE_OPERATION, BEGIN_OPERATION, encode_watermarks, frontier_i64, generation_i64,
    now_micros, read_generation, require_active_generation, storage, storage_message,
};

impl RegisteredGlobalDb {
    pub async fn begin_session_generation_rebuild_result(
        &self,
        request: SessionGenerationRebuildRequestV1,
    ) -> SessionStoreResult<SessionGenerationRebuildReceiptV1> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(BEGIN_OPERATION, error))?;
        bootstrap_first_active_generation(
            &transaction,
            request.session_id(),
            request.snapshot().watermarks(),
        )
        .await?;
        require_active_generation(
            &transaction,
            request.session_id(),
            request.snapshot().watermarks().active_generation(),
            BEGIN_OPERATION,
        )
        .await?;
        let watermarks_json = encode_watermarks(request.snapshot().watermarks(), BEGIN_OPERATION)?;
        let existing = read_generation(
            &transaction,
            request.session_id(),
            request.candidate_generation(),
            BEGIN_OPERATION,
        )
        .await?;
        let disposition = if let Some(existing) = existing {
            if existing.frozen_watermarks_json != watermarks_json {
                return Err(SessionStoreError::FrozenWatermarkMismatch);
            }
            match existing.state.as_str() {
                "building" => SessionGenerationRebuildDispositionV1::Resumed,
                "ready" | "active" | "superseded" => {
                    SessionGenerationRebuildDispositionV1::Complete
                }
                state => {
                    return Err(storage_message(
                        BEGIN_OPERATION,
                        format!("generation cannot resume from state {state}"),
                    ));
                }
            }
        } else {
            let recorded_at = now_micros(BEGIN_OPERATION)?;
            transaction
                .execute(
                    "INSERT INTO session_temporal_generations (
                        session_id, generation, state, frozen_watermarks_json, created_at
                     ) VALUES (?1, ?2, 'building', ?3, ?4)",
                    params![
                        request.session_id().as_str(),
                        generation_i64(request.candidate_generation(), BEGIN_OPERATION)?,
                        watermarks_json,
                        recorded_at.0,
                    ],
                )
                .await
                .map_err(|error| storage(BEGIN_OPERATION, error))?;
            SessionGenerationRebuildDispositionV1::Started
        };
        let recorded_at = now_micros(BEGIN_OPERATION)?;
        transaction
            .commit()
            .await
            .map_err(|error| storage(BEGIN_OPERATION, error))?;
        SessionGenerationRebuildReceiptV1::new(&request, disposition, recorded_at)
    }

    pub async fn activate_session_temporal_generation_result(
        &self,
        request: SessionGenerationActivationRequestV1,
    ) -> SessionStoreResult<SessionGenerationActivationReceiptV1> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
        let previous_generation = require_active_generation(
            &transaction,
            request.session_id(),
            request.snapshot().watermarks().active_generation(),
            ACTIVATE_OPERATION,
        )
        .await?;
        let candidate = read_generation(
            &transaction,
            request.session_id(),
            request.generation(),
            ACTIVATE_OPERATION,
        )
        .await?
        .ok_or(SessionStoreError::MissingGeneration {
            generation: request.generation(),
        })?;
        if candidate.frozen_watermarks_json
            != encode_watermarks(request.snapshot().watermarks(), ACTIVATE_OPERATION)?
        {
            return Err(SessionStoreError::FrozenWatermarkMismatch);
        }
        if !matches!(candidate.state.as_str(), "building" | "ready") {
            return Err(storage_message(
                ACTIVATE_OPERATION,
                format!("generation cannot activate from state {}", candidate.state),
            ));
        }

        let generation = generation_i64(request.generation(), ACTIVATE_OPERATION)?;
        validate_final_projection_receipt(
            &transaction,
            request.session_id(),
            request.generation(),
            request.snapshot().watermarks(),
        )
        .await?;
        validate_candidate_frontier(
            &transaction,
            request.session_id().as_str(),
            generation,
            request.snapshot().watermarks().source_frontier(),
        )
        .await?;

        let activated_at = now_micros(ACTIVATE_OPERATION)?;
        if candidate.state == "building" {
            transaction
                .execute(
                    "UPDATE session_temporal_generations
                     SET state = 'ready', ready_at = ?3
                     WHERE session_id = ?1 AND generation = ?2 AND state = 'building'",
                    params![request.session_id().as_str(), generation, activated_at.0],
                )
                .await
                .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
        }
        if let Some(previous_generation) = previous_generation {
            transaction
                .execute(
                    "UPDATE session_temporal_generations
                     SET state = 'superseded', completed_at = ?3
                     WHERE session_id = ?1 AND generation = ?2 AND state = 'active'",
                    params![
                        request.session_id().as_str(),
                        generation_i64(previous_generation, ACTIVATE_OPERATION)?,
                        activated_at.0,
                    ],
                )
                .await
                .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
        }
        let changed = transaction
            .execute(
                "UPDATE session_temporal_generations
                 SET state = 'active', activated_at = ?3
                 WHERE session_id = ?1 AND generation = ?2 AND state = 'ready'",
                params![request.session_id().as_str(), generation, activated_at.0],
            )
            .await
            .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
        if changed != 1 {
            return Err(storage_message(
                ACTIVATE_OPERATION,
                "candidate generation did not transition from ready to active",
            ));
        }
        transaction
            .commit()
            .await
            .map_err(|error| storage(ACTIVATE_OPERATION, error))?;

        // Receipt watermarks pin the newly active generation; durable frozen
        // watermarks remain immutable after insert (schema transition guard).
        let frozen = request.snapshot().watermarks();
        let mut active_watermarks = SessionFrozenWatermarksV1::new(
            request.generation(),
            frozen.source_frontier(),
            frozen.projection_frontier(),
            frozen.summary_frontier(),
        );
        if let Some(cursor_key) = frozen.cursor_key() {
            active_watermarks = active_watermarks.with_cursor_key(cursor_key.clone());
        }
        SessionGenerationActivationReceiptV1::new(&request, active_watermarks, activated_at)
    }
}

async fn bootstrap_first_active_generation(
    conn: &impl Executor,
    session_id: &tracedecay_domain::SessionId,
    watermarks: &SessionFrozenWatermarksV1,
) -> SessionStoreResult<()> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM session_temporal_generations WHERE session_id = ?1",
            params![session_id.as_str()],
        )
        .await
        .map_err(|error| storage(BEGIN_OPERATION, error))?;
    let count: i64 = rows
        .next()
        .await
        .map_err(|error| storage(BEGIN_OPERATION, error))?
        .ok_or_else(|| storage_message(BEGIN_OPERATION, "generation count query returned no row"))?
        .get(0)
        .map_err(|error| storage(BEGIN_OPERATION, error))?;
    drop(rows);
    if count != 0 {
        return Ok(());
    }
    if watermarks.active_generation().value() != 1 {
        return Err(SessionStoreError::MissingGeneration {
            generation: watermarks.active_generation(),
        });
    }

    let recorded_at = now_micros(BEGIN_OPERATION)?;
    let frozen = encode_watermarks(watermarks, BEGIN_OPERATION)?;
    conn.execute(
        "INSERT INTO session_temporal_generations (
            session_id, generation, state, frozen_watermarks_json, created_at
         ) VALUES (?1, 1, 'building', ?2, ?3)",
        params![session_id.as_str(), frozen, recorded_at.0],
    )
    .await
    .map_err(|error| storage(BEGIN_OPERATION, error))?;
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'ready', ready_at = ?2
         WHERE session_id = ?1 AND generation = 1 AND state = 'building'",
        params![session_id.as_str(), recorded_at.0],
    )
    .await
    .map_err(|error| storage(BEGIN_OPERATION, error))?;
    conn.execute(
        "UPDATE session_temporal_generations
         SET state = 'active', activated_at = ?2
         WHERE session_id = ?1 AND generation = 1 AND state = 'ready'",
        params![session_id.as_str(), recorded_at.0],
    )
    .await
    .map_err(|error| storage(BEGIN_OPERATION, error))?;
    Ok(())
}

pub(super) async fn validate_candidate_frontier(
    conn: &impl QueryExecutor,
    session_id: &str,
    generation: i64,
    source_frontier: u64,
) -> SessionStoreResult<()> {
    let mut expected = BTreeSet::new();
    let mut expected_copies = BTreeSet::new();
    let parent_resolver =
        canonical_parent_message_resolver(conn, session_id, source_frontier, ACTIVATE_OPERATION)
            .await?;
    let mut canonical_outputs = Vec::new();
    let mut rows = conn
        .query(
            "SELECT observation_json
             FROM observations
             WHERE sequence <= ?1
             ORDER BY sequence, observation_id",
            params![frontier_i64(source_frontier, ACTIVATE_OPERATION)?],
        )
        .await
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?
    {
        let encoded: String = row
            .get(0)
            .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
        let observation: DurableObservationV1 =
            serde_json::from_str(&encoded).map_err(|error| storage(ACTIVATE_OPERATION, error))?;
        let envelope = serde_json::from_value::<tracedecay_domain::CanonicalObservationEnvelopeV1>(
            observation.payload().clone(),
        )
        .ok();
        let projection =
            derive_projection(&observation).map_err(|error| storage(ACTIVATE_OPERATION, error))?;
        for output in projection
            .messages()
            .filter(|output| output.session().session_id == session_id)
        {
            let occurrence_id = MessageOccurrenceIdV1::derive(
                observation.observation_id(),
                ProjectionOutputOrdinalV1::new(output.output_ordinal()),
            )
            .as_str()
            .to_owned();
            // Mirrors `derive_retained_projection_relations`: only a re-emission
            // of the same logical message is a derived logical copy. A parent
            // link to a *different* message is conversation threading and must
            // not be demanded as copy coverage.
            let parent_message_id = envelope
                .as_ref()
                .and_then(|value| value.relations().parent_message_id())
                .filter(|parent| parent.as_str() == output.message().message_id)
                .map(|value| value.as_str().to_owned());
            canonical_outputs.push((occurrence_id, parent_message_id));
        }
    }
    for (occurrence_id, parent_message_id) in canonical_outputs {
        if let Some(parent_message_id) = parent_message_id
            && let Some(parent_occurrence_id) = parent_resolver.resolve(&parent_message_id)
        {
            expected_copies.insert((occurrence_id.clone(), parent_occurrence_id.to_owned()));
        }
        expected.insert(occurrence_id);
    }
    if expected.is_empty() && source_frontier != 0 {
        return Err(storage_message(
            ACTIVATE_OPERATION,
            "candidate generation has no canonical message outputs at its frozen frontier",
        ));
    }

    let mut actual = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT occurrence_id
             FROM session_occurrences
             WHERE session_id = ?1 AND generation = ?2
             ORDER BY occurrence_id",
            params![session_id, generation],
        )
        .await
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?
    {
        actual.insert(
            row.get::<String>(0)
                .map_err(|error| storage(ACTIVATE_OPERATION, error))?,
        );
    }
    if actual != expected {
        return Err(storage_message(
            ACTIVATE_OPERATION,
            "candidate occurrence coverage does not equal the frozen source frontier",
        ));
    }
    let mut actual_copies = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT occurrence_id, copied_from_occurrence_id
             FROM session_logical_copy_edges
             WHERE session_id = ?1 AND generation = ?2
             ORDER BY occurrence_id, copied_from_occurrence_id",
            params![session_id, generation],
        )
        .await
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?
    {
        actual_copies.insert((
            row.get::<String>(0)
                .map_err(|error| storage(ACTIVATE_OPERATION, error))?,
            row.get::<String>(1)
                .map_err(|error| storage(ACTIVATE_OPERATION, error))?,
        ));
    }
    // Parent-message copies are mandatory canonical coverage. Additional copy
    // edges are allowed only because batch persistence already validated their
    // typed retained-evidence proof and the final immutable receipt re-hashed
    // the complete edge set before activation.
    if !expected_copies.is_subset(&actual_copies) {
        return Err(storage_message(
            ACTIVATE_OPERATION,
            "candidate copy coverage omits canonical parent-message relations",
        ));
    }
    Ok(())
}
