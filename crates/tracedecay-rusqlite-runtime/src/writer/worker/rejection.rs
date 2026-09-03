//! Settling queued work the worker will never run.
//!
//! Every path here drains a queue and settles each item, so nothing is dropped
//! without its caller being told why.

use std::collections::VecDeque;

use tracedecay_store::{RuntimeCancellationStageV1, RuntimeSubmitOutcomeV1, UnavailableReasonV1};

use crate::{
    RuntimeWriteAuthorityStage,
    admission::{FairQueue, QueueItem},
    exact_sql::{WriterCommand as ExactSqlWriterCommand, reject_writer_command},
    telemetry::WriterTelemetry,
};

use super::super::{
    WriterActorError, WriterOnlineBackupError,
    backup::OnlineBackupCommand,
    request::{AcceptedRequest, IncrementalVacuumCommand},
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

pub(super) fn reject_all(queue: &mut FairQueue<AcceptedRequest>, telemetry: &WriterTelemetry) {
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

pub(super) fn reject_all_exact_sql(queue: &mut VecDeque<ExactSqlWriterCommand>) {
    for command in queue.drain(..) {
        reject_writer_command(command);
    }
}

pub(super) fn reject_online_backup(command: OnlineBackupCommand) {
    command.settle(Err(WriterActorError::OnlineBackupFailed(
        WriterOnlineBackupError::WriterShuttingDown,
    )));
}

pub(super) fn reject_all_online_backup(queue: &mut VecDeque<OnlineBackupCommand>) {
    for command in queue.drain(..) {
        reject_online_backup(command);
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
