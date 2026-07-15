use libsql::params;
use tracedecay_domain::CanonicalObservationIdV1;
use tracedecay_store::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ClaudeObservationProjection, ProjectionCheckpoint,
    ProjectionPersistOutcome, ProjectionRebuildOutcome, ProjectionStoreError,
    ProjectionStoreResult,
};

use super::super::GlobalDb;
use super::apply::{apply_effect, derive_projection_with_alias, verify_effect};
use super::state::{
    consume_projection_queue_item, decode_observation_row, decode_sequence,
    ensure_projection_output_state_cache, queued_sequence, read_checkpoint, read_observation,
    storage, storage_message, write_checkpoint,
};

const REBUILD_PAGE_SIZE: i64 = 128;

impl GlobalDb {
    pub(crate) async fn next_queued_observation_result(
        &self,
    ) -> ProjectionStoreResult<Option<CanonicalObservationIdV1>> {
        let mut rows = self
            .conn
            .query(
                "SELECT observation_id FROM projection_queue
                 ORDER BY observation_sequence ASC LIMIT 1",
                (),
            )
            .await
            .map_err(|error| storage("read next projection queue item", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage("read next projection queue item", error))?
        else {
            return Ok(None);
        };
        let observation_id = row
            .get::<String>(0)
            .map_err(|error| storage("read next projection queue item", error))?;
        CanonicalObservationIdV1::new(observation_id)
            .map(Some)
            .map_err(ProjectionStoreError::Contract)
    }

    pub(crate) async fn project_observation_result(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ProjectionStoreResult<ProjectionPersistOutcome> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage("begin projection transaction", error))?;
        ensure_projection_output_state_cache(&transaction).await?;
        let checkpoint = read_checkpoint(&transaction).await?;
        let Some((sequence, observation)) = read_observation(&transaction, observation_id).await?
        else {
            return Err(ProjectionStoreError::ObservationNotFound);
        };
        let effect = derive_projection_with_alias(&transaction, &observation).await?;
        if sequence <= checkpoint.last_sequence() {
            verify_effect(&transaction, &observation, &effect).await?;
            consume_projection_queue_item(&transaction, observation_id).await?;
            transaction
                .commit()
                .await
                .map_err(|error| storage("commit projection transaction", error))?;
            return Ok(ProjectionPersistOutcome::ExactDuplicate(checkpoint));
        }
        let expected = checkpoint.last_sequence().saturating_add(1);
        if sequence != expected {
            return Err(ProjectionStoreError::Gap {
                expected,
                actual: sequence,
            });
        }
        if queued_sequence(&transaction, observation_id).await? != Some(sequence) {
            return Err(ProjectionStoreError::NotQueued);
        }

        apply_effect(&transaction, sequence, &observation, &effect).await?;
        consume_projection_queue_item(&transaction, observation_id).await?;
        let checkpoint = write_checkpoint(&transaction, sequence).await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit projection transaction", error))?;
        Ok(match effect {
            ClaudeObservationProjection::Message(_) => {
                ProjectionPersistOutcome::Projected(checkpoint)
            }
            ClaudeObservationProjection::Skipped(reason) => {
                ProjectionPersistOutcome::Skipped { checkpoint, reason }
            }
        })
    }

    pub(crate) async fn projection_checkpoint_result(
        &self,
    ) -> ProjectionStoreResult<ProjectionCheckpoint> {
        read_checkpoint(&self.conn).await
    }

