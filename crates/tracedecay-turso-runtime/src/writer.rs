//! Native transaction driver for an existing shard writer actor.
//! Scheduling and admission stay with the runtime. Exclusive mutation of this
//! driver preserves dequeue order; separate drivers on separate files overlap.
use crate::ledger::{self, LedgerDisposition, LedgerError, RequestSql};
use crate::{Access, Connection, Database, ExecutionGuard, TransactionBehavior};
use tracedecay_store::{
    DurabilityClassV1, IdempotencyIdentityV1, RepositoryWritePayloadV1, RuntimeCancellationStageV1,
    RuntimeInterruptionV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1,
    ShardWatermarkV1, StoreRuntimeBindingV1,
};

/// A closed domain executor receives only the request SQL capability; the
/// transaction driver remains the sole owner of commit and savepoint control.
pub trait NativeWriteExecutor {
    fn execute(
        &mut self,
        sql: &RequestSql<'_>,
        payload: &RepositoryWritePayloadV1,
    ) -> Result<(), LedgerError>;
}

pub struct NativeSubmission<'a> {
    pub request: &'a RuntimeSubmitRequestV1,
    pub guard: &'a ExecutionGuard,
    pub probe: &'a dyn RuntimeRequestProbeV1,
}

pub type NativeRequestResult = Result<RuntimeSubmitOutcomeV1, LedgerError>;

pub struct NativeWriter {
    connection: Connection,
    binding: StoreRuntimeBindingV1,
}

