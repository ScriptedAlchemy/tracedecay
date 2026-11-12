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
use tracedecay_store::canonical_body::LOAD_CANONICAL_BODIES_SQL;
use tracedecay_store::{
    CanonicalBodyError, SessionFrozenWatermarksV1, SessionStoreError, SessionStoreResult,
    collect_body_refs, parse_stored_observation, unpack_body,
};

use crate::sql::{
    SHARED_GENERATION_TABLES, SharedGenerationTable, discard_candidate_rows_sql,
    discard_session_rows_sql, retire_superseded_versions_sql,
};

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

pub(super) fn decode_generation_i64(
    value: i64,
    operation: &'static str,
) -> SessionStoreResult<SessionProjectionGenerationV1> {
    let value = u64::try_from(value).map_err(|error| storage(operation, error))?;
    SessionProjectionGenerationV1::new(value).map_err(SessionStoreError::from)
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

/// Refuses a second open candidate: generation `G` reads every row numbered
/// `<= G`, so only one candidate may add rows above the active generation.
pub(super) async fn require_no_open_candidate(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &SessionId,
    except: Option<SessionProjectionGenerationV1>,
    operation: &'static str,
) -> SessionStoreResult<()> {
    let except = except
        .map(|generation| generation_i64(generation, operation))
        .transpose()?;
    let mut rows = conn
        .query(
            "SELECT 1 FROM session_temporal_generations
             WHERE session_id = ?1 AND state IN ('building', 'ready')
               AND (?2 IS NULL OR generation <> ?2)
             LIMIT 1",
            params![session_id.as_str(), except],
        )
        .await
        .map_err(|error| storage(operation, error))?;
    if rows
        .next()
        .await
        .map_err(|error| storage(operation, error))?
        .is_some()
    {
        return Err(SessionStoreError::IdempotencyConflict {
            context: "session generation candidate busy",
        });
    }
    Ok(())
}

/// Deletes every row a terminated candidate introduced.
pub(super) async fn discard_candidate_rows(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    operation: &'static str,
) -> SessionStoreResult<()> {
    execute_per_shared_table(
        conn,
        session_id,
        generation,
        operation,
        SHARED_GENERATION_TABLES.iter(),
        discard_candidate_rows_sql,
    )
    .await
}

/// Deletes every row the generations through `generation` introduced.
pub(super) async fn discard_session_rows(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    operation: &'static str,
) -> SessionStoreResult<()> {
    execute_per_shared_table(
        conn,
        session_id,
        generation,
        operation,
        SHARED_GENERATION_TABLES.iter(),
        discard_session_rows_sql,
    )
    .await
}

/// Deletes the row versions an activating generation superseded.
pub(super) async fn retire_superseded_versions(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    operation: &'static str,
) -> SessionStoreResult<()> {
    execute_per_shared_table(
        conn,
        session_id,
        generation,
        operation,
        SHARED_GENERATION_TABLES
            .iter()
            .filter(|table| table.versioned),
        retire_superseded_versions_sql,
    )
    .await
}

async fn execute_per_shared_table<'a>(
    conn: &impl crate::handle::SessionTemporalExec,
    session_id: &SessionId,
    generation: SessionProjectionGenerationV1,
    operation: &'static str,
    tables: impl Iterator<Item = &'a SharedGenerationTable>,
    sql: fn(&SharedGenerationTable) -> String,
) -> SessionStoreResult<()> {
    let generation = generation_i64(generation, operation)?;
    for table in tables {
        conn.execute(&sql(table), params![session_id.as_str(), generation])
            .await
            .map_err(|error| storage(operation, error))?;
    }
    Ok(())
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
    let observation = decode_stored_observation(conn, &encoded, PERSIST_OPERATION).await?;
    Ok((sequence, observation))
}

