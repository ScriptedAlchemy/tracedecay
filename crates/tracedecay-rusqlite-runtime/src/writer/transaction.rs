use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use rusqlite::{Connection, Savepoint, Transaction, TransactionBehavior};
use tracedecay_store::{
    RuntimeCancellationStageV1, RuntimeSubmitOutcomeV1, StorageRuntimeErrorV1,
    StoreCommitReceiptV1, StoreRuntimeBindingV1, UnavailableReasonV1,
};

use crate::{
    RuntimeWriteAuthorityStage,
    admission::QueueItem,
    connection,
    read_consistency::{CommitWatermarkPublicationError, CommittedWatermarkPublisher},
    telemetry::{
        LockWorkScope, WriterBatchMetrics, WriterLockWorkSnapshot, WriterTelemetry,
        WriterTransactionMetrics, WriterTransactionOutcome, take_observed_vm,
    },
};

use super::{
    WriterPersistence, WriterState,
    request::{AcceptedRequest, ExecutionBatch, RequestResult},
    settlement::{
        DriverFailure, committed_outcome, driver_failure, idempotency_outcome, infrastructure,
        interruption_outcome, invalid_response, is_corrupt, micros,
    },
};

enum PreparedResult {
    Final(RequestResult),
    /// The request savepoint was released, but the outer transaction is not yet
    /// durable. This is deliberately not named or reported as committed.
    AwaitingTransactionCommit(StoreCommitReceiptV1),
}

struct PreparedRequest {
    item: AcceptedRequest,
    result: PreparedResult,
}

struct Processed {
    prepared: PreparedRequest,
    fatal: Option<StorageRuntimeErrorV1>,
}

/// When a batch left the queue, and how long it had waited to get there.
///
/// Both are fixed at the dequeue boundary and are only ever reported together,
/// so they travel as one value rather than as two positional arguments that
/// could be transposed.
#[derive(Clone, Copy)]
pub(super) struct BatchTiming {
    pub(super) dequeued_at: Instant,
    pub(super) queue_wait_micros: u64,
}

/// The writer actor's shared reporting handles.
///
/// None of these varies per batch — they are the actor's, not the batch's —
/// and every batch reports through all three, so passing them as one borrow
/// keeps the per-batch arguments to the things that actually differ per batch.
#[derive(Clone, Copy)]
pub(super) struct WriterReporting<'reporting> {
    pub(super) telemetry: &'reporting WriterTelemetry,
    pub(super) state: &'reporting AtomicU8,
    pub(super) watermark_publisher: &'reporting CommittedWatermarkPublisher,
}

