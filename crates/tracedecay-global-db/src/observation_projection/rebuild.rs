use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_domain::{CanonicalObservationIdV1, DurableObservationV1};
use tracedecay_runtime_core::db::engine::{
    Connection, Executor, QueryExecutor, TransactionBehavior, params,
};
use tracedecay_sessions::compatibility::{
    derived_text_for_index, derived_text_for_snippet, projected_content_hash,
};
use tracedecay_store::{
    ObservationProjection, ProjectedObservation, ProjectionPersistOutcome,
    ProjectionRebuildOutcome, ProjectionSkipReason, ProjectionStoreError, ProjectionStoreResult,
    SESSION_MESSAGE_PROJECTOR_VERSION, SessionMessageProjection, SessionMessageRecord,
    SessionRecord, WorkflowFactProjection, workflow_semantic_kind,
};

use super::super::session_temporal::record_canonical_observation_effect;
use super::apply::{
    apply_effect, derive_projection_for_rebuild, derive_projection_with_alias, verify_effect,
};
use super::state::{
    consume_projection_queue_item, decode_observation_row, decode_sequence,
    ensure_projection_output_state_cache, queued_sequence, read_checkpoint, read_message,
    read_observation, read_session, storage, storage_message, write_checkpoint,
};
use super::transition::{
    MessageTransition, MessageTransitionState, WorkflowFactTarget, WorkflowFactTransition,
    message_transition, write_workflow_fact_transition,
};

const REBUILD_PAGE_SIZE: i64 = 128;
const REBUILD_MAX_STEPS_PER_INVOCATION: usize = 4;
static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

pub async fn project_observation_with_engine(
    conn: &Connection,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<ProjectionPersistOutcome> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| storage("begin projection transaction", error))?;
    let outcome = project_observation_in_transaction(&transaction, observation_id).await?;
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection transaction", error))?;
    Ok(outcome)
}

pub async fn rebuild_projection_with_engine(
    conn: &Connection,
    frontier_sequence: u64,
) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
    rebuild_projection_until_cancelled_with_engine(conn, frontier_sequence, &NEVER_CANCELLED).await
}

async fn rebuild_projection_until_cancelled_with_engine(
    conn: &Connection,
    frontier_sequence: u64,
    cancelled: &AtomicBool,
) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
    prepare_projection_rebuild_with_engine(conn, frontier_sequence).await?;
    for _ in 0..REBUILD_MAX_STEPS_PER_INVOCATION {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        match advance_projection_rebuild_with_engine(conn, frontier_sequence, cancelled).await? {
            RebuildAdvance::Pending => {}
            RebuildAdvance::Complete(outcome) => return Ok(outcome),
        }
    }
    projection_rebuild_progress_on(conn).await
}

pub(super) async fn resume_projection_rebuild_with_engine(
    conn: &Connection,
    cancelled: &AtomicBool,
) -> ProjectionStoreResult<Option<bool>> {
    let Some(job) = read_optional_rebuild_job(conn).await? else {
        return Ok(None);
    };
    let frontier = decode_sequence(job.frontier, "resume projection rebuild frontier")?;
    rebuild_projection_until_cancelled_with_engine(conn, frontier, cancelled)
        .await
        .map(|outcome| Some(outcome.is_complete()))
}

pub(super) async fn projection_rebuild_pending(
    conn: &impl QueryExecutor,
) -> ProjectionStoreResult<bool> {
    read_optional_rebuild_job(conn)
        .await
        .map(|job| job.is_some())
}

pub(super) async fn prepare_projection_rebuild_with_engine(
    conn: &Connection,
    frontier_sequence: u64,
) -> ProjectionStoreResult<()> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| storage("begin projection rebuild staging", error))?;
    start_or_resume_projection_rebuild_transaction(&transaction, frontier_sequence).await?;
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection rebuild staging", error))
}

async fn advance_projection_rebuild_with_engine(
    conn: &Connection,
    frontier_sequence: u64,
    cancelled: &AtomicBool,
) -> ProjectionStoreResult<RebuildAdvance> {
    let job = read_rebuild_job(conn).await?;
    match job.state {
        RebuildState::Aliasing => {
            stage_projection_alias_batch_with_engine(conn).await?;
            Ok(RebuildAdvance::Pending)
        }
        RebuildState::Building => {
            stage_projection_rebuild_batch_with_engine(conn, cancelled).await?;
            Ok(RebuildAdvance::Pending)
        }
        RebuildState::Ready => activate_projection_rebuild_with_engine(conn, frontier_sequence)
            .await
            .map(RebuildAdvance::Complete),
    }
}

async fn stage_projection_alias_batch_with_engine(conn: &Connection) -> ProjectionStoreResult<()> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| storage("begin projection alias staging", error))?;
    stage_projection_alias_batch_transaction(&transaction).await?;
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection alias batch", error))
}

async fn stage_projection_rebuild_batch_with_engine(
    conn: &Connection,
    cancelled: &AtomicBool,
) -> ProjectionStoreResult<bool> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| storage("begin projection rebuild batch", error))?;
    let outcome = stage_projection_rebuild_batch_transaction(&transaction, cancelled).await?;
    let commit_operation = match outcome {
        RebuildBatchStage::AlreadyReady => "commit completed projection rebuild batch",
        RebuildBatchStage::Advanced { .. } => "commit projection rebuild batch",
    };
    transaction
        .commit()
        .await
        .map_err(|error| storage(commit_operation, error))?;
    Ok(outcome.still_building())
}

async fn activate_projection_rebuild_with_engine(
    conn: &Connection,
    frontier_sequence: u64,
) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await
        .map_err(|error| storage("begin projection rebuild activation", error))?;
    let outcome = activate_projection_rebuild_transaction(&transaction, frontier_sequence).await?;
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection rebuild activation", error))?;
    Ok(outcome)
}

async fn project_observation_in_transaction(
    transaction: &impl Executor,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<ProjectionPersistOutcome> {
    ensure_projection_output_state_cache(transaction).await?;
    let checkpoint = read_checkpoint(transaction).await?;
    let Some((sequence, observation)) = read_observation(transaction, observation_id).await? else {
        return Err(ProjectionStoreError::ObservationNotFound);
    };
    let mut effect = derive_projection_with_alias(transaction, &observation).await?;
    if sequence <= checkpoint.last_sequence() {
        verify_effect(transaction, &observation, &effect).await?;
        record_canonical_observation_effect(transaction, sequence, &observation, &effect).await?;
        consume_projection_queue_item(transaction, observation_id).await?;
        return Ok(ProjectionPersistOutcome::ExactDuplicate(checkpoint));
    }
    let expected = checkpoint.last_sequence().saturating_add(1);
    if sequence != expected {
        return Err(ProjectionStoreError::Gap {
            expected,
            actual: sequence,
        });
    }
    if queued_sequence(transaction, observation_id).await? != Some(sequence) {
        return Err(ProjectionStoreError::NotQueued);
    }

    write_effect_converging_collisions(
        transaction,
        &CollisionGuardedWrite::Drain,
        sequence,
        &observation,
        &mut effect,
    )
    .await?;
    record_canonical_observation_effect(transaction, sequence, &observation, &effect).await?;
    consume_projection_queue_item(transaction, observation_id).await?;
    let checkpoint = write_checkpoint(transaction, sequence).await?;
    let output_count = effect.output_count();
    Ok(match effect {
        ObservationProjection::Message(_) | ObservationProjection::Composite { .. } => {
            ProjectionPersistOutcome::Projected(ProjectedObservation::new(checkpoint, output_count))
        }
        ObservationProjection::Skipped(reason) => {
            ProjectionPersistOutcome::Skipped { checkpoint, reason }
        }
    })
}

async fn start_or_resume_projection_rebuild_transaction(
    transaction: &impl Executor,
    frontier_sequence: u64,
) -> ProjectionStoreResult<()> {
    validate_rebuild_frontier(transaction, frontier_sequence).await?;
    let frontier = sequence_i64(frontier_sequence)?;
    let existing = read_optional_rebuild_job(transaction).await?;
    if existing
        .as_ref()
        .is_some_and(|job| job.frontier != frontier)
    {
        transaction
            .execute(
                "DELETE FROM observation_projection_rebuilds WHERE projector_version = ?1",
                params![SESSION_MESSAGE_PROJECTOR_VERSION],
            )
            .await
            .map_err(|error| storage("replace projection rebuild generation", error))?;
    }
    if existing.is_none_or(|job| job.frontier != frontier) {
        transaction
            .execute(
                "INSERT INTO observation_projection_rebuilds (
                    projector_version, generation, frontier_sequence,
                    aliases_staged_through, staged_through, projected_rows,
                    skipped_observations, state
                 ) VALUES (
                    ?1, lower(hex(randomblob(16))), ?2, 0, 0, 0, 0, 'aliasing'
                 )",
                params![SESSION_MESSAGE_PROJECTOR_VERSION, frontier],
            )
            .await
            .map_err(|error| storage("create projection rebuild generation", error))?;
    }
    Ok(())
}

