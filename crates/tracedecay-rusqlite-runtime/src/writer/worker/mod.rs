//! The writer's single worker task on the storage pool.
//!
//! [`Worker::run`] owns the connection and the loop; the siblings own the two
//! halves the loop leans on, [`ingress`] for how work arrives and where it is
//! parked, and [`rejection`] for settling work that will never run.

use std::{
    collections::{HashMap, VecDeque},
    panic::AssertUnwindSafe,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::SyncSender,
    },
    time::{Duration, Instant},
};

use asupersync::{
    Cx,
    channel::{mpsc, watch},
};
use futures_util::FutureExt;
use rusqlite::TransactionBehavior;
use tracedecay_store::{
    AdmissionConfigV1, OperationPriorityV1, RuntimeBatchCompatibilityV1, RuntimeInterruptionV1,
    RuntimeRequestProbeV1, StoreOperationIdV1, StoreRuntimeBindingV1, UnavailableReasonV1,
};
use tracing::Instrument;

#[cfg(not(any(unix, windows)))]
use crate::connection::ConnectionMode;
use crate::{
    RuntimeWriteAuthorityStage,
    admission::{FairQueue, QueueItem},
    checkpoint::{
        CheckpointBlockerSource, CheckpointConfig, CheckpointDecision, CheckpointInterruption,
        CheckpointOutcome, CheckpointPressure, CheckpointResult, CheckpointStatus, CheckpointWal,
        FENCE_READ_DEADLINE, RusqliteCheckpointDriver, WriterCheckpointController,
        foreign_wal_holders,
    },
    connection::{self, OpenedDatabaseFile},
    exact_sql::{
        WriterCommand as ExactSqlWriterCommand, reject_writer_command, run_writer_command,
    },
    telemetry::{
        LockWorkScope, WalCheckpointSample, WriterTelemetry, duration_micros, take_observed_vm,
    },
    watermark::CommittedWatermarkPublisher,
};

use super::{
    WriterActorError, WriterStartError, WriterState,
    request::{
        AcceptedRequest, CheckpointCommand, ExecutionBatch, IncrementalVacuumCommand, SharedReply,
    },
    transaction::{BatchTiming, WriterReporting, process_batch},
};

mod ingress;
mod rejection;

use ingress::{
    AuxiliaryWork, WorkerWake, apply_wake, drain_command_ingress, drain_ingress, enqueue,
    select_auxiliary_work, wait_for_work,
};
use rejection::{
    cancel_waiting, reject_all, reject_all_exact_sql, reject_all_incremental_vacuum,
    reject_incremental_vacuum, reject_unauthorized,
};

/// When the blocked writer retries its drain: the fence's short interval, or
/// the backoff once the fence expired and the WAL passed its write ceiling.
fn drain_retry_due(checkpoint: &WriterCheckpointController) -> Option<Instant> {
    if checkpoint.hard_drain_required() {
        checkpoint.retry_due(Instant::now())
    } else {
        None
    }
}

/// Admission-anchored window for waiting on additional ingress. Once the
/// worker dispatches, every compatible request already queued may share the
/// transaction subject to the ordinary count and byte caps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BatchCoalescingWindow {
    deadline: Instant,
    max_operations: usize,
    max_bytes: u64,
    operations: usize,
    bytes: u64,
}

