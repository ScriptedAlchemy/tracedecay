//! Retires the records a rewritten file-byte source no longer offers.
//!
//! Admission records where each record sits in the newest layout that offered
//! it. A rewrite keeps the byte prefix it retained and re-offers the rest under
//! a new generation; once that generation has been read to the end of the
//! file, every record it did not offer again is retired from projection. A
//! later layout's accepted offer queues the retired record for projection.

use std::future::Future;
use std::pin::Pin;

use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1,
};
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_store::{
    ProjectionStoreError, ProjectionStoreResult, SESSION_MESSAGE_PROJECTOR_VERSION,
};

use super::RegisteredGlobalDb;
use super::observation_projection::retire_source_record;
use super::registered::JobExit;

/// Each committed page settles at most this many source-presence records,
/// releasing the writer lease and making restart continuation durable.
const UNOFFERED_RECORD_PAGE_SIZE: i64 = 128;

/// One page is small enough for the SQLite JSON virtual table and writer
/// deadline. The entire supplied presence batch still commits atomically.
const SOURCE_PRESENCE_PAGE_ROWS: usize = 128;

/// One record a source layout offered, under the identity admission derived.
pub struct ObservationSourcePresenceV1 {
    pub observation_id: CanonicalObservationIdV1,
    pub generation: ObservationSourceGenerationV1,
    pub start_offset: u64,
}

fn storage(
    operation: &'static str,
    error: impl std::error::Error + Send + Sync + 'static,
) -> ProjectionStoreError {
    ProjectionStoreError::storage(operation, error)
}

fn source_key(
    source: &ObservationSourceIdentityV1,
    scope: &ObservationScopeV1,
) -> ProjectionStoreResult<String> {
    serde_json::to_string(&(source, scope)).map_err(|error| storage("encode source key", error))
}

fn offset(value: u64) -> ProjectionStoreResult<i64> {
    i64::try_from(value).map_err(|_| ProjectionStoreError::SequenceOverflow(value))
}

async fn pending_rewrite(
    conn: &impl QueryExecutor,
    key: &str,
) -> ProjectionStoreResult<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT generation FROM observation_source_rewrites WHERE source_key = ?1",
            params![key],
        )
        .await
        .map_err(|error| storage("read pending source rewrite", error))?;
    rows.next()
        .await
        .map_err(|error| storage("read pending source rewrite", error))?
        .map(|row| {
            row.get::<String>(0)
                .map_err(|error| storage("read pending source rewrite", error))
        })
        .transpose()
}