/// Hydrate one observation's distinct canonical bodies in bounded SQL pages.
/// The shared ordered query preserves missing/corrupt verdict order; pages
/// split only when their combined bodies exceed SQL materialization capacity.
pub(super) async fn decode_stored_observation(
    conn: &impl crate::handle::SessionTemporalQuery,
    encoded: &str,
    operation: &'static str,
) -> SessionStoreResult<DurableObservationV1> {
    const BODY_READ_BATCH: usize = 16;
    let hashes = collect_body_refs(encoded).map_err(|error| storage(operation, error))?;
    let mut bodies = HashMap::with_capacity(hashes.len());
    for page in hashes.chunks(BODY_READ_BATCH) {
        let mut pending = vec![(0, page.len())];
        while let Some((start, end)) = pending.pop() {
            let chunk = &page[start..end];
            let references =
                serde_json::to_string(chunk).map_err(|error| storage(operation, error))?;
            let mut rows = match conn
                .query(LOAD_CANONICAL_BODIES_SQL, params![references])
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    let error = storage(operation, error);
                    // The original single-hash path can read oversized
                    // combinations. Split only on the exact-SQL materialization
                    // ceiling, preserving typed errors for every other failure.
                    if chunk.len() > 1
                        && observation_prefetch_exceeded_materialization_limit(&error)
                    {
                        let mid = start + chunk.len() / 2;
                        pending.push((mid, end));
                        pending.push((start, mid));
                        continue;
                    }
                    return Err(error);
                }
            };
            let mut stored = HashMap::with_capacity(chunk.len());
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| storage(operation, error))?
            {
                let hash: String = row.get(0).map_err(|error| storage(operation, error))?;
                stored.insert(hash, row);
            }
            // Decode and verify in requested-hash order, independently of
            // SQLite's IN row ordering and any later row's invalid encoding.
            for hash in chunk {
                let row = stored.remove(hash).ok_or_else(|| {
                    storage(
                        operation,
                        CanonicalBodyError::Missing {
                            content_hash: hash.clone(),
                        },
                    )
                })?;
                let encoding = row
                    .get::<Option<String>>(1)
                    .map_err(|error| storage(operation, error))?
                    .ok_or_else(|| {
                        storage(
                            operation,
                            CanonicalBodyError::Missing {
                                content_hash: hash.clone(),
                            },
                        )
                    })?;
                let blob: Vec<u8> = row.get(2).map_err(|error| storage(operation, error))?;
                let uncompressed: i64 = row.get(3).map_err(|error| storage(operation, error))?;
                let unpacked = unpack_body(hash, &encoding, &blob, uncompressed)
                    .map_err(|error| storage(operation, error))?;
                bodies.insert(hash.clone(), unpacked);
            }
        }
    }
    parse_stored_observation(encoded, |hash| {
        bodies
            .get(hash)
            .cloned()
            .ok_or_else(|| CanonicalBodyError::Missing {
                content_hash: hash.to_owned(),
            })
    })
    .map_err(|error| storage(operation, error))
}

/// First prefetch width. A batch of full `observation_json` bodies is bounded
/// by the exact-SQL materialization ceiling (64 MiB), not by SQLite's variable
/// limit, so this starts small and splits when a page still does not fit.
const OBSERVATION_READ_BATCH: usize = 32;

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
    let mut pending = Vec::new();
    let mut offset = 0usize;
    while offset < unique.len() {
        let end = offset
            .saturating_add(OBSERVATION_READ_BATCH)
            .min(unique.len());
        pending.push((offset, end));
        offset = end;
    }
    while let Some((start, end)) = pending.pop() {
        if start >= end {
            continue;
        }
        let chunk = &unique[start..end];
        let placeholders = (1..=chunk.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT observation_id, sequence, observation_json
             FROM observations
             WHERE observation_id IN ({placeholders})"
        );
        let mut rows = match conn
            .query(&sql, params_from_iter(chunk.iter().copied()))
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                let error = storage(PERSIST_OPERATION, error);
                // One materialized page of bodies exceeded the exact-SQL
                // ceiling. Split until a single observation remains; that
                // observation is then a typed storage failure, not a retry
                // that looks like a busy source.
                if observation_prefetch_exceeded_materialization_limit(&error) && chunk.len() > 1 {
                    let mid = start + chunk.len() / 2;
                    pending.push((mid, end));
                    pending.push((start, mid));
                    continue;
                }
                return Err(error);
            }
        };
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
            let observation = decode_stored_observation(conn, &encoded, PERSIST_OPERATION).await?;
            observations.insert(observation_id, (sequence, observation));
        }
    }
    Ok(observations)
}

fn observation_prefetch_exceeded_materialization_limit(error: &SessionStoreError) -> bool {
    match error {
        SessionStoreError::Storage { source, .. } => source
            .to_string()
            .contains("materialization exceeded its limit"),
        _ => false,
    }
}

/// The error `read_observation` raises for an id the store does not hold, reused
/// by callers that resolve prefetched observations out of a batch map.
pub(super) fn missing_observation(observation_id: &CanonicalObservationIdV1) -> SessionStoreError {
    storage_message(
        PERSIST_OPERATION,
        format!("source observation {} is missing", observation_id.as_str()),
    )
}

#[cfg(test)]
mod tests {
    use super::{PERSIST_OPERATION, observation_prefetch_exceeded_materialization_limit, storage};
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracedecay_runtime_core::db::engine;