impl BatchCoalescingWindow {
    #[allow(clippy::too_many_arguments)]
    fn new(
        enqueued_at: Instant,
        priority: OperationPriorityV1,
        isolated: bool,
        interrupted: bool,
        operations: usize,
        bytes: u64,
        config: &AdmissionConfigV1,
    ) -> Option<Self> {
        if priority == OperationPriorityV1::Health || isolated || interrupted {
            return None;
        }
        let budget = match priority {
            OperationPriorityV1::Background => &config.background_batch,
            OperationPriorityV1::Foreground => &config.foreground_batch,
            OperationPriorityV1::Health => return None,
        };
        if operations >= budget.max_operations as usize || bytes >= budget.max_bytes {
            return None;
        }
        Some(Self {
            deadline: enqueued_at.checked_add(Duration::from_millis(budget.max_delay_ms))?,
            max_operations: budget.max_operations as usize,
            max_bytes: budget.max_bytes,
            operations,
            bytes,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn accepts(
        &self,
        enqueued_at: Instant,
        priority_matches: bool,
        compatibility_matches: bool,
        isolated: bool,
        interrupted: bool,
        bytes: u64,
    ) -> bool {
        enqueued_at < self.deadline
            && priority_matches
            && compatibility_matches
            && !isolated
            && !interrupted
            && self.operations < self.max_operations
            && self
                .bytes
                .checked_add(bytes)
                .is_some_and(|total| total <= self.max_bytes)
    }

    fn admit(&mut self, bytes: u64) {
        self.operations += 1;
        self.bytes += bytes;
    }
}

/// The probes retained while requests are temporarily parked back in the fair
/// queue. Probes have no async notifier, so every ingress wake and the hard
/// window deadline re-polls them. A cancellation first observed mid-dwell can
/// therefore wait no longer than the configured `max_delay_ms` from the first
/// request's enqueue time, and any ingress or auxiliary wake shortens that
/// bound.
struct PendingBatchDwell {
    window: BatchCoalescingWindow,
    priority: OperationPriorityV1,
    compatibility: RuntimeBatchCompatibilityV1,
    probes: Vec<Arc<dyn RuntimeRequestProbeV1>>,
}

impl PendingBatchDwell {
    fn from_selected(
        selected: &[AcceptedRequest],
        config: &AdmissionConfigV1,
        now: Instant,
    ) -> Option<Self> {
        let first = selected.first()?;
        let priority = first.priority();
        let compatibility = first.request.transaction_scope().compatibility.clone();
        let mut pending = Self {
            window: BatchCoalescingWindow::new(
                first.enqueued_at,
                priority,
                first.probe.requires_isolated_commit(),
                first.probe.interruption().is_some(),
                1,
                first.admission_bytes(),
                config,
            )?,
            priority,
            compatibility,
            probes: vec![Arc::clone(&first.probe)],
        };
        for item in &selected[1..] {
            if !pending.accepts(item, true) {
                return None;
            }
            pending.admit(item);
        }
        if pending.window.operations >= pending.window.max_operations
            || pending.window.bytes >= pending.window.max_bytes
        {
            return None;
        }
        (now < pending.window.deadline).then_some(pending)
    }

    fn accepts(&self, item: &AcceptedRequest, poll_interruption: bool) -> bool {
        self.window.accepts(
            item.enqueued_at,
            item.priority() == self.priority,
            item.request.transaction_scope().compatibility == self.compatibility,
            item.probe.requires_isolated_commit(),
            poll_interruption && item.probe.interruption().is_some(),
            item.admission_bytes(),
        )
    }

    fn admit(&mut self, item: &AcceptedRequest) {
        self.window.admit(item.admission_bytes());
        self.probes.push(Arc::clone(&item.probe));
    }

    fn interrupted(&self) -> bool {
        self.probes
            .iter()
            .any(|probe| probe.interruption().is_some())
    }
}

#[allow(clippy::too_many_arguments)]
async fn dwell_for_batch(
    cx: &Cx,
    mut pending: PendingBatchDwell,
    receiver: &mut mpsc::Receiver<AcceptedRequest>,
    exact_sql_receiver: &mut mpsc::Receiver<ExactSqlWriterCommand>,
    incremental_vacuum_receiver: &mut mpsc::Receiver<IncrementalVacuumCommand>,
    checkpoint_receiver: &mut mpsc::Receiver<CheckpointCommand>,
    shutdown_receiver: &mut mpsc::UnboundedReceiver<()>,
    queue: &mut FairQueue<AcceptedRequest>,
    inflight: &mut HashMap<StoreOperationIdV1, SharedReply>,
    exact_sql_queue: &mut VecDeque<ExactSqlWriterCommand>,
    incremental_vacuum_queue: &mut VecDeque<IncrementalVacuumCommand>,
    checkpoint_queue: &mut VecDeque<CheckpointCommand>,
    telemetry: &WriterTelemetry,
    input_closed: &mut bool,
    exact_sql_closed: &mut bool,
    incremental_vacuum_closed: &mut bool,
    checkpoint_closed: &mut bool,
) {
    loop {
        let remaining = pending
            .window
            .deadline
            .saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        let wake = wait_for_work(
            cx,
            receiver,
            exact_sql_receiver,
            incremental_vacuum_receiver,
            checkpoint_receiver,
            shutdown_receiver,
            *input_closed,
            *exact_sql_closed,
            *incremental_vacuum_closed,
            *checkpoint_closed,
            Some(pending.window.deadline),
        )
        .await;
        if matches!(wake, WorkerWake::CheckpointRetry) {
            return;
        }
        let interrupted = pending.interrupted();
        match wake {
            WorkerWake::Write(Some(item)) => {
                if !interrupted && pending.accepts(&item, true) {
                    let bytes = item.admission_bytes();
                    let probe = Arc::clone(&item.probe);
                    if enqueue(queue, inflight, item, telemetry) {
                        pending.window.admit(bytes);
                        pending.probes.push(probe);
                    }
                    if pending.window.operations >= pending.window.max_operations
                        || pending.window.bytes >= pending.window.max_bytes
                    {
                        return;
                    }
                } else {
                    let _ = enqueue(queue, inflight, item, telemetry);
                    return;
                }
            }
            wake => {
                apply_wake(
                    wake,
                    queue,
                    inflight,
                    exact_sql_queue,
                    incremental_vacuum_queue,
                    checkpoint_queue,
                    telemetry,
                    input_closed,
                    exact_sql_closed,
                    incremental_vacuum_closed,
                    checkpoint_closed,
                );
                return;
            }
        }
    }
}

pub(super) struct Worker {
    pub(super) cx: Cx,
    pub(super) path: PathBuf,
    #[cfg(unix)]
    pub(super) canonical_path: PathBuf,
    pub(super) expected_file_identity: Option<u64>,
    pub(super) _opened_database: Option<Arc<OpenedDatabaseFile>>,
    pub(super) binding: StoreRuntimeBindingV1,
    pub(super) config: AdmissionConfigV1,
    pub(super) receiver: mpsc::Receiver<AcceptedRequest>,
    pub(super) exact_sql_receiver: mpsc::Receiver<ExactSqlWriterCommand>,
    pub(super) incremental_vacuum_receiver: mpsc::Receiver<IncrementalVacuumCommand>,
    pub(super) checkpoint_receiver: mpsc::Receiver<CheckpointCommand>,
    pub(super) shutdown_receiver: mpsc::UnboundedReceiver<()>,
    pub(super) persistence: crate::persistence::RuntimeWriterPersistence<
        Box<dyn crate::StorageOperationExecutor + Send>,
    >,
    pub(super) state: Arc<AtomicU8>,
    pub(super) shutdown_requested: Arc<AtomicBool>,
    pub(super) telemetry: WriterTelemetry,
    /// The worker-only capability that advances read-consistency state.
    pub(super) watermark_publisher: CommittedWatermarkPublisher,
    pub(super) checkpoint_status: watch::Sender<CheckpointStatus>,
    pub(super) checkpoint_pressure: watch::Sender<CheckpointPressure>,
    pub(super) checkpoint_blockers: Arc<dyn CheckpointBlockerSource>,
    pub(super) on_file_lost: Option<super::FileLossNotice>,
    pub(super) started: SyncSender<Result<Option<u64>, WriterStartError>>,
}

/// How often the writer checks that its pinned file is still the store's
/// file, between batches and while idle.
pub const FILE_PROBE_INTERVAL: Duration = Duration::from_secs(2);

struct FileProbe {
    file: Arc<OpenedDatabaseFile>,
    path: PathBuf,
    due: Instant,
}

impl FileProbe {
    fn lost(&mut self, now: Instant) -> bool {
        if now < self.due {
            return false;
        }
        self.due = now + FILE_PROBE_INTERVAL;
        self.file.is_lost(&self.path)
    }
}

fn earliest(deadline: Option<Instant>, probe: Option<&FileProbe>) -> Option<Instant> {
    match (deadline, probe.map(|probe| probe.due)) {
        (Some(deadline), Some(due)) => Some(deadline.min(due)),
        (deadline, due) => deadline.or(due),
    }
}

impl Worker {
    pub(super) async fn run(self) {
        #[cfg(any(unix, windows))]
        if let Some(opened_database) = self._opened_database.as_deref() {
            #[cfg(unix)]
            let canonical_path = &self.canonical_path;
            #[cfg(windows)]
            let canonical_path = &self.path;
            if let Err(error) = opened_database.verify_current_path(canonical_path) {
                return self.fail_start(WriterStartError::OpenedDatabaseIdentity(error));
            }
        }
        #[cfg(unix)]
        let canonical_path = &self.canonical_path;
        #[cfg(windows)]
        let canonical_path = &self.path;
        #[cfg(any(unix, windows))]
        let connection = match connection::open_writer(
            &self.path,
            self._opened_database.as_deref(),
            canonical_path,
            connection::Durability::for_scope(&self.binding.shard_id.scope),
        ) {
            Ok(connection) => connection,
            Err(connection::WriterOpenError::Identity(error)) => {
                return self.fail_start(WriterStartError::OpenedDatabaseIdentity(error));
            }
            Err(connection::WriterOpenError::Policy(error)) if error.is_open_failure() => {
                return self.fail_start(WriterStartError::OpenFailed);
            }
            Err(connection::WriterOpenError::Policy(error)) => {
                return self
                    .fail_start(WriterStartError::ConnectionPolicyFailed(error.to_string()));
            }
        };
        #[cfg(not(any(unix, windows)))]
        let connection = match connection::open(&self.path, ConnectionMode::Writer) {
            Ok(connection) => connection,
            Err(error) if error.is_open_failure() => {
                return self.fail_start(WriterStartError::OpenFailed);
            }
            Err(error) => {
                return self
                    .fail_start(WriterStartError::ConnectionPolicyFailed(error.to_string()));
            }
        };
        #[cfg(unix)]
        if let Some(opened_database) = self._opened_database.as_deref()
            && let Err(error) = opened_database.verify_connection(&connection, &self.canonical_path)
        {
            return self.fail_start(WriterStartError::OpenedDatabaseIdentity(error));
        }
        #[cfg(windows)]
        if let Some(opened_database) = self._opened_database.as_deref()
            && let Err(error) = opened_database.verify_connection(&connection, &self.path)
        {
            return self.fail_start(WriterStartError::OpenedDatabaseIdentity(error));
        }
        #[cfg(unix)]
        if self.expected_file_identity.is_some()
            && connection
                .path()
                .and_then(|path| std::fs::canonicalize(path).ok())
                .as_deref()
                != Some(self.canonical_path.as_path())
        {
            return self.fail_start(WriterStartError::OpenedDatabasePathMismatch);
        }
        let opened_file_identity = match self.expected_file_identity {
            Some(expected) => {
                let actual = match OpenedDatabaseFile::pin(&self.path) {
                    Ok(opened) => opened.identity(),
                    Err(error) => {
                        return self.fail_start(WriterStartError::OpenedDatabaseIdentity(error));
                    }
                };
                if actual != expected {
                    return self.fail_start(WriterStartError::OpenedDatabaseIdentityMismatch {
                        expected,
                        actual,
                    });
                }
                Some(actual)
            }
            None => None,
        };
        let page_cache = match connection::WriterPageCache::new(&connection) {
            Ok(page_cache) => page_cache,
            Err(error) => {
                return self
                    .fail_start(WriterStartError::ConnectionPolicyFailed(error.to_string()));
            }
        };
        let mut checkpoint = match WriterCheckpointController::new(
            RusqliteCheckpointDriver::new(connection),
            checkpoint_config(&self.config),
        ) {
            Ok(checkpoint) => checkpoint,
            Err(_) => return self.fail_start(WriterStartError::CheckpointSetupFailed),
        };
        #[cfg(any(unix, windows))]
        if let Some(opened_database) = self._opened_database.as_deref()
            && let Err(error) =
                opened_database.verify_connection(checkpoint.connection_mut(), canonical_path)
        {
            return self.fail_start(WriterStartError::OpenedDatabaseIdentity(error));
        }
        self.state
            .store(WriterState::Ready as u8, Ordering::Release);
        if self.started.send(Ok(opened_file_identity)).is_err() {
            self.state
                .store(WriterState::Draining as u8, Ordering::Release);
            return;
        }
        let state = Arc::clone(&self.state);
        let telemetry = self.telemetry.clone();
        if let Err(payload) = AssertUnwindSafe(self.run_loop(checkpoint, page_cache))
            .catch_unwind()
            .await
        {
            // The fault is otherwise invisible: every later request only sees
            // "writer unavailable". Name the panic so a faulted writer can be
            // traced from the process log.
            let message = payload
                .downcast_ref::<&str>()
                .map(|message| (*message).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_owned());
            eprintln!(
                "[tracedecay] rusqlite writer worker panicked; writer is now faulted: {message}"
            );
            state.store(WriterState::Faulted as u8, Ordering::Release);
            telemetry.fault_unsettled();
        }
    }

    fn fail_start(&self, error: WriterStartError) {
        self.state
            .store(WriterState::Closed as u8, Ordering::Release);
        let _ = self.started.send(Err(error));
    }

    async fn run_loop(
        mut self,
        mut checkpoint: WriterCheckpointController,
        mut page_cache: connection::WriterPageCache,
    ) {
        let mut queue = FairQueue::default();
        let mut inflight = HashMap::new();
        let mut exact_sql_queue = VecDeque::new();
        let mut incremental_vacuum_queue = VecDeque::new();
        let mut checkpoint_queue = VecDeque::new();
        let mut input_closed = false;
        let mut exact_sql_closed = false;
        let mut incremental_vacuum_closed = false;
        let mut checkpoint_closed = false;
        let mut prefer_auxiliary = true;
        let mut next_auxiliary = AuxiliaryWork::IncrementalVacuum;
        let mut hard_checkpoint_retry_due = None;
        #[cfg(unix)]
        let probe_path = self.canonical_path.clone();
        #[cfg(not(unix))]
        let probe_path = self.path.clone();
        let mut probe = self._opened_database.clone().map(|file| FileProbe {
            file,
            path: probe_path,
            due: Instant::now() + FILE_PROBE_INTERVAL,
        });
        loop {
            // The state flips before ingress drains, so work sent before
            // admission saw the flip is still collected and failed below.
            let lost = probe
                .as_mut()
                .is_some_and(|probe| probe.lost(Instant::now()));
            if lost {
                self.state.store(WriterState::Lost as u8, Ordering::Release);
            }
            {
                let _span = tracing::trace_span!("rusqlite.writer.drain_ingress").entered();
                {
                    drain_ingress(
                        &mut self.receiver,
                        &mut queue,
                        &mut inflight,
                        &self.telemetry,
                        &mut input_closed,
                    );
                    drain_command_ingress(
                        &mut self.checkpoint_receiver,
                        &mut checkpoint_queue,
                        &mut checkpoint_closed,
                    );
                    drain_command_ingress(
                        &mut self.exact_sql_receiver,
                        &mut exact_sql_queue,
                        &mut exact_sql_closed,
                    );
                    drain_command_ingress(
                        &mut self.incremental_vacuum_receiver,
                        &mut incremental_vacuum_queue,
                        &mut incremental_vacuum_closed,
                    );
                }
            };
            if lost {
                reject_all(&mut queue, &self.telemetry, UnavailableReasonV1::FileLost);
                reject_all_exact_sql(&mut exact_sql_queue);
                reject_all_incremental_vacuum(&mut incremental_vacuum_queue);
                checkpoint_queue.clear();
                if let Some(probe) = &probe {
                    tracing::warn!(
                        path = %probe.path.display(),
                        "store file was unlinked or replaced; its writer stopped"
                    );
                }
                if let Some(notice) = self.on_file_lost.take() {
                    notice();
                }
                return;
            }
            if self.shutdown_requested.load(Ordering::Acquire)
                && queue.is_empty()
                && exact_sql_queue.is_empty()
                && incremental_vacuum_queue.is_empty()
            {
                checkpoint_queue.clear();
                break;
            }
            if self.state.load(Ordering::Acquire) == WriterState::Faulted as u8 {
                reject_all(&mut queue, &self.telemetry, UnavailableReasonV1::Faulted);
                reject_all_exact_sql(&mut exact_sql_queue);
                reject_all_incremental_vacuum(&mut incremental_vacuum_queue);
                checkpoint_queue.clear();
                if input_closed
                    && exact_sql_closed
                    && incremental_vacuum_closed
                    && checkpoint_closed
                {
                    break;
                }
                let wake = wait_for_work(
                    &self.cx,
                    &mut self.receiver,
                    &mut self.exact_sql_receiver,
                    &mut self.incremental_vacuum_receiver,
                    &mut self.checkpoint_receiver,
                    &mut self.shutdown_receiver,
                    input_closed,
                    exact_sql_closed,
                    incremental_vacuum_closed,
                    checkpoint_closed,
                    earliest(None, probe.as_ref()),
                )
                .await;
                apply_wake(
                    wake,
                    &mut queue,
                    &mut inflight,
                    &mut exact_sql_queue,
                    &mut incremental_vacuum_queue,
                    &mut checkpoint_queue,
                    &self.telemetry,
                    &mut input_closed,
                    &mut exact_sql_closed,
                    &mut incremental_vacuum_closed,
                    &mut checkpoint_closed,
                );
                continue;
            }
            if let Some(command) = checkpoint_queue.pop_front() {
                self.run_requested_checkpoint(&mut checkpoint, command)
                    .await;
                hard_checkpoint_retry_due = drain_retry_due(&checkpoint);
                continue;
            }
            if checkpoint.hard_drain_required() {
                cancel_waiting(&mut queue, &self.telemetry);
                reject_unauthorized(&mut queue, &self.telemetry);
                let now = Instant::now();
                let retry_due = hard_checkpoint_retry_due.get_or_insert(now);
                if now >= *retry_due {
                    self.telemetry.checkpoint_hard_retry();
                    self.run_scheduled_checkpoint(&mut checkpoint);
                    hard_checkpoint_retry_due = drain_retry_due(&checkpoint);
                    continue;
                }
                let wake = wait_for_work(
                    &self.cx,
                    &mut self.receiver,
                    &mut self.exact_sql_receiver,
                    &mut self.incremental_vacuum_receiver,
                    &mut self.checkpoint_receiver,
                    &mut self.shutdown_receiver,
                    input_closed,
                    exact_sql_closed,
                    incremental_vacuum_closed,
                    checkpoint_closed,
                    earliest(Some(*retry_due), probe.as_ref()),
                )
                .await;
                if !matches!(wake, WorkerWake::CheckpointRetry) {
                    apply_wake(
                        wake,
                        &mut queue,
                        &mut inflight,
                        &mut exact_sql_queue,
                        &mut incremental_vacuum_queue,
                        &mut checkpoint_queue,
                        &self.telemetry,
                        &mut input_closed,
                        &mut exact_sql_closed,
                        &mut incremental_vacuum_closed,
                        &mut checkpoint_closed,
                    );
                }
                continue;
            }
            hard_checkpoint_retry_due = None;
            if let Some(auxiliary) = select_auxiliary_work(
                !exact_sql_queue.is_empty(),
                !incremental_vacuum_queue.is_empty(),
                queue.is_empty(),
                prefer_auxiliary,
                next_auxiliary,
            ) {
                match auxiliary {
                    AuxiliaryWork::ExactSql => {
                        let command = exact_sql_queue
                            .pop_front()
                            .expect("exact SQL queue checked non-empty");
                        if self.state.load(Ordering::Acquire) == WriterState::Ready as u8 {
                            let started = Instant::now();
                            let connection = checkpoint.connection_mut();
                            let rows_before = connection.total_changes();
                            let lock_work = LockWorkScope::enter();
                            let reply =
                                run_writer_command(connection, command, &self.shutdown_requested)
                                    .instrument(tracing::trace_span!("rusqlite.writer.exact_sql"))
                                    .await;
                            self.telemetry.exact_sql_command(
                                1,
                                connection.total_changes().saturating_sub(rows_before),
                                duration_micros(started.elapsed()),
                                take_observed_vm(),
                                lock_work.take(),
                            );
                            // Answer only once the work is in the telemetry, so
                            // a caller's snapshot after its reply includes it.
                            reply.send();
                            self.fit_page_cache(&mut page_cache, &mut checkpoint);
                            self.run_scheduled_checkpoint(&mut checkpoint);
                            hard_checkpoint_retry_due = drain_retry_due(&checkpoint);
                        } else {
                            reject_writer_command(command);
                        }
                        next_auxiliary = AuxiliaryWork::IncrementalVacuum;
                    }
                    AuxiliaryWork::IncrementalVacuum => {
                        let command = incremental_vacuum_queue
                            .pop_front()
                            .expect("incremental vacuum queue checked non-empty");
                        if self.state.load(Ordering::Acquire) == WriterState::Ready as u8 {
                            {
                                let _span =
                                    tracing::trace_span!("rusqlite.writer.incremental_vacuum")
                                        .entered();
                                {
                                    run_incremental_vacuum(checkpoint.connection_mut(), command);
                                }
                            };
                        } else {
                            reject_incremental_vacuum(command);
                        }
                        next_auxiliary = AuxiliaryWork::ExactSql;
                    }
                }
                if !queue.is_empty() {
                    prefer_auxiliary = false;
                }
                crate::storage_pool::yield_now().await;
                continue;
            }
            if queue.is_empty() {
                if input_closed
                    && exact_sql_closed
                    && incremental_vacuum_closed
                    && checkpoint_closed
                {
                    break;
                }
                // An expired fence keeps retrying with backoff even while no
                // write arrives to run the checkpoint after its batch.
                let checkpoint_retry = checkpoint.retry_due(Instant::now());
                let wake = wait_for_work(
                    &self.cx,
                    &mut self.receiver,
                    &mut self.exact_sql_receiver,
                    &mut self.incremental_vacuum_receiver,
                    &mut self.checkpoint_receiver,
                    &mut self.shutdown_receiver,
                    input_closed,
                    exact_sql_closed,
                    incremental_vacuum_closed,
                    checkpoint_closed,
                    earliest(checkpoint_retry, probe.as_ref()),
                )
                .await;
                if matches!(wake, WorkerWake::CheckpointRetry) {
                    if checkpoint_retry.is_some_and(|due| Instant::now() >= due) {
                        self.run_scheduled_checkpoint(&mut checkpoint);
                    }
                    continue;
                }
                apply_wake(
                    wake,
                    &mut queue,
                    &mut inflight,
                    &mut exact_sql_queue,
                    &mut incremental_vacuum_queue,
                    &mut checkpoint_queue,
                    &self.telemetry,
                    &mut input_closed,
                    &mut exact_sql_closed,
                    &mut incremental_vacuum_closed,
                    &mut checkpoint_closed,
                );
                continue;
            }
            cancel_waiting(&mut queue, &self.telemetry);
            reject_unauthorized(&mut queue, &self.telemetry);
            if queue.is_empty() {
                continue;
            }
            let selected = queue.drain_fair();
            debug_assert!(!selected.is_empty());
            if !input_closed
                && !self.shutdown_requested.load(Ordering::Acquire)
                && let Some(pending) =
                    PendingBatchDwell::from_selected(&selected, &self.config, Instant::now())
            {
                for item in selected {
                    let _ = enqueue(&mut queue, &mut inflight, item, &self.telemetry);
                }
                dwell_for_batch(
                    &self.cx,
                    pending,
                    &mut self.receiver,
                    &mut self.exact_sql_receiver,
                    &mut self.incremental_vacuum_receiver,
                    &mut self.checkpoint_receiver,
                    &mut self.shutdown_receiver,
                    &mut queue,
                    &mut inflight,
                    &mut exact_sql_queue,
                    &mut incremental_vacuum_queue,
                    &mut checkpoint_queue,
                    &self.telemetry,
                    &mut input_closed,
                    &mut exact_sql_closed,
                    &mut incremental_vacuum_closed,
                    &mut checkpoint_closed,
                )
                .instrument(tracing::trace_span!("rusqlite.writer.batch_dwell"))
                .await;
                // A probe cannot notify this actor directly, so every dwell
                // wake and the hard deadline re-poll interruption and
                // authority before auxiliary scheduling can add more delay.
                cancel_waiting(&mut queue, &self.telemetry);
                reject_unauthorized(&mut queue, &self.telemetry);
                continue;
            }
            let executing: Vec<StoreOperationIdV1> = selected
                .iter()
                .map(|item| item.operation_id().clone())
                .collect();
            for item in &selected {
                inflight.insert(item.operation_id().clone(), item.shared_reply());
            }
            if !input_closed {
                drain_ingress(
                    &mut self.receiver,
                    &mut queue,
                    &mut inflight,
                    &self.telemetry,
                    &mut input_closed,
                );
            }
            let mut batches = build_batches(selected, &self.config).into_iter();
            while let Some(batch) = batches.next() {
                self.telemetry.released(
                    u32::try_from(batch.items.len()).unwrap_or(u32::MAX),
                    batch.bytes,
                );
                process_execution_batch(
                    checkpoint.connection_mut(),
                    &self.binding,
                    batch,
                    &mut self.persistence,
                    &self.telemetry,
                    &self.state,
                    &self.watermark_publisher,
                );
                self.fit_page_cache(&mut page_cache, &mut checkpoint);
                self.run_scheduled_checkpoint(&mut checkpoint);
                if checkpoint.hard_drain_required() {
                    for pending in batches {
                        for item in pending.items {
                            inflight.remove(item.operation_id());
                            let _ = enqueue(&mut queue, &mut inflight, item, &self.telemetry);
                        }
                    }
                    hard_checkpoint_retry_due = drain_retry_due(&checkpoint);
                    break;
                }
                if self.state.load(Ordering::Acquire) == WriterState::Faulted as u8 {
                    break;
                }
            }
            if !input_closed {
                drain_ingress(
                    &mut self.receiver,
                    &mut queue,
                    &mut inflight,
                    &self.telemetry,
                    &mut input_closed,
                );
            }
            for operation_id in executing {
                inflight.remove(&operation_id);
            }
            prefer_auxiliary = true;
            crate::storage_pool::yield_now().await;
        }
        if self.state.load(Ordering::Acquire) == WriterState::Faulted as u8 {
            return;
        }
        match checkpoint.truncate_at_shutdown() {
            Ok(result) => {
                self.publish_checkpoint_result(result);
                self.state
                    .store(WriterState::Closed as u8, Ordering::Release);
            }
            Err(_) => self
                .state
                .store(WriterState::Faulted as u8, Ordering::Release),
        }
    }

    fn fit_page_cache(
        &self,
        page_cache: &mut connection::WriterPageCache,
        checkpoint: &mut WriterCheckpointController,
    ) {
        if page_cache.fit(checkpoint.connection_mut()).is_err() {
            self.state
                .store(WriterState::Faulted as u8, Ordering::Release);
        }
    }

    fn run_scheduled_checkpoint(&self, checkpoint: &mut WriterCheckpointController) {
        let now = Instant::now();
        if checkpoint.defers_scheduled(now) {
            return;
        }
        if checkpoint.revocation_due(now) {
            self.checkpoint_blockers
                .revoke_snapshots_older_than(FENCE_READ_DEADLINE);
        }
        let mut snapshot_blockers = self.checkpoint_blockers.checkpoint_blockers();
        if checkpoint.names_holders(now) && snapshot_blockers.is_clear() {
            #[cfg(unix)]
            let database = &self.canonical_path;
            #[cfg(not(unix))]
            let database = &self.path;
            snapshot_blockers = foreign_wal_holders(database);
        }
        let match_result = {
            let _span = tracing::trace_span!("rusqlite.writer.checkpoint").entered();
            checkpoint.evaluate_scheduled(snapshot_blockers)
        };
        match match_result {
            Ok(result) => self.publish_checkpoint_result(result),
            // A scheduled checkpoint that could not run this time because
            // readers still hold snapshots, the authority lapsed, or SQLite
            // reported the WAL busy is retried on the next schedule; only a
            // broken policy or a real driver failure faults the writer, the
            // same classification the requested path applies.
            Err(error) if scheduled_checkpoint_error_is_transient(&error) => {}
            Err(_) => {
                self.state
                    .store(WriterState::Faulted as u8, Ordering::Release);
            }
        }
    }

    async fn run_requested_checkpoint(
        &mut self,
        checkpoint: &mut WriterCheckpointController,
        command: CheckpointCommand,
    ) {
        if let Err(error) = command.verify(RuntimeWriteAuthorityStage::Dequeued) {
            command.settle(Err(error));
            return;
        }
        self.checkpoint_blockers.await_released_snapshots().await;
        let (snapshot_blockers, probe, authority, reply) = command.into_parts();
        let result = checkpoint.evaluate_interruptible(snapshot_blockers, move || {
            match probe.interruption() {
                Some(RuntimeInterruptionV1::Cancelled) => Some(CheckpointInterruption::Cancelled),
                Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                    Some(CheckpointInterruption::DeadlineExceeded)
                }
                None => None,
            }
        });
        match result {
            Ok(result) => {
                if authority
                    .verify(RuntimeWriteAuthorityStage::BeforeCommit)
                    .is_err()
                {
                    reply.settle(Err(crate::checkpoint::CheckpointError::AuthorityDenied(
                        RuntimeWriteAuthorityStage::BeforeCommit,
                    )));
                    return;
                }
                self.publish_checkpoint_result(result.clone());
                reply.settle(Ok(result));
            }
            Err(error) => {
                if matches!(
                    &error,
                    crate::checkpoint::CheckpointError::Driver(_)
                        | crate::checkpoint::CheckpointError::InvalidConfig(_)
                ) {
                    self.state
                        .store(WriterState::Faulted as u8, Ordering::Release);
                }
                reply.settle(Err(error));
            }
        }
    }

    fn publish_checkpoint_result(&self, result: CheckpointResult) {
        self.telemetry.checkpoint(checkpoint_sample(&result));
        if let Some(pressure) = checkpoint_pressure_signal(&result) {
            let _ = self.checkpoint_pressure.send(pressure);
        }
        let _ = self.checkpoint_status.send(CheckpointStatus {
            latest: Some(CheckpointOutcome::from_internal(result)),
        });
    }
}

/// The checkpoint policy this writer runs under.
///
/// The operator's WAL budget arrives on `AdmissionConfigV1` and is validated
/// against the contract ceilings before the writer starts, so it, not a
/// crate-local default, is what governs when the controller checkpoints.
pub(super) fn checkpoint_config(config: &AdmissionConfigV1) -> CheckpointConfig {
    CheckpointConfig::from(&config.wal)
}

pub(super) fn checkpoint_pressure_signal(result: &CheckpointResult) -> Option<CheckpointPressure> {
    match result {
        CheckpointResult::Decision {
            sample,
            decision:
                CheckpointDecision::Pending {
                    snapshot_blockers,
                    hard_drain_required: true,
                    ..
                },
        } => Some(CheckpointPressure::BlockGeneral {
            wal: CheckpointWal::from_sample(*sample),
            blockers: snapshot_blockers.clone(),
        }),
        CheckpointResult::Decision {
            sample,
            decision:
                CheckpointDecision::Pending {
                    snapshot_blockers,
                    fence_expired: true,
                    ..
                },
        } => Some(CheckpointPressure::FenceExpired {
            wal: CheckpointWal::from_sample(*sample),
            holders: snapshot_blockers.clone(),
        }),
        CheckpointResult::Decision { .. } => Some(CheckpointPressure::Open),
        CheckpointResult::Interrupted { .. } => None,
    }
}

fn checkpoint_sample(result: &CheckpointResult) -> WalCheckpointSample {
    match result {
        CheckpointResult::Decision { sample, decision } => match decision {
            CheckpointDecision::BelowSoftLimit { .. } => WalCheckpointSample {
                wal_frames: sample.frames,
                wal_bytes: sample.bytes,
                ..WalCheckpointSample::default()
            },
            CheckpointDecision::Complete { report, .. } => WalCheckpointSample {
                wal_frames: sample.frames,
                wal_bytes: sample.bytes,
                checkpointed_frames: report.checkpointed_frames,
                reclaimed_frames: report.checkpointed_frames,
                busy: report.busy,
                completed: true,
                ..WalCheckpointSample::default()
            },
            CheckpointDecision::Pending {
                report,
                snapshot_blockers,
                hard_drain_required,
                ..
            } => WalCheckpointSample {
                wal_frames: sample.frames,
                wal_bytes: sample.bytes,
                checkpointed_frames: report.checkpointed_frames,
                busy: report.busy,
                blocker_count: u64::try_from(snapshot_blockers.count()).unwrap_or(u64::MAX),
                hard_pressure: *hard_drain_required,
                ..WalCheckpointSample::default()
            },
        },
        CheckpointResult::Interrupted {
            sample,
            snapshot_blockers,
            ..
        } => WalCheckpointSample {
            wal_frames: sample.map_or(0, |sample| sample.frames),
            wal_bytes: sample.map_or(0, |sample| sample.bytes),
            blocker_count: u64::try_from(snapshot_blockers.count()).unwrap_or(u64::MAX),
            ..WalCheckpointSample::default()
        },
    }
}

#[tracing::instrument(
    name = "rusqlite_runtime.writer.execution_batch",
    level = "trace",
    skip_all
)]
pub(super) fn process_execution_batch<E: crate::StorageOperationExecutor>(
    connection: &mut rusqlite::Connection,
    binding: &StoreRuntimeBindingV1,
    batch: ExecutionBatch,
    persistence: &mut crate::persistence::RuntimeWriterPersistence<E>,
    telemetry: &WriterTelemetry,
    state: &AtomicU8,
    watermark_publisher: &CommittedWatermarkPublisher,
) {
    // Freeze queue latency at the service boundary. It deliberately includes
    // configured coalescing dwell, whose own span is separate, and
    // must not re-read `enqueued_at` after transaction work has elapsed.
    let dequeued_at = Instant::now();
    let queue_wait_micros =
        queue_wait_micros(batch.items.iter().map(|item| item.enqueued_at), dequeued_at);
    // Cancellation is checked for each request before and after its savepoint
    // work. Aggregating probes into one SQLite progress handler lets a
    // cancelled request interrupt unrelated requests in the same transaction.
    process_batch(
        connection,
        binding,
        batch,
        BatchTiming {
            dequeued_at,
            queue_wait_micros,
        },
        persistence,
        WriterReporting {
            telemetry,
            state,
            watermark_publisher,
        },
    );
}

