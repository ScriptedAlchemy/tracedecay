use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_domain::{CanonicalObservationIdV1, DurableObservationV1};
use tracedecay_lcm::retrieval_content::projected_content_hash;
use tracedecay_runtime_core::db::{
    Database,
    engine::{Executor, QueryExecutor, Row, params},
};
use tracedecay_store::{
    ObservationProjection, PROVIDER_USAGE_PROJECTOR_VERSION, ProjectedObservation,
    ProjectionBatchItem, ProjectionDrainBatch, ProjectionPersistOutcome,
    ProjectionPredecessorConvergence, ProjectionRebuildOutcome, ProjectionSkipReason,
    ProjectionStoreError, ProjectionStoreResult, SESSION_MESSAGE_PROJECTOR_VERSION,
    SESSION_MESSAGE_PROJECTOR_VERSION_V4, SessionMessageProjection, SessionMessageRecord,
    SessionRecord, WorkflowFactProjection, workflow_semantic_kind,
};

use super::apply::{
    apply_effect, apply_skip_disposition, derive_projection_for_rebuild,
    derive_projection_with_alias, stage_provider_usage_effects, verify_effect,
};
use super::state::{
    canonicalize_session_project_paths, consume_projection_queue_item, decode_observation_row,
    decode_sequence, ensure_projection_output_state_cache, projection_retry_state, queued_sequence,
    read_checkpoint, read_message, read_observation, read_session,
    reaggregate_output_state_for_output, reconcile_session_rows_detailed,
    schedule_projection_retry, storage, storage_message, write_checkpoint,
};
use super::transition::{
    MessageTransition, MessageTransitionState, WorkflowFactTarget, WorkflowFactTransition,
    message_transition, write_workflow_fact_transition,
};
use tracedecay_session_temporal_store::record_canonical_observation_effect;

pub(super) const REBUILD_PAGE_SIZE: i64 = 128;
const REBUILD_MAX_STEPS_PER_INVOCATION: usize = 4;
const PROJECTION_RETRY_BASE_MICROS: i64 = 5_000_000;
const PROJECTION_RETRY_MAX_MICROS: i64 = 300_000_000;
static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

const SESSION_JSON_COLUMN: &str = "session_json";
const MERGED_SESSION_JSON_COLUMN: &str = "merged.value";
const MESSAGE_JSON_COLUMN: &str = "message_json";
const STAGED_MESSAGE_JSON_COLUMN: &str = "staged.message_json";

const SESSION_JSON_FIELDS: &[&str] = &[
    "project_key",
    "project_path",
    "title",
    "started_at",
    "ended_at",
    "transcript_path",
    "metadata_json",
    "parent_session_id",
    "is_subagent",
    "agent_id",
    "parent_tool_use_id",
];

const MESSAGE_JSON_FIELDS: &[&str] = &[
    "session_id",
    "role",
    "timestamp",
    "ordinal",
    "text",
    "kind",
    "model",
    "tool_names",
    "source_path",
    "source_offset",
    "metadata_json",
];

fn json_extract_expr(column: &str, field: &str) -> String {
    format!("json_extract({column}, '$.{field}')")
}

fn json_extract_select_list(column: &str, fields: &[&str]) -> String {
    fields
        .iter()
        .map(|field| json_extract_expr(column, field))
        .collect::<Vec<_>>()
        .join(",\n                ")
}

fn json_extract_neq_predicates(left_alias: &str, json_column: &str, fields: &[&str]) -> String {
    fields
        .iter()
        .map(|field| {
            format!(
                "{left_alias}.{field} IS NOT {}",
                json_extract_expr(json_column, field)
            )
        })
        .collect::<Vec<_>>()
        .join("\n                     OR ")
}