    struct CountedRead<'a> {
        inner: crate::sql::TemporalSqlRead<'a>,
        queries: AtomicUsize,
    }

    impl crate::handle::SessionTemporalQuery for CountedRead<'_> {
        fn query<P>(
            &self,
            sql: &str,
            params: P,
        ) -> impl Future<Output = engine::Result<engine::Rows>> + Send
        where
            P: engine::IntoParams + Send,
        {
            self.queries.fetch_add(1, Ordering::Relaxed);
            self.inner.query(sql, params)
        }
    }

    #[tokio::test]
    async fn observation_hydration_batches_distinct_bodies_and_preserves_integrity() {
        use tracedecay_domain::{
            ComponentVersion, DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
            ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
            ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadReferenceV1,
            RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
            SanitizerDispositionV1, SensitivityV1, SessionId,
        };
        use tracedecay_runtime_core::db::engine::{TestConnection, params};
        use tracedecay_store::{
            CANONICAL_BODIES_TABLE_SQL, CanonicalBodyError, UPSERT_CANONICAL_BODY_SQL,
            collect_body_refs, slim_stored_json,
        };

        let mut payload = serde_json::Map::new();
        for index in 0..18 {
            payload.insert(
                format!("part-{index:02}"),
                serde_json::json!(format!("body-{index:02}-{}", "z".repeat(4096))),
            );
        }
        let repeated = payload["part-00"].clone();
        payload.insert("repeat".to_owned(), repeated);
        let payload = serde_json::Value::Object(payload);
        let identity = ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::new(SessionId::new("session.batch").unwrap()).unwrap(),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(7).unwrap(),
            ObservationSourceRangeV1::new(12, 34).unwrap(),
            ObservationOrderingDomainV1::FileBytes,
            ObservationId::new("record.batch").unwrap(),
        )
        .unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                SanitizationReceiptId::new("receipt.batch").unwrap(),
                ComponentVersion::new("sanitizer.batch-test.v1").unwrap(),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
        )
        .unwrap();
        let original = DurableObservationV1::new(
            identity,
            receipt,
            RetentionClass::new("transcript.batch-test").unwrap(),
            payload,
        )
        .unwrap();
        let (stored, packed) =
            slim_stored_json(&serde_json::to_string(&original).unwrap()).unwrap();
        let hashes = collect_body_refs(&stored).unwrap();
        assert_eq!(packed.len(), 19, "repeat reference shares a content hash");
        assert_eq!(hashes.len(), 18);

        let directory = tempfile::TempDir::new().unwrap();
        let connection = TestConnection::open(&directory.path().join("canonical-read.db"));
        connection
            .execute_batch(CANONICAL_BODIES_TABLE_SQL)
            .await
            .unwrap();
        for body in &packed {
            connection
                .execute(
                    UPSERT_CANONICAL_BODY_SQL,
                    params![
                        body.content_hash.as_str(),
                        body.encoding,
                        body.blob.as_slice(),
                        body.uncompressed_bytes
                    ],
                )
                .await
                .unwrap();
        }
        let counted = CountedRead {
            inner: crate::sql::TemporalSqlRead::engine_connection(&connection),
            queries: AtomicUsize::new(0),
        };
        let decoded = super::decode_stored_observation(&counted, &stored, super::PERSIST_OPERATION)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert_eq!(
            counted.queries.load(Ordering::Relaxed),
            2,
            "18 distinct references need two bounded SQL reads, not 18"
        );

        let inline = serde_json::to_string(&original).unwrap();
        let decoded = super::decode_stored_observation(&counted, &inline, super::PERSIST_OPERATION)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert_eq!(
            counted.queries.load(Ordering::Relaxed),
            2,
            "inline observations need no canonical-body read"
        );

        let first = &hashes[0];
        connection
            .execute(
                "DELETE FROM session_canonical_bodies WHERE content_hash = ?1",
                params![first.as_str()],
            )
            .await
            .unwrap();
        let error = super::decode_stored_observation(&counted, &stored, super::PERSIST_OPERATION)
            .await
            .unwrap_err();
        let tracedecay_store::SessionStoreError::Storage { source, .. } = error else {
            panic!("expected missing canonical-body storage error");
        };
        assert!(matches!(
            source.downcast_ref::<CanonicalBodyError>(),
            Some(CanonicalBodyError::Missing { content_hash }) if content_hash == first
        ));

        let original_body = packed
            .iter()
            .find(|body| &body.content_hash == first)
            .unwrap();
        connection
            .execute(
                UPSERT_CANONICAL_BODY_SQL,
                params![
                    original_body.content_hash.as_str(),
                    original_body.encoding,
                    original_body.blob.as_slice(),
                    original_body.uncompressed_bytes
                ],
            )
            .await
            .unwrap();
        connection
            .execute(
                "UPDATE session_canonical_bodies SET encoding = 'identity', body = ?1 WHERE content_hash = ?2",
                params![b"corrupt".as_slice(), first.as_str()],
            )
            .await
            .unwrap();
        let error = super::decode_stored_observation(&counted, &stored, super::PERSIST_OPERATION)
            .await
            .unwrap_err();
        let tracedecay_store::SessionStoreError::Storage { source, .. } = error else {
            panic!("expected corrupted canonical-body storage error");
        };
        assert!(matches!(
            source.downcast_ref::<CanonicalBodyError>(),
            Some(CanonicalBodyError::Corrupt { content_hash, .. }) if content_hash == first
        ));
    }

    #[test]
    fn materialization_limit_is_the_prefetch_split_signal() {
        let exceeded = storage(
            PERSIST_OPERATION,
            std::io::Error::other("exact SQL query materialization exceeded its limit"),
        );
        let locked = storage(
            PERSIST_OPERATION,
            std::io::Error::other("database is locked"),
        );
        assert!(observation_prefetch_exceeded_materialization_limit(
            &exceeded
        ));
        assert!(!observation_prefetch_exceeded_materialization_limit(
            &locked
        ));
    }
}