    pub(crate) async fn rebuild_projection_result(
        &self,
        frontier_sequence: u64,
    ) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| storage("begin projection rebuild", error))?;
        ensure_projection_output_state_cache(&transaction).await?;
        let mut max_rows = transaction
            .query("SELECT COALESCE(MAX(sequence), 0) FROM observations", ())
            .await
            .map_err(|error| storage("read projection rebuild frontier", error))?;
        let committed = max_rows
            .next()
            .await
            .map_err(|error| storage("read projection rebuild frontier", error))?
            .ok_or_else(|| storage_message("read projection rebuild frontier", "no row"))?
            .get::<i64>(0)
            .map_err(|error| storage("read projection rebuild frontier", error))?;
        drop(max_rows);
        let committed = decode_sequence(committed, "read projection rebuild frontier")?;
        if frontier_sequence > committed {
            return Err(ProjectionStoreError::InvalidRebuildFrontier {
                frontier: frontier_sequence,
                committed,
            });
        }
        let frontier_i64 = i64::try_from(frontier_sequence)
            .map_err(|_| ProjectionStoreError::SequenceOverflow(frontier_sequence))?;
        transaction
            .execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS observation_projection_rebuild_retained_outputs (
                    output_provider TEXT NOT NULL,
                    output_message_id TEXT NOT NULL,
                    PRIMARY KEY(output_provider, output_message_id)
                 ) WITHOUT ROWID;
                 DELETE FROM temp.observation_projection_rebuild_retained_outputs;",
            )
            .await
            .map_err(|error| storage("prepare retained projection outputs", error))?;
        transaction
            .execute(
                "INSERT INTO temp.observation_projection_rebuild_retained_outputs (
                    output_provider, output_message_id
                 )
                 SELECT DISTINCT current.output_provider, current.output_message_id
                 FROM observation_projection_provenance AS current
                 WHERE current.projector_version = ?1
                   AND EXISTS (
                    SELECT 1 FROM observation_projection_provenance AS retained
                    WHERE retained.output_provider = current.output_provider
                      AND retained.output_message_id = current.output_message_id
                      AND retained.projector_version <> current.projector_version
                   )",
                params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("materialize retained projection outputs", error))?;
        transaction
            .execute(
                "DELETE FROM session_messages
                 WHERE EXISTS (
                    SELECT 1 FROM observation_projection_provenance AS provenance
                    WHERE provenance.projector_version = ?1
                      AND provenance.message_created = 1
                      AND provenance.output_provider = session_messages.provider
                      AND provenance.output_message_id = session_messages.message_id
                      AND NOT EXISTS (
                        SELECT 1
                        FROM temp.observation_projection_rebuild_retained_outputs AS retained
                        WHERE retained.output_provider = provenance.output_provider
                          AND retained.output_message_id = provenance.output_message_id
                      )
                 )",
                params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("clear projection rows for rebuild", error))?;
        transaction
            .execute(
                "DELETE FROM observation_projection_provenance
                 WHERE projector_version = ?1
                   AND NOT EXISTS (
                    SELECT 1
                    FROM temp.observation_projection_rebuild_retained_outputs AS retained
                    WHERE retained.output_provider = observation_projection_provenance.output_provider
                      AND retained.output_message_id = observation_projection_provenance.output_message_id
                   )",
                params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("clear projection provenance for rebuild", error))?;
        transaction
            .execute(
                "DELETE FROM temp.observation_projection_output_state_meta",
                (),
            )
            .await
            .map_err(|error| storage("invalidate projection output state for rebuild", error))?;
        ensure_projection_output_state_cache(&transaction).await?;
        transaction
            .execute(
                "DELETE FROM observation_projection_dispositions WHERE projector_version = ?1",
                params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("clear projection dispositions for rebuild", error))?;
        transaction
            .execute(
                "DELETE FROM observation_projection_checkpoints WHERE projector_version = ?1",
                params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("clear projection checkpoint for rebuild", error))?;

        let mut last_sequence = 0_i64;
        let mut projected_rows = 0_usize;
        let mut skipped_observations = 0_usize;
        loop {
            let mut rows = transaction
                .query(
                    "SELECT sequence, observation_json FROM observations
                     WHERE sequence > ?1 AND sequence <= ?2
                     ORDER BY sequence ASC LIMIT ?3",
                    params![last_sequence, frontier_i64, REBUILD_PAGE_SIZE],
                )
                .await
                .map_err(|error| storage("read projection rebuild page", error))?;
            let mut page = Vec::with_capacity(REBUILD_PAGE_SIZE as usize);
            while let Some(row) = rows
                .next()
                .await
                .map_err(|error| storage("read projection rebuild page", error))?
            {
                page.push(decode_observation_row(
                    &row,
                    "read projection rebuild page",
                )?);
            }
            drop(rows);
            if page.is_empty() {
                break;
            }
            for (sequence, observation) in page {
                let effect = derive_projection_with_alias(&transaction, &observation).await?;
                match effect {
                    ClaudeObservationProjection::Message(_) => projected_rows += 1,
                    ClaudeObservationProjection::Skipped(_) => skipped_observations += 1,
                }
                apply_effect(&transaction, sequence, &observation, &effect).await?;
                last_sequence = i64::try_from(sequence)
                    .map_err(|_| ProjectionStoreError::SequenceOverflow(sequence))?;
            }
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO projection_queue (observation_id, observation_sequence)
                 SELECT observation_id, sequence FROM observations WHERE sequence > ?1",
                params![frontier_i64],
            )
            .await
            .map_err(|error| storage("requeue observations past rebuild frontier", error))?;
        transaction
            .execute(
                "DELETE FROM projection_queue WHERE observation_sequence <= ?1",
                params![frontier_i64],
            )
            .await
            .map_err(|error| storage("consume rebuilt projection queue", error))?;
        let checkpoint = write_checkpoint(&transaction, frontier_sequence).await?;
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit projection rebuild", error))?;
        Ok(ProjectionRebuildOutcome::new(
            checkpoint,
            projected_rows,
            skipped_observations,
        ))
    }
}
