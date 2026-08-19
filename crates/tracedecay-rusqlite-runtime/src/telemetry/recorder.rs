use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use tracedecay_store::{CommitSequenceV1, OperationPriorityV1, StoreClientIdV1};

use super::{
    MAX_TRACKED_WRITER_CLIENTS, WriterBatchMetrics, WriterClientServiceSnapshot,
    WriterCommitSnapshot, WriterServiceCounts, WriterTelemetrySnapshot,
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

    pub(crate) fn fault_unsettled(&self) {
        self.update(|state| {
            let unsettled = state
                .snapshot
                .operations
                .admitted_operations
                .saturating_sub(state.snapshot.operations.completed_operations);
            state.snapshot.operations.completed_operations =
                state.snapshot.operations.admitted_operations;
            state.snapshot.queue = Default::default();
            state.snapshot.error_events =
                state.snapshot.error_events.saturating_add(unsettled.max(1));
        });
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
