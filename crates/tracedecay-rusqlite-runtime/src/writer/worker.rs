use std::{
    collections::VecDeque,
    future::poll_fn,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::SyncSender,
    },
    task::Poll,
    time::Duration,
};

use rusqlite::TransactionBehavior;
use tokio::{
    runtime::Runtime,
    sync::{mpsc, watch},
};
use tracedecay_store::{
    AdmissionConfigV1, OperationPriorityV1, RuntimeBatchCompatibilityV1,
    RuntimeCancellationStageV1, RuntimeInterruptionV1, RuntimeSubmitOutcomeV1,
    StoreRuntimeBindingV1, UnavailableReasonV1,
};

use crate::{
    RuntimeWriteAuthorityStage,
    admission::{FairQueue, QueueItem},
    checkpoint::{
        CheckpointBlockers, CheckpointConfig, CheckpointDecision, CheckpointInterruption,
        CheckpointOutcome, CheckpointPressure, CheckpointResult, CheckpointStatus, CheckpointWal,
        MaintenanceCheckpointMode, RusqliteCheckpointDriver, WriterCheckpointController,
    },
    connection::{self, ConnectionMode, OpenedDatabaseFile},
    migration_sql::{
        WriterCommand as MigrationSqlWriterCommand, reject_writer_command, run_writer_command,
    },
    read_consistency::CommittedWatermarkPublisher,
    telemetry::WriterTelemetry,
};

use super::{
    WriterActorError, WriterPersistence, WriterStartError, WriterState,
    backup::{OnlineBackupCommand, run_online_backup},
    request::{
        AcceptedRequest, CheckpointCommand, CheckpointCommandKind, ExecutionBatch,
        IncrementalVacuumCommand,
    },
    settlement::{infrastructure, interruption_outcome},
    transaction::process_batch,
};

const HARD_CHECKPOINT_RETRY_INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct Worker {
    pub(super) path: PathBuf,
    #[cfg(unix)]
    pub(super) canonical_path: PathBuf,
    pub(super) expected_file_identity: Option<u64>,
    pub(super) _opened_database: Option<Arc<OpenedDatabaseFile>>,
    pub(super) binding: StoreRuntimeBindingV1,
    pub(super) config: AdmissionConfigV1,
    pub(super) receiver: mpsc::Receiver<AcceptedRequest>,
    pub(super) migration_sql_receiver: mpsc::Receiver<MigrationSqlWriterCommand>,
    pub(super) incremental_vacuum_receiver: mpsc::Receiver<IncrementalVacuumCommand>,
    pub(super) online_backup_receiver: mpsc::Receiver<OnlineBackupCommand>,
    pub(super) checkpoint_receiver: mpsc::Receiver<CheckpointCommand>,
    pub(super) shutdown_receiver: mpsc::UnboundedReceiver<()>,
    pub(super) persistence: Box<dyn WriterPersistence>,
    pub(super) state: Arc<AtomicU8>,
    pub(super) shutdown_requested: Arc<AtomicBool>,
    pub(super) telemetry: WriterTelemetry,
    /// The worker-only capability that advances read-consistency state.
    pub(super) watermark_publisher: CommittedWatermarkPublisher,
    pub(super) checkpoint_status: watch::Sender<CheckpointStatus>,
    pub(super) checkpoint_pressure: watch::Sender<CheckpointPressure>,
    pub(super) started: SyncSender<Result<Option<u64>, WriterStartError>>,
}