/// Projects one queued observation through the guarded registered database
/// client. The transaction remains bound to that client for its whole life;
/// no physical engine handle escapes the runtime boundary.
#[hotpath::measure(
    future = true,
    label = "global_db.observation_projection.persist.project"
)]
pub async fn project_observation(
    database: &Database,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<ProjectionPersistOutcome> {
    crate::hotpath_observe::record_transaction_rows(1);
    let transaction = database
        .begin_write_transaction("begin projection transaction")
        .await
        .map_err(|error| storage("begin projection transaction", error))?;
    let now_micros = tracedecay_contracts::clock::now_micros().0;
    if let Some(retry) = projection_retry_state(&transaction, observation_id).await?
        && retry.next_retry_at_micros > now_micros
    {
        transaction
            .rollback()
            .await
            .map_err(|error| storage("rollback deferred projection transaction", error))?;
        return Err(ProjectionStoreError::RetryDeferred {
            attempt_count: retry.attempt_count,
            next_retry_at_micros: retry.next_retry_at_micros,
            last_error: retry.last_error.ok_or_else(|| {
                storage_message(
                    "read deferred projection retry",
                    "deferred projection retry has no failure detail",
                )
            })?,
        });
    }
    match project_observation_in_transaction(&transaction, observation_id).await {
        Ok(outcome) => match transaction.commit().await {
            Ok(()) => Ok(outcome),
            Err(commit_error) => {
                let error = storage("commit projection transaction", commit_error);
                persist_projection_retry_on_database(
                    database,
                    observation_id,
                    now_micros,
                    &error.durable_detail(),
                )
                .await?;
                Err(error)
            }
        },
        Err(error) => {
            transaction.rollback().await.map_err(|rollback_error| {
                storage("rollback failed projection transaction", rollback_error)
            })?;
            if matches!(error, ProjectionStoreError::Storage { .. }) {
                persist_projection_retry_on_database(
                    database,
                    observation_id,
                    now_micros,
                    &error.durable_detail(),
                )
                .await?;
            } else if matches!(error, ProjectionStoreError::Contract(_)) {
                persist_projection_rejection_on_database(
                    database,
                    observation_id,
                    ProjectionSkipReason::InvalidContract,
                )
                .await?;
            } else if matches!(error, ProjectionStoreError::SanitizationRefused { .. }) {
                persist_projection_rejection_on_database(
                    database,
                    observation_id,
                    ProjectionSkipReason::SanitizationRefused,
                )
                .await?;
            }
            Err(error)
        }
    }
}

/// Projects up to `max` ready queue-head observations inside one write
/// transaction, in strict sequence order, committing once for the window.
///
/// Each item runs the exact per-item projection (`project_observation_in_transaction`
/// with its gap, queue, and duplicate checks). A retry-deferred queue head
/// stops the window before consuming it, mirroring the scalar head-of-queue
/// gate. Any per-item error rolls the whole window back and surfaces the
/// error, so callers fall back to per-item draining whose durable retry and
/// skip dispositions stay authoritative for failures.
#[hotpath::measure(
    future = true,
    label = "global_db.observation_projection.persist.project_window"
)]
pub async fn project_queued_observations(
    database: &Database,
    max: usize,
) -> ProjectionStoreResult<ProjectionDrainBatch> {
    if max == 0 {
        return Ok(ProjectionDrainBatch::default());
    }
    let transaction = database
        .begin_write_transaction("begin projection window transaction")
        .await
        .map_err(|error| storage("begin projection window transaction", error))?;
    let now_micros = tracedecay_contracts::clock::now_micros().0;
    let mut items = Vec::new();
    while items.len() < max {
        let Some(observation_id) = next_ready_projection_head(&transaction, now_micros).await?
        else {
            break;
        };
        // Boxed for the same reason as the scalar drain: the collision-guarded
        // apply subtree overflows the base-opt worker stack when inlined.
        match Box::pin(project_observation_in_transaction_with_session(
            &transaction,
            &observation_id,
        ))
        .await
        {
            Ok((outcome, session_id)) => items.push(ProjectionBatchItem {
                outcome,
                session_id,
            }),
            Err(error) => {
                transaction.rollback().await.map_err(|rollback_error| {
                    storage("rollback failed projection window", rollback_error)
                })?;
                return Err(error);
            }
        }
    }
    let has_more = projection_queue_has_items(&transaction).await?;
    crate::hotpath_observe::record_transaction_rows(items.len() as u64);
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection window transaction", error))?;
    Ok(ProjectionDrainBatch { items, has_more })
}

/// Ready queue head under the same gate as the runtime's
/// `NextQueuedProjection` read: strict minimum sequence, retry deadline
/// passed, and no active projection rebuild generation.
async fn next_ready_projection_head(
    conn: &impl QueryExecutor,
    now_micros: i64,
) -> ProjectionStoreResult<Option<CanonicalObservationIdV1>> {
    let mut rows = conn
        .query(
            "SELECT observation_id FROM projection_queue
             WHERE next_retry_at_micros <= ?2
               AND observation_sequence = (
                 SELECT MIN(observation_sequence) FROM projection_queue
               )
               AND NOT EXISTS (
                 SELECT 1 FROM observation_projection_rebuilds
                 WHERE projector_version = ?1
               )
             LIMIT 1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION, now_micros],
        )
        .await
        .map_err(|error| storage("read projection queue head", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read projection queue head", error))?
    else {
        return Ok(None);
    };
    let observation_id = row
        .get::<String>(0)
        .map_err(|error| storage("decode projection queue head", error))?;
    Ok(Some(
        CanonicalObservationIdV1::new(observation_id).map_err(ProjectionStoreError::Contract)?,
    ))
}

async fn projection_queue_has_items(conn: &impl QueryExecutor) -> ProjectionStoreResult<bool> {
    let mut rows = conn
        .query("SELECT 1 FROM projection_queue LIMIT 1", ())
        .await
        .map_err(|error| storage("probe projection queue", error))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| storage("probe projection queue", error))?
        .is_some())
}

fn projection_retry_delay_micros(attempt_count: u32) -> i64 {
    let shift = attempt_count.saturating_sub(1).min(16);
    PROJECTION_RETRY_BASE_MICROS
        .saturating_mul(1_i64 << shift)
        .min(PROJECTION_RETRY_MAX_MICROS)
}

async fn persist_projection_retry_on_database(
    database: &Database,
    observation_id: &CanonicalObservationIdV1,
    now_micros: i64,
    last_error: &str,
) -> ProjectionStoreResult<()> {
    let transaction = database
        .begin_write_transaction("begin projection retry transaction")
        .await
        .map_err(|error| storage("begin projection retry transaction", error))?;
    let retry = projection_retry_state(&transaction, observation_id)
        .await?
        .ok_or(ProjectionStoreError::NotQueued)?;
    let attempt_count = retry.attempt_count.saturating_add(1);
    let next_retry_at_micros =
        now_micros.saturating_add(projection_retry_delay_micros(attempt_count));
    schedule_projection_retry(
        &transaction,
        observation_id,
        attempt_count,
        next_retry_at_micros,
        last_error,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection retry transaction", error))
}

async fn persist_projection_rejection_on_database(
    database: &Database,
    observation_id: &CanonicalObservationIdV1,
    reason: ProjectionSkipReason,
) -> ProjectionStoreResult<()> {
    let transaction = database
        .begin_write_transaction("begin projection rejection transaction")
        .await
        .map_err(|error| storage("begin projection rejection transaction", error))?;
    let checkpoint = read_checkpoint(&transaction).await?;
    let Some((sequence, observation)) = read_observation(&transaction, observation_id).await?
    else {
        return Err(ProjectionStoreError::ObservationNotFound);
    };
    let expected = checkpoint.last_sequence().saturating_add(1);
    if sequence > checkpoint.last_sequence() && sequence != expected {
        return Err(ProjectionStoreError::Gap {
            expected,
            actual: sequence,
        });
    }
    if queued_sequence(&transaction, observation_id).await? != Some(sequence) {
        return Err(ProjectionStoreError::NotQueued);
    }
    apply_skip_disposition(&transaction, &observation, reason).await?;
    consume_projection_queue_item(&transaction, observation_id).await?;
    if sequence > checkpoint.last_sequence() {
        write_checkpoint(&transaction, sequence).await?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection rejection transaction", error))
}

#[hotpath::measure(
    future = true,
    label = "global_db.observation_projection.persist.rebuild"
)]
pub async fn rebuild_projection(
    database: &Database,
    frontier_sequence: u64,
) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
    rebuild_projection_until_cancelled(database, frontier_sequence, &NEVER_CANCELLED).await
}