impl NativeWriter {
    pub fn new(
        database: &Database,
        binding: StoreRuntimeBindingV1,
        guard: &ExecutionGuard,
    ) -> Result<Self, LedgerError> {
        let mut connection = database.connect(Access::Writer)?;
        connection.set_full_durability(guard)?;
        connection.begin(TransactionBehavior::Immediate, guard)?;
        let install = ledger::initialize_writer_ledger(&RequestSql::new(&mut connection, guard));
        match install {
            Ok(()) => {
                if let Err(error) = connection.commit(guard) {
                    connection.rollback()?;
                    return Err(error.into());
                }
            }
            Err(error) => {
                connection.rollback()?;
                return Err(error);
            }
        }
        Ok(Self {
            connection,
            binding,
        })
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn current_watermark(
        &mut self,
        guard: &ExecutionGuard,
    ) -> Result<Option<ShardWatermarkV1>, LedgerError> {
        ledger::current_watermark(&RequestSql::new(&mut self.connection, guard), &self.binding)
    }

    pub fn lookup_receipt(
        &mut self,
        identity: &IdempotencyIdentityV1,
        guard: &ExecutionGuard,
    ) -> Result<Option<tracedecay_store::StoreCommitReceiptV1>, LedgerError> {
        ledger::lookup_receipt(
            &RequestSql::new(&mut self.connection, guard),
            &self.binding,
            identity,
        )
    }

    /// The actor supplies a guard covering every batch member's live authority
    /// and shutdown state. Request probes arbitrate cancellation at the durable
    /// commit boundary using the existing runtime contract.
    pub fn process_batch<E: NativeWriteExecutor>(
        &mut self,
        submissions: &[NativeSubmission<'_>],
        batch_guard: &ExecutionGuard,
        executor: &mut E,
    ) -> Result<Vec<NativeRequestResult>, LedgerError> {
        if submissions.is_empty() {
            return Ok(Vec::new());
        }
        let compatibility = &submissions[0].request.transaction_scope().compatibility;
        for submission in submissions {
            submission
                .request
                .validate()
                .map_err(LedgerError::InvalidRequest)?;
            if submission.request.binding() != &self.binding
                || &submission.request.transaction_scope().compatibility != compatibility
                || submission.probe.cancellation_identity()
                    != &submission.request.control().cancellation
                || submission.probe.deadline_identity() != &submission.request.control().deadline
                || (submissions.len() > 1 && submission.probe.requires_isolated_commit())
            {
                return Err(crate::Error::InvalidOperation(
                    "native writer request binding/probe/batch mismatch".into(),
                )
                .into());
            }
        }
        if compatibility.durability == DurabilityClassV1::Full {
            self.connection.set_full_durability(batch_guard)?;
        }
        self.connection
            .begin(TransactionBehavior::Immediate, batch_guard)?;
        let result = self.prepare_batch(submissions, batch_guard, executor);
        match result {
            Ok(results) => Ok(results),
            Err(error) => {
                self.connection.rollback()?;
                Err(error)
            }
        }
    }

    fn prepare_batch<E: NativeWriteExecutor>(
        &mut self,
        submissions: &[NativeSubmission<'_>],
        batch_guard: &ExecutionGuard,
        executor: &mut E,
    ) -> Result<Vec<NativeRequestResult>, LedgerError> {
        let mut prepared = Vec::with_capacity(submissions.len());
        for (index, submission) in submissions.iter().enumerate() {
            if let Some(interruption) = interruption(submission) {
                prepared.push(Ok(interruption));
                continue;
            }
            submission.guard.verify()?;
            let disposition = {
                let sql = RequestSql::new(&mut self.connection, submission.guard);
                ledger::disposition(&sql, submission.request)?
            };
            match disposition {
                LedgerDisposition::Replay(receipt) => {
                    prepared.push(Ok(RuntimeSubmitOutcomeV1::ExactReplay { receipt }));
                    continue;
                }
                LedgerDisposition::Conflict(existing_receipt) => {
                    prepared.push(Ok(RuntimeSubmitOutcomeV1::IdempotencyConflict {
                        existing_receipt,
                    }));
                    continue;
                }
                LedgerDisposition::New => {}
                LedgerDisposition::Committed(_) => {
                    return Err(crate::Error::InvalidOperation(
                        "lookup returned new commit".into(),
                    )
                    .into());
                }
            }
            let name = format!("td_request_{index}");
            self.connection.savepoint(&name, submission.guard)?;
            let result = {
                let sql = RequestSql::new(&mut self.connection, submission.guard);
                executor
                    .execute(&sql, &submission.request.envelope().payload)
                    .and_then(|()| {
                        ledger::record_runtime_commit(
                            &sql,
                            &submission.request.envelope().metadata,
                            submission.request.transaction_scope(),
                            &submission.request.envelope().payload,
                        )
                    })
            };
            match result {
                Ok(LedgerDisposition::Committed(receipt)) => {
                    receipt
                        .validate_for(&submission.request.envelope().metadata)
                        .map_err(LedgerError::InvalidRequest)?;
                    if let Some(interruption) = interruption(submission) {
                        self.connection.abort_savepoint(&name)?;
                        prepared.push(Ok(interruption));
                    } else {
                        submission.guard.verify()?;
                        self.connection.release_savepoint(&name, submission.guard)?;
                        prepared.push(Ok(RuntimeSubmitOutcomeV1::Committed { receipt }));
                    }
                }
                Ok(_) => {
                    return Err(crate::Error::InvalidOperation(
                        "idempotency disposition changed after lookup".into(),
                    )
                    .into());
                }
                Err(error) => {
                    let fatal = matches!(&error, LedgerError::Corrupt { .. })
                        || matches!(&error, LedgerError::Native(native) if native.requires_transaction_retry());
                    self.connection.abort_savepoint(&name)?;
                    if fatal {
                        return Err(error);
                    }
                    if let Some(interruption) = interruption(submission) {
                        prepared.push(Ok(interruption));
                    } else {
                        prepared.push(Err(error));
                    }
                }
            }
        }
        // Revalidate every authority, including replayed members, before any
        // probe claims a commit. A denial discards the shared transaction.
        for (submission, outcome) in submissions.iter().zip(&prepared) {
            if let Some(authority) = &submission.guard.authority {
                authority().map_err(crate::Error::Authority)?;
            }
            if matches!(outcome, Ok(RuntimeSubmitOutcomeV1::Committed { .. })) {
                submission.guard.verify()?;
            }
        }
        for (submission, outcome) in submissions.iter().zip(&prepared) {
            if matches!(outcome, Ok(RuntimeSubmitOutcomeV1::Committed { .. }))
                && !submission.probe.try_begin_commit()
            {
                return Err(crate::Error::Cancelled.into());
            }
        }
        self.connection.commit(batch_guard)?;
        for (submission, outcome) in submissions.iter().zip(&mut prepared) {
            if let Some(RuntimeInterruptionV1::Cancelled) = submission.probe.interruption()
                && let Ok(RuntimeSubmitOutcomeV1::Committed { receipt }) = outcome
            {
                *outcome = Ok(RuntimeSubmitOutcomeV1::CommittedAfterCancellation {
                    receipt: receipt.clone(),
                    cancellation: submission.request.control().cancellation.clone(),
                });
            }
        }
        Ok(prepared)
    }
}

fn interruption(submission: &NativeSubmission<'_>) -> Option<RuntimeSubmitOutcomeV1> {
    submission
        .probe
        .interruption()
        .map(|interruption| match interruption {
            RuntimeInterruptionV1::Cancelled => RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
                cancellation: submission.request.control().cancellation.clone(),
                stage: RuntimeCancellationStageV1::BeforeCommit,
            },
            RuntimeInterruptionV1::DeadlineExceeded => {
                RuntimeSubmitOutcomeV1::DeadlineExceededBeforeCommit {
                    deadline: submission.request.control().deadline.clone(),
                }
            }
        })
}