impl Worker {
    pub(super) fn run(self) {
        let connection = match connection::open(&self.path, ConnectionMode::Writer) {
            Ok(connection) => connection,
            Err(error) if error.is_open_failure() => {
                return self.fail_start(WriterStartError::OpenFailed);
            }
            Err(_) => return self.fail_start(WriterStartError::BusyTimeoutSetupFailed),
        };
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
        let checkpoint = match WriterCheckpointController::new(
            RusqliteCheckpointDriver::new(connection),
            CheckpointConfig::default(),
        ) {
            Ok(checkpoint) => checkpoint,
            Err(_) => return self.fail_start(WriterStartError::CheckpointSetupFailed),
        };
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return self.fail_start(WriterStartError::CheckpointSchedulerSetupFailed),
        };
        self.state
            .store(WriterState::Ready as u8, Ordering::Release);
        if self.started.send(Ok(opened_file_identity)).is_err() {
            self.state
                .store(WriterState::Draining as u8, Ordering::Release);
            return;
        }
        let state = Arc::clone(&self.state);
        let telemetry = self.telemetry.clone();
        if catch_unwind(AssertUnwindSafe(|| self.run_loop(checkpoint, runtime))).is_err() {
            state.store(WriterState::Faulted as u8, Ordering::Release);
            telemetry.fault_unsettled();
        }
    }

    fn fail_start(&self, error: WriterStartError) {
        self.state
            .store(WriterState::Closed as u8, Ordering::Release);
        let _ = self.started.send(Err(error));
    }

    fn run_loop(
        mut self,
        mut checkpoint: WriterCheckpointController<RusqliteCheckpointDriver>,
        runtime: Runtime,
    ) {
        let mut queue = FairQueue::default();
        let mut migration_sql_queue = VecDeque::new();
        let mut incremental_vacuum_queue = VecDeque::new();
        let mut online_backup_queue = VecDeque::new();
        let mut checkpoint_queue = VecDeque::new();
        let mut input_closed = false;
        let mut migration_sql_closed = false;
        let mut incremental_vacuum_closed = false;
        let mut online_backup_closed = false;
        let mut checkpoint_closed = false;
        let mut prefer_auxiliary = true;
        let mut next_auxiliary = AuxiliaryWork::IncrementalVacuum;
        let mut latest_blockers = CheckpointBlockers::default();
        loop {
            drain_ingress(
                &mut self.receiver,
                &mut queue,
                &self.telemetry,
                &mut input_closed,
            );
            drain_checkpoint_ingress(
                &mut self.checkpoint_receiver,
                &mut checkpoint_queue,
                &mut checkpoint_closed,
            );
            drain_migration_sql_ingress(
                &mut self.migration_sql_receiver,
                &mut migration_sql_queue,
                &mut migration_sql_closed,
            );
            drain_incremental_vacuum_ingress(
                &mut self.incremental_vacuum_receiver,
                &mut incremental_vacuum_queue,
                &mut incremental_vacuum_closed,
            );
            drain_online_backup_ingress(
                &mut self.online_backup_receiver,
                &mut online_backup_queue,
                &mut online_backup_closed,
            );
            if self.shutdown_requested.load(Ordering::Acquire)
                && queue.is_empty()
                && migration_sql_queue.is_empty()
                && incremental_vacuum_queue.is_empty()
                && online_backup_queue.is_empty()
            {
                checkpoint_queue.clear();
                break;
            }
            if self.state.load(Ordering::Acquire) == WriterState::Faulted as u8 {
                reject_all(&mut queue, &self.telemetry);
                reject_all_migration_sql(&mut migration_sql_queue);
                reject_all_incremental_vacuum(&mut incremental_vacuum_queue);
                reject_all_online_backup(&mut online_backup_queue);
                checkpoint_queue.clear();
                if input_closed
                    && migration_sql_closed
                    && incremental_vacuum_closed
                    && online_backup_closed
                    && checkpoint_closed
                {
                    break;
                }
                let wake = runtime.block_on(wait_for_work(
                    &mut self.receiver,
                    &mut self.migration_sql_receiver,
                    &mut self.incremental_vacuum_receiver,
                    &mut self.online_backup_receiver,
                    &mut self.checkpoint_receiver,
                    &mut self.shutdown_receiver,
                    input_closed,
                    migration_sql_closed,
                    incremental_vacuum_closed,
                    online_backup_closed,
                    checkpoint_closed,
                    false,
                ));
                apply_wake(
                    wake,
                    &mut queue,
                    &mut migration_sql_queue,
                    &mut incremental_vacuum_queue,
                    &mut online_backup_queue,
                    &mut checkpoint_queue,
                    &self.telemetry,
                    &mut input_closed,
                    &mut migration_sql_closed,
                    &mut incremental_vacuum_closed,
                    &mut online_backup_closed,
                    &mut checkpoint_closed,
                );
                continue;
            }
            if let Some(command) = checkpoint_queue.pop_front() {
                latest_blockers = command.snapshot_blockers.clone();
                self.run_requested_checkpoint(&mut checkpoint, command);
                continue;
            }
            if let Some(auxiliary) = select_auxiliary_work(
                !migration_sql_queue.is_empty(),
                !incremental_vacuum_queue.is_empty(),
                !online_backup_queue.is_empty(),
                queue.is_empty(),
                prefer_auxiliary,
                next_auxiliary,
            ) {
                match auxiliary {
                    AuxiliaryWork::MigrationSql => {
                        let command = migration_sql_queue
                            .pop_front()
                            .expect("migration SQL queue checked non-empty");
                        if self.state.load(Ordering::Acquire) == WriterState::Ready as u8 {
                            run_writer_command(
                                checkpoint.connection_mut(),
                                command,
                                &self.shutdown_requested,
                            );
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
                            run_incremental_vacuum(checkpoint.connection_mut(), command);
                        } else {
                            reject_incremental_vacuum(command);
                        }
                        next_auxiliary = AuxiliaryWork::OnlineBackup;
                    }
                    AuxiliaryWork::OnlineBackup => {
                        let command = online_backup_queue
                            .pop_front()
                            .expect("online backup queue checked non-empty");
                        if self.state.load(Ordering::Acquire) == WriterState::Ready as u8 {
                            run_online_backup(
                                checkpoint.connection_mut(),
                                &self.binding,
                                &self.watermark_publisher,
                                &self.shutdown_requested,
                                command,
                            );
                        } else {
                            reject_online_backup(command);
                        }
                        next_auxiliary = AuxiliaryWork::MigrationSql;
                    }
                }
                if !queue.is_empty() {
                    prefer_auxiliary = false;
                }
                continue;
            }
            if queue.is_empty() {
                if input_closed
                    && migration_sql_closed
                    && incremental_vacuum_closed
                    && online_backup_closed
                    && checkpoint_closed
                {
                    break;
                }
                let wake = runtime.block_on(wait_for_work(
                    &mut self.receiver,
                    &mut self.migration_sql_receiver,
                    &mut self.incremental_vacuum_receiver,
                    &mut self.online_backup_receiver,
                    &mut self.checkpoint_receiver,
                    &mut self.shutdown_receiver,
                    input_closed,
                    migration_sql_closed,
                    incremental_vacuum_closed,
                    online_backup_closed,
                    checkpoint_closed,
                    checkpoint.hard_drain_required(),
                ));
                if matches!(wake, WorkerWake::CheckpointRetry) {
                    self.run_scheduled_checkpoint(&mut checkpoint, latest_blockers.clone());
                } else {
                    apply_wake(
                        wake,
                        &mut queue,
                        &mut migration_sql_queue,
                        &mut incremental_vacuum_queue,
                        &mut online_backup_queue,
                        &mut checkpoint_queue,
                        &self.telemetry,
                        &mut input_closed,
                        &mut migration_sql_closed,
                        &mut incremental_vacuum_closed,
                        &mut online_backup_closed,
                        &mut checkpoint_closed,
                    );
                }
                continue;
            }
            cancel_waiting(&mut queue, &self.telemetry);
            reject_unauthorized(&mut queue, &self.telemetry);
            if queue.is_empty() {
                continue;
            }
            let selected = queue.drain_fair();
            debug_assert!(!selected.is_empty());
            for batch in build_batches(selected, &self.config) {
                self.telemetry.released(
                    u32::try_from(batch.items.len()).unwrap_or(u32::MAX),
                    batch.bytes,
                );
                process_execution_batch(
                    checkpoint.connection_mut(),
                    &self.binding,
                    batch,
                    self.persistence.as_mut(),
                    &self.telemetry,
                    &self.state,
                    &self.watermark_publisher,
                );
                self.run_scheduled_checkpoint(&mut checkpoint, latest_blockers.clone());
                if self.state.load(Ordering::Acquire) == WriterState::Faulted as u8 {
                    break;
                }
            }
            prefer_auxiliary = true;
        }
        if self.state.load(Ordering::Acquire) != WriterState::Faulted as u8 {
            self.state
                .store(WriterState::Closed as u8, Ordering::Release);
        }
    }

    fn run_scheduled_checkpoint(
        &self,
        checkpoint: &mut WriterCheckpointController<RusqliteCheckpointDriver>,
        snapshot_blockers: CheckpointBlockers,
    ) {
        match checkpoint.evaluate_scheduled(snapshot_blockers) {
            Ok(result) => self.publish_checkpoint_result(result),
            Err(_) => {
                self.state
                    .store(WriterState::Faulted as u8, Ordering::Release);
            }
        }
    }

    fn run_requested_checkpoint(
        &self,
        checkpoint: &mut WriterCheckpointController<RusqliteCheckpointDriver>,
        command: CheckpointCommand,
    ) {
        if let Err(error) = command.verify(RuntimeWriteAuthorityStage::Dequeued) {
            command.settle(Err(error));
            return;
        }
        let (snapshot_blockers, kind, authority, reply) = command.into_parts();
        let result = match kind {
            CheckpointCommandKind::Passive { probe } => {
                checkpoint.evaluate_interruptible(snapshot_blockers, move || {
                    match probe.interruption() {
                        Some(RuntimeInterruptionV1::Cancelled) => {
                            Some(CheckpointInterruption::Cancelled)
                        }
                        Some(RuntimeInterruptionV1::DeadlineExceeded) => {
                            Some(CheckpointInterruption::DeadlineExceeded)
                        }
                        None => None,
                    }
                })
            }
            CheckpointCommandKind::Maintenance { mode, permit } => match mode {
                MaintenanceCheckpointMode::Restart => {
                    checkpoint.restart_scheduled(&permit, snapshot_blockers)
                }
                MaintenanceCheckpointMode::Truncate => {
                    checkpoint.truncate_scheduled(&permit, snapshot_blockers)
                }
            },
        };
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
        if let Some(pressure) = checkpoint_pressure_signal(&result) {
            self.checkpoint_pressure.send_replace(pressure);
        }
        self.checkpoint_status.send_replace(CheckpointStatus {
            latest: Some(CheckpointOutcome::from_internal(result)),
        });
    }
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
        CheckpointResult::Decision { .. } => Some(CheckpointPressure::Open),
        CheckpointResult::Interrupted { .. } => None,
    }
}

