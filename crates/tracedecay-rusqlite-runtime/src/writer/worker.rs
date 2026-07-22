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
    admission::{FairQueue, QueueItem},
    checkpoint::{
        CheckpointBlockers, CheckpointConfig, CheckpointDecision, CheckpointInterruption,
        CheckpointOutcome, CheckpointPressure, CheckpointResult, CheckpointStatus, CheckpointWal,
        MaintenanceCheckpointMode, RusqliteCheckpointDriver, WriterCheckpointController,
    },
    connection::{self, ConnectionMode},
    read_consistency::CommittedWatermarkPublisher,
    telemetry::WriterTelemetry,
};

use super::{
    WriterPersistence, WriterStartError, WriterState,
    request::{AcceptedRequest, CheckpointCommand, CheckpointCommandKind, ExecutionBatch},
    settlement::{infrastructure, interruption_outcome},
    transaction::process_batch,
};

const HARD_CHECKPOINT_RETRY_INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct Worker {
    pub(super) path: PathBuf,
    pub(super) binding: StoreRuntimeBindingV1,
    pub(super) config: AdmissionConfigV1,
    pub(super) receiver: mpsc::Receiver<AcceptedRequest>,
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
    pub(super) started: SyncSender<Result<(), WriterStartError>>,
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
        if self.started.send(Ok(())).is_err() {
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
        let mut checkpoint_queue = VecDeque::new();
        let mut input_closed = false;
        let mut checkpoint_closed = false;
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
            if self.shutdown_requested.load(Ordering::Acquire) && queue.is_empty() {
                checkpoint_queue.clear();
                break;
            }
            if self.state.load(Ordering::Acquire) == WriterState::Faulted as u8 {
                reject_all(&mut queue, &self.telemetry);
                checkpoint_queue.clear();
                if input_closed && checkpoint_closed {
                    break;
                }
                let wake = runtime.block_on(wait_for_work(
                    &mut self.receiver,
                    &mut self.checkpoint_receiver,
                    &mut self.shutdown_receiver,
                    input_closed,
                    checkpoint_closed,
                    false,
                ));
                apply_wake(
                    wake,
                    &mut queue,
                    &mut checkpoint_queue,
                    &self.telemetry,
                    &mut input_closed,
                    &mut checkpoint_closed,
                );
                continue;
            }
            if let Some(command) = checkpoint_queue.pop_front() {
                latest_blockers = command.snapshot_blockers.clone();
                self.run_requested_checkpoint(&mut checkpoint, command);
                continue;
            }
            if queue.is_empty() {
                if input_closed && checkpoint_closed {
                    break;
                }
                let wake = runtime.block_on(wait_for_work(
                    &mut self.receiver,
                    &mut self.checkpoint_receiver,
                    &mut self.shutdown_receiver,
                    input_closed,
                    checkpoint_closed,
                    checkpoint.hard_drain_required(),
                ));
                if matches!(wake, WorkerWake::CheckpointRetry) {
                    self.run_scheduled_checkpoint(&mut checkpoint, latest_blockers.clone());
                } else {
                    apply_wake(
                        wake,
                        &mut queue,
                        &mut checkpoint_queue,
                        &self.telemetry,
                        &mut input_closed,
                        &mut checkpoint_closed,
                    );
                }
                continue;
            }
            cancel_waiting(&mut queue, &self.telemetry);
            if queue.is_empty() {
                continue;
            }
            let selected = queue.drain_fair();
            debug_assert!(!selected.is_empty());
            for batch in build_batches(selected, &self.config) {
                let probes = batch
                    .items
                    .iter()
                    .map(|item| Arc::clone(&item.probe))
                    .collect::<Vec<_>>();
                self.telemetry.released(
                    u32::try_from(batch.items.len()).unwrap_or(u32::MAX),
                    batch.bytes,
                );
                connection::with_progress_cancellation(
                    checkpoint.connection_mut(),
                    move || probes.iter().any(|probe| probe.interruption().is_some()),
                    |connection| {
                        process_batch(
                            connection,
                            &self.binding,
                            batch,
                            self.persistence.as_mut(),
                            &self.telemetry,
                            &self.state,
                            &self.watermark_publisher,
                        );
                    },
                )
                .expect("install worker-local SQLite progress handler");
                self.run_scheduled_checkpoint(&mut checkpoint, latest_blockers.clone());
                if self.state.load(Ordering::Acquire) == WriterState::Faulted as u8 {
                    break;
                }
            }
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
        let (snapshot_blockers, kind, reply) = command.into_parts();
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

enum WorkerWake {
    Write(Option<AcceptedRequest>),
    Checkpoint(Box<Option<CheckpointCommand>>),
    Shutdown,
    CheckpointRetry,
}

async fn wait_for_work(
    receiver: &mut mpsc::Receiver<AcceptedRequest>,
    checkpoint_receiver: &mut mpsc::Receiver<CheckpointCommand>,
    shutdown_receiver: &mut mpsc::UnboundedReceiver<()>,
    input_closed: bool,
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

fn apply_wake(
    wake: WorkerWake,
    queue: &mut FairQueue<AcceptedRequest>,
    checkpoint_queue: &mut VecDeque<CheckpointCommand>,
    telemetry: &WriterTelemetry,
    input_closed: &mut bool,
    checkpoint_closed: &mut bool,
) {
    match wake {
        WorkerWake::Write(Some(item)) => enqueue(queue, item, telemetry),
        WorkerWake::Write(None) => *input_closed = true,
        WorkerWake::Checkpoint(command) => match *command {
            Some(command) => checkpoint_queue.push_back(command),
            None => *checkpoint_closed = true,
        },
        WorkerWake::Shutdown => {}
        WorkerWake::CheckpointRetry => {}
    }
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
