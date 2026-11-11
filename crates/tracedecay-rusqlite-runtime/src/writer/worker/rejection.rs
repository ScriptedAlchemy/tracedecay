//! Settling queued work the worker will never run.
//!
//! Every path here drains a queue and settles each item, so nothing is dropped
//! without its caller being told why.

use std::collections::VecDeque;

use tokio::sync::mpsc;

use tracedecay_store::{RuntimeCancellationStageV1, RuntimeSubmitOutcomeV1, UnavailableReasonV1};

use crate::{
    RuntimeWriteAuthorityStage,
    admission::{FairQueue, QueueItem},
    exact_sql::{WriterCommand as ExactSqlWriterCommand, reject_writer_command},
    telemetry::WriterTelemetry,
};

use super::super::{
    WriterActorError,
    request::{AcceptedRequest, ExecutionBatch, IncrementalVacuumCommand},
    settlement::{interruption_outcome, missing_authority},
};

pub(super) fn cancel_waiting(queue: &mut FairQueue<AcceptedRequest>, telemetry: &WriterTelemetry) {
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

pub(super) fn reject_unauthorized(
    queue: &mut FairQueue<AcceptedRequest>,
    telemetry: &WriterTelemetry,
) {
    for item in queue.drain_matching(|item| {
        item.authority
            .verify(RuntimeWriteAuthorityStage::Dequeued)
            .is_err()
    }) {
        let bytes = item.admission_bytes();
        let result = Ok(missing_authority());
        telemetry.released(1, bytes);
        telemetry.completed(&result);
        item.settle(result);
    }
}

/// A retired writer must settle every accepted request, including those not yet
/// transferred from ingress to its fair queue. Closing first prevents a racing
/// sender from losing its reply and leaving queue telemetry inflated.
pub(super) fn reject_retired_write_ingress(
    receiver: &mut mpsc::Receiver<AcceptedRequest>,
    queue: &mut FairQueue<AcceptedRequest>,
    telemetry: &WriterTelemetry,
    reason: UnavailableReasonV1,
) {
    receiver.close();
    reject_all(queue, telemetry, reason);
    while let Ok(item) = receiver.try_recv() {
        settle_unavailable(item, telemetry, reason);
    }
}

pub(super) fn reject_all(
    queue: &mut FairQueue<AcceptedRequest>,
    telemetry: &WriterTelemetry,
    reason: UnavailableReasonV1,
) {
    for item in queue.drain_all() {
        settle_unavailable(item, telemetry, reason);
    }
}

/// Selected batches are no longer in the fair queue. If an earlier batch
/// faults the writer, settle every remaining admission before retiring it.
pub(super) fn reject_faulted_execution_batches(
    batches: impl IntoIterator<Item = ExecutionBatch>,
    telemetry: &WriterTelemetry,
) {
    for batch in batches {
        for item in batch.items {
            settle_unavailable(item, telemetry, UnavailableReasonV1::Faulted);
        }
    }
}

fn settle_unavailable(
    item: AcceptedRequest,
    telemetry: &WriterTelemetry,
    reason: UnavailableReasonV1,
) {
    let bytes = item.admission_bytes();
    let result = Ok(RuntimeSubmitOutcomeV1::Unavailable { reason });
    telemetry.released(1, bytes);
    telemetry.completed(&result);
    item.settle(result);
}

pub(super) fn reject_all_exact_sql(queue: &mut VecDeque<ExactSqlWriterCommand>) {
    for command in queue.drain(..) {
        reject_writer_command(command);
    }
}

pub(super) fn reject_incremental_vacuum(command: IncrementalVacuumCommand) {
    command.settle(Err(WriterActorError::IncrementalVacuumFailed(
        "writer is unavailable".to_owned(),
    )));
}

pub(super) fn reject_all_incremental_vacuum(queue: &mut VecDeque<IncrementalVacuumCommand>) {
    for command in queue.drain(..) {
        reject_incremental_vacuum(command);
    }
}