async fn stage_projection_alias_batch_transaction(
    transaction: &impl Executor,
) -> ProjectionStoreResult<()> {
    let job = read_rebuild_job(transaction).await?;
    if job.state != RebuildState::Aliasing {
        return Err(storage_message(
            "stage projection alias batch",
            "projection rebuild is not aliasing",
        ));
    }
    let mut rows = transaction
        .query(
            "SELECT sequence FROM observations
             WHERE sequence > ?1 AND sequence <= ?2
             ORDER BY sequence ASC LIMIT ?3",
            params![job.aliases_staged_through, job.frontier, REBUILD_PAGE_SIZE],
        )
        .await
        .map_err(|error| storage("read projection alias batch", error))?;
    let mut aliases_staged_through = job.aliases_staged_through;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection alias batch", error))?
    {
        let sequence = row
            .get(0)
            .map_err(|error| storage("read projection alias batch", error))?;
        let expected = aliases_staged_through.checked_add(1).ok_or_else(|| {
            storage_message(
                "stage projection alias batch",
                "observation sequence overflow before alias frontier",
            )
        })?;
        if sequence != expected {
            return Err(storage_message(
                "stage projection alias batch",
                "observation sequence gap before alias frontier",
            ));
        }
        aliases_staged_through = sequence;
    }
    drop(rows);
    if aliases_staged_through < job.frontier && aliases_staged_through == job.aliases_staged_through
    {
        return Err(storage_message(
            "stage projection alias batch",
            "observation sequence gap before alias frontier",
        ));
    }
    transaction
        .execute(
            "INSERT OR IGNORE INTO observation_projection_rebuild_aliases (
                projector_version, generation, observation_id,
                output_provider, output_message_id
             )
             SELECT alias.projector_version, ?2, alias.observation_id,
                    alias.output_provider, alias.output_message_id
             FROM observation_projection_aliases AS alias
             JOIN observations AS observation
               ON observation.observation_id = alias.observation_id
             WHERE alias.projector_version = ?1
               AND observation.sequence > ?3 AND observation.sequence <= ?4",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                job.generation.as_str(),
                job.aliases_staged_through,
                aliases_staged_through,
            ],
        )
        .await
        .map_err(|error| storage("capture projection alias batch", error))?;
    let state = if aliases_staged_through == job.frontier {
        RebuildState::Building
    } else {
        RebuildState::Aliasing
    };
    transaction
        .execute(
            "UPDATE observation_projection_rebuilds
             SET aliases_staged_through = ?3, state = ?4
             WHERE projector_version = ?1 AND generation = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                job.generation.as_str(),
                aliases_staged_through,
                state.as_str(),
            ],
        )
        .await
        .map_err(|error| storage("advance projection alias batch", error))?;
    Ok(())
}