#[hotpath::measure]
pub(super) fn process_batch(
    connection: &mut Connection,
    binding: &StoreRuntimeBindingV1,
    batch: ExecutionBatch,
    timing: BatchTiming,
    persistence: &mut dyn WriterPersistence,
    reporting: WriterReporting<'_>,
) {
    let BatchTiming {
        dequeued_at: started,
        queue_wait_micros,
    } = timing;
    let WriterReporting {
        telemetry,
        state,
        watermark_publisher,
    } = reporting;
    let command_count = u64::try_from(batch.items.len()).unwrap_or(u64::MAX);
    let rows_before = connection.total_changes();
    let lock_work = LockWorkScope::enter();
    let mut transaction = match hotpath::measure_block!("rusqlite.writer.begin", {
        connection.transaction_with_behavior(TransactionBehavior::Immediate)
    }) {
        Ok(transaction) => transaction,
        Err(error) => {
            let failure = driver_failure(error, "begin writer transaction");
            record_transaction(
                telemetry,
                transaction_outcome(&failure),
                command_count,
                0,
                started,
                Duration::ZERO,
                lock_work.take(),
            );
            drop(lock_work);
            settle_batch_failure(batch.items, failure, telemetry);
            return;
        }
    };
    let lock_held_from = Instant::now();
    let mut prepared = Vec::new();
    let mut items = batch.items.into_iter();
    let mut fatal = None;
    for item in items.by_ref() {
        let probe = Arc::clone(&item.probe);
        let processed = connection::with_transaction_progress_cancellation(
            &mut transaction,
            move || probe.interruption().is_some(),
            |transaction| process_request(transaction, binding, item, persistence),
        )
        .expect("install request-local SQLite progress handler");
        fatal = processed.fatal;
        prepared.push(processed.prepared);
        if fatal.is_some() {
            break;
        }
    }

    if let Some(error) = fatal {
        prepared.extend(items.map(|item| PreparedRequest {
            item,
            result: PreparedResult::Final(Ok(RuntimeSubmitOutcomeV1::Unavailable {
                reason: UnavailableReasonV1::Faulted,
            })),
        }));
        drop(transaction);
        let lock_held = lock_held_from.elapsed();
        record_transaction(
            telemetry,
            WriterTransactionOutcome::Error,
            command_count,
            connection.total_changes().saturating_sub(rows_before),
            started,
            lock_held,
            lock_work.take(),
        );
        drop(lock_work);
        state.store(WriterState::Faulted as u8, Ordering::Release);
        telemetry.error();
        settle_prepared(
            prepared,
            Some(DriverFailure::Error(error)),
            started,
            queue_wait_micros,
            lock_held,
            telemetry,
        );
        return;
    }

    let authority_denied = prepared
        .iter()
        .map(|prepared| {
            prepared
                .item
                .authority
                .verify(RuntimeWriteAuthorityStage::BeforeCommit)
                .is_err()
        })
        .collect::<Vec<_>>();
    if authority_denied.iter().any(|denied| *denied) {
        drop(transaction);
        record_transaction(
            telemetry,
            WriterTransactionOutcome::RolledBack,
            command_count,
            connection.total_changes().saturating_sub(rows_before),
            started,
            lock_held_from.elapsed(),
            lock_work.take(),
        );
        drop(lock_work);
        settle_authority_denied(prepared, authority_denied, telemetry);
        return;
    }

    let commit_denied = prepared
        .iter()
        .map(|prepared| {
            matches!(
                &prepared.result,
                PreparedResult::AwaitingTransactionCommit(_)
            ) && !prepared.item.probe.try_begin_commit()
        })
        .collect::<Vec<_>>();
    if commit_denied.iter().any(|denied| *denied) {
        drop(transaction);
        record_transaction(
            telemetry,
            WriterTransactionOutcome::RolledBack,
            command_count,
            connection.total_changes().saturating_sub(rows_before),
            started,
            lock_held_from.elapsed(),
            lock_work.take(),
        );
        drop(lock_work);
        settle_commit_denied(prepared, commit_denied, telemetry);
        return;
    }

    let commit_failure = match hotpath::measure_block!("rusqlite.commit", transaction.commit()) {
        Err(error) => Some(driver_failure(error, "commit writer transaction")),
        Ok(()) => match publish_committed(&prepared, watermark_publisher) {
            Ok(()) => None,
            Err(_) => {
                state.store(WriterState::Faulted as u8, Ordering::Release);
                Some(DriverFailure::Error(infrastructure(
                    "publish committed writer watermark",
                )))
            }
        },
    };
    let lock_held = lock_held_from.elapsed();
    record_transaction(
        telemetry,
        commit_failure
            .as_ref()
            .map(transaction_outcome)
            .unwrap_or(WriterTransactionOutcome::Committed),
        command_count,
        connection.total_changes().saturating_sub(rows_before),
        started,
        lock_held,
        lock_work.take(),
    );
    drop(lock_work);
    settle_prepared(
        prepared,
        commit_failure,
        started,
        queue_wait_micros,
        lock_held,
        telemetry,
    );
}

fn publish_committed(
    prepared: &[PreparedRequest],
    publisher: &CommittedWatermarkPublisher,
) -> Result<(), CommitWatermarkPublicationError> {
    publish_results(prepared.iter().map(|prepared| &prepared.result), publisher)
}

fn publish_results<'a>(
    results: impl IntoIterator<Item = &'a PreparedResult>,
    publisher: &CommittedWatermarkPublisher,
) -> Result<(), CommitWatermarkPublicationError> {
    for result in results {
        if let PreparedResult::AwaitingTransactionCommit(receipt) = result {
            publisher.publish_committed(receipt)?;
        }
    }
    Ok(())
}

