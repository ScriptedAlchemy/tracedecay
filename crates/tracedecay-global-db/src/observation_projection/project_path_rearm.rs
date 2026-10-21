//! Session projection used to refuse a Codex observation whose session row
//! already stored a different concrete `project_path`, parking the queue row
//! on the terminal deadline with that collision as its last error. Projection
//! now keeps the stored path, so those rows are re-armed once per store and
//! project their messages. Collisions on any other field stay parked.

use std::future::Future;
use std::pin::Pin;

use tracedecay_domain::CanonicalObservationIdV1;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::db::schema_version;
use tracedecay_runtime_core::db::{Database, DatabaseWriteTransaction};
use tracedecay_session_temporal_store::record_canonical_observation_effect;
use tracedecay_store::{
    ObservationProjection, PROJECTION_TERMINAL_RETRY_MICROS, ProjectionSkipReason,
    ProjectionStoreError, ProjectionStoreResult, SESSION_MESSAGE_PROJECTOR_VERSION,
};

use super::apply::verify_effect;
use super::rebuild::{REBUILD_PAGE_SIZE, reproject_project_path_collision};
use super::state::{query_optional_i64, read_observation, storage};
use crate::schema_contract::invariants::TEMPORAL_OBSERVATION_EFFECT_DELETE_GUARD_SQL;

const OPERATION: &str = "rearm Codex project path collisions";
/// Recorded once the rows parked on the obsolete `project_path` collision are
/// re-armed.
const CONVERGENCE: &str = "codex-project-path-collision-rearm-v1";

/// Re-arms queue rows parked terminal only because of the `project_path`
/// collision, once per store, and returns how many it re-armed.
///
/// The rowid cutoff excludes new work, which cannot emit the obsolete error.
/// Each committed window bounds both the scan and the update, including when
/// matching errors are sparse. An interrupted walk can revisit its committed
/// windows: their cleared deadlines no longer match, and the completion
/// marker is written only with the final window.
#[tracing::instrument(
    name = "global_db.observation_projection.rearm_project_path_collisions",
    level = "trace",
    skip_all
)]
pub(crate) async fn rearm_project_path_collisions(
    database: &Database,
) -> ProjectionStoreResult<u64> {
    let last_rowid = query_optional_i64(
        &database.read_connection(),
        "SELECT MAX(rowid) FROM projection_queue",
        params![],
    )
    .await?
    .unwrap_or(0);
    let mut after_rowid = 0;
    let mut rearmed = 0;
    while let Some((batch_end, rows)) =
        rearm_project_path_collision_batch(database, after_rowid, last_rowid, REBUILD_PAGE_SIZE)
            .await?
    {
        after_rowid = batch_end;
        rearmed += rows;
        asupersync::runtime::yield_now().await;
    }
    Ok(rearmed)
}