async fn stage_projection_rebuild_batch_transaction(
    transaction: &impl Executor,
    cancelled: &AtomicBool,
) -> ProjectionStoreResult<RebuildBatchStage> {
    let job = read_rebuild_job(transaction).await?;
    if job.state == RebuildState::Ready {
        return Ok(RebuildBatchStage::AlreadyReady);
    }
    if job.state != RebuildState::Building || job.aliases_staged_through != job.frontier {
        return Err(storage_message(
            "stage projection rebuild batch",
            "projection alias snapshot is incomplete",
        ));
    }
    let mut rows = transaction
        .query(
            "SELECT sequence, observation_json FROM observations
             WHERE sequence > ?1 AND sequence <= ?2
             ORDER BY sequence ASC LIMIT ?3",
            params![job.staged_through, job.frontier, REBUILD_PAGE_SIZE],
        )
        .await
        .map_err(|error| storage("read projection rebuild batch", error))?;
    let mut page = Vec::with_capacity(REBUILD_PAGE_SIZE as usize);
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection rebuild batch", error))?
    {
        page.push(decode_observation_row(
            &row,
            "read projection rebuild batch",
        )?);
    }
    drop(rows);

    let mut staged_through = job.staged_through;
    let mut projected_rows = job.projected_rows;
    let mut skipped_observations = job.skipped_observations;
    for (sequence, observation) in page {
        let sequence_i64 = sequence_i64(sequence)?;
        let expected = staged_through.checked_add(1).ok_or_else(|| {
            storage_message(
                "stage projection rebuild batch",
                "observation sequence overflow before rebuild frontier",
            )
        })?;
        if sequence_i64 != expected {
            return Err(storage_message(
                "stage projection rebuild batch",
                "observation sequence gap before rebuild frontier",
            ));
        }
        let mut effect =
            derive_projection_for_rebuild(transaction, &observation, &job.generation).await?;
        write_effect_converging_collisions(
            transaction,
            &CollisionGuardedWrite::Stage {
                generation: &job.generation,
            },
            sequence,
            &observation,
            &mut effect,
        )
        .await?;
        record_canonical_observation_effect(transaction, sequence, &observation, &effect).await?;
        match &effect {
            ObservationProjection::Message(_) | ObservationProjection::Composite { .. } => {
                projected_rows = projected_rows.saturating_add(effect.output_count());
            }
            ObservationProjection::Skipped(_) => {
                skipped_observations = skipped_observations.saturating_add(1);
            }
        }
        staged_through = sequence_i64;
        if cancelled.load(Ordering::Acquire) {
            break;
        }
    }
    if staged_through < job.frontier && staged_through == job.staged_through {
        return Err(storage_message(
            "stage projection rebuild batch",
            "observation sequence gap before rebuild frontier",
        ));
    }
    let state = if staged_through == job.frontier {
        RebuildState::Ready
    } else {
        RebuildState::Building
    };
    transaction
        .execute(
            "UPDATE observation_projection_rebuilds
             SET staged_through = ?3, projected_rows = ?4,
                 skipped_observations = ?5, state = ?6
             WHERE projector_version = ?1 AND generation = ?2",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                job.generation.as_str(),
                staged_through,
                usize_i64(projected_rows)?,
                usize_i64(skipped_observations)?,
                state.as_str(),
            ],
        )
        .await
        .map_err(|error| storage("advance projection rebuild batch", error))?;
    Ok(RebuildBatchStage::Advanced {
        still_building: state == RebuildState::Building,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RebuildBatchStage {
    AlreadyReady,
    Advanced { still_building: bool },
}

impl RebuildBatchStage {
    const fn still_building(self) -> bool {
        match self {
            Self::AlreadyReady => false,
            Self::Advanced { still_building } => still_building,
        }
    }
}

async fn activate_projection_rebuild_transaction(
    transaction: &impl Executor,
    frontier_sequence: u64,
) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
    validate_rebuild_frontier(transaction, frontier_sequence).await?;
    let job = read_rebuild_job(transaction).await?;
    if job.state != RebuildState::Ready
        || job.frontier != sequence_i64(frontier_sequence)?
        || job.staged_through != job.frontier
        || job.aliases_staged_through != job.frontier
    {
        return Err(storage_message(
            "activate projection rebuild",
            "projection rebuild generation is incomplete",
        ));
    }
    clear_active_projection(transaction, &job.generation).await?;
    activate_rebuild_sessions(transaction, &job.generation).await?;
    prepare_rebuild_output_activation(transaction, &job.generation).await?;
    activate_rebuild_messages(transaction, &job.generation).await?;
    activate_rebuild_provenance(transaction, &job.generation).await?;
    activate_rebuild_workflow_facts(transaction, &job.generation).await?;
    activate_rebuild_dispositions(transaction, &job.generation).await?;

    transaction
        .execute(
            "INSERT OR IGNORE INTO projection_queue (observation_id, observation_sequence)
             SELECT observation_id, sequence FROM observations WHERE sequence > ?1",
            params![job.frontier],
        )
        .await
        .map_err(|error| storage("requeue observations past rebuild frontier", error))?;
    transaction
        .execute(
            "DELETE FROM projection_queue WHERE observation_sequence <= ?1",
            params![job.frontier],
        )
        .await
        .map_err(|error| storage("consume rebuilt projection queue", error))?;
    let checkpoint = write_checkpoint(transaction, frontier_sequence).await?;
    transaction
        .execute(
            "DELETE FROM observation_projection_rebuilds WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| storage("clear activated projection rebuild generation", error))?;
    Ok(ProjectionRebuildOutcome::new(
        checkpoint,
        job.projected_rows,
        job.skipped_observations,
    ))
}

async fn projection_rebuild_progress_on(
    conn: &impl QueryExecutor,
) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
    let job = read_rebuild_job(conn).await?;
    let checkpoint = read_checkpoint(conn).await?;
    Ok(ProjectionRebuildOutcome::in_progress(
        checkpoint,
        job.projected_rows,
        job.skipped_observations,
    ))
}

/// Single savepoint name shared by both collision-guarded write paths. Each
/// path opens it in its own transaction, so one constant is sufficient and
/// keeps the rollback pairing symmetric.
const PROJECTION_COLLISION_SAVEPOINT: &str = "projection_collision_guard";

/// Which write a collision-guarded persist runs. The live queue drain writes
/// straight into the active projection tables; a rebuild batch writes into the
/// generation's staging tables. Both share the savepoint/rollback/skip shape.
enum CollisionGuardedWrite<'a> {
    Drain,
    Stage { generation: &'a str },
}

impl CollisionGuardedWrite<'_> {
    async fn run(
        &self,
        conn: &impl Executor,
        sequence: u64,
        observation: &DurableObservationV1,
        effect: &ObservationProjection,
    ) -> ProjectionStoreResult<()> {
        match self {
            Self::Drain => apply_effect(conn, sequence, observation, effect).await,
            Self::Stage { generation } => {
                stage_rebuild_effect(conn, generation, sequence, observation, effect).await
            }
        }
    }
}

/// Persist `effect` inside a savepoint. On success the savepoint is released.
/// On an output-identity collision the savepoint is rolled back, `effect` is
/// substituted with a durable `OutputCollision` skip, and the write is re-run
/// so the live drain and a rebuild converge on the same skip instead of
/// wedging on the collided output. This is the write-time backstop for
/// collisions not yet recorded as a disposition.
async fn write_effect_converging_collisions(
    conn: &impl Executor,
    write: &CollisionGuardedWrite<'_>,
    sequence: u64,
    observation: &DurableObservationV1,
    effect: &mut ObservationProjection,
) -> ProjectionStoreResult<()> {
    conn.execute_batch(&format!("SAVEPOINT {PROJECTION_COLLISION_SAVEPOINT};"))
        .await
        .map_err(|error| storage("begin projection collision savepoint", error))?;
    match write.run(conn, sequence, observation, effect).await {
        Ok(()) => {
            conn.execute_batch(&format!("RELEASE {PROJECTION_COLLISION_SAVEPOINT};"))
                .await
                .map_err(|error| storage("release projection collision savepoint", error))?;
            Ok(())
        }
        Err(ProjectionStoreError::OutputCollision {
            provider,
            message_id,
        }) => {
            tracing::warn!(
                %provider,
                %message_id,
                observation = observation.observation_id().as_str(),
                "projection output collided; recording a durable skip disposition"
            );
            conn.execute_batch(&format!(
                "ROLLBACK TO {PROJECTION_COLLISION_SAVEPOINT}; \
                 RELEASE {PROJECTION_COLLISION_SAVEPOINT};"
            ))
            .await
            .map_err(|error| storage("rollback projection collision savepoint", error))?;
            *effect = ObservationProjection::Skipped(ProjectionSkipReason::OutputCollision);
            write.run(conn, sequence, observation, effect).await
        }
        Err(error) => Err(error),
    }
}