fn queue_wait_micros(enqueued_at: impl IntoIterator<Item = Instant>, dequeued_at: Instant) -> u64 {
    enqueued_at.into_iter().fold(0, |longest, enqueued_at| {
        longest.max(duration_micros(
            dequeued_at.saturating_duration_since(enqueued_at),
        ))
    })
}

fn run_incremental_vacuum(
    connection: &mut rusqlite::Connection,
    command: IncrementalVacuumCommand,
) {
    if command
        .authority
        .verify(RuntimeWriteAuthorityStage::Dequeued)
        .is_err()
    {
        command.settle(Err(WriterActorError::AuthorityDenied {
            stage: RuntimeWriteAuthorityStage::Dequeued,
        }));
        return;
    }
    let match_result = {
        let _span = tracing::trace_span!("rusqlite.begin_immediate").entered();
        connection.transaction_with_behavior(TransactionBehavior::Immediate)
    };
    let transaction = match match_result {
        Ok(transaction) => transaction,
        Err(error) => {
            command.settle(Err(WriterActorError::IncrementalVacuumFailed(
                error.to_string(),
            )));
            return;
        }
    };
    if let Err(error) =
        transaction.pragma_update(None, "incremental_vacuum", command.max_pages.max(1))
    {
        command.settle(Err(WriterActorError::IncrementalVacuumFailed(
            error.to_string(),
        )));
        return;
    }
    if command
        .authority
        .verify(RuntimeWriteAuthorityStage::BeforeCommit)
        .is_err()
    {
        let result = match transaction.rollback() {
            Ok(()) => Err(WriterActorError::AuthorityDenied {
                stage: RuntimeWriteAuthorityStage::BeforeCommit,
            }),
            Err(error) => Err(WriterActorError::IncrementalVacuumFailed(format!(
                "rollback after authority loss: {error}"
            ))),
        };
        command.settle(result);
        return;
    }
    let result = transaction
        .commit()
        .map_err(|error| WriterActorError::IncrementalVacuumFailed(error.to_string()));
    command.settle(result);
}