fn process_request(
    transaction: &mut Transaction<'_>,
    binding: &StoreRuntimeBindingV1,
    item: AcceptedRequest,
    persistence: &mut dyn WriterPersistence,
) -> Processed {
    if item
        .authority
        .verify(RuntimeWriteAuthorityStage::Dequeued)
        .is_err()
    {
        return processed(item, Ok(super::settlement::missing_authority()), false);
    }
    if let Some(outcome) = interruption_outcome(
        &item.request,
        item.probe.as_ref(),
        RuntimeCancellationStageV1::BeforeCommit,
    ) {
        return processed(item, Ok(outcome), false);
    }
    match persistence.lookup_idempotency(
        transaction,
        binding,
        &item.request.envelope().metadata.idempotency,
    ) {
        Ok(Some(receipt)) => {
            let result = idempotency_outcome(&item.request, receipt);
            return processed(item, result, false);
        }
        Ok(None) => {}
        Err(error) => {
            if let Some(outcome) = interruption_outcome(
                &item.request,
                item.probe.as_ref(),
                RuntimeCancellationStageV1::BeforeCommit,
            ) {
                return processed(item, Ok(outcome), false);
            }
            return processed(item, Err(error.clone()), is_corrupt(&error));
        }
    }
    apply_new(transaction, binding, item, persistence)
}

fn apply_new(
    transaction: &mut Transaction<'_>,
    binding: &StoreRuntimeBindingV1,
    item: AcceptedRequest,
    persistence: &mut dyn WriterPersistence,
) -> Processed {
    let mut savepoint = match transaction.savepoint() {
        Ok(savepoint) => savepoint,
        Err(error) => {
            let result = driver_failure(error, "open request savepoint").result(&item.request);
            return processed(item, result, false);
        }
    };
    let receipt = match apply_and_record(persistence, &mut savepoint, binding, &item.request) {
        Ok(receipt) => receipt,
        Err(error) => {
            if let Some(outcome) = interruption_outcome(
                &item.request,
                item.probe.as_ref(),
                RuntimeCancellationStageV1::BeforeCommit,
            ) {
                return match savepoint.rollback() {
                    Ok(()) => processed(item, Ok(outcome), false),
                    Err(error) => {
                        let result = driver_failure(error, "rollback interrupted request")
                            .result(&item.request);
                        processed(item, result, false)
                    }
                };
            }
            let corrupt = is_corrupt(&error);
            let error = rollback_or(savepoint, error, "rollback receipt/checkpoint/outbox");
            return processed(item, Err(error.clone()), corrupt || is_corrupt(&error));
        }
    };
    if let Err(error) = receipt.validate_for(&item.request.envelope().metadata) {
        let error = rollback_or(
            savepoint,
            invalid_response(error),
            "rollback invalid receipt",
        );
        return processed(item, Err(error), false);
    }
    if let Some(outcome) = interruption_outcome(
        &item.request,
        item.probe.as_ref(),
        RuntimeCancellationStageV1::BeforeCommit,
    ) {
        return match savepoint.rollback() {
            Ok(()) => processed(item, Ok(outcome), false),
            Err(error) => {
                let result =
                    driver_failure(error, "rollback cancelled receipt").result(&item.request);
                processed(item, result, false)
            }
        };
    }
    if item
        .authority
        .verify(RuntimeWriteAuthorityStage::BeforeCommit)
        .is_err()
    {
        return match savepoint.rollback() {
            Ok(()) => processed(item, Ok(super::settlement::missing_authority()), false),
            Err(error) => {
                let result =
                    driver_failure(error, "rollback unauthorized receipt").result(&item.request);
                processed(item, result, false)
            }
        };
    }
    match savepoint.commit() {
        Ok(()) => Processed {
            prepared: PreparedRequest {
                item,
                result: PreparedResult::AwaitingTransactionCommit(receipt),
            },
            fatal: None,
        },
        Err(error) => {
            let result = driver_failure(error, "release request savepoint").result(&item.request);
            processed(item, result, false)
        }
    }
}

/// The transaction path has one operation+ledger boundary. The persistence
/// implementation must return the receipt produced by this same savepoint.
fn apply_and_record(
    persistence: &mut dyn WriterPersistence,
    savepoint: &mut Savepoint<'_>,
    binding: &StoreRuntimeBindingV1,
    request: &tracedecay_store::RuntimeSubmitRequestV1,
) -> Result<StoreCommitReceiptV1, StorageRuntimeErrorV1> {
    persistence.apply_and_record(savepoint, binding, request)
}

