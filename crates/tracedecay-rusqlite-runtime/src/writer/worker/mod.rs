//! The writer's single worker thread.
//!
//! [`Worker::run`] owns the connection and the loop; the siblings own the two
//! halves the loop leans on — [`ingress`] for how work arrives and where it is
//! parked, and [`rejection`] for settling work that will never run.

use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::SyncSender,
    },
    time::Duration,
};

use rusqlite::TransactionBehavior;
use tokio::{
    runtime::Runtime,
    sync::{mpsc, watch},
};
use tracedecay_store::{
    AdmissionConfigV1, OperationPriorityV1, RuntimeBatchCompatibilityV1, RuntimeInterruptionV1,
    StoreRuntimeBindingV1,
};

#[cfg(not(any(unix, windows)))]
use crate::connection::ConnectionMode;
use crate::{
    RuntimeWriteAuthorityStage,
    admission::{FairQueue, QueueItem},
    checkpoint::{
        CheckpointBlockers, CheckpointConfig, CheckpointDecision, CheckpointInterruption,
        CheckpointOutcome, CheckpointPressure, CheckpointResult, CheckpointStatus, CheckpointWal,
        MaintenanceCheckpointMode, RusqliteCheckpointDriver, WriterCheckpointController,
    },
    connection::{self, OpenedDatabaseFile},
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
    transaction::process_batch,
};

mod ingress;
mod rejection;

use ingress::{
    AuxiliaryWork, WorkerWake, apply_wake, drain_command_ingress, drain_ingress,
    select_auxiliary_work, wait_for_work,
};
use rejection::{
    cancel_waiting, reject_all, reject_all_incremental_vacuum, reject_all_migration_sql,
    reject_all_online_backup, reject_incremental_vacuum, reject_online_backup, reject_unauthorized,
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
        let mut checkpoint = match WriterCheckpointController::new(
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
            drain_command_ingress(
                &mut self.checkpoint_receiver,
                &mut checkpoint_queue,
                &mut checkpoint_closed,
            );
            drain_command_ingress(
                &mut self.migration_sql_receiver,
                &mut migration_sql_queue,
                &mut migration_sql_closed,
            );
            drain_command_ingress(
                &mut self.incremental_vacuum_receiver,
                &mut incremental_vacuum_queue,
                &mut incremental_vacuum_closed,
            );
            drain_command_ingress(
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