enum RebuildAdvance {
    Pending,
    Complete(ProjectionRebuildOutcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RebuildState {
    Aliasing,
    Building,
    Ready,
}

impl RebuildState {
    fn parse(value: &str) -> ProjectionStoreResult<Self> {
        match value {
            "aliasing" => Ok(Self::Aliasing),
            "building" => Ok(Self::Building),
            "ready" => Ok(Self::Ready),
            _ => Err(storage_message(
                "decode projection rebuild state",
                format!("unknown rebuild state {value}"),
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Aliasing => "aliasing",
            Self::Building => "building",
            Self::Ready => "ready",
        }
    }
}

struct RebuildJob {
    generation: String,
    frontier: i64,
    aliases_staged_through: i64,
    staged_through: i64,
    projected_rows: usize,
    skipped_observations: usize,
    state: RebuildState,
}

struct RebuildOutputState {
    latest_observation: DurableObservationV1,
    latest_sequence: u64,
    projector_owned: bool,
}

fn sequence_i64(sequence: u64) -> ProjectionStoreResult<i64> {
    i64::try_from(sequence).map_err(|_| ProjectionStoreError::SequenceOverflow(sequence))
}

fn usize_i64(value: usize) -> ProjectionStoreResult<i64> {
    i64::try_from(value)
        .map_err(|_| storage_message("encode projection rebuild counter", "counter overflow"))
}

fn decode_usize(value: i64, operation: &'static str) -> ProjectionStoreResult<usize> {
    usize::try_from(value).map_err(|_| storage_message(operation, "invalid rebuild counter"))
}

/// Reads the current observation frontier: `COALESCE(MAX(sequence), 0)` over
/// `observations`. The query's row cursor lives only inside this function and
/// is fully consumed and dropped before it returns, so a caller may safely go
/// on to read or write further rows through the same connection and observe
/// them — a cursor left open past that point would otherwise pin the
/// connection's read snapshot and hide those subsequent writes.
pub(super) async fn read_observation_frontier(
    conn: &impl QueryExecutor,
) -> ProjectionStoreResult<u64> {
    let mut rows = conn
        .query("SELECT COALESCE(MAX(sequence), 0) FROM observations", ())
        .await
        .map_err(|error| storage("read projection rebuild frontier", error))?;
    let frontier = rows
        .next()
        .await
        .map_err(|error| storage("read projection rebuild frontier", error))?
        .ok_or_else(|| storage_message("read projection rebuild frontier", "no row"))?
        .get::<i64>(0)
        .map_err(|error| storage("read projection rebuild frontier", error))?;
    decode_sequence(frontier, "read projection rebuild frontier")
}

async fn validate_rebuild_frontier(
    conn: &impl QueryExecutor,
    frontier: u64,
) -> ProjectionStoreResult<()> {
    let committed = read_observation_frontier(conn).await?;
    if frontier > committed {
        Err(ProjectionStoreError::InvalidRebuildFrontier {
            frontier,
            committed,
        })
    } else {
        Ok(())
    }
}

async fn read_optional_rebuild_job(
    conn: &impl QueryExecutor,
) -> ProjectionStoreResult<Option<RebuildJob>> {
    let mut rows = conn
        .query(
            "SELECT generation, frontier_sequence, aliases_staged_through, staged_through,
                    projected_rows, skipped_observations, state
             FROM observation_projection_rebuilds WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| storage("read projection rebuild generation", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection rebuild generation", error))?
    else {
        return Ok(None);
    };
    let state = row
        .get::<String>(6)
        .map_err(|error| storage("read projection rebuild generation", error))?;
    Ok(Some(RebuildJob {
        generation: row
            .get(0)
            .map_err(|error| storage("read projection rebuild generation", error))?,
        frontier: row
            .get(1)
            .map_err(|error| storage("read projection rebuild generation", error))?,
        aliases_staged_through: row
            .get(2)
            .map_err(|error| storage("read projection rebuild generation", error))?,
        staged_through: row
            .get(3)
            .map_err(|error| storage("read projection rebuild generation", error))?,
        projected_rows: decode_usize(
            row.get(4)
                .map_err(|error| storage("read projection rebuild generation", error))?,
            "read projection rebuild generation",
        )?,
        skipped_observations: decode_usize(
            row.get(5)
                .map_err(|error| storage("read projection rebuild generation", error))?,
            "read projection rebuild generation",
        )?,
        state: RebuildState::parse(&state)?,
    }))
}

async fn read_rebuild_job(conn: &impl QueryExecutor) -> ProjectionStoreResult<RebuildJob> {
    read_optional_rebuild_job(conn).await?.ok_or_else(|| {
        storage_message(
            "read projection rebuild generation",
            "projection rebuild generation is missing",
        )
    })
}

fn encode_json<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> ProjectionStoreResult<String> {
    serde_json::to_string(value).map_err(|error| storage(operation, error))
}

fn decode_json<T: serde::de::DeserializeOwned>(
    value: &str,
    operation: &'static str,
) -> ProjectionStoreResult<T> {
    serde_json::from_str(value).map_err(|error| storage(operation, error))
}

async fn read_staged_session(
    conn: &impl QueryExecutor,
    generation: &str,
    provider: &str,
    session_id: &str,
) -> ProjectionStoreResult<Option<SessionRecord>> {
    let mut rows = conn
        .query(
            "SELECT session_json FROM observation_projection_rebuild_sessions
             WHERE projector_version = ?1 AND generation = ?2
               AND provider = ?3 AND session_id = ?4",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                generation,
                provider,
                session_id
            ],
        )
        .await
        .map_err(|error| storage("read staged projection session", error))?;
    rows.next()
        .await
        .map_err(|error| storage("read staged projection session", error))?
        .map(|row| {
            let json: String = row
                .get(0)
                .map_err(|error| storage("read staged projection session", error))?;
            decode_json(&json, "decode staged projection session")
        })
        .transpose()
}

async fn stage_rebuild_session(
    conn: &impl Executor,
    generation: &str,
    expected: &SessionRecord,
) -> ProjectionStoreResult<()> {
    // Match apply_session / verify_rows: normalize host spellings before pure
    // string reconcile so macOS /var firmlinks and user symlink families converge.
    let expected = super::state::canonicalize_session_project_paths(expected);
    let actual =
        match read_staged_session(conn, generation, &expected.provider, &expected.session_id)
            .await?
        {
            Some(session) => Some(session),
            None => read_session(conn, &expected.provider, &expected.session_id).await?,
        };
    let session = match actual {
        Some(actual) => {
            super::state::reconcile_session_rows(&actual, &expected).ok_or_else(|| {
                ProjectionStoreError::OutputCollision {
                    provider: expected.provider.clone(),
                    message_id: format!("session:{}", expected.session_id),
                }
            })?
        }
        None => expected,
    };
    let json = encode_json(&session, "encode staged projection session")?;
    conn.execute(
        "INSERT INTO observation_projection_rebuild_sessions (
            projector_version, generation, provider, session_id, session_json
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(projector_version, generation, provider, session_id)
         DO UPDATE SET session_json = excluded.session_json",
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            generation,
            session.provider.as_str(),
            session.session_id.as_str(),
            json.as_str(),
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("stage projection session", error))
}

async fn write_staged_message(
    conn: &impl Executor,
    generation: &str,
    message: &SessionMessageRecord,
) -> ProjectionStoreResult<()> {
    let json = encode_json(message, "encode staged projection message")?;
    let content_hash = projected_content_hash(&message.text);
    let snippet = derived_text_for_snippet(&message.text);
    let index = derived_text_for_index(&message.text);
    conn.execute(
        "INSERT INTO observation_projection_rebuild_messages (
            projector_version, generation, output_provider, output_message_id,
            message_json, content_hash, snippet_text, index_text
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(projector_version, generation, output_provider, output_message_id)
         DO UPDATE SET message_json = excluded.message_json,
                       content_hash = excluded.content_hash,
                       snippet_text = excluded.snippet_text,
                       index_text = excluded.index_text",
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            generation,
            message.provider.as_str(),
            message.message_id.as_str(),
            json.as_str(),
            content_hash.as_str(),
            snippet.as_str(),
            index.as_str(),
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("stage projection message", error))
}

async fn read_staged_message(
    conn: &impl QueryExecutor,
    generation: &str,
    provider: &str,
    message_id: &str,
) -> ProjectionStoreResult<Option<SessionMessageRecord>> {
    let mut rows = conn
        .query(
            "SELECT message_json FROM observation_projection_rebuild_messages
             WHERE projector_version = ?1 AND generation = ?2
               AND output_provider = ?3 AND output_message_id = ?4",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                generation,
                provider,
                message_id
            ],
        )
        .await
        .map_err(|error| storage("read staged projection message", error))?;
    rows.next()
        .await
        .map_err(|error| storage("read staged projection message", error))?
        .map(|row| {
            let json: String = row
                .get(0)
                .map_err(|error| storage("read staged projection message", error))?;
            decode_json(&json, "decode staged projection message")
        })
        .transpose()
}

async fn ensure_staged_output_baseline(
    conn: &impl Executor,
    generation: &str,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    let message = projection.message();
    if read_staged_message(conn, generation, &message.provider, &message.message_id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let mut rows = conn
        .query(
            "SELECT
                COALESCE(MAX(CASE WHEN projector_version = ?1 THEN message_created ELSE 0 END), 0),
                COALESCE(MAX(CASE WHEN projector_version <> ?1 THEN 1 ELSE 0 END), 0)
             FROM observation_projection_provenance
             WHERE output_provider = ?2 AND output_message_id = ?3",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                message.provider.as_str(),
                message.message_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage("read projection rebuild output owners", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("read projection rebuild output owners", error))?
        .ok_or_else(|| storage_message("read projection rebuild output owners", "no row"))?;
    let current_created = row
        .get::<i64>(0)
        .map_err(|error| storage("read projection rebuild output owners", error))?
        != 0;
    let cross_owned = row
        .get::<i64>(1)
        .map_err(|error| storage("read projection rebuild output owners", error))?
        != 0;
    drop(rows);

    if cross_owned {
        conn.execute(
            "INSERT OR IGNORE INTO observation_projection_rebuild_provenance (
                projector_version, generation, observation_id, output_ordinal,
                retrieval_anchor_id, receipt_id, output_provider, output_message_id,
                output_digest, message_created
             )
             SELECT projector_version, ?2, observation_id, output_ordinal,
                    retrieval_anchor_id, receipt_id, output_provider, output_message_id,
                    output_digest, message_created
             FROM observation_projection_provenance
             WHERE projector_version = ?1 AND output_provider = ?3 AND output_message_id = ?4",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                generation,
                message.provider.as_str(),
                message.message_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage("stage retained projection provenance", error))?;
    }
    if (!current_created || cross_owned)
        && let Some(actual) = read_message(conn, &message.provider, &message.message_id).await?
    {
        write_staged_message(conn, generation, &actual).await?;
    }
    Ok(())
}

async fn read_staged_output_state(
    conn: &impl QueryExecutor,
    generation: &str,
    provider: &str,
    message_id: &str,
) -> ProjectionStoreResult<Option<RebuildOutputState>> {
    let mut rows = conn
        .query(
            "SELECT observation.sequence, observation.observation_json,
                    (SELECT COALESCE(MAX(owner.message_created), 0)
                     FROM observation_projection_rebuild_provenance AS owner
                     WHERE owner.projector_version = provenance.projector_version
                       AND owner.generation = provenance.generation
                       AND owner.output_provider = provenance.output_provider
                       AND owner.output_message_id = provenance.output_message_id)
             FROM observation_projection_rebuild_provenance AS provenance
             JOIN observations AS observation
               ON observation.observation_id = provenance.observation_id
             WHERE provenance.projector_version = ?1 AND provenance.generation = ?2
               AND provenance.output_provider = ?3 AND provenance.output_message_id = ?4
             ORDER BY observation.sequence DESC, provenance.observation_id DESC
             LIMIT 1",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                generation,
                provider,
                message_id,
            ],
        )
        .await
        .map_err(|error| storage("read staged projection output state", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read staged projection output state", error))?
    else {
        return Ok(None);
    };
    let latest_sequence = decode_sequence(
        row.get(0)
            .map_err(|error| storage("read staged projection output state", error))?,
        "read staged projection output state",
    )?;
    let json: String = row
        .get(1)
        .map_err(|error| storage("read staged projection output state", error))?;
    Ok(Some(RebuildOutputState {
        latest_observation: decode_json(&json, "decode staged projection output state")?,
        latest_sequence,
        projector_owned: row
            .get::<i64>(2)
            .map_err(|error| storage("read staged projection output state", error))?
            != 0,
    }))
}

async fn stage_rebuild_provenance(
    conn: &impl Executor,
    generation: &str,
    projection: &SessionMessageProjection,
    message_created: bool,
) -> ProjectionStoreResult<()> {
    let provenance = projection.provenance();
    let message = projection.message();
    conn.execute(
        "INSERT OR IGNORE INTO observation_projection_rebuild_provenance (
            projector_version, generation, observation_id, output_ordinal, retrieval_anchor_id,
            receipt_id, output_provider, output_message_id, output_digest, message_created
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            generation,
            provenance.observation_id().as_str(),
            i64::from(projection.output_ordinal()),
            provenance.retrieval_anchor_id().as_str(),
            provenance.receipt_id(),
            message.provider.as_str(),
            message.message_id.as_str(),
            projection.output_digest()?.as_str(),
            i64::from(message_created),
        ],
    )
    .await
    .map_err(|error| storage("stage projection provenance", error))?;
    let mut rows = conn
        .query(
            "SELECT retrieval_anchor_id, receipt_id, output_provider, output_message_id,
                    output_digest
             FROM observation_projection_rebuild_provenance
             WHERE projector_version = ?1 AND generation = ?2
               AND observation_id = ?3 AND output_ordinal = ?4",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                generation,
                provenance.observation_id().as_str(),
                i64::from(projection.output_ordinal()),
            ],
        )
        .await
        .map_err(|error| storage("verify staged projection provenance", error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage("verify staged projection provenance", error))?
        .ok_or(ProjectionStoreError::ProvenanceCollision)?;
    let actual = (
        row.get::<String>(0)
            .map_err(|error| storage("verify staged projection provenance", error))?,
        row.get::<String>(1)
            .map_err(|error| storage("verify staged projection provenance", error))?,
        row.get::<String>(2)
            .map_err(|error| storage("verify staged projection provenance", error))?,
        row.get::<String>(3)
            .map_err(|error| storage("verify staged projection provenance", error))?,
        row.get::<String>(4)
            .map_err(|error| storage("verify staged projection provenance", error))?,
    );
    let expected = (
        provenance.retrieval_anchor_id().as_str().to_owned(),
        provenance.receipt_id().to_owned(),
        message.provider.clone(),
        message.message_id.clone(),
        projection.output_digest()?.as_str().to_owned(),
    );
    if actual == expected {
        Ok(())
    } else {
        Err(ProjectionStoreError::ProvenanceCollision)
    }
}

async fn stage_rebuild_message(
    conn: &impl Executor,
    generation: &str,
    sequence: u64,
    observation: &DurableObservationV1,
    projection: &SessionMessageProjection,
) -> ProjectionStoreResult<()> {
    stage_rebuild_session(conn, generation, projection.session()).await?;
    ensure_staged_output_baseline(conn, generation, projection).await?;
    let message = projection.message();
    let existing =
        read_staged_message(conn, generation, &message.provider, &message.message_id).await?;
    let state =
        read_staged_output_state(conn, generation, &message.provider, &message.message_id).await?;
    let transition_state = state.as_ref().map(|state| {
        MessageTransitionState::new(
            observation,
            &state.latest_observation,
            state.latest_sequence,
            state.projector_owned,
        )
    });
    let (transition, _) = message_transition(
        conn,
        sequence,
        projection,
        existing.as_ref(),
        transition_state,
    )
    .await?;
    match transition {
        MessageTransition::Insert | MessageTransition::Supersede => {
            write_staged_message(conn, generation, message).await?;
        }
        MessageTransition::Retain => {}
    }
    stage_rebuild_provenance(
        conn,
        generation,
        projection,
        transition == MessageTransition::Insert,
    )
    .await
}

async fn stage_rebuild_workflow_fact(
    conn: &impl Executor,
    generation: &str,
    sequence: u64,
    projection: &WorkflowFactProjection,
) -> ProjectionStoreResult<()> {
    stage_rebuild_session(conn, generation, projection.session()).await?;
    let content_json = projection
        .fact()
        .content
        .as_ref()
        .map(|content| encode_json(content, "encode staged workflow fact content"))
        .transpose()?;
    let transition = WorkflowFactTransition::new(sequence, projection)?;
    let inserted = write_workflow_fact_transition(
        conn,
        WorkflowFactTarget::Staged { generation },
        &transition,
        workflow_semantic_kind(transition.fact().semantic_kind),
        content_json.as_deref(),
    )
    .await?;
    if inserted == 1 {
        Ok(())
    } else {
        Err(ProjectionStoreError::ProvenanceCollision)
    }
}

async fn stage_rebuild_disposition(
    conn: &impl Executor,
    generation: &str,
    observation: &DurableObservationV1,
    reason: ProjectionSkipReason,
) -> ProjectionStoreResult<()> {
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO observation_projection_rebuild_dispositions (
                projector_version, generation, observation_id, receipt_id, reason
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                generation,
                observation.observation_id().as_str(),
                observation.receipt().receipt().receipt_id().as_str(),
                reason.as_str(),
            ],
        )
        .await
        .map_err(|error| storage("stage projection disposition", error))?;
    if inserted == 1 {
        Ok(())
    } else {
        Err(ProjectionStoreError::ProvenanceCollision)
    }
}

async fn stage_rebuild_effect(
    conn: &impl Executor,
    generation: &str,
    sequence: u64,
    observation: &DurableObservationV1,
    effect: &ObservationProjection,
) -> ProjectionStoreResult<()> {
    match effect {
        ObservationProjection::Message(projection) => {
            stage_rebuild_message(conn, generation, sequence, observation, projection).await
        }
        ObservationProjection::Composite {
            message,
            derived_messages,
            workflow_facts,
        } => {
            if let Some(message) = message {
                stage_rebuild_message(conn, generation, sequence, observation, message).await?;
            }
            for message in derived_messages {
                stage_rebuild_message(conn, generation, sequence, observation, message).await?;
            }
            for fact in workflow_facts {
                stage_rebuild_workflow_fact(conn, generation, sequence, fact).await?;
            }
            Ok(())
        }
        ObservationProjection::Skipped(reason) => {
            stage_rebuild_disposition(conn, generation, observation, *reason).await
        }
    }
}

async fn clear_active_projection(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    ensure_projection_output_state_cache(conn).await?;
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS observation_projection_rebuild_retained_outputs (
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            PRIMARY KEY(output_provider, output_message_id)
         ) WITHOUT ROWID;
         CREATE TEMP TABLE IF NOT EXISTS observation_projection_rebuild_cleared_outputs (
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            PRIMARY KEY(output_provider, output_message_id)
         ) WITHOUT ROWID;
         DELETE FROM temp.observation_projection_rebuild_retained_outputs;
         DELETE FROM temp.observation_projection_rebuild_cleared_outputs;",
    )
    .await
    .map_err(|error| storage("prepare projection rebuild activation", error))?;
    conn.execute(
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
        params![SESSION_MESSAGE_PROJECTOR_VERSION],
    )
    .await
    .map_err(|error| storage("materialize retained projection outputs", error))?;
    conn.execute(
        "INSERT INTO temp.observation_projection_rebuild_cleared_outputs (
            output_provider, output_message_id
         )
         SELECT DISTINCT provenance.output_provider, provenance.output_message_id
         FROM observation_projection_provenance AS provenance
         WHERE provenance.projector_version = ?1 AND provenance.message_created = 1
           AND NOT EXISTS (
             SELECT 1 FROM temp.observation_projection_rebuild_retained_outputs AS retained
             WHERE retained.output_provider = provenance.output_provider
               AND retained.output_message_id = provenance.output_message_id
           )
           AND NOT EXISTS (
             SELECT 1 FROM observation_projection_rebuild_messages AS staged
             WHERE staged.projector_version = ?1 AND staged.generation = ?2
               AND staged.output_provider = provenance.output_provider
               AND staged.output_message_id = provenance.output_message_id
           )",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
    )
    .await
    .map_err(|error| storage("materialize cleared projection outputs", error))?;
    conn.execute(
        "DELETE FROM lcm_raw_messages
         WHERE provider <> 'hermes' AND EXISTS (
           SELECT 1 FROM temp.observation_projection_rebuild_cleared_outputs AS cleared
           WHERE cleared.output_provider = lcm_raw_messages.provider
             AND cleared.output_message_id = lcm_raw_messages.message_id
         )",
        (),
    )
    .await
    .map_err(|error| storage("clear projected LCM raw rows for rebuild", error))?;
    conn.execute(
        "DELETE FROM session_messages WHERE EXISTS (
           SELECT 1 FROM temp.observation_projection_rebuild_cleared_outputs AS cleared
           WHERE cleared.output_provider = session_messages.provider
             AND cleared.output_message_id = session_messages.message_id
         )",
        (),
    )
    .await
    .map_err(|error| storage("clear projection message rows for rebuild", error))?;
    conn.execute(
        "DELETE FROM observation_projection_provenance
         WHERE projector_version = ?1 AND NOT EXISTS (
           SELECT 1 FROM temp.observation_projection_rebuild_retained_outputs AS retained
           WHERE retained.output_provider = observation_projection_provenance.output_provider
             AND retained.output_message_id = observation_projection_provenance.output_message_id
         )",
        params![SESSION_MESSAGE_PROJECTOR_VERSION],
    )
    .await
    .map_err(|error| storage("clear projection provenance for rebuild", error))?;
    conn.execute(
        "DELETE FROM observation_projection_dispositions WHERE projector_version = ?1",
        params![SESSION_MESSAGE_PROJECTOR_VERSION],
    )
    .await
    .map_err(|error| storage("clear projection dispositions for rebuild", error))?;
    conn.execute(
        "DELETE FROM observation_workflow_facts WHERE projector_version = ?1",
        params![SESSION_MESSAGE_PROJECTOR_VERSION],
    )
    .await
    .map_err(|error| storage("clear projection workflow facts for rebuild", error))?;
    conn.execute(
        "DELETE FROM observation_projection_checkpoints WHERE projector_version = ?1",
        params![SESSION_MESSAGE_PROJECTOR_VERSION],
    )
    .await
    .map_err(|error| storage("clear projection checkpoint for rebuild", error))?;
    conn.execute(
        "DELETE FROM temp.observation_projection_output_state_meta",
        (),
    )
    .await
    .map_err(|error| storage("invalidate projection output state for rebuild", error))?;
    Ok(())
}

async fn activate_rebuild_sessions(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    let mut conflicts = conn
        .query(
            "SELECT staged.provider, staged.session_id
             FROM observation_projection_rebuild_sessions AS staged
             JOIN sessions AS active
               ON active.provider = staged.provider AND active.session_id = staged.session_id
             WHERE staged.projector_version = ?1 AND staged.generation = ?2
               AND (
                 (active.project_key <> json_extract(staged.session_json, '$.project_key')
                   AND active.project_key <> 'user'
                   AND json_extract(staged.session_json, '$.project_key') <> 'user')
                 OR (active.project_path <> json_extract(staged.session_json, '$.project_path')
                   AND active.project_path <> active.project_key
                   AND json_extract(staged.session_json, '$.project_path')
                       <> json_extract(staged.session_json, '$.project_key'))
                 OR (active.transcript_path IS NOT NULL
                   AND json_extract(staged.session_json, '$.transcript_path') IS NOT NULL
                   AND active.transcript_path IS NOT json_extract(staged.session_json, '$.transcript_path'))
                 OR (active.parent_session_id IS NOT NULL
                   AND json_extract(staged.session_json, '$.parent_session_id') IS NOT NULL
                   AND active.parent_session_id IS NOT json_extract(staged.session_json, '$.parent_session_id'))
                 OR (active.agent_id IS NOT NULL
                   AND json_extract(staged.session_json, '$.agent_id') IS NOT NULL
                   AND active.agent_id IS NOT json_extract(staged.session_json, '$.agent_id'))
                 OR (active.parent_tool_use_id IS NOT NULL
                   AND json_extract(staged.session_json, '$.parent_tool_use_id') IS NOT NULL
                   AND active.parent_tool_use_id IS NOT json_extract(staged.session_json, '$.parent_tool_use_id'))
                 OR (active.metadata_json IS NOT NULL
                   AND json_extract(staged.session_json, '$.metadata_json') IS NOT NULL
                   AND active.metadata_json IS NOT json_extract(staged.session_json, '$.metadata_json')
                   AND (
                     json_valid(active.metadata_json) = 0
                     OR json_valid(json_extract(staged.session_json, '$.metadata_json')) = 0
                     OR json_type(active.metadata_json) <> 'object'
                     OR json_type(json_extract(staged.session_json, '$.metadata_json')) <> 'object'
                     OR EXISTS (
                       SELECT 1
                       FROM json_each(json_extract(staged.session_json, '$.metadata_json')) AS expected
                       JOIN json_each(active.metadata_json) AS actual USING (key)
                       WHERE expected.key NOT IN ('source', 'usage')
                         AND actual.value IS NOT expected.value
                     )
                   ))
               )
             LIMIT 1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
        )
        .await
        .map_err(|error| storage("validate staged projection sessions", error))?;
    if let Some(row) = conflicts
        .next()
        .await
        .map_err(|error| storage("validate staged projection sessions", error))?
    {
        return Err(ProjectionStoreError::OutputCollision {
            provider: row
                .get(0)
                .map_err(|error| storage("validate staged projection sessions", error))?,
            message_id: format!(
                "session:{}",
                row.get::<String>(1)
                    .map_err(|error| storage("validate staged projection sessions", error))?
            ),
        });
    }
    drop(conflicts);
    conn.execute(
        "INSERT INTO sessions (
            provider, session_id, project_key, project_path, title, started_at, ended_at,
            transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
            parent_tool_use_id
         )
         SELECT provider, session_id,
                json_extract(session_json, '$.project_key'),
                json_extract(session_json, '$.project_path'),
                json_extract(session_json, '$.title'),
                json_extract(session_json, '$.started_at'),
                json_extract(session_json, '$.ended_at'),
                json_extract(session_json, '$.transcript_path'),
                json_extract(session_json, '$.metadata_json'),
                json_extract(session_json, '$.parent_session_id'),
                json_extract(session_json, '$.is_subagent'),
                json_extract(session_json, '$.agent_id'),
                json_extract(session_json, '$.parent_tool_use_id')
         FROM observation_projection_rebuild_sessions
         WHERE projector_version = ?1 AND generation = ?2
         ON CONFLICT(provider, session_id) DO UPDATE SET
            project_key = CASE
              WHEN sessions.project_key = 'user' THEN excluded.project_key
              ELSE sessions.project_key END,
            project_path = CASE
              WHEN sessions.project_path = sessions.project_key THEN excluded.project_path
              ELSE sessions.project_path END,
            title = COALESCE(sessions.title, excluded.title),
            started_at = CASE
              WHEN sessions.started_at IS NULL THEN excluded.started_at
              WHEN excluded.started_at IS NULL THEN sessions.started_at
              ELSE MIN(sessions.started_at, excluded.started_at) END,
            ended_at = CASE
              WHEN sessions.ended_at IS NULL THEN excluded.ended_at
              WHEN excluded.ended_at IS NULL THEN sessions.ended_at
              ELSE MAX(sessions.ended_at, excluded.ended_at) END,
            transcript_path = COALESCE(sessions.transcript_path, excluded.transcript_path),
            metadata_json = CASE
              WHEN sessions.metadata_json IS NULL THEN excluded.metadata_json
              WHEN excluded.metadata_json IS NULL THEN sessions.metadata_json
              ELSE json_patch(excluded.metadata_json, sessions.metadata_json) END,
            parent_session_id = COALESCE(sessions.parent_session_id, excluded.parent_session_id),
            is_subagent = MAX(sessions.is_subagent, excluded.is_subagent),
            agent_id = COALESCE(sessions.agent_id, excluded.agent_id),
            parent_tool_use_id = COALESCE(
              sessions.parent_tool_use_id, excluded.parent_tool_use_id
            )",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("activate rebuilt projection sessions", error))
}

async fn prepare_rebuild_output_activation(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.observation_projection_rebuild_preexisting_outputs;
         CREATE TEMP TABLE observation_projection_rebuild_preexisting_outputs (
            output_provider TEXT NOT NULL,
            output_message_id TEXT NOT NULL,
            active_exists INTEGER NOT NULL CHECK(active_exists IN (0, 1)),
            current_owned INTEGER NOT NULL CHECK(current_owned IN (0, 1)),
            cross_owned INTEGER NOT NULL CHECK(cross_owned IN (0, 1)),
            staged_created INTEGER NOT NULL CHECK(staged_created IN (0, 1)),
            PRIMARY KEY(output_provider, output_message_id)
         ) WITHOUT ROWID;",
    )
    .await
    .map_err(|error| storage("prepare staged projection output activation", error))?;
    conn.execute(
        "INSERT INTO temp.observation_projection_rebuild_preexisting_outputs (
            output_provider, output_message_id, active_exists,
            current_owned, cross_owned, staged_created
         )
         SELECT staged.output_provider, staged.output_message_id,
                EXISTS (
                  SELECT 1 FROM session_messages AS active
                  WHERE active.provider = staged.output_provider
                    AND active.message_id = staged.output_message_id
                ),
                EXISTS (
                  SELECT 1 FROM observation_projection_provenance AS owner
                  WHERE owner.projector_version = ?1 AND owner.message_created = 1
                    AND owner.output_provider = staged.output_provider
                    AND owner.output_message_id = staged.output_message_id
                ),
                EXISTS (
                  SELECT 1 FROM observation_projection_provenance AS owner
                  WHERE owner.projector_version <> ?1
                    AND owner.output_provider = staged.output_provider
                    AND owner.output_message_id = staged.output_message_id
                ),
                EXISTS (
                  SELECT 1 FROM observation_projection_rebuild_provenance AS owner
                  WHERE owner.projector_version = ?1 AND owner.generation = ?2
                    AND owner.message_created = 1
                    AND owner.output_provider = staged.output_provider
                    AND owner.output_message_id = staged.output_message_id
                )
         FROM observation_projection_rebuild_messages AS staged
         WHERE staged.projector_version = ?1 AND staged.generation = ?2",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
    )
    .await
    .map_err(|error| storage("materialize preexisting projection outputs", error))?;

    let mut conflicts = conn
        .query(
            "SELECT staged.output_provider, staged.output_message_id
             FROM observation_projection_rebuild_messages AS staged
             JOIN temp.observation_projection_rebuild_preexisting_outputs AS ownership
               ON ownership.output_provider = staged.output_provider
              AND ownership.output_message_id = staged.output_message_id
             LEFT JOIN session_messages AS active
               ON active.provider = staged.output_provider
              AND active.message_id = staged.output_message_id
             WHERE staged.projector_version = ?1 AND staged.generation = ?2
               AND (
                 (ownership.active_exists = 0 AND ownership.staged_created = 0)
                 OR (
                   ownership.active_exists = 1
                   AND NOT (ownership.current_owned = 1 AND ownership.cross_owned = 0)
                   AND (
                     active.session_id IS NOT json_extract(staged.message_json, '$.session_id')
                     OR active.role IS NOT json_extract(staged.message_json, '$.role')
                     OR active.timestamp IS NOT json_extract(staged.message_json, '$.timestamp')
                     OR active.ordinal IS NOT json_extract(staged.message_json, '$.ordinal')
                     OR active.text IS NOT json_extract(staged.message_json, '$.text')
                     OR active.kind IS NOT json_extract(staged.message_json, '$.kind')
                     OR active.model IS NOT json_extract(staged.message_json, '$.model')
                     OR active.tool_names IS NOT json_extract(staged.message_json, '$.tool_names')
                     OR active.source_path IS NOT json_extract(staged.message_json, '$.source_path')
                     OR active.source_offset IS NOT json_extract(staged.message_json, '$.source_offset')
                     OR active.metadata_json IS NOT json_extract(staged.message_json, '$.metadata_json')
                   )
                 )
               )
             LIMIT 1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
        )
        .await
        .map_err(|error| storage("validate staged projection outputs", error))?;
    if let Some(row) = conflicts
        .next()
        .await
        .map_err(|error| storage("validate staged projection outputs", error))?
    {
        return Err(ProjectionStoreError::OutputCollision {
            provider: row
                .get(0)
                .map_err(|error| storage("validate staged projection outputs", error))?,
            message_id: row
                .get(1)
                .map_err(|error| storage("validate staged projection outputs", error))?,
        });
    }
    Ok(())
}