fn processed(item: AcceptedRequest, result: RequestResult, fatal: bool) -> Processed {
    let fatal_error = fatal.then(|| {
        result
            .as_ref()
            .expect_err("fatal result is an error")
            .clone()
    });
    Processed {
        prepared: PreparedRequest {
            item,
            result: PreparedResult::Final(result),
        },
        fatal: fatal_error,
    }
}

fn rollback_or(
    mut savepoint: Savepoint<'_>,
    fallback: StorageRuntimeErrorV1,
    operation: &'static str,
) -> StorageRuntimeErrorV1 {
    savepoint
        .rollback()
        .err()
        .map(|error| driver_failure(error, operation).storage_error())
        .unwrap_or(fallback)
}

fn settle_batch_failure(
    items: Vec<AcceptedRequest>,
    failure: DriverFailure,
    telemetry: &WriterTelemetry,
) {
    match failure {
        DriverFailure::Busy => telemetry.busy(),
        DriverFailure::Error(_) => telemetry.error(),
    }
    for item in items {
        let result = failure.result(&item.request);
        telemetry.completed(&result);
        item.settle(result);
    }
}

fn settle_prepared(
    prepared: Vec<PreparedRequest>,
    commit_failure: Option<DriverFailure>,
    started: Instant,
    queue_wait_micros: u64,
    lock_held: Duration,
    telemetry: &WriterTelemetry,
) {
    if commit_failure.is_none() {
        record_commit(&prepared, started, queue_wait_micros, lock_held, telemetry);
    } else if matches!(commit_failure, Some(DriverFailure::Busy)) {
        telemetry.busy();
    } else {
        telemetry.error();
    }
    for prepared in prepared {
        let result = match prepared.result {
            PreparedResult::Final(result) => result,
            PreparedResult::AwaitingTransactionCommit(receipt) => match &commit_failure {
                Some(failure) => failure.result(&prepared.item.request),
                None => committed_outcome(&prepared.item, receipt),
            },
        };
        telemetry.completed(&result);
        prepared.item.settle(result);
    }
}

/// Settles a batch discarded because at least one member lost write authority
/// before the commit.
///
/// Only the members that actually failed the recheck are told their authority
/// is missing. Their peers were fully authorized and merely had their work
/// rolled back with the shared transaction, so reporting `MissingAuthority` to
/// them blames them for an unrelated request's revocation and reads as a
/// non-retryable outcome. They get `Faulted` instead — the same "rolled back,
/// safe to resubmit" shape the fatal path above uses — and a member that had
/// already reached a `Final` outcome keeps it.
fn settle_authority_denied(
    prepared: Vec<PreparedRequest>,
    authority_denied: Vec<bool>,
    telemetry: &WriterTelemetry,
) {
    debug_assert_eq!(prepared.len(), authority_denied.len());
    for (prepared, authority_denied) in prepared.into_iter().zip(authority_denied) {
        let PreparedRequest { item, result } = prepared;
        let settled = if authority_denied {
            Ok(super::settlement::missing_authority())
        } else {
            match result {
                PreparedResult::Final(result) => result,
                PreparedResult::AwaitingTransactionCommit(_) => {
                    Ok(RuntimeSubmitOutcomeV1::Unavailable {
                        reason: UnavailableReasonV1::Faulted,
                    })
                }
            }
        };
        telemetry.completed(&settled);
        item.settle(settled);
    }
}

fn settle_commit_denied(
    prepared: Vec<PreparedRequest>,
    commit_denied: Vec<bool>,
    telemetry: &WriterTelemetry,
) {
    debug_assert_eq!(prepared.len(), commit_denied.len());
    for (prepared, commit_denied) in prepared.into_iter().zip(commit_denied) {
        let PreparedRequest { item, result } = prepared;
        let settled = if commit_denied {
            interruption_outcome(
                &item.request,
                item.probe.as_ref(),
                RuntimeCancellationStageV1::BeforeCommit,
            )
            .map(Ok)
            .unwrap_or_else(|| {
                Ok(RuntimeSubmitOutcomeV1::Unavailable {
                    reason: UnavailableReasonV1::Faulted,
                })
            })
        } else {
            match result {
                PreparedResult::Final(result) => result,
                PreparedResult::AwaitingTransactionCommit(_) => {
                    Ok(RuntimeSubmitOutcomeV1::Unavailable {
                        reason: UnavailableReasonV1::Faulted,
                    })
                }
            }
        };
        telemetry.completed(&settled);
        item.settle(settled);
    }
}