/// Converges retained v4 output ownership before an ordinary v5 queue drain.
///
/// The predecessor probe and first rebuild generation are bound in one write
/// transaction. Once a generation exists, later calls resume its frozen
/// frontier instead of replacing it with a moving committed frontier. Each
/// invocation performs only the ordinary bounded rebuild step budget.
#[hotpath::measure(
    future = true,
    label = "global_db.observation_projection.persist.converge"
)]
pub async fn converge_projection_predecessor(
    database: &Database,
) -> ProjectionStoreResult<ProjectionPredecessorConvergence> {
    let Some(frontier_sequence) = prepare_predecessor_projection_rebuild(database).await? else {
        return Ok(ProjectionPredecessorConvergence::Current);
    };
    let outcome =
        advance_projection_rebuild_with_budget(database, frontier_sequence, &NEVER_CANCELLED)
            .await?;
    Ok(ProjectionPredecessorConvergence::RebuildRequired(outcome))
}

async fn rebuild_projection_until_cancelled(
    database: &Database,
    frontier_sequence: u64,
    cancelled: &AtomicBool,
) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
    prepare_projection_rebuild(database, frontier_sequence).await?;
    advance_projection_rebuild_with_budget(database, frontier_sequence, cancelled).await
}

async fn advance_projection_rebuild_with_budget(
    database: &Database,
    frontier_sequence: u64,
    cancelled: &AtomicBool,
) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
    for _ in 0..REBUILD_MAX_STEPS_PER_INVOCATION {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        match advance_projection_rebuild(database, frontier_sequence, cancelled).await? {
            RebuildAdvance::Pending => {}
            RebuildAdvance::Complete(outcome) => return Ok(outcome),
        }
    }
    projection_rebuild_progress_on(&database.read_connection()).await
}

async fn prepare_predecessor_projection_rebuild(
    database: &Database,
) -> ProjectionStoreResult<Option<u64>> {
    let transaction = database
        .begin_write_transaction("begin predecessor projection convergence")
        .await
        .map_err(|error| storage("begin predecessor projection convergence", error))?;
    let mut predecessor_rows = transaction
        .query(
            "SELECT 1 FROM observation_projection_provenance
             WHERE projector_version = ?1 LIMIT 1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION_V4],
        )
        .await
        .map_err(|error| storage("read predecessor projection ownership", error))?;
    let predecessor_present = predecessor_rows
        .next()
        .await
        .map_err(|error| storage("read predecessor projection ownership", error))?
        .is_some();
    drop(predecessor_rows);
    if !predecessor_present {
        transaction
            .commit()
            .await
            .map_err(|error| storage("commit predecessor projection probe", error))?;
        return Ok(None);
    }

    let frontier_sequence = match read_optional_rebuild_job(&transaction).await? {
        Some(job) => decode_sequence(job.frontier, "read predecessor rebuild frontier")?,
        None => read_observation_frontier(&transaction).await?,
    };
    start_or_resume_projection_rebuild_transaction(&transaction, frontier_sequence).await?;
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit predecessor projection convergence", error))?;
    Ok(Some(frontier_sequence))
}

async fn prepare_projection_rebuild(
    database: &Database,
    frontier_sequence: u64,
) -> ProjectionStoreResult<()> {
    let transaction = database
        .begin_write_transaction("begin projection rebuild staging")
        .await
        .map_err(|error| storage("begin projection rebuild staging", error))?;
    start_or_resume_projection_rebuild_transaction(&transaction, frontier_sequence).await?;
    transaction
        .commit()
        .await
        .map_err(|error| storage("commit projection rebuild staging", error))
}

async fn advance_projection_rebuild(
    database: &Database,
    frontier_sequence: u64,
    cancelled: &AtomicBool,
) -> ProjectionStoreResult<RebuildAdvance> {
    let job = read_rebuild_job(&database.read_connection()).await?;
    match job.state {
        RebuildState::Aliasing => {
            let transaction = database
                .begin_write_transaction("begin projection alias staging")
                .await
                .map_err(|error| storage("begin projection alias staging", error))?;
            stage_projection_alias_batch_transaction(&transaction).await?;
            transaction
                .commit()
                .await
                .map_err(|error| storage("commit projection alias batch", error))?;
            Ok(RebuildAdvance::Pending)
        }
        RebuildState::Building => {
            let transaction = database
                .begin_write_transaction("begin projection rebuild batch")
                .await
                .map_err(|error| storage("begin projection rebuild batch", error))?;
            let outcome =
                stage_projection_rebuild_batch_transaction(&transaction, cancelled).await?;
            let commit_operation = match outcome {
                RebuildBatchStage::AlreadyReady => "commit completed projection rebuild batch",
                RebuildBatchStage::Advanced => "commit projection rebuild batch",
            };
            transaction
                .commit()
                .await
                .map_err(|error| storage(commit_operation, error))?;
            Ok(RebuildAdvance::Pending)
        }
        RebuildState::Ready => {
            let transaction = database
                .begin_write_transaction("begin projection rebuild activation")
                .await
                .map_err(|error| storage("begin projection rebuild activation", error))?;
            let outcome =
                activate_projection_rebuild_transaction(&transaction, frontier_sequence).await?;
            transaction
                .commit()
                .await
                .map_err(|error| storage("commit projection rebuild activation", error))?;
            Ok(RebuildAdvance::Complete(outcome))
        }
    }
}