fn build_batches(
    selected: Vec<AcceptedRequest>,
    config: &AdmissionConfigV1,
) -> Vec<ExecutionBatch> {
    let mut batches = Vec::new();
    let mut current: Option<(
        OperationPriorityV1,
        RuntimeBatchCompatibilityV1,
        ExecutionBatch,
    )> = None;
    for item in selected {
        let priority = item.priority();
        let budget = match priority {
            OperationPriorityV1::Background => &config.background_batch,
            OperationPriorityV1::Health | OperationPriorityV1::Foreground => {
                &config.foreground_batch
            }
        };
        let compatibility = item.request.transaction_scope().compatibility.clone();
        let needs_new = current
            .as_ref()
            .is_some_and(|(existing_priority, existing, batch)| {
                existing_priority != &priority
                    || existing != &compatibility
                    || item.probe.requires_isolated_commit()
                    || batch
                        .items
                        .first()
                        .is_some_and(|item| item.probe.requires_isolated_commit())
                    || batch.items.len() >= budget.max_operations as usize
                    || batch
                        .bytes
                        .checked_add(item.admission_bytes())
                        .is_none_or(|bytes| bytes > budget.max_bytes)
            });
        if needs_new {
            batches.push(current.take().expect("existing batch").2);
        }
        let (_, _, execution) = current.get_or_insert_with(|| {
            (
                priority,
                compatibility,
                ExecutionBatch {
                    bytes: 0,
                    items: Vec::new(),
                },
            )
        });
        execution.bytes = execution.bytes.saturating_add(item.admission_bytes());
        execution.items.push(item);
    }
    if let Some((_, _, batch)) = current {
        batches.push(batch);
    }
    batches
}