fn record_commit(
    prepared: &[PreparedRequest],
    started: Instant,
    queue_wait_micros: u64,
    lock_held: Duration,
    telemetry: &WriterTelemetry,
) {
    let durable = prepared
        .iter()
        .filter_map(|prepared| match &prepared.result {
            PreparedResult::AwaitingTransactionCommit(receipt) => Some((prepared, receipt)),
            PreparedResult::Final(_) => None,
        })
        .collect::<Vec<_>>();
    let Some((first, _)) = durable.first() else {
        return;
    };
    let sequence = durable
        .last()
        .expect("non-empty durable requests")
        .1
        .commit_sequence;
    let bytes = durable.iter().fold(0_u64, |total, (prepared, _)| {
        total.saturating_add(prepared.item.admission_bytes())
    });
    telemetry.committed(
        sequence,
        WriterBatchMetrics {
            priority: first.item.priority(),
            durability: first.item.request.envelope().metadata.durability,
            batch_operations: u32::try_from(durable.len()).unwrap_or(u32::MAX),
            batch_bytes: bytes,
            queue_wait_micros,
            transaction_micros: micros(started.elapsed()),
            lock_held_micros: micros(lock_held),
        },
        durable
            .iter()
            .map(|(prepared, _)| (prepared.item.client_id().clone(), prepared.item.priority())),
    );
}

fn record_transaction(
    telemetry: &WriterTelemetry,
    outcome: WriterTransactionOutcome,
    commands: u64,
    rows: u64,
    started: Instant,
    lock_held: Duration,
    lock_work: WriterLockWorkSnapshot,
) {
    telemetry.transaction_closed(WriterTransactionMetrics {
        outcome,
        commands,
        rows,
        lock_held_micros: micros(lock_held),
        transaction_micros: micros(started.elapsed()),
        sqlite_vm: take_observed_vm(),
        lock_work,
    });
}

fn transaction_outcome(failure: &DriverFailure) -> WriterTransactionOutcome {
    match failure {
        DriverFailure::Busy => WriterTransactionOutcome::Busy,
        DriverFailure::Error(_) => WriterTransactionOutcome::Error,
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_store::{CommitSequenceV1, StoreCommitReceiptV1};

    use super::*;
    use crate::{
        read_consistency::{CommitWatermarkSource, WatermarkSourceState},
        test_support::{binding, metadata},
    };

    fn receipt(sequence: u64) -> (StoreRuntimeBindingV1, StoreCommitReceiptV1) {
        let metadata = metadata("operation.publish", "key.publish", 'a');
        let binding = binding(&metadata);
        let receipt = StoreCommitReceiptV1 {
            operation_id: metadata.operation_id,
            idempotency: metadata.idempotency,
            shard_id: metadata.shard_id,
            incarnation: metadata.incarnation,
            authority_epoch: metadata.authority_epoch,
            commit_sequence: CommitSequenceV1(sequence),
            committed_at: metadata.admitted_at,
        };
        (binding, receipt)
    }

    #[test]
    fn committed_result_publishes_exact_receipt_watermark() {
        let (binding, receipt) = receipt(4);
        let publisher = CommittedWatermarkPublisher::new(binding.clone());

        publish_results(
            [&PreparedResult::AwaitingTransactionCommit(receipt.clone())],
            &publisher,
        )
        .unwrap();

        let WatermarkSourceState::Available(observed) =
            publisher.subscribe().current(&binding.shard_id)
        else {
            panic!("committed watermark must be available");
        };
        assert_eq!(observed.commit_sequence, receipt.commit_sequence);
        assert_eq!(observed.shard_id, receipt.shard_id);
        assert_eq!(observed.incarnation, receipt.incarnation);
        assert_eq!(observed.authority_epoch, receipt.authority_epoch);
    }

    #[test]
    fn rolled_back_result_does_not_publish() {
        let (binding, _) = receipt(1);
        let publisher = CommittedWatermarkPublisher::new(binding.clone());
        let result = PreparedResult::Final(Err(infrastructure("rolled back")));

        publish_results([&result], &publisher).unwrap();

        let WatermarkSourceState::Available(observed) =
            publisher.subscribe().current(&binding.shard_id)
        else {
            panic!("initial watermark must be available");
        };
        assert_eq!(observed.commit_sequence, CommitSequenceV1(0));
    }
}
