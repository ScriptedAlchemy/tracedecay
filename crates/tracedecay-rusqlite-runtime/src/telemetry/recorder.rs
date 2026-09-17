use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use tracedecay_store::{CommitSequenceV1, OperationPriorityV1, StoreClientIdV1};

use super::{
    MAX_TRACKED_WRITER_CLIENTS, WalCheckpointSample, WriterBatchMetrics,
    WriterClientServiceSnapshot, WriterCommitSnapshot, WriterServiceCounts,
    WriterTelemetrySnapshot, WriterTransactionMetrics, WriterTransactionOutcome,
};

#[derive(Default)]
struct State {
    snapshot: WriterTelemetrySnapshot,
    clients: BTreeMap<StoreClientIdV1, WriterServiceCounts>,
}

/// Cloneable handle to the one synchronized telemetry record. Submit and the
/// worker mutate this same state; snapshots never need atomic patch-ups.
#[derive(Clone, Default)]
pub(crate) struct WriterTelemetry(Arc<Mutex<State>>);

impl WriterTelemetry {
    fn update(&self, mutate: impl FnOnce(&mut State)) {
        mutate(
            &mut self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }

    pub(crate) fn snapshot(&self) -> WriterTelemetrySnapshot {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut snapshot = state.snapshot.clone();
        snapshot.client_services = state
            .clients
            .iter()
            .map(|(client_id, services)| WriterClientServiceSnapshot {
                client_id: client_id.clone(),
                services: *services,
            })
            .collect();
        snapshot
    }

    pub(crate) fn offered(&self) {
        self.update(|state| {
            state.snapshot.operations.offered_operations = state
                .snapshot
                .operations
                .offered_operations
                .saturating_add(1);
        });
    }

    pub(crate) fn admitted(&self, bytes: u64) {
        self.update(|state| {
            let snapshot = &mut state.snapshot;
            snapshot.operations.admitted_operations =
                snapshot.operations.admitted_operations.saturating_add(1);
            snapshot.queue.queued_operations = snapshot.queue.queued_operations.saturating_add(1);
            snapshot.queue.queued_bytes = snapshot.queue.queued_bytes.saturating_add(bytes);
        });
        crate::hotpath_observe::record_writer_queue_admitted(bytes);
    }

    pub(crate) fn shed(&self) {
        self.update(|state| {
            state.snapshot.operations.shed_operations =
                state.snapshot.operations.shed_operations.saturating_add(1);
        });
    }

    pub(crate) fn released(&self, operations: u32, bytes: u64) {
        self.update(|state| {
            state.snapshot.queue.queued_operations = state
                .snapshot
                .queue
                .queued_operations
                .saturating_sub(operations);
            state.snapshot.queue.queued_bytes =
                state.snapshot.queue.queued_bytes.saturating_sub(bytes);
        });
        crate::hotpath_observe::record_writer_queue_released(operations, bytes);
    }

    pub(crate) fn completed(
        &self,
        result: &Result<
            tracedecay_store::RuntimeSubmitOutcomeV1,
            tracedecay_store::StorageRuntimeErrorV1,
        >,
    ) {
        use tracedecay_store::RuntimeSubmitOutcomeV1;
        self.update(|state| {
            let operations = &mut state.snapshot.operations;
            operations.completed_operations = operations.completed_operations.saturating_add(1);
            match result {
                Ok(RuntimeSubmitOutcomeV1::ExactReplay { .. }) => {
                    operations.retried_operations = operations.retried_operations.saturating_add(1)
                }
                Ok(RuntimeSubmitOutcomeV1::IdempotencyConflict { .. }) => {
                    operations.conflicted_operations =
                        operations.conflicted_operations.saturating_add(1)
                }
                Ok(RuntimeSubmitOutcomeV1::CancelledBeforeCommit { .. }) => {
                    operations.cancelled_operations =
                        operations.cancelled_operations.saturating_add(1)
                }
                Ok(RuntimeSubmitOutcomeV1::DeadlineExceededBeforeCommit { .. }) => {
                    operations.deadline_exceeded_operations =
                        operations.deadline_exceeded_operations.saturating_add(1)
                }
                Ok(RuntimeSubmitOutcomeV1::Saturated { .. }) => {
                    operations.shed_operations = operations.shed_operations.saturating_add(1)
                }
                Err(_) => {
                    state.snapshot.error_events = state.snapshot.error_events.saturating_add(1)
                }
                _ => {}
            }
        });
    }

    pub(crate) fn busy(&self) {
        self.update(|state| {
            state.snapshot.busy_events = state.snapshot.busy_events.saturating_add(1)
        });
    }

    pub(crate) fn error(&self) {
        self.update(|state| {
            state.snapshot.error_events = state.snapshot.error_events.saturating_add(1)
        });
    }

    pub(crate) fn committed(
        &self,
        observed_sequence: CommitSequenceV1,
        batch: WriterBatchMetrics,
        clients: impl IntoIterator<Item = (StoreClientIdV1, OperationPriorityV1)>,
    ) {
        self.update(|state| {
            // Telemetry only observes the sequence assigned by commit authority.
            // It neither increments nor publishes writer commit truth.
            if observed_sequence < state.snapshot.commit_sequence
                || state
                    .snapshot
                    .latest_commit
                    .is_some_and(|latest| latest.commit_sequence == observed_sequence)
            {
                return;
            }
            let operations = u64::from(batch.batch_operations);
            let totals = &mut state.snapshot.batches;
            totals.committed_batches = totals.committed_batches.saturating_add(1);
            totals.batch_operations = totals.batch_operations.saturating_add(operations);
            totals.batch_bytes = totals.batch_bytes.saturating_add(batch.batch_bytes);
            totals.queue_wait_micros = totals
                .queue_wait_micros
                .saturating_add(batch.queue_wait_micros);
            totals.transaction_micros = totals
                .transaction_micros
                .saturating_add(batch.transaction_micros);
            totals.lock_held_micros = totals
                .lock_held_micros
                .saturating_add(batch.lock_held_micros);
            totals.total_latency_micros = totals
                .total_latency_micros
                .saturating_add(batch.queue_wait_micros)
                .saturating_add(batch.transaction_micros);
            state
                .snapshot
                .priority_services
                .record(batch.priority, operations);
            if batch.priority == OperationPriorityV1::Health {
                state.snapshot.health_lane_services = state
                    .snapshot
                    .health_lane_services
                    .saturating_add(operations);
            }
            state.snapshot.commit_sequence = observed_sequence;
            state.snapshot.latest_commit = Some(WriterCommitSnapshot {
                commit_sequence: observed_sequence,
                batch,
            });
            for (client, priority) in clients {
                record_client(state, client, priority);
            }
        });
    }

    pub(crate) fn transaction_closed(&self, metrics: WriterTransactionMetrics) {
        self.update(|state| {
            let transactions = &mut state.snapshot.transactions;
            match metrics.outcome {
                WriterTransactionOutcome::Committed => {
                    transactions.committed_transactions =
                        transactions.committed_transactions.saturating_add(1);
                }
                WriterTransactionOutcome::RolledBack => {
                    transactions.rolled_back_transactions =
                        transactions.rolled_back_transactions.saturating_add(1);
                }
                WriterTransactionOutcome::Busy | WriterTransactionOutcome::Error => {
                    transactions.rolled_back_transactions =
                        transactions.rolled_back_transactions.saturating_add(1);
                }
            }
            transactions.commands = transactions.commands.saturating_add(metrics.commands);
            transactions.rows = transactions.rows.saturating_add(metrics.rows);
            transactions.lock_held_micros = transactions
                .lock_held_micros
                .saturating_add(metrics.lock_held_micros);
            transactions.transaction_micros = transactions
                .transaction_micros
                .saturating_add(metrics.transaction_micros);
            state.snapshot.sqlite_vm = state.snapshot.sqlite_vm.saturating_add(metrics.sqlite_vm);
            state.snapshot.lock_work.bytes_encoded = state
                .snapshot
                .lock_work
                .bytes_encoded
                .saturating_add(metrics.lock_work.bytes_encoded);
            state.snapshot.lock_work.bytes_decoded = state
                .snapshot
                .lock_work
                .bytes_decoded
                .saturating_add(metrics.lock_work.bytes_decoded);
        });
        crate::hotpath_observe::record_writer_transaction(metrics.rows, metrics.lock_held_micros);
    }

    pub(crate) fn checkpoint(&self, sample: WalCheckpointSample) {
        self.update(|state| {
            let wal = &mut state.snapshot.wal;
            wal.wal_frames = sample.wal_frames;
            wal.wal_bytes = sample.wal_bytes;
            wal.checkpointed_frames = wal
                .checkpointed_frames
                .saturating_add(sample.checkpointed_frames);
            if sample.completed {
                wal.reclaimed_frames = wal.reclaimed_frames.saturating_add(sample.reclaimed_frames);
            }
            if sample.busy {
                wal.busy_events = wal.busy_events.saturating_add(1);
            }
            wal.blocker_count = wal.blocker_count.saturating_add(sample.blocker_count);
            if sample.hard_pressure {
                wal.hard_pressure_events = wal.hard_pressure_events.saturating_add(1);
            }
        });
    }

    pub(crate) fn checkpoint_hard_retry(&self) {
        self.update(|state| {
            state.snapshot.wal.hard_retry_wakes =
                state.snapshot.wal.hard_retry_wakes.saturating_add(1);
        });
        crate::hotpath_observe::record_checkpoint_hard_retry_wake();
    }

    pub(crate) fn exact_sql_command(
        &self,
        commands: u64,
        rows: u64,
        elapsed_micros: u64,
        sqlite_vm: super::SqliteVmSnapshot,
        lock_work: super::WriterLockWorkSnapshot,
    ) {
        self.update(|state| {
            let transactions = &mut state.snapshot.transactions;
            transactions.commands = transactions.commands.saturating_add(commands);
            transactions.rows = transactions.rows.saturating_add(rows);
            transactions.lock_held_micros =
                transactions.lock_held_micros.saturating_add(elapsed_micros);
            transactions.transaction_micros = transactions
                .transaction_micros
                .saturating_add(elapsed_micros);
            state.snapshot.sqlite_vm = state.snapshot.sqlite_vm.saturating_add(sqlite_vm);
            state.snapshot.lock_work.bytes_encoded = state
                .snapshot
                .lock_work
                .bytes_encoded
                .saturating_add(lock_work.bytes_encoded);
            state.snapshot.lock_work.bytes_decoded = state
                .snapshot
                .lock_work
                .bytes_decoded
                .saturating_add(lock_work.bytes_decoded);
        });
        crate::hotpath_observe::record_writer_transaction(rows, elapsed_micros);
    }

    pub(crate) fn fault_unsettled(&self) {
        let mut released = super::WriterQueueSnapshot::default();
        self.update(|state| {
            let unsettled = state
                .snapshot
                .operations
                .admitted_operations
                .saturating_sub(state.snapshot.operations.completed_operations);
            state.snapshot.operations.completed_operations =
                state.snapshot.operations.admitted_operations;
            released = state.snapshot.queue;
            state.snapshot.queue = Default::default();
            state.snapshot.error_events =
                state.snapshot.error_events.saturating_add(unsettled.max(1));
        });
        crate::hotpath_observe::record_writer_queue_released(
            released.queued_operations,
            released.queued_bytes,
        );
    }
}

fn record_client(state: &mut State, client: StoreClientIdV1, priority: OperationPriorityV1) {
    if let Some(services) = state.clients.get_mut(&client) {
        services.record(priority, 1);
        return;
    }
    if state.clients.len() < MAX_TRACKED_WRITER_CLIENTS {
        let mut services = WriterServiceCounts::default();
        services.record(priority, 1);
        state.clients.insert(client, services);
        return;
    }
    let retain = state
        .clients
        .last_key_value()
        .is_some_and(|(largest, _)| &client < largest);
    if retain {
        if let Some((_, displaced)) = state.clients.pop_last() {
            state.snapshot.omitted_client_service_operations = state
                .snapshot
                .omitted_client_service_operations
                .saturating_add(displaced.total());
        }
        let mut services = WriterServiceCounts::default();
        services.record(priority, 1);
        state.clients.insert(client, services);
    } else {
        state.snapshot.omitted_client_service_operations = state
            .snapshot
            .omitted_client_service_operations
            .saturating_add(1);
    }
}