async fn activate_rebuild_messages(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "INSERT INTO session_messages (
            provider, message_id, session_id, role, timestamp, ordinal, text, kind,
            model, tool_names, source_path, source_offset, metadata_json
         )
         SELECT output_provider, output_message_id,
                json_extract(message_json, '$.session_id'),
                json_extract(message_json, '$.role'),
                json_extract(message_json, '$.timestamp'),
                json_extract(message_json, '$.ordinal'),
                json_extract(message_json, '$.text'),
                json_extract(message_json, '$.kind'),
                json_extract(message_json, '$.model'),
                json_extract(message_json, '$.tool_names'),
                json_extract(message_json, '$.source_path'),
                json_extract(message_json, '$.source_offset'),
                json_extract(message_json, '$.metadata_json')
         FROM observation_projection_rebuild_messages
         WHERE projector_version = ?1 AND generation = ?2
         ON CONFLICT(provider, message_id) DO UPDATE SET
            session_id = excluded.session_id,
            role = excluded.role,
            timestamp = excluded.timestamp,
            ordinal = excluded.ordinal,
            text = excluded.text,
            kind = excluded.kind,
            model = excluded.model,
            tool_names = excluded.tool_names,
            source_path = excluded.source_path,
            source_offset = excluded.source_offset,
            metadata_json = excluded.metadata_json
         WHERE session_messages.session_id IS NOT excluded.session_id
            OR session_messages.role IS NOT excluded.role
            OR session_messages.timestamp IS NOT excluded.timestamp
            OR session_messages.ordinal IS NOT excluded.ordinal
            OR session_messages.text IS NOT excluded.text
            OR session_messages.kind IS NOT excluded.kind
            OR session_messages.model IS NOT excluded.model
            OR session_messages.tool_names IS NOT excluded.tool_names
            OR session_messages.source_path IS NOT excluded.source_path
            OR session_messages.source_offset IS NOT excluded.source_offset
            OR session_messages.metadata_json IS NOT excluded.metadata_json",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
    )
    .await
    .map_err(|error| storage("activate rebuilt projection messages", error))?;
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp, content,
            content_hash, storage_kind, payload_ref, snippet_text, index_text,
            legacy_source, legacy_truncated, metadata_json
         )
         SELECT output_provider, output_message_id,
                json_extract(message_json, '$.session_id'),
                json_extract(message_json, '$.role'),
                json_extract(message_json, '$.ordinal'),
                json_extract(message_json, '$.timestamp'),
                json_extract(message_json, '$.text'), content_hash, 'inline', NULL,
                snippet_text, index_text, 0, 0,
                json_extract(message_json, '$.metadata_json')
         FROM observation_projection_rebuild_messages
         WHERE projector_version = ?1 AND generation = ?2 AND output_provider <> 'hermes'
         ON CONFLICT(provider, message_id) DO UPDATE SET
            session_id = excluded.session_id,
            role = excluded.role,
            ordinal = excluded.ordinal,
            timestamp = excluded.timestamp,
            content = excluded.content,
            content_hash = excluded.content_hash,
            storage_kind = excluded.storage_kind,
            payload_ref = excluded.payload_ref,
            snippet_text = excluded.snippet_text,
            index_text = excluded.index_text,
            legacy_source = 0,
            legacy_truncated = 0,
            metadata_json = excluded.metadata_json",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("activate rebuilt projected LCM raw messages", error))
}