async fn project_observation_in_transaction(
    transaction: &impl Executor,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<ProjectionPersistOutcome> {
    project_observation_in_transaction_with_session(transaction, observation_id)
        .await
        .map(|(outcome, _)| outcome)
}

/// Like [`project_observation_in_transaction`], additionally returning the
/// observation's session identity so batched drains need no follow-up point
/// read per projected item.
async fn project_observation_in_transaction_with_session(
    transaction: &impl Executor,
    observation_id: &CanonicalObservationIdV1,
) -> ProjectionStoreResult<(ProjectionPersistOutcome, String)> {
    ensure_projection_output_state_cache(transaction).await?;
    let checkpoint = read_checkpoint(transaction).await?;
    let Some((sequence, observation)) = read_observation(transaction, observation_id).await? else {
        return Err(ProjectionStoreError::ObservationNotFound);
    };
    let session_id = observation.source().session_id().as_str().to_owned();
    let mut effect = derive_projection_with_alias(transaction, &observation).await?;
    if sequence <= checkpoint.last_sequence() {
        verify_effect(transaction, &observation, &effect).await?;
        if !matches!(
            effect,
            ObservationProjection::Skipped(
                ProjectionSkipReason::InvalidContract
                    | ProjectionSkipReason::NativeSourceSuperseded
            )
        ) {
            record_canonical_observation_effect(transaction, sequence, &observation, &effect)
                .await?;
        }
        consume_projection_queue_item(transaction, observation_id).await?;
        return Ok((
            ProjectionPersistOutcome::ExactDuplicate(checkpoint),
            session_id,
        ));
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

    // Boxed: the collision-guarded write composes the whole apply subtree;
    // inlining it into the drain future overflows the base-opt worker stack.
    Box::pin(write_effect_converging_collisions(
        transaction,
        &CollisionGuardedWrite::Drain,
        sequence,
        &observation,
        &mut effect,
    ))
    .await?;
    if !matches!(
        effect,
        ObservationProjection::Skipped(
            ProjectionSkipReason::InvalidContract | ProjectionSkipReason::NativeSourceSuperseded
        )
    ) {
        record_canonical_observation_effect(transaction, sequence, &observation, &effect).await?;
    }
    consume_projection_queue_item(transaction, observation_id).await?;
    let checkpoint = write_checkpoint(transaction, sequence).await?;
    let output_count = effect.output_count();
    let outcome = match effect {
        ObservationProjection::Message(_) | ObservationProjection::Composite { .. } => {
            ProjectionPersistOutcome::Projected(ProjectedObservation::new(checkpoint, output_count))
        }
        ObservationProjection::Skipped(reason) => {
            ProjectionPersistOutcome::Skipped { checkpoint, reason }
        }
    };
    Ok((outcome, session_id))
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
        if !matches!(
            effect,
            ObservationProjection::Skipped(
                ProjectionSkipReason::InvalidContract
                    | ProjectionSkipReason::NativeSourceSuperseded
            )
        ) {
            record_canonical_observation_effect(transaction, sequence, &observation, &effect)
                .await?;
        }
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
    Ok(RebuildBatchStage::Advanced)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RebuildBatchStage {
    AlreadyReady,
    Advanced,
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
    activate_rebuild_provider_usage(transaction, &job.generation).await?;
    activate_rebuild_dispositions(transaction, &job.generation).await?;
    super::source_transition::activate_native_source_transitions(transaction, &job.generation)
        .await?;

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
    #[hotpath::skip]
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
            converge_collided_effect(conn, write, sequence, observation, effect).await
        }
        Err(error) => Err(error),
    }
}

/// Collision convergence: roll the guarded write back, substitute the durable
/// `OutputCollision` skip, and re-run so the drain or rebuild checkpoints past
/// the collided observation.
///
/// A live drain additionally reconciles any provenance rows already durable
/// under the collided observation's key: the skip authority contract
/// (`schema_contract::invariants`) defines a valid skip as zero provenance
/// rows plus exactly one disposition, so a stale row left behind by an
/// earlier projection era must not survive next to the skip it contradicts.
/// The projected-output ownership cache is re-aggregated for every output the
/// removed rows touched.
async fn converge_collided_effect(
    conn: &impl Executor,
    write: &CollisionGuardedWrite<'_>,
    sequence: u64,
    observation: &DurableObservationV1,
    effect: &mut ObservationProjection,
) -> ProjectionStoreResult<()> {
    conn.execute_batch(&format!(
        "ROLLBACK TO {PROJECTION_COLLISION_SAVEPOINT}; \
         RELEASE {PROJECTION_COLLISION_SAVEPOINT};"
    ))
    .await
    .map_err(|error| storage("rollback projection collision savepoint", error))?;
    if matches!(write, CollisionGuardedWrite::Drain) {
        reconcile_collided_observation_provenance(conn, observation).await?;
    }
    *effect = ObservationProjection::Skipped(ProjectionSkipReason::OutputCollision);
    write.run(conn, sequence, observation, effect).await
}

/// Removes provenance rows durable under the collided observation's key and
/// re-aggregates the temp ownership cache for the outputs they bound.
async fn reconcile_collided_observation_provenance(
    conn: &impl Executor,
    observation: &DurableObservationV1,
) -> ProjectionStoreResult<()> {
    let observation_id = observation.observation_id().as_str();
    let mut affected = Vec::new();
    let mut rows = conn
        .query(
            "SELECT DISTINCT output_provider, output_message_id
             FROM observation_projection_provenance
             WHERE projector_version = ?1 AND observation_id = ?2",
            params![SESSION_MESSAGE_PROJECTOR_VERSION, observation_id],
        )
        .await
        .map_err(|error| storage("read collided projection provenance", error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage("read collided projection provenance", error))?
    {
        affected.push((
            row.get::<String>(0)
                .map_err(|error| storage("read collided projection provenance", error))?,
            row.get::<String>(1)
                .map_err(|error| storage("read collided projection provenance", error))?,
        ));
    }
    drop(rows);
    if affected.is_empty() {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM observation_projection_provenance
         WHERE projector_version = ?1 AND observation_id = ?2",
        params![SESSION_MESSAGE_PROJECTOR_VERSION, observation_id],
    )
    .await
    .map_err(|error| storage("remove collided projection provenance", error))?;
    for (output_provider, output_message_id) in affected {
        reaggregate_output_state_for_output(conn, &output_provider, &output_message_id).await?;
    }
    Ok(())
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

    #[hotpath::skip]
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
/// them, a cursor left open past that point would otherwise pin the
/// connection's read snapshot and hide those subsequent writes.
async fn read_observation_frontier(conn: &impl QueryExecutor) -> ProjectionStoreResult<u64> {
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
    let expected = canonicalize_session_project_paths(expected);
    let actual =
        match read_staged_session(conn, generation, &expected.provider, &expected.session_id)
            .await?
        {
            Some(session) => Some(session),
            None => read_session(conn, &expected.provider, &expected.session_id).await?,
        };
    let session = match actual {
        Some(actual) => {
            reconcile_session_rows_detailed(&canonicalize_session_project_paths(&actual), &expected)
                .map_err(|conflict| ProjectionStoreError::SessionOutputCollision {
                    provider: expected.provider.clone(),
                    session_id: expected.session_id.clone(),
                    field: conflict.field(),
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
    conn.execute(
        "INSERT INTO observation_projection_rebuild_messages (
            projector_version, generation, output_provider, output_message_id,
            message_json, content_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(projector_version, generation, output_provider, output_message_id)
         DO UPDATE SET message_json = excluded.message_json,
                       content_hash = excluded.content_hash",
        params![
            SESSION_MESSAGE_PROJECTOR_VERSION,
            generation,
            message.provider.as_str(),
            message.message_id.as_str(),
            json.as_str(),
            content_hash.as_str(),
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
                COALESCE(MAX(CASE
                    WHEN projector_version <> ?1 AND projector_version <> ?2 THEN 1 ELSE 0
                END), 0)
             FROM observation_projection_provenance
             WHERE output_provider = ?3 AND output_message_id = ?4",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                SESSION_MESSAGE_PROJECTOR_VERSION_V4,
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
    let inserted = conn
        .execute(
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
    // A fresh insert wrote exactly the tuple above inside this transaction, so
    // only a conflicting pre-existing row can disagree with this derivation;
    // confine the verification read to conflicts.
    if inserted == 1 {
        return Ok(());
    }
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
    if effect.skip_reason() == Some(ProjectionSkipReason::NativeSourceSuperseded) {
        return stage_rebuild_disposition(
            conn,
            generation,
            observation,
            ProjectionSkipReason::NativeSourceSuperseded,
        )
        .await;
    }
    stage_provider_usage_effects(conn, generation, sequence, observation).await?;
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
    }?;
    if effect
        .skip_reason()
        .is_none_or(|reason| reason == ProjectionSkipReason::NonConversationalRecord)
    {
        super::source_transition::settle_native_source_transition(
            conn,
            observation,
            super::source_transition::SourceTransitionTarget::Staged(generation),
        )
        .await?;
    }
    Ok(())
}

async fn activate_rebuild_provider_usage(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO observation_provider_usage (
            projector_version, observation_id, usage_ordinal, receipt_id,
            observation_sequence, scope_kind, project_id, provider, model_json,
            native_scope, counter_semantics, counters_json, session_id, turn_id,
            message_id, request_id, native_kind, native_field, ordering_domain,
            source_start, source_end, native_timestamp
         )
         SELECT ?1, observation_id, usage_ordinal, receipt_id,
                observation_sequence, scope_kind, project_id, provider, model_json,
                native_scope, counter_semantics, counters_json, session_id, turn_id,
                message_id, request_id, native_kind, native_field, ordering_domain,
                source_start, source_end, native_timestamp
         FROM observation_projection_rebuild_provider_usage
         WHERE projector_version = ?2 AND generation = ?3",
        params![
            PROVIDER_USAGE_PROJECTOR_VERSION,
            SESSION_MESSAGE_PROJECTOR_VERSION,
            generation
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("activate rebuilt provider usage", error))
}

async fn clear_active_projection(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    ensure_projection_output_state_cache(conn).await?;
    retire_projection_predecessor_output_ownership(conn).await?;
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

async fn retire_projection_predecessor_output_ownership(
    conn: &impl Executor,
) -> ProjectionStoreResult<()> {
    conn.execute(
        "DELETE FROM observation_projection_provenance WHERE projector_version = ?1",
        params![SESSION_MESSAGE_PROJECTOR_VERSION_V4],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("retire predecessor projection provenance", error))
}

fn decode_overlapping_session(row: &Row) -> ProjectionStoreResult<SessionRecord> {
    macro_rules! cell {
        ($index:literal) => {
            row.get($index)
                .map_err(|error| storage("decode overlapping projection session", error))?
        };
        ($index:literal, $ty:ty) => {
            row.get::<$ty>($index)
                .map_err(|error| storage("decode overlapping projection session", error))?
        };
    }
    Ok(SessionRecord {
        provider: cell!(1),
        session_id: cell!(2),
        project_key: cell!(3),
        project_path: cell!(4),
        title: cell!(5),
        started_at: cell!(6),
        ended_at: cell!(7),
        transcript_path: cell!(8),
        metadata_json: cell!(9),
        parent_session_id: cell!(10),
        is_subagent: cell!(11, i64) != 0,
        agent_id: cell!(12),
        parent_tool_use_id: cell!(13),
    })
}

/// Write every reconciled overlap in one set-based statement. Rebuild
/// activation owns the database writer, so a per-row `UPDATE` loop would hold
/// admission for as long as the history is large; the merge itself already ran
/// in Rust, so each column is taken verbatim from the merged row.
async fn write_reconciled_sessions(
    conn: &impl Executor,
    merged: &[SessionRecord],
) -> ProjectionStoreResult<()> {
    if merged.is_empty() {
        return Ok(());
    }
    let rows = encode_json(&merged, "encode reconciled projection sessions")?;
    let session_extracts =
        json_extract_select_list(MERGED_SESSION_JSON_COLUMN, SESSION_JSON_FIELDS);
    let assignments = SESSION_JSON_FIELDS
        .iter()
        .map(|field| format!("{field} = excluded.{field}"))
        .collect::<Vec<_>>()
        .join(",\n            ");
    conn.execute(
        &format!(
            "INSERT INTO sessions (
            provider, session_id, project_key, project_path, title, started_at, ended_at,
            transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
            parent_tool_use_id
         )
         SELECT {}, {},
                {session_extracts}
         FROM json_each(?1) AS merged
         -- `WHERE true` disambiguates the upsert clause from a join constraint.
         WHERE true
         ON CONFLICT(provider, session_id) DO UPDATE SET
            {assignments}",
            json_extract_expr(MERGED_SESSION_JSON_COLUMN, "provider"),
            json_extract_expr(MERGED_SESSION_JSON_COLUMN, "session_id"),
        ),
        params![rows.as_str()],
    )
    .await
    .map(|_| ())
    .map_err(|error| storage("activate reconciled projection sessions", error))
}

/// Classify every staged session that already exists through
/// [`reconcile_session_rows_detailed`], the same authority live apply uses.
/// A parallel SQL predicate used to report those conflicts as message
/// `OutputCollision` values with `message_id = session:{id}`, which erased
/// the field and sent session conflicts down the message-skip path.
/// Paged by `(provider, session_id)` because the exact-SQL transport refuses a
/// result set past `MAX_QUERY_ROWS` (10_000 rows) or 64 MiB, and a rebuild
/// overlapping more history than that would fail activation instead of
/// reconciling it. Both the staged primary key and `sessions` are unique on
/// that pair, so one page's writes never move a later page's cursor.
async fn reconcile_overlapping_rebuild_sessions(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    let mut cursor: Option<(String, String)> = None;
    loop {
        let (after_provider, after_session) = match cursor.as_ref() {
            Some((provider, session_id)) => (Some(provider.as_str()), Some(session_id.as_str())),
            None => (None, None),
        };
        let mut overlaps = conn
            .query(
                "SELECT staged.session_json,
                    active.provider, active.session_id, active.project_key,
                    active.project_path, active.title, active.started_at,
                    active.ended_at, active.transcript_path, active.metadata_json,
                    active.parent_session_id, active.is_subagent, active.agent_id,
                    active.parent_tool_use_id
             FROM observation_projection_rebuild_sessions AS staged
             JOIN sessions AS active
               ON active.provider = staged.provider AND active.session_id = staged.session_id
             WHERE staged.projector_version = ?1 AND staged.generation = ?2
               AND (?3 IS NULL
                    OR staged.provider > ?3
                    OR (staged.provider = ?3 AND staged.session_id > ?4))
             ORDER BY staged.provider, staged.session_id
             LIMIT ?5",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    generation,
                    after_provider,
                    after_session,
                    REBUILD_PAGE_SIZE
                ],
            )
            .await
            .map_err(|error| storage("read overlapping projection sessions", error))?;
        let mut updates = Vec::new();
        let mut scanned = 0_i64;
        let mut last = None;
        while let Some(row) = overlaps
            .next()
            .await
            .map_err(|error| storage("read overlapping projection sessions", error))?
        {
            let staged_json: String = row
                .get(0)
                .map_err(|error| storage("read overlapping projection sessions", error))?;
            let staged: SessionRecord =
                decode_json(&staged_json, "decode staged projection session")?;
            let actual = decode_overlapping_session(&row)?;
            scanned += 1;
            last = Some((actual.provider.clone(), actual.session_id.clone()));
            let expected = canonicalize_session_project_paths(&staged);
            let normalized_actual = canonicalize_session_project_paths(&actual);
            let merged = reconcile_session_rows_detailed(&normalized_actual, &expected).map_err(
                |conflict| ProjectionStoreError::SessionOutputCollision {
                    provider: expected.provider.clone(),
                    session_id: expected.session_id.clone(),
                    field: conflict.field(),
                },
            )?;
            if merged != actual {
                updates.push(merged);
            }
        }
        drop(overlaps);
        write_reconciled_sessions(conn, &updates).await?;
        if scanned < REBUILD_PAGE_SIZE {
            return Ok(());
        }
        cursor = last;
    }
}

async fn activate_rebuild_sessions(
    conn: &impl Executor,
    generation: &str,
) -> ProjectionStoreResult<()> {
    reconcile_overlapping_rebuild_sessions(conn, generation).await?;
    let session_extracts = json_extract_select_list(SESSION_JSON_COLUMN, SESSION_JSON_FIELDS);
    conn.execute(
        &format!(
            "INSERT INTO sessions (
            provider, session_id, project_key, project_path, title, started_at, ended_at,
            transcript_path, metadata_json, parent_session_id, is_subagent, agent_id,
            parent_tool_use_id
         )
         SELECT staged.provider, staged.session_id,
                {session_extracts}
         FROM observation_projection_rebuild_sessions AS staged
         WHERE staged.projector_version = ?1 AND staged.generation = ?2
           AND NOT EXISTS (
             SELECT 1 FROM sessions AS active
             WHERE active.provider = staged.provider
               AND active.session_id = staged.session_id
           )"
        ),
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

    let message_conflicts =
        json_extract_neq_predicates("active", STAGED_MESSAGE_JSON_COLUMN, MESSAGE_JSON_FIELDS);
    let mut conflicts = conn
        .query(
            &format!(
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
                     {message_conflicts}
                   )
                 )
               )
             LIMIT 1"
            ),
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
    let message_extracts = json_extract_select_list(MESSAGE_JSON_COLUMN, MESSAGE_JSON_FIELDS);
    conn.execute(
        &format!(
            "INSERT INTO session_messages (
            provider, message_id, session_id, role, timestamp, ordinal, text, kind,
            model, tool_names, source_path, source_offset, metadata_json
         )
         SELECT output_provider, output_message_id,
                {message_extracts}
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
            OR session_messages.metadata_json IS NOT excluded.metadata_json"
        ),
        params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
    )
    .await
    .map_err(|error| storage("activate rebuilt projection messages", error))?;
    let lcm_session_id = json_extract_expr(MESSAGE_JSON_COLUMN, "session_id");
    let lcm_role = json_extract_expr(MESSAGE_JSON_COLUMN, "role");
    let lcm_ordinal = json_extract_expr(MESSAGE_JSON_COLUMN, "ordinal");
    let lcm_timestamp = json_extract_expr(MESSAGE_JSON_COLUMN, "timestamp");
    let lcm_text = json_extract_expr(MESSAGE_JSON_COLUMN, "text");
    let lcm_metadata = json_extract_expr(MESSAGE_JSON_COLUMN, "metadata_json");
    conn.execute(
        &format!(
            "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, timestamp, content,
            content_hash, storage_kind, payload_ref, placeholder_text, metadata_json
         )
         SELECT output_provider, output_message_id,
                {lcm_session_id},
                {lcm_role},
                {lcm_ordinal},
                {lcm_timestamp},
                {lcm_text}, content_hash, 'inline', NULL, NULL,
                {lcm_metadata}
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
            placeholder_text = excluded.placeholder_text,
            metadata_json = excluded.metadata_json"
        ),
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod activation_tests {
    use super::{
        REBUILD_PAGE_SIZE, SESSION_MESSAGE_PROJECTOR_VERSION, activate_rebuild_sessions,
        reconcile_overlapping_rebuild_sessions,
    };
    use crate::tests::harness::RegisteredGlobalDbHarness;
    use tracedecay_runtime_core::db::engine::{Executor, params};
    use tracedecay_store::{ProjectionStoreError, SessionRecord};

    const SESSION_ID: &str = "002bd803-dc62-46e2-b66a-a61cc282f0dc";
    /// One past the exact-SQL transport's `MAX_QUERY_ROWS`
    /// (`tracedecay-rusqlite-runtime/src/exact_sql/mod.rs`), which refuses a
    /// result set rather than truncating it.
    const OVERLAPS_PAST_TRANSPORT_ROW_CAP: i64 = 10_001;

    fn session(transcript_path: Option<&str>, title: Option<&str>) -> SessionRecord {
        SessionRecord {
            provider: "cursor".to_owned(),
            session_id: SESSION_ID.to_owned(),
            project_key: "project.fixture".to_owned(),
            project_path: "project.fixture".to_owned(),
            title: title.map(str::to_owned),
            started_at: Some(1),
            ended_at: Some(2),
            transcript_path: transcript_path.map(str::to_owned),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        }
    }

    async fn stage(transaction: &impl Executor, generation: &str, staged: &SessionRecord) {
        transaction
            .execute(
                "INSERT INTO observation_projection_rebuilds (
                    projector_version, generation, frontier_sequence, state
                 ) VALUES (?1, ?2, 0, 'ready')",
                params![SESSION_MESSAGE_PROJECTOR_VERSION, generation],
            )
            .await
            .unwrap();
        transaction
            .execute(
                "INSERT INTO observation_projection_rebuild_sessions (
                    projector_version, generation, provider, session_id, session_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    generation,
                    staged.provider.as_str(),
                    staged.session_id.as_str(),
                    serde_json::to_string(staged).unwrap().as_str(),
                ],
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn overlap_reconciliation_pages_past_the_transport_row_cap() {
        const GENERATION: &str = "generation.session-overlap-paging";
        let harness = RegisteredGlobalDbHarness::open("session-overlap-paging").await;
        let mut staged_rows = Vec::with_capacity(OVERLAPS_PAST_TRANSPORT_ROW_CAP as usize);
        for index in 0..OVERLAPS_PAST_TRANSPORT_ROW_CAP {
            let session_id = format!("session.{index:05}");
            let active = SessionRecord {
                session_id: session_id.clone(),
                ..session(None, None)
            };
            assert!(harness.registered.upsert_session(&active).await);
            staged_rows.push(SessionRecord {
                session_id,
                title: Some(format!("composer {index:05}")),
                ..session(None, None)
            });
        }
        let transaction = harness.registered.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "INSERT INTO observation_projection_rebuilds (
                    projector_version, generation, frontier_sequence, state
                 ) VALUES (?1, ?2, 0, 'ready')",
                params![SESSION_MESSAGE_PROJECTOR_VERSION, GENERATION],
            )
            .await
            .unwrap();
        let staged_json = serde_json::to_string(&staged_rows).unwrap();
        transaction
            .execute(
                "INSERT INTO observation_projection_rebuild_sessions (
                    projector_version, generation, provider, session_id, session_json
                 )
                 SELECT ?1, ?2,
                        json_extract(staged.value, '$.provider'),
                        json_extract(staged.value, '$.session_id'),
                        staged.value
                 FROM json_each(?3) AS staged",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    GENERATION,
                    staged_json.as_str()
                ],
            )
            .await
            .unwrap();

        reconcile_overlapping_rebuild_sessions(&transaction, GENERATION)
            .await
            .expect("an overlap larger than one transport page must still reconcile");

        let mut rows = transaction
            .query(
                "SELECT COUNT(*) FROM sessions
                 WHERE provider = 'cursor' AND title LIKE 'composer %'",
                (),
            )
            .await
            .unwrap();
        let reconciled = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
        assert_eq!(
            reconciled, OVERLAPS_PAST_TRANSPORT_ROW_CAP,
            "every overlapping session must be reconciled, not one page of {REBUILD_PAGE_SIZE}",
        );
    }

    #[tokio::test]
    async fn activation_names_the_session_field_instead_of_a_message_collision() {
        let harness = RegisteredGlobalDbHarness::open("session-collision-field").await;
        let active = session(Some("/private/old-transcript.jsonl"), None);
        assert!(harness.registered.upsert_session(&active).await);
        let transaction = harness.registered.begin_write_transaction().await.unwrap();
        let staged = session(Some("/private/new-transcript.jsonl"), None);
        stage(&transaction, "generation.session-collision", &staged).await;

        let error = activate_rebuild_sessions(&transaction, "generation.session-collision")
            .await
            .expect_err("incompatible transcript paths must not activate");
        match &error {
            ProjectionStoreError::SessionOutputCollision {
                provider,
                session_id,
                field,
            } => {
                assert_eq!(provider, "cursor");
                assert_eq!(session_id, SESSION_ID);
                assert_eq!(*field, "transcript_path");
            }
            other => panic!("session conflict classified as {other}"),
        }
        let rendered = error.to_string();
        assert!(rendered.contains("transcript_path"));
        assert!(!rendered.contains("session:"));
        assert!(!rendered.contains("/private/old-transcript.jsonl"));
        assert!(!rendered.contains("/private/new-transcript.jsonl"));

        let mut rows = transaction
            .query(
                "SELECT transcript_path FROM sessions WHERE provider = 'cursor' AND session_id = ?1",
                params![SESSION_ID],
            )
            .await
            .unwrap();
        let persisted = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        assert_eq!(persisted, "/private/old-transcript.jsonl");
    }

    #[tokio::test]
    async fn activation_merges_a_compatible_session_and_inserts_a_new_one() {
        let harness = RegisteredGlobalDbHarness::open("session-collision-merge").await;
        let active = session(None, None);
        assert!(harness.registered.upsert_session(&active).await);
        let second_active = SessionRecord {
            session_id: "session.second".to_owned(),
            ..active.clone()
        };
        assert!(harness.registered.upsert_session(&second_active).await);
        let transaction = harness.registered.begin_write_transaction().await.unwrap();
        let staged = session(None, Some("Composer session"));
        stage(&transaction, "generation.session-merge", &staged).await;
        // A second overlap keeps the set-based reconciled write honest: each
        // merged row must land on its own session, not the first one twice.
        let second_staged = SessionRecord {
            title: Some("Second composer session".to_owned()),
            ..second_active.clone()
        };
        let fresh = SessionRecord {
            provider: "cursor".to_owned(),
            session_id: "session.fresh".to_owned(),
            title: Some("fresh session".to_owned()),
            ..active.clone()
        };
        for staged in [&second_staged, &fresh] {
            transaction
                .execute(
                    "INSERT INTO observation_projection_rebuild_sessions (
                    projector_version, generation, provider, session_id, session_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        SESSION_MESSAGE_PROJECTOR_VERSION,
                        "generation.session-merge",
                        staged.provider.as_str(),
                        staged.session_id.as_str(),
                        serde_json::to_string(staged).unwrap().as_str(),
                    ],
                )
                .await
                .unwrap();
        }

        activate_rebuild_sessions(&transaction, "generation.session-merge")
            .await
            .unwrap();

        let mut rows = transaction
            .query(
                "SELECT title FROM sessions WHERE provider = 'cursor' AND session_id = ?1",
                params![SESSION_ID],
            )
            .await
            .unwrap();
        let title = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        assert_eq!(title, "Composer session");
        drop(rows);
        let mut rows = transaction
            .query(
                "SELECT title FROM sessions WHERE provider = 'cursor' AND session_id = 'session.second'",
                (),
            )
            .await
            .unwrap();
        let second_title = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        assert_eq!(second_title, "Second composer session");
        drop(rows);
        let mut rows = transaction
            .query(
                "SELECT title FROM sessions WHERE provider = 'cursor' AND session_id = 'session.fresh'",
                (),
            )
            .await
            .unwrap();
        let fresh_title = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap();
        assert_eq!(fresh_title, "fresh session");
    }
}