pub(super) fn process_execution_batch(
    connection: &mut rusqlite::Connection,
    binding: &StoreRuntimeBindingV1,
    batch: ExecutionBatch,
    persistence: &mut dyn WriterPersistence,
    telemetry: &WriterTelemetry,
    state: &AtomicU8,
    watermark_publisher: &CommittedWatermarkPublisher,
) {
    // Cancellation is checked for each request before and after its savepoint
    // work. Aggregating probes into one SQLite progress handler lets a
    // cancelled request interrupt unrelated requests in the same transaction.
    process_batch(
        connection,
        binding,
        batch,
        persistence,
        telemetry,
        state,
        watermark_publisher,
    );
}

enum WorkerWake {
    Write(Option<AcceptedRequest>),
    MigrationSql(Box<Option<MigrationSqlWriterCommand>>),
    IncrementalVacuum(Box<Option<IncrementalVacuumCommand>>),
    OnlineBackup(Box<Option<OnlineBackupCommand>>),
    Checkpoint(Box<Option<CheckpointCommand>>),
    Shutdown,
    CheckpointRetry,
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_work(
    receiver: &mut mpsc::Receiver<AcceptedRequest>,
    migration_sql_receiver: &mut mpsc::Receiver<MigrationSqlWriterCommand>,
    incremental_vacuum_receiver: &mut mpsc::Receiver<IncrementalVacuumCommand>,
    online_backup_receiver: &mut mpsc::Receiver<OnlineBackupCommand>,
    checkpoint_receiver: &mut mpsc::Receiver<CheckpointCommand>,
    shutdown_receiver: &mut mpsc::UnboundedReceiver<()>,
    input_closed: bool,
    migration_sql_closed: bool,
    incremental_vacuum_closed: bool,
    online_backup_closed: bool,
    checkpoint_closed: bool,
    retry_checkpoint: bool,
) -> WorkerWake {
    let receive = poll_fn(|context| {
        if Pin::new(&mut *shutdown_receiver)
            .poll_recv(context)
            .is_ready()
        {
            return Poll::Ready(WorkerWake::Shutdown);
        }
        if !checkpoint_closed
            && let Poll::Ready(command) = Pin::new(&mut *checkpoint_receiver).poll_recv(context)
        {
            return Poll::Ready(WorkerWake::Checkpoint(Box::new(command)));
        }
        if !migration_sql_closed
            && let Poll::Ready(command) = Pin::new(&mut *migration_sql_receiver).poll_recv(context)
        {
            return Poll::Ready(WorkerWake::MigrationSql(Box::new(command)));
        }
        if !incremental_vacuum_closed
            && let Poll::Ready(command) =
                Pin::new(&mut *incremental_vacuum_receiver).poll_recv(context)
        {
            return Poll::Ready(WorkerWake::IncrementalVacuum(Box::new(command)));
        }
        if !online_backup_closed
            && let Poll::Ready(command) = Pin::new(&mut *online_backup_receiver).poll_recv(context)
        {
            return Poll::Ready(WorkerWake::OnlineBackup(Box::new(command)));
        }
        if !input_closed && let Poll::Ready(item) = Pin::new(&mut *receiver).poll_recv(context) {
            return Poll::Ready(WorkerWake::Write(item));
        }
        Poll::Pending
    });
    if retry_checkpoint {
        match tokio::time::timeout(HARD_CHECKPOINT_RETRY_INTERVAL, receive).await {
            Ok(wake) => wake,
            Err(_) => WorkerWake::CheckpointRetry,
        }
    } else {
        receive.await
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_wake(
    wake: WorkerWake,
    queue: &mut FairQueue<AcceptedRequest>,
    migration_sql_queue: &mut VecDeque<MigrationSqlWriterCommand>,
    incremental_vacuum_queue: &mut VecDeque<IncrementalVacuumCommand>,
    online_backup_queue: &mut VecDeque<OnlineBackupCommand>,
    checkpoint_queue: &mut VecDeque<CheckpointCommand>,
    telemetry: &WriterTelemetry,
    input_closed: &mut bool,
    migration_sql_closed: &mut bool,
    incremental_vacuum_closed: &mut bool,
    online_backup_closed: &mut bool,
    checkpoint_closed: &mut bool,
) {
    match wake {
        WorkerWake::Write(Some(item)) => enqueue(queue, item, telemetry),
        WorkerWake::Write(None) => *input_closed = true,
        WorkerWake::MigrationSql(command) => match *command {
            Some(command) => migration_sql_queue.push_back(command),
            None => *migration_sql_closed = true,
        },
        WorkerWake::IncrementalVacuum(command) => match *command {
            Some(command) => incremental_vacuum_queue.push_back(command),
            None => *incremental_vacuum_closed = true,
        },
        WorkerWake::OnlineBackup(command) => match *command {
            Some(command) => online_backup_queue.push_back(command),
            None => *online_backup_closed = true,
        },
        WorkerWake::Checkpoint(command) => match *command {
            Some(command) => checkpoint_queue.push_back(command),
            None => *checkpoint_closed = true,
        },
        WorkerWake::Shutdown => {}
        WorkerWake::CheckpointRetry => {}
    }
}

fn drain_incremental_vacuum_ingress(
    receiver: &mut mpsc::Receiver<IncrementalVacuumCommand>,
    queue: &mut VecDeque<IncrementalVacuumCommand>,
    input_closed: &mut bool,
) {
    loop {
        match receiver.try_recv() {
            Ok(command) => queue.push_back(command),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                *input_closed = true;
                break;
            }
        }
    }
}

fn drain_online_backup_ingress(
    receiver: &mut mpsc::Receiver<OnlineBackupCommand>,
    queue: &mut VecDeque<OnlineBackupCommand>,
    input_closed: &mut bool,
) {
    loop {
        match receiver.try_recv() {
            Ok(command) => queue.push_back(command),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                *input_closed = true;
                break;
            }
        }
    }
}

fn drain_migration_sql_ingress(
    receiver: &mut mpsc::Receiver<MigrationSqlWriterCommand>,
    queue: &mut VecDeque<MigrationSqlWriterCommand>,
    input_closed: &mut bool,
) {
    loop {
        match receiver.try_recv() {
            Ok(command) => queue.push_back(command),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                *input_closed = true;
                break;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuxiliaryWork {
    MigrationSql,
    IncrementalVacuum,
    OnlineBackup,
}

fn select_auxiliary_work(
    migration_waiting: bool,
    incremental_vacuum_waiting: bool,
    online_backup_waiting: bool,
    product_queue_empty: bool,
    prefer_auxiliary: bool,
    next: AuxiliaryWork,
) -> Option<AuxiliaryWork> {
    if !product_queue_empty && !prefer_auxiliary {
        return None;
    }
    let waiting = |work| match work {
        AuxiliaryWork::MigrationSql => migration_waiting,
        AuxiliaryWork::IncrementalVacuum => incremental_vacuum_waiting,
        AuxiliaryWork::OnlineBackup => online_backup_waiting,
    };
    let order = match next {
        AuxiliaryWork::MigrationSql => [
            AuxiliaryWork::MigrationSql,
            AuxiliaryWork::IncrementalVacuum,
            AuxiliaryWork::OnlineBackup,
        ],
        AuxiliaryWork::IncrementalVacuum => [
            AuxiliaryWork::IncrementalVacuum,
            AuxiliaryWork::OnlineBackup,
            AuxiliaryWork::MigrationSql,
        ],
        AuxiliaryWork::OnlineBackup => [
            AuxiliaryWork::OnlineBackup,
            AuxiliaryWork::MigrationSql,
            AuxiliaryWork::IncrementalVacuum,
        ],
    };
    order.into_iter().find(|work| waiting(*work))
}

fn drain_checkpoint_ingress(
    receiver: &mut mpsc::Receiver<CheckpointCommand>,
    queue: &mut VecDeque<CheckpointCommand>,
    input_closed: &mut bool,
) {
    loop {
        match receiver.try_recv() {
            Ok(command) => queue.push_back(command),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                *input_closed = true;
                break;
            }
        }
    }
}

fn drain_ingress(
    receiver: &mut mpsc::Receiver<AcceptedRequest>,
    queue: &mut FairQueue<AcceptedRequest>,
    telemetry: &WriterTelemetry,
    input_closed: &mut bool,
) {
    loop {
        match receiver.try_recv() {
            Ok(item) => enqueue(queue, item, telemetry),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                *input_closed = true;
                break;
            }
        }
    }
}

fn enqueue(
    queue: &mut FairQueue<AcceptedRequest>,
    item: AcceptedRequest,
    telemetry: &WriterTelemetry,
) {
    if let Err(item) = queue.push(item) {
        let result = Err(infrastructure(
            "duplicate operation id reached persistent writer",
        ));
        telemetry.released(1, item.admission_bytes());
        telemetry.completed(&result);
        item.settle(result);
    }
}

fn cancel_waiting(queue: &mut FairQueue<AcceptedRequest>, telemetry: &WriterTelemetry) {
    for item in queue.drain_matching(|item| item.probe.interruption().is_some()) {
        let bytes = item.admission_bytes();
        let outcome = interruption_outcome(
            &item.request,
            item.probe.as_ref(),
            RuntimeCancellationStageV1::Queued,
        )
        .expect("selected request is interrupted");
        let result = Ok(outcome);
        telemetry.released(1, bytes);
        telemetry.completed(&result);
        item.settle(result);
    }
}

fn reject_unauthorized(queue: &mut FairQueue<AcceptedRequest>, telemetry: &WriterTelemetry) {
    for item in queue.drain_matching(|item| {
        item.authority
            .verify(RuntimeWriteAuthorityStage::Dequeued)
            .is_err()
    }) {
        let bytes = item.admission_bytes();
        let result = Ok(super::settlement::missing_authority());
        telemetry.released(1, bytes);
        telemetry.completed(&result);
        item.settle(result);
    }
}

fn reject_all(queue: &mut FairQueue<AcceptedRequest>, telemetry: &WriterTelemetry) {
    for item in queue.drain_all() {
        let bytes = item.admission_bytes();
        let result = Ok(RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::Faulted,
        });
        telemetry.released(1, bytes);
        telemetry.completed(&result);
        item.settle(result);
    }
}

fn reject_all_migration_sql(queue: &mut VecDeque<MigrationSqlWriterCommand>) {
    for command in queue.drain(..) {
        reject_writer_command(command);
    }
}

fn reject_online_backup(command: OnlineBackupCommand) {
    command.settle(Err(WriterActorError::OnlineBackupFailed(
        super::WriterOnlineBackupError::WriterShuttingDown,
    )));
}

fn reject_all_online_backup(queue: &mut VecDeque<OnlineBackupCommand>) {
    for command in queue.drain(..) {
        reject_online_backup(command);
    }
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
    let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
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

fn reject_incremental_vacuum(command: IncrementalVacuumCommand) {
    command.settle(Err(WriterActorError::IncrementalVacuumFailed(
        "writer is unavailable".to_owned(),
    )));
}

fn reject_all_incremental_vacuum(queue: &mut VecDeque<IncrementalVacuumCommand>) {
    for command in queue.drain(..) {
        reject_incremental_vacuum(command);
    }
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

#[cfg(test)]
mod auxiliary_scheduling_tests {
    use std::sync::{Arc, Mutex};

    use tokio::sync::oneshot;

    use crate::{RuntimeWriteAuthority, RuntimeWriteAuthorityError, RuntimeWriteAuthorityStage};

    use super::{
        AuxiliaryWork, IncrementalVacuumCommand, WriterActorError, run_incremental_vacuum,
        select_auxiliary_work,
    };

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
            select_auxiliary_work(
                true,
                true,
                false,
                false,
                true,
                AuxiliaryWork::IncrementalVacuum,
            ),
            Some(AuxiliaryWork::IncrementalVacuum)
        );
        assert_eq!(
            select_auxiliary_work(true, true, false, false, false, AuxiliaryWork::MigrationSql,),
            None
        );
    }

    #[test]
    fn auxiliary_work_alternates_when_product_queue_is_empty() {
        assert_eq!(
            select_auxiliary_work(
                true,
                true,
                false,
                true,
                false,
                AuxiliaryWork::IncrementalVacuum,
            ),
            Some(AuxiliaryWork::IncrementalVacuum)
        );
        assert_eq!(
            select_auxiliary_work(true, true, false, true, false, AuxiliaryWork::MigrationSql,),
            Some(AuxiliaryWork::MigrationSql)
        );
        assert_eq!(
            select_auxiliary_work(true, true, true, true, false, AuxiliaryWork::OnlineBackup,),
            Some(AuxiliaryWork::OnlineBackup)
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
