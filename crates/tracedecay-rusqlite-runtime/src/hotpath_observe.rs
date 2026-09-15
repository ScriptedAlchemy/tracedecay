//! Opt-in rusqlite-runtime Hotpath gauges.
//!
//! These exist to answer one question the writer's own timings cannot: when
//! `begin_immediate` averages 10.65 ms against an 82 µs p95, a few transactions
//! are blocking for ~200 ms while nearly all are instant. `BEGIN IMMEDIATE`
//! takes SQLite's write lock, so something else was holding it — and the
//! maintenance paths (checkpoint, incremental vacuum, online backup) take that
//! lock outside the normal write queue.
//!
//! Function timings alone cannot separate those, because every write funnels
//! through the same `repository::execute` / `submit_authorized` frames whatever
//! submitted it. Counting the maintenance operations and the time they hold the
//! database makes the blocking attributable instead of merely suspected.
//!
//! Counts and durations only; never table contents, paths, or SQL text.

#[inline(always)]
fn add(name: &'static str, delta: u64) {
    #[cfg(feature = "hotpath")]
    {
        if delta == 0 {
            return;
        }
        hotpath::gauge!(name).inc(delta);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = (name, delta);
}

#[inline(always)]
fn subtract(name: &'static str, delta: u64) {
    #[cfg(feature = "hotpath")]
    {
        if delta == 0 {
            return;
        }
        hotpath::gauge!(name).dec(delta);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = (name, delta);
}

#[derive(Clone, Copy)]
pub(crate) enum CheckpointAttribution {
    Passive,
    Restart,
    Truncate,
}

#[inline(always)]
fn add_checkpoint_mode(
    mode: CheckpointAttribution,
    passive: &'static str,
    restart: &'static str,
    truncate: &'static str,
    delta: u64,
) {
    add(
        match mode {
            CheckpointAttribution::Passive => passive,
            CheckpointAttribution::Restart => restart,
            CheckpointAttribution::Truncate => truncate,
        },
        delta,
    );
}

/// One successful WAL checkpoint, and the already-returned work it performed.
///
/// A checkpoint holds the database while it copies WAL frames back, so its
/// total duration is the budget every concurrent writer waits inside. Compare
/// this against the writer's immediate-begin timings: if they track each
/// other, the writer tail is checkpointing, not write throughput.
#[inline(always)]
pub(crate) fn record_checkpoint(
    mode: CheckpointAttribution,
    elapsed: std::time::Duration,
    complete: bool,
    wal_bytes: u64,
    checkpointed_frames: u64,
) {
    record_checkpoint_attempt(mode, elapsed);
    add_checkpoint_mode(
        mode,
        "rusqlite.checkpoint.input_wal_bytes.passive",
        "rusqlite.checkpoint.input_wal_bytes.restart",
        "rusqlite.checkpoint.input_wal_bytes.truncate",
        wal_bytes,
    );
    add_checkpoint_mode(
        mode,
        "rusqlite.checkpoint.frames.passive",
        "rusqlite.checkpoint.frames.restart",
        "rusqlite.checkpoint.frames.truncate",
        checkpointed_frames,
    );
    if !complete {
        add("rusqlite.checkpoint.incomplete", 1);
        add_checkpoint_mode(
            mode,
            "rusqlite.checkpoint.incomplete.passive",
            "rusqlite.checkpoint.incomplete.restart",
            "rusqlite.checkpoint.incomplete.truncate",
            1,
        );
    }
}

#[inline(always)]
pub(crate) fn record_checkpoint_error(mode: CheckpointAttribution, elapsed: std::time::Duration) {
    record_checkpoint_attempt(mode, elapsed);
    add("rusqlite.checkpoint.errors", 1);
    add_checkpoint_mode(
        mode,
        "rusqlite.checkpoint.errors.passive",
        "rusqlite.checkpoint.errors.restart",
        "rusqlite.checkpoint.errors.truncate",
        1,
    );
}

#[inline(always)]
fn record_checkpoint_attempt(mode: CheckpointAttribution, elapsed: std::time::Duration) {
    let elapsed_micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    add("rusqlite.checkpoint.runs", 1);
    add("rusqlite.checkpoint.micros", elapsed_micros);
    add_checkpoint_mode(
        mode,
        "rusqlite.checkpoint.runs.passive",
        "rusqlite.checkpoint.runs.restart",
        "rusqlite.checkpoint.runs.truncate",
        1,
    );
    add_checkpoint_mode(
        mode,
        "rusqlite.checkpoint.micros.passive",
        "rusqlite.checkpoint.micros.restart",
        "rusqlite.checkpoint.micros.truncate",
        elapsed_micros,
    );
}

#[inline(always)]
pub(crate) fn record_scheduled_checkpoint_dispatch() {
    add("rusqlite.checkpoint.dispatch.scheduled", 1);
}

#[inline(always)]
pub(crate) fn record_requested_checkpoint_dispatch() {
    add("rusqlite.checkpoint.dispatch.requested", 1);
}

/// One valid writer offer, before bounded admission makes its decision.
///
/// Session ingestion and code indexing share every writer frame, so a run that
/// does both cannot attribute `submit_authorized` counts from timings alone —
/// which is exactly what made an earlier batches-per-frame ratio unquotable.
/// Priority is already on the operation metadata, so splitting on it costs
/// nothing and needs no new field threaded through the contract.
#[inline(always)]
pub(crate) fn record_writer_offered(priority: tracedecay_store::OperationPriorityV1) {
    add(
        match priority {
            tracedecay_store::OperationPriorityV1::Foreground => {
                "rusqlite.writer.offered.foreground"
            }
            tracedecay_store::OperationPriorityV1::Background => {
                "rusqlite.writer.offered.background"
            }
            tracedecay_store::OperationPriorityV1::Health => "rusqlite.writer.offered.health",
        },
        1,
    );
}

#[inline(always)]
pub(crate) fn record_writer_admitted(priority: tracedecay_store::OperationPriorityV1) {
    add(
        match priority {
            tracedecay_store::OperationPriorityV1::Foreground => {
                "rusqlite.writer.admitted.foreground"
            }
            tracedecay_store::OperationPriorityV1::Background => {
                "rusqlite.writer.admitted.background"
            }
            tracedecay_store::OperationPriorityV1::Health => "rusqlite.writer.admitted.health",
        },
        1,
    );
}

#[inline(always)]
pub(crate) fn record_writer_shed(priority: tracedecay_store::OperationPriorityV1) {
    add(
        match priority {
            tracedecay_store::OperationPriorityV1::Foreground => "rusqlite.writer.shed.foreground",
            tracedecay_store::OperationPriorityV1::Background => "rusqlite.writer.shed.background",
            tracedecay_store::OperationPriorityV1::Health => "rusqlite.writer.shed.health",
        },
        1,
    );
}

#[inline(always)]
pub(crate) fn record_writer_queue_admitted(bytes: u64) {
    add("rusqlite.writer.queue.operations", 1);
    add("rusqlite.writer.queue.bytes", bytes);
}

#[inline(always)]
pub(crate) fn record_writer_queue_released(operations: u32, bytes: u64) {
    subtract("rusqlite.writer.queue.operations", u64::from(operations));
    subtract("rusqlite.writer.queue.bytes", bytes);
}

#[inline(always)]
pub(crate) fn record_writer_batch(
    priority: tracedecay_store::OperationPriorityV1,
    operations: u64,
    bytes: u64,
    queue_wait_micros: u64,
) {
    add("rusqlite.writer.batch.runs", 1);
    add("rusqlite.writer.batch.operations", operations);
    add("rusqlite.writer.batch.bytes", bytes);
    add("rusqlite.writer.batch.queue_wait_micros", queue_wait_micros);
    add(
        match priority {
            tracedecay_store::OperationPriorityV1::Foreground => {
                "rusqlite.writer.dispatch.foreground"
            }
            tracedecay_store::OperationPriorityV1::Background => {
                "rusqlite.writer.dispatch.background"
            }
            tracedecay_store::OperationPriorityV1::Health => "rusqlite.writer.dispatch.health",
        },
        operations,
    );
}

#[inline(always)]
pub(crate) fn record_writer_transaction(rows: u64, lock_held_micros: u64) {
    add("rusqlite.writer.transaction.rows", rows);
    add(
        "rusqlite.writer.transaction.lock_held_micros",
        lock_held_micros,
    );
}

/// One admission refusal, split by which resource was saturated.
///
/// `submit_authorized` already counts sheds by priority, but a shed alone
/// cannot say whether the operation lane or the byte budget filled first —
/// and the fix differs (batch fan-out versus payload size). The two causes
/// are recorded at the sole admission authority so any future caller of
/// [`crate::admission::Admission::reserve`] is counted the same way.
#[inline(always)]
pub(crate) fn record_admission_refused_operations() {
    add("rusqlite.admission.refused.operations", 1);
}

#[inline(always)]
pub(crate) fn record_admission_refused_bytes() {
    add("rusqlite.admission.refused.bytes", 1);
}

/// Rows the bounded per-commit retention pass actually deleted.
///
/// The prune shares every commit's transaction, so its span alone cannot say
/// whether a slow commit was deleting a real backlog or scanning an empty
/// candidate set. A rising row count alongside a hot
/// `rusqlite.ledger.prune_superseded` span means backlog convergence; a flat
/// zero means the scan itself is the cost.
#[inline(always)]
pub(crate) fn record_ledger_pruned_rows(rows: u64) {
    add("rusqlite.ledger.pruned_rows", rows);
}

/// How an interactive exact-SQL transaction released the writer thread.
///
/// The `rusqlite.exact_sql.transaction` span measures how long the writer
/// thread was held, but a duration cannot say whether the hold ended in a
/// durable commit or was wasted work. Expired and abandoned holds are the
/// cancellations that block every queued write behind them for nothing.
#[derive(Clone, Copy)]
pub(crate) enum ExactSqlTransactionOutcome {
    Committed,
    CommitFailed,
    RolledBack,
    Expired,
    Abandoned,
    BeginFailed,
}

#[inline(always)]
pub(crate) fn record_exact_sql_transaction_outcome(outcome: ExactSqlTransactionOutcome) {
    add(
        match outcome {
            ExactSqlTransactionOutcome::Committed => "rusqlite.exact_sql.transaction.committed",
            ExactSqlTransactionOutcome::CommitFailed => {
                "rusqlite.exact_sql.transaction.commit_failed"
            }
            ExactSqlTransactionOutcome::RolledBack => "rusqlite.exact_sql.transaction.rolled_back",
            ExactSqlTransactionOutcome::Expired => "rusqlite.exact_sql.transaction.expired",
            ExactSqlTransactionOutcome::Abandoned => "rusqlite.exact_sql.transaction.abandoned",
            ExactSqlTransactionOutcome::BeginFailed => {
                "rusqlite.exact_sql.transaction.begin_failed"
            }
        },
        1,
    );
}

/// One idle-loop wake that re-ran a checkpoint because hard drain is pending.
///
/// Under hard WAL pressure the worker retries every 100ms until blockers
/// clear. `rusqlite.checkpoint.dispatch.scheduled` counts those retries in
/// with ordinary post-batch evaluations; this counter isolates the retry
/// loop, so a stuck drain shows up as this number climbing while
/// `rusqlite.checkpoint.frames.*` stays flat.
#[inline(always)]
pub(crate) fn record_checkpoint_hard_retry_wake() {
    add("rusqlite.checkpoint.dispatch.hard_retry", 1);
}

#[inline(always)]
pub(crate) fn record_exact_sql_dispatch() {
    add("rusqlite.writer.dispatch.exact_sql", 1);
}

#[inline(always)]
pub(crate) fn record_incremental_vacuum_dispatch() {
    add("rusqlite.writer.dispatch.incremental_vacuum", 1);
}

#[inline(always)]
pub(crate) fn record_online_backup_dispatch() {
    add("rusqlite.writer.dispatch.online_backup", 1);
}