#[tracing::instrument(
    name = "global_db.observation_projection.rearm_project_path_collisions.batch",
    level = "trace",
    skip_all
)]
pub(super) async fn rearm_project_path_collision_batch(
    database: &Database,
    after_rowid: i64,
    last_rowid: i64,
    batch_rows: i64,
) -> ProjectionStoreResult<Option<(i64, u64)>> {
    let transaction = database
        .begin_write_transaction(OPERATION)
        .await
        .map_err(|error| storage(OPERATION, error))?;
    let converged = schema_version::read_version(&transaction, CONVERGENCE)
        .await
        .map_err(|error| storage(OPERATION, error))?
        .is_some();
    if converged {
        transaction
            .commit()
            .await
            .map_err(|error| storage(OPERATION, error))?;
        return Ok(None);
    }
    let batch_end = query_optional_i64(
        &transaction,
        "SELECT MAX(rowid) FROM (
            SELECT rowid FROM projection_queue
            WHERE rowid > ?1 AND rowid <= ?2
            ORDER BY rowid LIMIT ?3
         )",
        params![after_rowid, last_rowid, batch_rows],
    )
    .await?;
    let Some(batch_end) = batch_end else {
        schema_version::record_version(&transaction, CONVERGENCE, 1)
            .await
            .map_err(|error| storage(OPERATION, error))?;
        transaction
            .commit()
            .await
            .map_err(|error| storage(OPERATION, error))?;
        return Ok(None);
    };
    let replay = completed_project_path_collisions(&transaction, after_rowid, batch_end).await?;
    let rows = transaction
        .execute(
            "UPDATE projection_queue SET next_retry_at_micros = 0
             WHERE rowid > ?1 AND rowid <= ?2
               AND next_retry_at_micros = ?3
               AND last_error LIKE 'projection session output collided at codex/%: project_path differs%'",
            params![after_rowid, batch_end, PROJECTION_TERMINAL_RETRY_MICROS],
        )
        .await
        .map_err(|error| storage(OPERATION, error))?;
    if !replay.is_empty() {
        // beta.88 recorded an OutputCollision disposition, advanced the
        // checkpoint, and wrote an empty temporal effect when it parked the
        // row. Clearing the deadline alone would acknowledge that old skip as
        // an ExactDuplicate. Replace only those verified empty derived
        // effects; observations and sanitization receipts remain immutable.
        // The guard is restored before the same transaction commits, and a
        // failed repair rolls back the guard and every row together.
        transaction
            .execute_batch(
                "DROP TRIGGER IF EXISTS session_temporal_observation_effects_immutable_delete_v1",
            )
            .await
            .map_err(|error| storage(OPERATION, error))?;
        for observation_id in replay {
            let repaired: Pin<Box<dyn Future<Output = ProjectionStoreResult<()>> + Send + '_>> =
                Box::pin(repair_completed_collision(&transaction, &observation_id));
            repaired.await?;
        }
        transaction
            .execute_batch(TEMPORAL_OBSERVATION_EFFECT_DELETE_GUARD_SQL)
            .await
            .map_err(|error| storage(OPERATION, error))?;
    }
    if batch_end == last_rowid {
        schema_version::record_version(&transaction, CONVERGENCE, 1)
            .await
            .map_err(|error| storage(OPERATION, error))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| storage(OPERATION, error))?;
    Ok(Some((batch_end, rows)))
}

async fn completed_project_path_collisions(
    transaction: &DatabaseWriteTransaction<'_>,
    after_rowid: i64,
    batch_end: i64,
) -> ProjectionStoreResult<Vec<CanonicalObservationIdV1>> {
    let mut rows = transaction
        .query(
            "SELECT queue.observation_id
             FROM projection_queue AS queue
             WHERE queue.rowid > ?1 AND queue.rowid <= ?2
               AND queue.next_retry_at_micros = ?3
               AND queue.last_error LIKE 'projection session output collided at codex/%: project_path differs%'
               AND EXISTS (
                   SELECT 1 FROM observation_projection_dispositions AS disposition
                   WHERE disposition.observation_id = queue.observation_id
                     AND disposition.projector_version = ?4 AND disposition.reason = ?5
               )
             ORDER BY queue.rowid",
            params![
                after_rowid,
                batch_end,
                PROJECTION_TERMINAL_RETRY_MICROS,
                SESSION_MESSAGE_PROJECTOR_VERSION,
                ProjectionSkipReason::OutputCollision.as_str(),
            ],
        )
        .await
        .map_err(|error| storage(OPERATION, error))?;
    let mut observations = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(OPERATION, error))?
    {
        let id = row
            .get::<String>(0)
            .map_err(|error| storage(OPERATION, error))?;
        observations
            .push(CanonicalObservationIdV1::new(id).map_err(ProjectionStoreError::Contract)?);
    }
    Ok(observations)
}

async fn repair_completed_collision(
    transaction: &DatabaseWriteTransaction<'_>,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<()> {
    let Some((sequence, observation)) = read_observation(transaction, observation_id).await? else {
        return Err(ProjectionStoreError::ObservationNotFound);
    };
    let skipped = ObservationProjection::Skipped(ProjectionSkipReason::OutputCollision);
    verify_effect(transaction, &observation, &skipped).await?;
    record_canonical_observation_effect(transaction, sequence, &observation, &skipped).await?;
    transaction
        .execute(
            "DELETE FROM observation_projection_dispositions
             WHERE projector_version = ?1 AND observation_id = ?2 AND reason = ?3",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation_id.as_str(),
                ProjectionSkipReason::OutputCollision.as_str(),
            ],
        )
        .await
        .map_err(|error| storage(OPERATION, error))?;
    Box::pin(reproject_project_path_collision(
        transaction,
        sequence,
        &observation,
    ))
    .await
}