impl RegisteredGlobalDb {
    /// Records that the source's current layout offers `records`.
    #[tracing::instrument(
        name = "global_db.registered.source_presence.persist",
        level = "trace",
        skip_all
    )]
    pub async fn record_observation_source_presence(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
        records: &[ObservationSourcePresenceV1],
    ) -> ProjectionStoreResult<()> {
        if records.is_empty() {
            return Ok(());
        }
        let key = source_key(source, scope)?;
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage("begin source presence transaction", error))?;
        let recorded = async {
            for chunk in records.chunks(SOURCE_PRESENCE_PAGE_ROWS) {
                let page = chunk
                    .iter()
                    .map(|record| {
                        Ok((
                            record.observation_id.as_str(),
                            record.generation.generation_id().to_string(),
                            offset(record.start_offset)?,
                        ))
                    })
                    .collect::<ProjectionStoreResult<Vec<_>>>()?;
                let encoded = serde_json::to_string(&page)
                    .map_err(|error| storage("encode source presence page", error))?;
                transaction
                    .execute(
                        "INSERT INTO observation_source_presence (
                            source_key, observation_id, generation, start_offset
                         )
                         SELECT ?1, json_extract(value, '$[0]'),
                                json_extract(value, '$[1]'), json_extract(value, '$[2]')
                         FROM json_each(?2) AS input
                         WHERE true ORDER BY CAST(input.key AS INTEGER)
                         ON CONFLICT(source_key, observation_id) DO UPDATE SET
                            generation = excluded.generation,
                            start_offset = excluded.start_offset
                         WHERE observation_source_presence.generation IS NOT excluded.generation
                            OR observation_source_presence.start_offset IS NOT excluded.start_offset",
                        params![key.as_str(), encoded.as_str()],
                    )
                    .await
                    .map_err(|error| storage("record source presence", error))?;
            }
            Ok(())
        }
        .await;
        match recorded {
            Ok(()) => transaction
                .commit()
                .await
                .map_err(|error| storage("commit source presence transaction", error)),
            Err(error) => {
                transaction.rollback().await.map_err(|rollback| {
                    storage("roll back source presence transaction", rollback)
                })?;
                Err(error)
            }
        }
    }

    /// Starts a rewrite of the source from `previous` to `generation`. The
    /// bytes before `retained_through` are the previous layout's, so the
    /// records there stay offered; every other record must be offered again
    /// before the rewrite completes.
    #[tracing::instrument(
        name = "global_db.registered.source_rewrite.begin",
        level = "trace",
        skip_all
    )]
    pub async fn begin_observation_source_rewrite(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
        previous: ObservationSourceGenerationV1,
        generation: ObservationSourceGenerationV1,
        retained_through: u64,
    ) -> ProjectionStoreResult<()> {
        if previous == generation {
            return Ok(());
        }
        let key = source_key(source, scope)?;
        let generation = generation.generation_id().to_string();
        let retained_through = offset(retained_through)?;
        let previous = previous.generation_id().to_string();
        self.transact_or(
            |error| storage("run source rewrite transaction", error),
            async move |transaction| {
                // A rewrite the previous generation never finished still names its
                // own layout: its retained prefix was relabeled to it.
                let unfinished = pending_rewrite(&transaction, &key).await?;
                transaction
                    .execute(
                        "UPDATE projection_queue SET revival_generation = ?2
                         FROM observation_source_presence AS presence
                         WHERE presence.source_key = ?1
                           AND presence.observation_id = projection_queue.observation_id
                           AND presence.start_offset < ?3
                           AND presence.generation IN (?4, ?5)
                           AND presence.generation = projection_queue.revival_generation
                           AND presence.start_offset = projection_queue.revival_start_offset
                           AND presence.generation <> ?2",
                        params![
                            key.as_str(),
                            generation.as_str(),
                            retained_through,
                            previous.as_str(),
                            unfinished.as_deref(),
                        ],
                    )
                    .await
                    .map_err(|error| storage("retain accepted source offer", error))?;
                transaction
                    .execute(
                        "UPDATE observation_source_presence SET generation = ?2
                         WHERE source_key = ?1 AND start_offset < ?3
                           AND generation IN (?4, ?5)
                           AND generation <> ?2",
                        params![
                            key.as_str(),
                            generation.as_str(),
                            retained_through,
                            previous.as_str(),
                            unfinished.as_deref(),
                        ],
                    )
                    .await
                    .map_err(|error| storage("retain source prefix", error))?;
                transaction
                    .execute(
                        "INSERT INTO observation_source_rewrites (
                            source_key, generation, retained_through
                         ) VALUES (?1, ?2, ?3)
                         ON CONFLICT(source_key) DO UPDATE SET
                            generation = excluded.generation,
                            retained_through = excluded.retained_through
                         WHERE observation_source_rewrites.generation IS NOT excluded.generation
                            OR observation_source_rewrites.retained_through IS NOT excluded.retained_through",
                        params![key.as_str(), generation.as_str(), retained_through],
                    )
                    .await
                    .map_err(|error| storage("record source rewrite", error))?;
                Ok(())
            },
        )
        .await
    }

    /// Completes a pending rewrite once `generation` was read to the end of
    /// the source, retiring every record it did not offer. Returns how many
    /// observations were retired.
    #[tracing::instrument(
        name = "global_db.registered.source_rewrite.complete",
        level = "trace",
        skip_all
    )]
    pub async fn complete_observation_source_rewrite(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
        generation: ObservationSourceGenerationV1,
    ) -> ProjectionStoreResult<u64> {
        let key = source_key(source, scope)?;
        let generation = generation.generation_id().to_string();
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| storage("read pending source rewrite", error))?;
        if pending_rewrite(&snapshot, &key).await?.as_deref() != Some(generation.as_str()) {
            return Ok(0);
        }
        drop(snapshot);
        let mut total_retired = 0_u64;
        // Durably deleted pages survive restart; within this invocation seek
        // past the already-visited current-generation survivor prefix.
        let mut resume_from = String::new();
        loop {
            // The remaining older-generation presence entries are the durable
            // continuation frontier. Each page commits its retirements and
            // deletes only the entries it has finished processing; a failed
            // page rolls back without repeating earlier committed pages.
            let key = key.clone();
            let generation = generation.clone();
            let after_id = resume_from.clone();
            let (retired, finished, next_cursor) = self
                .transact_or_rollback(
                    |error| storage("run source rewrite completion page", error),
                    async move |transaction| {
                        if pending_rewrite(&transaction, &key).await?.as_deref()
                            != Some(generation.as_str())
                        {
                            return Err(JobExit::Rollback((0_u64, true, String::new())));
                        }
                        let mut rows = transaction
                            .query(
                                "SELECT presence.observation_id,
                                        (
                                            (
                                                EXISTS (
                                                    SELECT 1 FROM observations AS observation
                                                    WHERE observation.observation_id = presence.observation_id
                                                )
                                                AND NOT EXISTS (
                                                    SELECT 1 FROM observation_projection_dispositions AS disposition
                                                    WHERE disposition.projector_version = ?4
                                                      AND disposition.observation_id = presence.observation_id
                                                )
                                            )
                                            OR EXISTS (
                                                SELECT 1 FROM projection_queue AS queued
                                                WHERE queued.observation_id = presence.observation_id
                                                  AND (queued.revival_generation IS NOT NULL
                                                       OR queued.revival_start_offset IS NOT NULL)
                                            )
                                        )
                                 FROM observation_source_presence AS presence
                                 WHERE presence.source_key = ?1 AND presence.generation <> ?2
                                   AND presence.observation_id >= ?5
                                 ORDER BY presence.observation_id
                                 LIMIT ?3",
                                params![
                                    key.as_str(),
                                    generation.as_str(),
                                    UNOFFERED_RECORD_PAGE_SIZE,
                                    SESSION_MESSAGE_PROJECTOR_VERSION,
                                    after_id.as_str(),
                                ],
                            )
                            .await
                            .map_err(|error| storage("read unoffered source records", error))?;
                        let mut page = Vec::new();
                        while let Some(row) = rows
                            .next()
                            .await
                            .map_err(|error| storage("read unoffered source records", error))?
                        {
                            page.push((
                                row.get::<String>(0).map_err(|error| {
                                    storage("read unoffered source records", error)
                                })?,
                                row.get::<i64>(1).map_err(|error| {
                                    storage("read unoffered source records", error)
                                })? != 0,
                            ));
                        }
                        drop(rows);
                        let mut retired = 0_u64;
                        for (observation_id, needs_retirement) in &page {
                            if !needs_retirement {
                                continue;
                            }
                            let retired_one: Pin<
                                Box<dyn Future<Output = ProjectionStoreResult<bool>> + Send + '_>,
                            > = Box::pin(retire_source_record(&transaction, observation_id));
                            if retired_one.await? {
                                retired += 1;
                            }
                        }
                        if let Some((last_id, _)) = page.last() {
                            transaction
                                .execute(
                                    "DELETE FROM observation_source_presence
                                     WHERE source_key = ?1 AND generation <> ?2
                                       AND observation_id >= ?4 AND observation_id <= ?3",
                                    params![
                                        key.as_str(),
                                        generation.as_str(),
                                        last_id.as_str(),
                                        after_id.as_str(),
                                    ],
                                )
                                .await
                                .map_err(|error| storage("settle source rewrite page", error))?;
                        }
                        // Another admitted writer can append an older-generation
                        // key behind this in-memory seek between commits. Recheck
                        // the entire source before dropping the durable marker.
                        let last_page = (page.len() as i64) < UNOFFERED_RECORD_PAGE_SIZE;
                        let finished = if last_page {
                            // With no seek this page covered the entire source
                            // under the writer lock, so no second scan is needed.
                            // A resumed page must check keys admitted behind it.
                            let empty = if after_id.is_empty() {
                                true
                            } else {
                                let mut remaining = transaction
                                    .query(
                                        "SELECT 1 FROM observation_source_presence
                                         WHERE source_key = ?1 AND generation <> ?2 LIMIT 1",
                                        params![key.as_str(), generation.as_str()],
                                    )
                                    .await
                                    .map_err(|error| storage("check remaining source records", error))?;
                                let empty = remaining
                                    .next()
                                    .await
                                    .map_err(|error| storage("check remaining source records", error))?
                                    .is_none();
                                drop(remaining);
                                empty
                            };
                            if empty {
                                transaction
                                    .execute(
                                        "DELETE FROM observation_source_rewrites
                                         WHERE source_key = ?1 AND generation = ?2",
                                        params![key.as_str(), generation.as_str()],
                                    )
                                    .await
                                    .map_err(|error| storage("settle source rewrite", error))?;
                            }
                            empty
                        } else {
                            false
                        };
                        let next_cursor = if last_page {
                            String::new()
                        } else {
                            page.last().map(|(id, _)| id.clone()).unwrap_or_default()
                        };
                        Ok((retired, finished, next_cursor))
                    },
                )
                .await?;
            total_retired += retired;
            if finished {
                return Ok(total_retired);
            }
            resume_from = next_cursor;
        }
    }
}
