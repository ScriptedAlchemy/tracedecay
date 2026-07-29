//! Native SQLite transaction adapter for canonical remote replay.

use std::sync::Mutex;

use rusqlite::{Connection, TransactionBehavior};
use tracedecay_application::remote::{
    capture::RemoteWriterAuthorityV1,
    replay::{
        RemoteReplayCommitReceiptV1, RemoteReplayFrameV1, RemoteReplayTransactionErrorV1,
        RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionPortV1,
    },
};
use tracedecay_store::{
    RepositoryWritePayloadV1, RuntimeSubmitRequestV1, StoreCommitReceiptV1, StoreRuntimeBindingV1,
};

use crate::{
    StorageOperationExecutor,
    ledger::{self, LedgerDisposition},
    operation,
};

pub trait RemoteReplayRequestFactoryV1: Send {
    fn build_request(
        &mut self,
        frame: &RemoteReplayFrameV1,
        binding: &StoreRuntimeBindingV1,
    ) -> Result<RuntimeSubmitRequestV1, RemoteReplayTransactionErrorV1>;
}

pub struct RusqliteRemoteReplayPort<E, F> {
    state: Mutex<(Connection, E, F)>,
    binding: StoreRuntimeBindingV1,
}

impl<E, F> RusqliteRemoteReplayPort<E, F> {
    pub fn new(
        connection: Connection,
        executor: E,
        request_factory: F,
        binding: StoreRuntimeBindingV1,
    ) -> Self {
        Self {
            state: Mutex::new((connection, executor, request_factory)),
            binding,
        }
    }
}

impl<E, F> RemoteReplayTransactionPortV1 for RusqliteRemoteReplayPort<E, F>
where
    E: StorageOperationExecutor + Send,
    F: RemoteReplayRequestFactoryV1,
{
    fn commit(
        &self,
        frame: &RemoteReplayFrameV1,
        current_writer: &RemoteWriterAuthorityV1,
    ) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1> {
        frame
            .validate()
            .map_err(|_| RemoteReplayTransactionErrorV1::FenceMismatch)?;
        validate_binding(current_writer, &self.binding)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
        let (connection, executor, request_factory) = &mut *state;
        let request = request_factory.build_request(frame, &self.binding)?;
        validate_request(frame, &self.binding, &request)?;
        match commit_remote_replay_transaction(connection, &request, executor)? {
            RemoteReplayStorageOutcomeV1::Admitted(receipt) => {
                Ok(RemoteReplayTransactionOutcomeV1::Admitted(
                    application_receipt(frame, current_writer, &receipt)?,
                ))
            }
            RemoteReplayStorageOutcomeV1::Duplicate(receipt) => {
                Ok(RemoteReplayTransactionOutcomeV1::Duplicate(
                    application_receipt(frame, current_writer, &receipt)?,
                ))
            }
        }
    }
}

enum RemoteReplayStorageOutcomeV1 {
    Admitted(StoreCommitReceiptV1),
    Duplicate(StoreCommitReceiptV1),
}

fn commit_remote_replay_transaction<E>(
    connection: &mut Connection,
    request: &RuntimeSubmitRequestV1,
    executor: &mut E,
) -> Result<RemoteReplayStorageOutcomeV1, RemoteReplayTransactionErrorV1>
where
    E: StorageOperationExecutor,
{
    let binding = request.binding();
    let mut transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
    ledger::initialize_schema(&transaction)
        .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
    if let Some(receipt) = ledger::lookup_receipt(
        &transaction,
        binding,
        &request.envelope().metadata.idempotency,
    )
    .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?
    {
        transaction
            .commit()
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
        return Ok(RemoteReplayStorageOutcomeV1::Duplicate(receipt));
    }
    let outcome = {
        let mut savepoint = transaction
            .savepoint()
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
        operation::execute(&savepoint, request, executor)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let disposition = ledger::record_runtime_commit(
            &savepoint,
            &request.envelope().metadata,
            request.transaction_scope(),
            &request.envelope().payload,
        )
        .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
        match disposition {
            LedgerDisposition::Committed(receipt) => {
                savepoint
                    .commit()
                    .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
                RemoteReplayStorageOutcomeV1::Admitted(receipt)
            }
            LedgerDisposition::Replay(receipt) => {
                savepoint
                    .rollback()
                    .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
                RemoteReplayStorageOutcomeV1::Duplicate(receipt)
            }
            LedgerDisposition::Conflict(_) => {
                return Err(RemoteReplayTransactionErrorV1::IdempotencyConflict);
            }
            LedgerDisposition::New => {
                return Err(RemoteReplayTransactionErrorV1::Unavailable);
            }
        }
    };
    transaction
        .commit()
        .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
    Ok(outcome)
}

fn validate_binding(
    writer: &RemoteWriterAuthorityV1,
    binding: &StoreRuntimeBindingV1,
) -> Result<(), RemoteReplayTransactionErrorV1> {
    writer
        .validate()
        .map_err(|_| RemoteReplayTransactionErrorV1::FenceMismatch)?;
    if writer.authority.fence.brain_id != binding.shard_id.brain_id
        || writer.authority.fence.authority_epoch.0 != binding.authority_epoch.get()
    {
        return Err(RemoteReplayTransactionErrorV1::FenceMismatch);
    }
    Ok(())
}

fn validate_request(
    frame: &RemoteReplayFrameV1,
    binding: &StoreRuntimeBindingV1,
    request: &RuntimeSubmitRequestV1,
) -> Result<(), RemoteReplayTransactionErrorV1> {
    if request.binding() != binding
        || request.envelope().metadata.idempotency.key.as_str() != frame.event_id
    {
        return Err(RemoteReplayTransactionErrorV1::FenceMismatch);
    }
    match &request.envelope().payload {
        RepositoryWritePayloadV1::Observation(write)
            if write.observation() == &frame.capture.observation =>
        {
            Ok(())
        }
        RepositoryWritePayloadV1::Observation(_) => {
            Err(RemoteReplayTransactionErrorV1::IdempotencyConflict)
        }
        _ => Err(RemoteReplayTransactionErrorV1::CanonicalEffect),
    }
}

fn application_receipt(
    frame: &RemoteReplayFrameV1,
    writer: &RemoteWriterAuthorityV1,
    receipt: &StoreCommitReceiptV1,
) -> Result<RemoteReplayCommitReceiptV1, RemoteReplayTransactionErrorV1> {
    receipt
        .validate()
        .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
    if receipt.idempotency.key.as_str() != frame.event_id {
        return Err(RemoteReplayTransactionErrorV1::IdempotencyConflict);
    }
    let receipt = RemoteReplayCommitReceiptV1 {
        event_id: frame.event_id.clone(),
        writer_fence: writer.authority.fence.clone(),
        commit_sequence: receipt.commit_sequence.0,
    };
    receipt
        .validate_for(frame, writer)
        .map_err(|_| RemoteReplayTransactionErrorV1::FenceMismatch)?;
    Ok(receipt)
}
