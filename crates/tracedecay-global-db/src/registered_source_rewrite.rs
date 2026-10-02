//! Retires the records a rewritten file-byte source no longer offers.
//!
//! Admission records where each record sits in the newest layout that offered
//! it. A rewrite keeps the byte prefix it retained and re-offers the rest under
//! a new generation; once that generation has been read to the end of the
//! file, every record it did not offer again is retired from projection.

use tracedecay_domain::{
    CanonicalObservationIdV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1,
};
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};
use tracedecay_store::{ProjectionStoreError, ProjectionStoreResult};

use super::RegisteredGlobalDb;
use super::observation_projection::{ensure_projection_output_state_cache, retire_source_record};

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
    ProjectionStoreError::Storage {
        operation,
        source: Box::new(error),
    }
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
        for record in records {
            transaction
                .execute(
                    "INSERT INTO observation_source_presence (
                        source_key, observation_id, generation, start_offset
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(source_key, observation_id) DO UPDATE SET
                        generation = excluded.generation,
                        start_offset = excluded.start_offset",
                    params![
                        key.as_str(),
                        record.observation_id.as_str(),
                        record.generation.generation_id().to_string(),
                        offset(record.start_offset)?,
                    ],
                )
                .await
                .map_err(|error| storage("record source presence", error))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit source presence", error))
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
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage("begin source rewrite transaction", error))?;
        // A rewrite the previous generation never finished still names its
        // own layout: its retained prefix was relabeled to it.
        let unfinished = pending_rewrite(&transaction, &key).await?;
        transaction
            .execute(
                "UPDATE observation_source_presence SET generation = ?2
                 WHERE source_key = ?1 AND start_offset < ?3
                   AND generation IN (?4, ?5)",
                params![
                    key.as_str(),
                    generation.as_str(),
                    offset(retained_through)?,
                    previous.generation_id().to_string(),
                    unfinished,
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
                    retained_through = excluded.retained_through",
                params![key.as_str(), generation.as_str(), offset(retained_through)?],
            )
            .await
            .map_err(|error| storage("record source rewrite", error))?;
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit source rewrite", error))
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
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage("begin source rewrite completion", error))?;
        if pending_rewrite(&transaction, &key).await?.as_deref() != Some(generation.as_str()) {
            transaction
                .rollback()
                .await
                .map_err(|error| storage("release source rewrite completion", error))?;
            return Ok(0);
        }
        let mut rows = transaction
            .query(
                "SELECT observation_id FROM observation_source_presence
                 WHERE source_key = ?1 AND generation <> ?2",
                params![key.as_str(), generation.as_str()],
            )
            .await
            .map_err(|error| storage("read unoffered source records", error))?;
        let mut unoffered = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("read unoffered source records", error))?
        {
            unoffered.push(
                row.get::<String>(0)
                    .map_err(|error| storage("read unoffered source records", error))?,
            );
        }
        drop(rows);
        ensure_projection_output_state_cache(&transaction).await?;
        let mut retired = 0_u64;
        for observation_id in &unoffered {
            if retire_source_record(&transaction, observation_id).await? {
                retired += 1;
            }
        }
        for sql in [
            "DELETE FROM observation_source_presence WHERE source_key = ?1 AND generation <> ?2",
            "DELETE FROM observation_source_rewrites WHERE source_key = ?1 AND generation = ?2",
        ] {
            transaction
                .execute(sql, params![key.as_str(), generation.as_str()])
                .await
                .map_err(|error| storage("settle source rewrite", error))?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit source rewrite completion", error))?;
        Ok(retired)
    }
}