async fn activate_rebuild_provenance(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    let mut conflicts = conn
        .query(
            "SELECT 1
             FROM observation_projection_rebuild_provenance AS staged
             JOIN observation_projection_provenance AS active
               ON active.projector_version = staged.projector_version
              AND active.observation_id = staged.observation_id
              AND active.output_ordinal = staged.output_ordinal
             WHERE staged.projector_version = ?1 AND staged.generation = ?2
               AND (active.retrieval_anchor_id <> staged.retrieval_anchor_id
                 OR active.receipt_id <> staged.receipt_id
                 OR active.output_provider <> staged.output_provider
                 OR active.output_message_id <> staged.output_message_id
                 OR active.output_digest <> staged.output_digest)
             LIMIT 1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
        )
        .await
        .map_err(|error| storage("validate staged projection provenance", error))?;
    if conflicts
        .next()
        .await
        .map_err(|error| storage("validate staged projection provenance", error))?
        .is_some()
    {
        return Err(ProjectionStoreError::ProvenanceCollision);
    }
    drop(conflicts);
    conn.execute(
        "INSERT OR IGNORE INTO observation_projection_provenance (
            projector_version, observation_id, output_ordinal, retrieval_anchor_id,
            receipt_id, output_provider, output_message_id, output_digest, message_created
         )
         SELECT staged.projector_version, staged.observation_id, staged.output_ordinal,
                staged.retrieval_anchor_id, staged.receipt_id, staged.output_provider,
                staged.output_message_id, staged.output_digest, staged.message_created
         FROM observation_projection_rebuild_provenance AS staged
         WHERE staged.projector_version = ?1 AND staged.generation = ?2",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
    )
    .await
    .map_err(|error| storage("activate rebuilt projection provenance", error))?;
    conn.execute(
        "DELETE FROM temp.observation_projection_output_state_meta",
        (),
    )
    .await
    .map_err(|error| storage("invalidate activated projection output state", error))?;
    ensure_projection_output_state_cache(conn).await
}

