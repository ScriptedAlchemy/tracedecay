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
use tracedecay_domain::{ObservationSourceCursorV1, canonical_sha256};
use tracedecay_store::{
    AnchoredObservationWrite, CommandDigestV1, DurabilityClassV1, IdempotencyIdentityV1,
    ObservationWrite, OperationPriorityV1, RepositoryOperationEnvelopeV1, RepositoryWritePayloadV1,
    RuntimeBatchCompatibilityV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeRequestControlV1, RuntimeSubmitRequestV1,
    RuntimeTransactionIdV1, RuntimeTransactionScopeV1, StoreClientIdV1, StoreCommitReceiptV1,
    StoreIdempotencyKeyV1, StoreOperationIdV1, StoreOperationMetadataV1, StoreRuntimeBindingV1,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
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

#[derive(Clone, Copy, Debug, Default)]
pub struct CanonicalRemoteReplayRequestFactoryV1;

impl RemoteReplayRequestFactoryV1 for CanonicalRemoteReplayRequestFactoryV1 {
    fn build_request(
        &mut self,
        frame: &RemoteReplayFrameV1,
        binding: &StoreRuntimeBindingV1,
    ) -> Result<RuntimeSubmitRequestV1, RemoteReplayTransactionErrorV1> {
        frame
            .validate()
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let observation = frame.capture.observation.clone();
        let position = observation.identity().position();
        let expected_cursor = (position.start() > 0)
            .then(|| {
                ObservationSourceCursorV1::for_ordering(
                    observation.source().clone(),
                    observation.scope().clone(),
                    observation.identity().generation(),
                    observation.identity().ordering_domain(),
                    position.start(),
                )
            })
            .transpose()
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let next_cursor = ObservationSourceCursorV1::for_ordering(
            observation.source().clone(),
            observation.scope().clone(),
            observation.identity().generation(),
            observation.identity().ordering_domain(),
            position.end(),
        )
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let write = ObservationWrite::new(observation, expected_cursor, next_cursor)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let projection_generation = frame.capture.writer.authority.fence.generation_id.clone();
        let authorization = build_observation_resolution_authorization_v1(
            write.observation(),
            "tracedecay.remote-replay.v1",
        )
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let retrieval_anchor = build_observation_retrieval_anchor_v2(
            write.observation(),
            projection_generation.clone(),
            frame.capture.captured_at,
            authorization,
        )
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let payload = RepositoryWritePayloadV1::Observation(Box::new(
            AnchoredObservationWrite::new(write, retrieval_anchor, projection_generation)
                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
        ));
        let capture_identity = (
            "tracedecay.remote-capture.v2",
            &frame.capture.enrollment_id,
            frame.capture.enrollment_revision,
            &frame.capture.node_id,
            &frame.capture.writer,
            frame.capture.policy_revision,
            &frame.capture.sequence,
            &frame.capture.observation,
            frame.capture.captured_at,
        );
        let capture_digest = canonical_sha256(&capture_identity)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        if frame.event_id != format!("remote.event.{}", capture_digest.as_str()) {
            return Err(RemoteReplayTransactionErrorV1::IdempotencyConflict);
        }
        let capture_bytes = serde_json::to_vec(&capture_identity)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
        let metadata = StoreOperationMetadataV1 {
            operation_id: StoreOperationIdV1::new(format!("remote.replay.{}", frame.event_id))
                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
            client_id: StoreClientIdV1::new(format!("remote.{}", frame.capture.node_id.as_str()))
                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
            shard_id: binding.shard_id.clone(),
            incarnation: binding.incarnation,
            authority_epoch: binding.authority_epoch,
            idempotency: IdempotencyIdentityV1 {
                key: StoreIdempotencyKeyV1::new(frame.event_id.clone())
                    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
                command_digest: CommandDigestV1::new(capture_digest.as_str())
                    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
            },
            durability: DurabilityClassV1::Full,
            priority: OperationPriorityV1::Foreground,
            admission_bytes: u64::try_from(capture_bytes.len())
                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
            admitted_at: frame.capture.captured_at,
        };
        let transaction_scope = RuntimeTransactionScopeV1 {
            transaction_id: RuntimeTransactionIdV1::new(format!(
                "remote.replay.{}",
                frame.event_id
            ))
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
            compatibility: RuntimeBatchCompatibilityV1::from_operation(&metadata)
                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
            opened_at: frame.capture.captured_at,
        };
        let control = RuntimeRequestControlV1 {
            requested_at: frame.capture.captured_at,
            deadline: RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new(format!("remote.replay.{}", frame.event_id))
                    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
            },
            cancellation: RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new(format!(
                    "remote.replay.{}",
                    frame.event_id
                ))
                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
                generation: frame.capture.sequence.sequence,
            },
        };
        RuntimeSubmitRequestV1::new(
            RepositoryOperationEnvelopeV1 { metadata, payload },
            transaction_scope,
            control,
        )
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)
    }
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