/// Whether a failed scheduled checkpoint is a deferral rather than a fault.
fn scheduled_checkpoint_error_is_transient(
    error: &crate::checkpoint::CheckpointError<crate::checkpoint::RusqliteCheckpointError>,
) -> bool {
    match error {
        crate::checkpoint::CheckpointError::AuthorityDenied(_) => true,
        crate::checkpoint::CheckpointError::Driver(
            crate::checkpoint::RusqliteCheckpointError::Sqlite(rusqlite::Error::SqliteFailure(
                failure,
                _,
            )),
        ) => matches!(
            failure.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        ),
        crate::checkpoint::CheckpointError::Driver(_)
        | crate::checkpoint::CheckpointError::InvalidConfig(_) => false,
    }
}

#[cfg(test)]
mod auxiliary_scheduling_tests {
    use std::{
        collections::{HashMap, VecDeque},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };

    use asupersync::{
        Budget,
        channel::{mpsc, oneshot},
        runtime::RuntimeBuilder,
    };
    use tracedecay_store::{
        OperationPriorityV1, RuntimeCancellationIdentityV1, RuntimeDeadlineV1,
        RuntimeInterruptionV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, UnavailableReasonV1,
    };

    use crate::{
        RuntimeWriteAuthority, RuntimeWriteAuthorityError, RuntimeWriteAuthorityStage,
        admission::{Admission, FairQueue, QueueItem},
        telemetry::WriterTelemetry,
        test_support::{metadata, request as test_request},
    };