async fn activate_rebuild_workflow_facts(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "INSERT INTO observation_workflow_facts (
            projector_version, observation_id, fact_ordinal, retrieval_anchor_id, receipt_id,
            observation_sequence, provider, session_id, semantic_kind, provider_reference, item_id,
            parent_reference, list_reference, state, status, item_order, native_revision,
            event_sequence, source_sequence, native_timestamp, ordering_domain, content_json,
            content_text, output_digest
         )
         SELECT projector_version, observation_id, fact_ordinal, retrieval_anchor_id,
                receipt_id, observation_sequence, provider, session_id, semantic_kind,
                provider_reference, item_id, parent_reference, list_reference, state, status,
                item_order, native_revision, event_sequence, source_sequence, native_timestamp,
                ordering_domain, content_json, content_text, output_digest
         FROM observation_projection_rebuild_workflow_facts
         WHERE projector_version = ?1 AND generation = ?2",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("activate rebuilt projection workflow facts", error))
}

async fn activate_rebuild_dispositions(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "INSERT INTO observation_projection_dispositions (
            projector_version, observation_id, receipt_id, reason
         )
         SELECT projector_version, observation_id, receipt_id, reason
         FROM observation_projection_rebuild_dispositions
         WHERE projector_version = ?1 AND generation = ?2",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("activate rebuilt projection dispositions", error))
}
