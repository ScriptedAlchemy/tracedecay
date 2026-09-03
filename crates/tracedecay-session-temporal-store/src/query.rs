use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use tracedecay_domain::{
    CanonicalObservationIdV1, DurableObservationV1, SessionId, SessionProjectionGenerationV1,
    UtcMicros,
};
use tracedecay_runtime_core::db::engine::{Row, params, params_from_iter};
use tracedecay_store::{SessionFrozenWatermarksV1, SessionStoreError, SessionStoreResult};

pub(super) const BEGIN_OPERATION: &str = "begin session temporal generation";
pub(super) const PERSIST_OPERATION: &str = "persist session temporal projection batch";
pub(super) const ACTIVATE_OPERATION: &str = "activate session temporal generation";

#[derive(Clone, Debug)]
pub(super) struct GenerationRow {
    pub state: String,
    pub frozen_watermarks_json: String,
}

pub(super) fn storage(
    operation: &'static str,
    source: impl Error + Send + Sync + 'static,
) -> SessionStoreError {
    SessionStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

pub(super) fn storage_message(
    operation: &'static str,
    message: impl Into<String>,
) -> SessionStoreError {
    storage(operation, io::Error::other(message.into()))
}

pub(super) fn now_micros(operation: &'static str) -> SessionStoreResult<UtcMicros> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| storage(operation, error))?;
    let micros = i64::try_from(duration.as_micros()).map_err(|error| storage(operation, error))?;
    Ok(UtcMicros(micros))
}

pub(super) fn generation_i64(
    generation: SessionProjectionGenerationV1,
    operation: &'static str,
) -> SessionStoreResult<i64> {
    i64::try_from(generation.value()).map_err(|error| storage(operation, error))
}

pub(super) fn frontier_i64(frontier: u64, operation: &'static str) -> SessionStoreResult<i64> {
    i64::try_from(frontier).map_err(|error| storage(operation, error))
}

pub(super) fn encode_watermarks(
    watermarks: &SessionFrozenWatermarksV1,
    operation: &'static str,
) -> SessionStoreResult<String> {
    let cursor_key = watermarks.cursor_key().map(|key| {
        json!({
            "key_id": key.key_id.as_str(),
            "version": key.version.value(),
        })
    });
    serde_json::to_string(&json!({
        "active_generation": watermarks.active_generation().value(),
        "cursor_key": cursor_key,
        "projection_frontier": watermarks.projection_frontier(),
        "source_frontier": watermarks.source_frontier(),
        "summary_frontier": watermarks.summary_frontier(),
    }))
    .map_err(|error| storage(operation, error))
}

pub(super) async fn read_generation(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    operation: &'static str,
) -> SessionStoreResult<Option<GenerationRow>> {
    let mut rows = conn
        .query(
            "SELECT state, frozen_watermarks_json
             FROM session_temporal_generations
             WHERE session_id = ?1 AND generation = ?2",
            params![session_id.as_str(), generation_i64(generation, operation)?],
        )
        .await
        .map_err(|error| storage(operation, error))?;
    rows.next()
        .await
        .map_err(|error| storage(operation, error))?
        .map(|row| decode_generation(&row, operation))
        .transpose()
}

fn decode_generation(row: &Row, operation: &'static str) -> SessionStoreResult<GenerationRow> {
    Ok(GenerationRow {
        state: row.get(0).map_err(|error| storage(operation, error))?,
        frozen_watermarks_json: row.get(1).map_err(|error| storage(operation, error))?,
    })
}

pub(super) async fn read_active_generation(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    operation: &'static str,
) -> SessionStoreResult<Option<SessionProjectionGenerationV1>> {
    let mut rows = conn
        .query(
            "SELECT generation
             FROM session_temporal_generations
             WHERE session_id = ?1 AND state = 'active'",
            params![session_id.as_str()],
        )
        .await
        .map_err(|error| storage(operation, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(operation, error))?
    else {
        return Ok(None);
    };
    let value: i64 = row.get(0).map_err(|error| storage(operation, error))?;
    let value = u64::try_from(value).map_err(|error| storage(operation, error))?;
    SessionProjectionGenerationV1::new(value)
        .map(Some)
        .map_err(SessionStoreError::from)
}

pub(super) async fn require_active_generation(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    expected: SessionProjectionGenerationV1,
    operation: &'static str,
) -> SessionStoreResult<Option<SessionProjectionGenerationV1>> {
    let actual = read_active_generation(conn, session_id, operation).await?;
    match actual {
        Some(actual) if actual == expected => Ok(Some(actual)),
        Some(actual) => Err(SessionStoreError::StaleGeneration { expected, actual }),
        None => Err(SessionStoreError::MissingGeneration {
            generation: expected,
        }),
    }
}

pub(super) async fn read_observation(
    conn: &impl crate::handle::SessionTemporalQuery,
    observation_id: &CanonicalObservationIdV1,
) -> SessionStoreResult<(u64, DurableObservationV1)> {
    let mut rows = conn
        .query(
            "SELECT sequence, observation_json
             FROM observations
             WHERE observation_id = ?1",
            params![observation_id.as_str()],
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
                format!("source observation {} is missing", observation_id.as_str()),
            )
        })?;
    let sequence: i64 = row
        .get(0)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let sequence = u64::try_from(sequence).map_err(|error| storage(PERSIST_OPERATION, error))?;
    let encoded: String = row
        .get(1)
        .map_err(|error| storage(PERSIST_OPERATION, error))?;
    let observation =
        serde_json::from_str(&encoded).map_err(|error| storage(PERSIST_OPERATION, error))?;
    Ok((sequence, observation))
}