    use super::super::{UnrestrictedRuntimeWriteAuthority, admission_limits};
    use super::{
        AcceptedRequest, AuxiliaryWork, BatchCoalescingWindow, IncrementalVacuumCommand,
        PendingBatchDwell, WriterActorError, build_batches, checkpoint_config, dwell_for_batch,
        enqueue, run_incremental_vacuum, select_auxiliary_work,
    };

    struct BatchProbe {
        cancellation: RuntimeCancellationIdentityV1,
        deadline: RuntimeDeadlineV1,
        interruption: Option<RuntimeInterruptionV1>,
        isolated: bool,
        commit_started: AtomicBool,
    }

    impl RuntimeRequestProbeV1 for BatchProbe {
        fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
            &self.cancellation
        }

        fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
            &self.deadline
        }

        fn interruption(&self) -> Option<RuntimeInterruptionV1> {
            self.interruption
        }

        fn try_begin_commit(&self) -> bool {
            self.commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        }

        fn requires_isolated_commit(&self) -> bool {
            self.isolated
        }
    }

    fn accepted_request(
        index: usize,
        enqueued_at: Instant,
        priority: OperationPriorityV1,
        isolated: bool,
    ) -> AcceptedRequest {
        accepted_request_with_interruption(index, enqueued_at, priority, isolated, None)
    }

    fn accepted_request_with_interruption(
        index: usize,
        enqueued_at: Instant,
        priority: OperationPriorityV1,
        isolated: bool,
        interruption: Option<RuntimeInterruptionV1>,
    ) -> AcceptedRequest {
        let mut operation_metadata = metadata(
            &format!("operation.batch-window.{index}"),
            &format!("key.batch-window.{index}"),
            char::from(b'a' + u8::try_from(index % 26).expect("fixture letter fits in one byte")),
        );
        operation_metadata.priority = priority;
        let config = tracedecay_store::AdmissionConfigV1::default();
        let admission = Admission::new(admission_limits(&config).expect("valid default limits"));
        let permit = admission
            .reserve(&operation_metadata)
            .expect("fixture request fits admission");
        let request = Arc::new(test_request(operation_metadata));
        let probe = Arc::new(BatchProbe {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
            interruption,
            isolated,
            commit_started: AtomicBool::new(false),
        });
        let (reply, _response) = oneshot::channel();
        let mut accepted = AcceptedRequest::new(
            request,
            probe,
            Arc::new(UnrestrictedRuntimeWriteAuthority),
            reply,
            permit,
        );
        accepted.enqueued_at = enqueued_at;
        accepted
    }

    fn accepted_request_with_reply(
        operation: &str,
        key: &str,
    ) -> (
        AcceptedRequest,
        oneshot::Receiver<Result<RuntimeSubmitOutcomeV1, tracedecay_store::StorageRuntimeErrorV1>>,
    ) {
        let operation_metadata = metadata(operation, key, 'a');
        let config = tracedecay_store::AdmissionConfigV1::default();
        let admission = Admission::new(admission_limits(&config).expect("valid default limits"));
        let permit = admission
            .reserve(&operation_metadata)
            .expect("fixture request fits admission");
        let request = Arc::new(test_request(operation_metadata));
        let probe = Arc::new(BatchProbe {
            cancellation: request.control().cancellation.clone(),
            deadline: request.control().deadline.clone(),
            interruption: None,
            isolated: false,
            commit_started: AtomicBool::new(false),
        });
        let (reply, response) = oneshot::channel();
        (
            AcceptedRequest::new(
                request,
                probe,
                Arc::new(UnrestrictedRuntimeWriteAuthority),
                reply,
                permit,
            ),
            response,
        )
    }

    #[test]
    fn duplicate_operation_attach_settles_both_requesters_with_the_same_outcome() {
        let telemetry = WriterTelemetry::default();
        let mut queue = FairQueue::default();
        let mut inflight = HashMap::new();
        let (leader, mut leader_rx) =
            accepted_request_with_reply("operation.duplicate-attach", "key.duplicate-attach");
        let (follower, mut follower_rx) =
            accepted_request_with_reply("operation.duplicate-attach", "key.duplicate-attach");
        assert!(enqueue(&mut queue, &mut inflight, leader, &telemetry));
        assert!(enqueue(&mut queue, &mut inflight, follower, &telemetry));
        let mut selected = queue.drain_fair();
        assert_eq!(selected.len(), 1);
        let outcome = RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::Faulted,
        };
        selected
            .pop()
            .expect("queued leader")
            .settle(Ok(outcome.clone()));
        assert_eq!(
            leader_rx.try_recv().expect("leader reply"),
            Ok(outcome.clone())
        );
        assert_eq!(follower_rx.try_recv().expect("follower reply"), Ok(outcome));
    }

    #[test]
    fn inflight_duplicate_attach_settles_both_requesters_with_the_same_outcome() {
        let telemetry = WriterTelemetry::default();
        let mut queue = FairQueue::default();
        let mut inflight = HashMap::new();
        let (leader, mut leader_rx) =
            accepted_request_with_reply("operation.inflight-attach", "key.inflight-attach");
        let (follower, mut follower_rx) =
            accepted_request_with_reply("operation.inflight-attach", "key.inflight-attach");
        assert!(enqueue(&mut queue, &mut inflight, leader, &telemetry));
        let mut selected = queue.drain_fair();
        assert_eq!(selected.len(), 1);
        let executing = selected.pop().expect("dequeued leader");
        inflight.insert(executing.operation_id().clone(), executing.shared_reply());
        assert!(enqueue(&mut queue, &mut inflight, follower, &telemetry));
        let outcome = RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::Faulted,
        };
        executing.settle(Ok(outcome.clone()));
        assert_eq!(
            leader_rx.try_recv().expect("leader reply"),
            Ok(outcome.clone())
        );
        assert_eq!(follower_rx.try_recv().expect("follower reply"), Ok(outcome));
    }

    #[test]
    fn duplicate_operation_with_different_idempotency_is_rejected() {
        let telemetry = WriterTelemetry::default();
        let mut queue = FairQueue::default();
        let mut inflight = HashMap::new();
        let (leader, mut leader_rx) =
            accepted_request_with_reply("operation.duplicate-conflict", "key.leader");
        let (conflict, mut conflict_rx) =
            accepted_request_with_reply("operation.duplicate-conflict", "key.conflict");
        assert!(enqueue(&mut queue, &mut inflight, leader, &telemetry));
        assert!(!enqueue(&mut queue, &mut inflight, conflict, &telemetry));
        assert!(matches!(
            conflict_rx.try_recv().expect("conflict reply"),
            Err(tracedecay_store::StorageRuntimeErrorV1::DuplicateOperationInFlight {
                operation_id
            }) if operation_id == "operation.duplicate-conflict"
        ));
        let outcome = RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::Faulted,
        };
        queue
            .drain_fair()
            .pop()
            .expect("leader remains queued")
            .settle(Ok(outcome.clone()));
        assert_eq!(leader_rx.try_recv().expect("leader reply"), Ok(outcome));
    }

    #[test]
    fn conflicting_submission_after_settlement_is_queued_for_the_ledger() {
        let telemetry = WriterTelemetry::default();
        let mut queue = FairQueue::default();
        let mut inflight = HashMap::new();
        let (leader, mut leader_rx) =
            accepted_request_with_reply("operation.settled-conflict", "key.leader");
        assert!(enqueue(&mut queue, &mut inflight, leader, &telemetry));
        let executing = queue.drain_fair().pop().expect("dequeued leader");
        inflight.insert(executing.operation_id().clone(), executing.shared_reply());
        let (concurrent, mut concurrent_rx) =
            accepted_request_with_reply("operation.settled-conflict", "key.conflict");
        assert!(!enqueue(&mut queue, &mut inflight, concurrent, &telemetry));
        assert_eq!(
            concurrent_rx.try_recv().expect("concurrent conflict reply"),
            Err(
                tracedecay_store::StorageRuntimeErrorV1::DuplicateOperationInFlight {
                    operation_id: "operation.settled-conflict".to_owned(),
                }
            )
        );
        let leader_outcome = RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::Faulted,
        };
        executing.settle(Ok(leader_outcome.clone()));
        assert_eq!(
            leader_rx.try_recv().expect("leader reply"),
            Ok(leader_outcome)
        );

        let (later, mut later_rx) =
            accepted_request_with_reply("operation.settled-conflict", "key.conflict");
        assert!(enqueue(&mut queue, &mut inflight, later, &telemetry));
        let later_outcome = RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::Closed,
        };
        queue
            .drain_fair()
            .pop()
            .expect("later conflict queued")
            .settle(Ok(later_outcome.clone()));
        assert_eq!(later_rx.try_recv().expect("later reply"), Ok(later_outcome));
    }

    #[test]
    fn dwell_collects_a_compatible_arrival_before_dispatch() {
        let runtime = RuntimeBuilder::current_thread().build().expect("runtime");
        let cx = runtime
            .handle()
            .try_request_cx_with_budget(Budget::INFINITE)
            .expect("context");
        // The foreground window is at most 2 ms of wall time, which a loaded
        // machine spends before the dwell starts. Stamping both requests in
        // the future keeps the window open the way a paused clock did.
        let enqueued_at = Instant::now() + Duration::from_secs(60);
        let mut config = tracedecay_store::AdmissionConfigV1::default();
        config.foreground_batch.max_operations = 2;
        config.validate().expect("valid foreground batch limit");
        let selected = vec![accepted_request(
            0,
            enqueued_at,
            OperationPriorityV1::Foreground,
            false,
        )];
        let pending = PendingBatchDwell::from_selected(&selected, &config, enqueued_at)
            .expect("first request opens a dwell window");
        let telemetry = WriterTelemetry::default();
        let mut queue = FairQueue::default();
        let mut inflight = HashMap::new();
        for item in selected {
            assert!(enqueue(&mut queue, &mut inflight, item, &telemetry));
        }

        let (write_tx, mut write_rx) = mpsc::channel(1);
        assert!(
            write_tx
                .try_send(accepted_request(
                    1,
                    enqueued_at + Duration::from_millis(1),
                    OperationPriorityV1::Foreground,
                    false,
                ))
                .is_ok()
        );
        let (_exact_sql_tx, mut exact_sql_rx) = mpsc::channel(1);
        let (_vacuum_tx, mut vacuum_rx) = mpsc::channel(1);
        let (_checkpoint_tx, mut checkpoint_rx) = mpsc::channel(1);
        let (_shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel();
        let mut exact_sql_queue = VecDeque::new();
        let mut vacuum_queue = VecDeque::new();
        let mut checkpoint_queue = VecDeque::new();
        let mut input_closed = false;
        let mut exact_sql_closed = false;
        let mut vacuum_closed = false;
        let mut checkpoint_closed = false;
        runtime.block_on(dwell_for_batch(
            &cx,
            pending,
            &mut write_rx,
            &mut exact_sql_rx,
            &mut vacuum_rx,
            &mut checkpoint_rx,
            &mut shutdown_rx,
            &mut queue,
            &mut inflight,
            &mut exact_sql_queue,
            &mut vacuum_queue,
            &mut checkpoint_queue,
            &telemetry,
            &mut input_closed,
            &mut exact_sql_closed,
            &mut vacuum_closed,
            &mut checkpoint_closed,
        ));

        let batches = build_batches(queue.drain_fair(), &config);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].items.len(), 2);
    }

    #[test]
    fn queued_compatible_arrivals_ignore_the_expired_dwell_window() {
        let enqueued_at = Instant::now();
        let batches = build_batches(
            vec![
                accepted_request(0, enqueued_at, OperationPriorityV1::Foreground, false),
                accepted_request(
                    1,
                    enqueued_at + Duration::from_millis(2),
                    OperationPriorityV1::Foreground,
                    false,
                ),
            ],
            &tracedecay_store::AdmissionConfigV1::default(),
        );

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].items.len(), 2);
    }

    #[test]
    fn execution_batch_count_is_bounded_by_the_operation_budget() {
        const REQUESTS: usize = 256;
        const MAX_OPERATIONS: usize = 64;
        let enqueued_at = Instant::now();
        let selected = (0..REQUESTS)
            .map(|index| {
                accepted_request(index, enqueued_at, OperationPriorityV1::Foreground, false)
            })
            .collect();
        let mut config = tracedecay_store::AdmissionConfigV1::default();
        config.foreground_batch.max_operations = MAX_OPERATIONS as u32;
        config
            .validate()
            .expect("valid operation-bounded batch policy");

        let batches = build_batches(selected, &config);

        assert_eq!(batches.len(), REQUESTS / MAX_OPERATIONS);
        assert_eq!(
            batches.iter().map(|batch| batch.items.len()).sum::<usize>(),
            REQUESTS
        );
        assert!(
            batches
                .iter()
                .all(|batch| batch.items.len() <= MAX_OPERATIONS)
        );
    }

    #[test]
    fn isolated_request_still_opens_its_own_execution_batch() {
        let enqueued_at = Instant::now();
        let selected = (0..8)
            .map(|index| {
                accepted_request(
                    index,
                    enqueued_at,
                    OperationPriorityV1::Foreground,
                    index == 7,
                )
            })
            .collect();

        let batches = build_batches(selected, &tracedecay_store::AdmissionConfigV1::default());

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].items.len(), 7);
        assert_eq!(batches[1].items.len(), 1);
    }

    #[test]
    fn selected_batch_at_either_capacity_bypasses_the_dwell_path() {
        let enqueued_at = Instant::now();

        let mut operation_limited = tracedecay_store::AdmissionConfigV1::default();
        operation_limited.foreground_batch.max_operations = 2;
        operation_limited
            .validate()
            .expect("valid operation-limited batch");
        let operations_full = vec![
            accepted_request(0, enqueued_at, OperationPriorityV1::Foreground, false),
            accepted_request(1, enqueued_at, OperationPriorityV1::Foreground, false),
        ];
        assert!(
            PendingBatchDwell::from_selected(&operations_full, &operation_limited, enqueued_at)
                .is_none()
        );

        let mut byte_limited = tracedecay_store::AdmissionConfigV1::default();
        byte_limited.foreground_batch.max_bytes = 256;
        byte_limited.validate().expect("valid byte-limited batch");
        let bytes_full = vec![
            accepted_request(2, enqueued_at, OperationPriorityV1::Foreground, false),
            accepted_request(3, enqueued_at, OperationPriorityV1::Foreground, false),
        ];
        assert!(
            PendingBatchDwell::from_selected(&bytes_full, &byte_limited, enqueued_at).is_none()
        );

        let mut dwelling = tracedecay_store::AdmissionConfigV1::default();
        dwelling.foreground_batch.max_delay_ms = 1;
        dwelling
            .validate()
            .expect("a one millisecond foreground dwell is valid");
        let pending = PendingBatchDwell::from_selected(
            &[accepted_request(
                4,
                enqueued_at,
                OperationPriorityV1::Foreground,
                false,
            )],
            &dwelling,
            enqueued_at,
        )
        .expect("one foreground request below both budgets opens a dwell");
        assert_eq!(
            pending.window.deadline.duration_since(enqueued_at),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn batch_coalescing_deadlines_use_the_exact_configured_delay() {
        let admitted_at = Instant::now();
        let mut config = tracedecay_store::AdmissionConfigV1::default();
        config.foreground_batch.max_delay_ms = 1;
        config.background_batch.max_delay_ms = 7;
        config
            .validate()
            .expect("tightened dwell windows are valid");

        let foreground = BatchCoalescingWindow::new(
            admitted_at,
            tracedecay_store::OperationPriorityV1::Foreground,
            false,
            false,
            1,
            1,
            &config,
        )
        .expect("foreground request can dwell");
        let background = BatchCoalescingWindow::new(
            admitted_at,
            tracedecay_store::OperationPriorityV1::Background,
            false,
            false,
            1,
            1,
            &config,
        )
        .expect("background request can dwell");

        assert_eq!(
            foreground.deadline.duration_since(admitted_at),
            Duration::from_millis(1)
        );
        assert_eq!(
            background.deadline.duration_since(admitted_at),
            Duration::from_millis(7)
        );
    }

    #[test]
    fn compatible_arrivals_only_join_before_the_exact_deadline() {
        let admitted_at = Instant::now();
        let config = tracedecay_store::AdmissionConfigV1::default();
        let window = BatchCoalescingWindow::new(
            admitted_at,
            tracedecay_store::OperationPriorityV1::Foreground,
            false,
            false,
            1,
            1,
            &config,
        )
        .expect("foreground request can dwell");

        assert!(window.accepts(
            admitted_at + Duration::from_micros(1_999),
            true,
            true,
            false,
            false,
            1,
        ));
        assert!(!window.accepts(window.deadline, true, true, false, false, 1));
        assert!(!window.accepts(
            window.deadline + Duration::from_nanos(1),
            true,
            true,
            false,
            false,
            1,
        ));
    }

    #[test]
    fn health_isolated_and_interrupted_requests_never_dwell() {
        let admitted_at = Instant::now();
        let mut config = tracedecay_store::AdmissionConfigV1::default();
        config.foreground_batch.max_delay_ms = 1;
        config
            .validate()
            .expect("a one millisecond foreground dwell is valid");
        let foreground = BatchCoalescingWindow::new(
            admitted_at,
            tracedecay_store::OperationPriorityV1::Foreground,
            false,
            false,
            1,
            1,
            &config,
        )
        .expect("an ordinary foreground request dwells");
        assert_eq!(
            foreground.deadline.duration_since(admitted_at),
            Duration::from_millis(1)
        );

        assert!(
            BatchCoalescingWindow::new(
                admitted_at,
                tracedecay_store::OperationPriorityV1::Health,
                false,
                false,
                1,
                1,
                &config,
            )
            .is_none()
        );
        assert!(
            BatchCoalescingWindow::new(
                admitted_at,
                tracedecay_store::OperationPriorityV1::Foreground,
                true,
                false,
                1,
                1,
                &config,
            )
            .is_none()
        );
        assert!(
            BatchCoalescingWindow::new(
                admitted_at,
                tracedecay_store::OperationPriorityV1::Background,
                false,
                true,
                1,
                1,
                &config,
            )
            .is_none()
        );
    }

    #[test]
    fn incompatible_or_full_arrivals_end_coalescing() {
        let admitted_at = Instant::now();
        let config = tracedecay_store::AdmissionConfigV1::default();
        let window = BatchCoalescingWindow::new(
            admitted_at,
            tracedecay_store::OperationPriorityV1::Background,
            false,
            false,
            1,
            config.background_batch.max_bytes - 1,
            &config,
        )
        .expect("one final byte still fits");

        assert!(window.accepts(admitted_at, true, true, false, false, 1));
        assert!(!window.accepts(admitted_at, false, true, false, false, 1));
        assert!(!window.accepts(admitted_at, true, false, false, false, 1));
        assert!(!window.accepts(admitted_at, true, true, true, false, 1));
        assert!(!window.accepts(admitted_at, true, true, false, true, 1));
        assert!(!window.accepts(admitted_at, true, true, false, false, 2));
    }

    /// The operator's WAL budget must be the writer's checkpoint policy. A
    /// crate-local default here would make every configured value decorative:
    /// validated, reported in telemetry, and never obeyed.
    #[test]
    fn writer_checkpoint_policy_is_the_configured_wal_budget() {
        let admission = tracedecay_store::AdmissionConfigV1 {
            wal: tracedecay_store::WalBudgetV1 {
                soft_limit_bytes: 4 * 1024 * 1024,
                hard_limit_bytes: 64 * 1024 * 1024,
            },
            ..Default::default()
        };
        admission
            .validate()
            .expect("a tightened WAL budget is inside the contract ceilings");

        let effective = checkpoint_config(&admission);

        assert_eq!(
            effective.soft_wal_bytes,
            4 * 1024 * 1024,
            "configured WAL soft limit must reach the checkpoint controller"
        );
        assert_eq!(
            effective.hard_wal_bytes,
            64 * 1024 * 1024,
            "configured WAL hard limit must reach the checkpoint controller"
        );
        assert_ne!(
            effective,
            super::CheckpointConfig::default(),
            "a configured budget must not collapse back onto the crate default"
        );
    }

    struct RecordingAuthority {
        stages: Arc<Mutex<Vec<RuntimeWriteAuthorityStage>>>,
        deny_before_commit: bool,
    }

    impl RuntimeWriteAuthority for RecordingAuthority {
        fn verify(
            &self,
            stage: RuntimeWriteAuthorityStage,
        ) -> Result<(), RuntimeWriteAuthorityError> {
            self.stages.lock().unwrap().push(stage);
            if self.deny_before_commit && stage == RuntimeWriteAuthorityStage::BeforeCommit {
                Err(RuntimeWriteAuthorityError::denied("revoked before commit"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn auxiliary_work_cannot_starve_product_writes() {
        assert_eq!(
            select_auxiliary_work(true, true, false, true, AuxiliaryWork::IncrementalVacuum),
            Some(AuxiliaryWork::IncrementalVacuum)
        );
        assert_eq!(
            select_auxiliary_work(true, true, false, false, AuxiliaryWork::ExactSql),
            None
        );
    }

    #[test]
    fn auxiliary_work_alternates_when_product_queue_is_empty() {
        assert_eq!(
            select_auxiliary_work(true, true, true, false, AuxiliaryWork::IncrementalVacuum),
            Some(AuxiliaryWork::IncrementalVacuum)
        );
        assert_eq!(
            select_auxiliary_work(true, true, true, false, AuxiliaryWork::ExactSql),
            Some(AuxiliaryWork::ExactSql)
        );
        assert_eq!(
            select_auxiliary_work(false, true, true, false, AuxiliaryWork::ExactSql),
            Some(AuxiliaryWork::IncrementalVacuum)
        );
    }

    #[test]
    fn incremental_vacuum_samples_worker_authority_stages() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "auto_vacuum", "INCREMENTAL")
            .unwrap();
        let stages = Arc::new(Mutex::new(Vec::new()));
        let authority = Arc::new(RecordingAuthority {
            stages: Arc::clone(&stages),
            deny_before_commit: false,
        });
        let (reply, mut response) = oneshot::channel();

        run_incremental_vacuum(
            &mut connection,
            IncrementalVacuumCommand::new(0, authority, reply),
        );

        assert!(response.try_recv().unwrap().is_ok());
        assert_eq!(
            *stages.lock().unwrap(),
            [
                RuntimeWriteAuthorityStage::Dequeued,
                RuntimeWriteAuthorityStage::BeforeCommit
            ]
        );
    }

    #[test]
    fn incremental_vacuum_rolls_back_when_authority_is_revoked() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "auto_vacuum", "INCREMENTAL")
            .unwrap();
        let authority = Arc::new(RecordingAuthority {
            stages: Arc::new(Mutex::new(Vec::new())),
            deny_before_commit: true,
        });
        let (reply, mut response) = oneshot::channel();

        run_incremental_vacuum(
            &mut connection,
            IncrementalVacuumCommand::new(8, authority, reply),
        );

        assert!(matches!(
            response.try_recv().unwrap(),
            Err(WriterActorError::AuthorityDenied {
                stage: RuntimeWriteAuthorityStage::BeforeCommit
            })
        ));
        assert!(connection.is_autocommit());
    }
}