/// The largest `observation_id IN (...)` batch one observation prefetch binds,
/// kept clear of `SQLite`'s default variable ceiling.
const OBSERVATION_READ_BATCH: usize = 500;

/// Prefetches the observations a projection pass is about to decode.
///
/// `observations.observation_id` is `UNIQUE`, so each id matches at most one row
/// and the map holds exactly what the equivalent per-id `read_observation` calls
/// would have returned. Ids that are absent are simply missing from the map:
/// reporting a missing observation stays with the caller so it still surfaces in
/// the caller's own iteration order, with the same message.
pub(super) async fn read_observations(
    conn: &impl crate::handle::SessionTemporalQuery,
    observation_ids: &[CanonicalObservationIdV1],
) -> SessionStoreResult<HashMap<String, (u64, DurableObservationV1)>> {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for observation_id in observation_ids {
        if seen.insert(observation_id.as_str()) {
            unique.push(observation_id.as_str());
        }
    }
    let mut observations = HashMap::with_capacity(unique.len());
    if unique.is_empty() {
        return Ok(observations);
    }
    for chunk in unique.chunks(OBSERVATION_READ_BATCH) {
        let placeholders = (1..=chunk.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT observation_id, sequence, observation_json
             FROM observations
             WHERE observation_id IN ({placeholders})"
        );
        let mut rows = conn
            .query(&sql, params_from_iter(chunk.iter().copied()))
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
            let sequence =
                u64::try_from(sequence).map_err(|error| storage(PERSIST_OPERATION, error))?;
            let encoded: String = row
                .get(2)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            let observation = serde_json::from_str(&encoded)
                .map_err(|error| storage(PERSIST_OPERATION, error))?;
            observations.insert(observation_id, (sequence, observation));
        }
    }
    Ok(observations)
}

/// The error `read_observation` raises for an id the store does not hold, reused
/// by callers that resolve prefetched observations out of a batch map.
pub(super) fn missing_observation(observation_id: &CanonicalObservationIdV1) -> SessionStoreError {
    storage_message(
        PERSIST_OPERATION,
        format!("source observation {} is missing", observation_id.as_str()),
    )
}
