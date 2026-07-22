use std::{
    error::Error,
    fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use tracedecay_store::{
    DurabilityClassV1, IdempotencyIdentityV1, InboxEffectDispositionV1, OperationPriorityV1,
    OutboxAcknowledgementReceiptV1, OutboxEffectStateV1, RepositoryOperationEnvelopeV1,
    RepositoryWritePayloadV1, RuntimeBatchCompatibilityV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, RuntimeTransactionIdV1,
    RuntimeTransactionScopeV1, StorageRuntimeContractErrorV1, StoreClientIdV1,
    StoreCommitReceiptV1, StoreEffectIdV1, StoreIdempotencyKeyV1, StoreOperationIdV1,
    StoreOperationMetadataV1, StoreRuntimeBindingV1, TransactionalInboxReceiptV1,
    TransactionalOutboxEntryV1,
};

#[path = "../inbox/mod.rs"]
mod inbox;
#[path = "../outbox/mod.rs"]
mod outbox;

use super::ports::{
    OriginDispatchPreparation, OriginEffectReplayTransactions, OriginEffectTransactions,
    TargetEffectTransactions,
};
use crate::{
    PersistentWriter, WriterActorError,
    ledger::{self, LedgerError},
};

#[derive(Debug)]
pub enum SqliteEffectPersistenceError {
    Contract(StorageRuntimeContractErrorV1),
    Sqlite(rusqlite::Error),
    Writer(WriterActorError),
    OrderingKeyBusy,
    StaleBinding { field: &'static str },
    Rejected(&'static str),
    Ledger(String),
    Clock,
}

impl fmt::Display for SqliteEffectPersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contract(error) => write!(formatter, "invalid durable effect: {error}"),
            Self::Sqlite(error) => write!(formatter, "durable effect SQLite read failed: {error}"),
            Self::Writer(error) => write!(formatter, "durable effect writer failed: {error}"),
            Self::OrderingKeyBusy => {
                formatter.write_str("durable effect ordering key has an earlier pending effect")
            }
            Self::StaleBinding { field } => {
                write!(formatter, "durable effect rejected stale {field}")
            }
            Self::Rejected(reason) => {
                write!(formatter, "durable effect was not committed: {reason}")
            }
            Self::Ledger(error) => write!(formatter, "durable effect ledger failure: {error}"),
            Self::Clock => formatter.write_str("durable effect clock is outside SQLite range"),
        }
    }
}

impl Error for SqliteEffectPersistenceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Contract(error) => Some(error),
            Self::Sqlite(error) => Some(error),
            Self::Writer(error) => Some(error),
            Self::OrderingKeyBusy
            | Self::StaleBinding { .. }
            | Self::Rejected(_)
            | Self::Ledger(_)
            | Self::Clock => None,
        }
    }
}

impl From<rusqlite::Error> for SqliteEffectPersistenceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<StorageRuntimeContractErrorV1> for SqliteEffectPersistenceError {
    fn from(error: StorageRuntimeContractErrorV1) -> Self {
        Self::Contract(error)
    }
}

impl From<WriterActorError> for SqliteEffectPersistenceError {
    fn from(error: WriterActorError) -> Self {
        Self::Writer(error)
    }
}

impl From<LedgerError> for SqliteEffectPersistenceError {
    fn from(error: LedgerError) -> Self {
        match error {
            LedgerError::ReplayBindingMismatch {
                field: "outbox ordering key busy",
            } => Self::OrderingKeyBusy,
            LedgerError::ReplayBindingMismatch { field } => Self::StaleBinding { field },
            error => Self::Ledger(error.to_string()),
        }
    }
}

pub struct SqliteOriginEffectTransactions<'writer> {
    writer: &'writer PersistentWriter,
    reader: Connection,
}

impl<'writer> SqliteOriginEffectTransactions<'writer> {
    pub fn open(writer: &'writer PersistentWriter) -> Result<Self, SqliteEffectPersistenceError> {
        let reader = Connection::open_with_flags(writer.path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self { writer, reader })
    }

    fn outbox_entry(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        effect_id: &StoreEffectIdV1,
    ) -> Result<Option<TransactionalOutboxEntryV1>, SqliteEffectPersistenceError> {
        let transaction = self.reader.transaction()?;
        let entry = ledger::outbox_entry(&transaction, binding, effect_id)?;
        transaction.commit()?;
        Ok(entry)
    }
}

impl OriginEffectTransactions for SqliteOriginEffectTransactions<'_> {
    type Error = SqliteEffectPersistenceError;

    async fn prepare_dispatch(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        effect_id: &StoreEffectIdV1,
    ) -> Result<Option<OriginDispatchPreparation>, Self::Error> {
        enforce_writer_binding(self.writer, binding)?;
        let Some(entry) = self.outbox_entry(binding, effect_id)? else {
            return Ok(None);
        };
        Ok(Some(match entry.state {
            OutboxEffectStateV1::Pending | OutboxEffectStateV1::EffectUnknown => {
                let previous_updated_at = entry.updated_at.0;
                let mut dispatched = entry;
                transition(
                    &mut dispatched,
                    OutboxEffectStateV1::Dispatched,
                    monotonic_now(previous_updated_at)?,
                )?;
                submit(
                    self.writer,
                    RepositoryWritePayloadV1::EnqueueOutbox(Box::new(dispatched.clone())),
                    &dispatched.identity,
                    "dispatch",
                    dispatched.updated_at.0,
                    dispatched.updated_at.0,
                )
                .await?;
                OriginDispatchPreparation::Prepared(dispatched)
            }
            OutboxEffectStateV1::Dispatched => {
                OriginDispatchPreparation::InFlightWithoutReceipt(entry)
            }
            OutboxEffectStateV1::Acknowledged => {
                OriginDispatchPreparation::Acknowledged(entry.acknowledgement.ok_or_else(|| {
                    SqliteEffectPersistenceError::Ledger(
                        "acknowledged outbox entry has no receipt".to_owned(),
                    )
                })?)
            }
        }))
    }

    async fn mark_effect_unknown(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        dispatched: &TransactionalOutboxEntryV1,
    ) -> Result<TransactionalOutboxEntryV1, Self::Error> {
        enforce_writer_binding(self.writer, binding)?;
        let current = self
            .outbox_entry(binding, &dispatched.identity.effect_id)?
            .ok_or_else(|| {
                SqliteEffectPersistenceError::Ledger("outbox entry disappeared".to_owned())
            })?;
        if current.identity != dispatched.identity || current.effect != dispatched.effect {
            return Err(SqliteEffectPersistenceError::Ledger(
                "outbox entry changed identity before unknown transition".to_owned(),
            ));
        }
        if current.state == OutboxEffectStateV1::EffectUnknown {
            return Ok(current);
        }
        if current.state != OutboxEffectStateV1::Dispatched {
            return Err(SqliteEffectPersistenceError::Ledger(
                "outbox entry is not dispatched".to_owned(),
            ));
        }
        let previous_updated_at = current.updated_at.0;
        let mut unknown = current;
        transition(
            &mut unknown,
            OutboxEffectStateV1::EffectUnknown,
            monotonic_now(previous_updated_at)?,
        )?;
        submit(
            self.writer,
            RepositoryWritePayloadV1::EnqueueOutbox(Box::new(unknown.clone())),
            &unknown.identity,
            "unknown",
            unknown.updated_at.0,
            unknown.updated_at.0,
        )
        .await?;
        Ok(unknown)
    }

    async fn acknowledge(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        inbox: &TransactionalInboxReceiptV1,
    ) -> Result<OutboxAcknowledgementReceiptV1, Self::Error> {
        inbox.validate()?;
        validate_authority_root(&inbox.identity)?;
        validate_source_binding(binding, &inbox.identity)?;
        enforce_writer_binding(self.writer, binding)?;
        let entry = self
            .outbox_entry(binding, &inbox.identity.effect_id)?
            .ok_or_else(|| {
                SqliteEffectPersistenceError::Ledger("outbox entry disappeared".to_owned())
            })?;
        if let Some(acknowledgement) = entry.acknowledgement {
            return Ok(acknowledgement);
        }
        let admitted_at = monotonic_now(entry.updated_at.0.max(inbox.committed_at.0))?;
        let (commit, _) = submit(
            self.writer,
            RepositoryWritePayloadV1::AcknowledgeOutbox(Box::new(inbox.clone())),
            &inbox.identity,
            "ack",
            0,
            admitted_at,
        )
        .await?;
        acknowledgement(&commit, inbox)
    }
}

impl OriginEffectReplayTransactions for SqliteOriginEffectTransactions<'_> {
    async fn replay_candidates(
        &mut self,
        origin_binding: &StoreRuntimeBindingV1,
        target_binding: &StoreRuntimeBindingV1,
        limit: usize,
    ) -> Result<Vec<StoreEffectIdV1>, <Self as OriginEffectTransactions>::Error> {
        enforce_writer_binding(self.writer, origin_binding)?;
        outbox::replay_candidates(&mut self.reader, origin_binding, target_binding, limit)
            .map_err(Into::into)
    }
}

pub struct SqliteTargetEffectTransactions<'writer> {
    writer: &'writer PersistentWriter,
}

impl<'writer> SqliteTargetEffectTransactions<'writer> {
    pub const fn new(writer: &'writer PersistentWriter) -> Self {
        Self { writer }
    }
}

impl TargetEffectTransactions for SqliteTargetEffectTransactions<'_> {
    type Error = SqliteEffectPersistenceError;

    async fn apply_once(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        entry: &TransactionalOutboxEntryV1,
    ) -> Result<TransactionalInboxReceiptV1, Self::Error> {
        entry.validate()?;
        validate_authority_root(&entry.identity)?;
        validate_target_binding(binding, &entry.identity)?;
        enforce_writer_binding(self.writer, binding)?;
        if entry.state != OutboxEffectStateV1::Dispatched || entry.acknowledgement.is_some() {
            return Err(SqliteEffectPersistenceError::Ledger(
                "target requires a dispatched outbox entry".to_owned(),
            ));
        }
        if let Some(mut receipt) = persisted_inbox(self.writer, binding, entry)? {
            receipt.disposition = InboxEffectDispositionV1::Replayed;
            return Ok(receipt);
        }
        let admitted_at = monotonic_now(entry.updated_at.0)?;
        let (_, replayed) = submit(
            self.writer,
            RepositoryWritePayloadV1::ApplyInbox(Box::new(entry.clone())),
            &entry.identity,
            "apply",
            0,
            admitted_at,
        )
        .await?;
        let mut receipt = persisted_inbox(self.writer, binding, entry)?.ok_or_else(|| {
            SqliteEffectPersistenceError::Ledger(
                "committed target effect has no durable inbox receipt".to_owned(),
            )
        })?;
        if replayed {
            receipt.disposition = InboxEffectDispositionV1::Replayed;
        }
        Ok(receipt)
    }
}

fn persisted_inbox(
    writer: &PersistentWriter,
    binding: &StoreRuntimeBindingV1,
    entry: &TransactionalOutboxEntryV1,
) -> Result<Option<TransactionalInboxReceiptV1>, SqliteEffectPersistenceError> {
    let mut reader = Connection::open_with_flags(writer.path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    inbox::receipt(&mut reader, binding, entry).map_err(Into::into)
}

struct EffectProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl RuntimeRequestProbeV1 for EffectProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }
}

async fn submit(
    writer: &PersistentWriter,
    payload: RepositoryWritePayloadV1,
    identity: &tracedecay_store::EffectIdentityV1,
    stage: &'static str,
    generation: i64,
    admitted_at_micros: i64,
) -> Result<(StoreCommitReceiptV1, bool), SqliteEffectPersistenceError> {
    let request = effect_request(
        writer.binding(),
        identity,
        payload,
        stage,
        generation,
        admitted_at_micros,
    )?;
    let probe = Arc::new(EffectProbe {
        cancellation: request.control().cancellation.clone(),
        deadline: request.control().deadline.clone(),
    });
    let outcome = writer.submit(request, probe).await?;
    match outcome {
        RuntimeSubmitOutcomeV1::Committed { receipt }
        | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { receipt, .. } => {
            Ok((receipt, false))
        }
        RuntimeSubmitOutcomeV1::ExactReplay { receipt } => Ok((receipt, true)),
        RuntimeSubmitOutcomeV1::IdempotencyConflict { .. } => Err(
            SqliteEffectPersistenceError::Rejected("idempotency conflict"),
        ),
        RuntimeSubmitOutcomeV1::Saturated { .. } => {
            Err(SqliteEffectPersistenceError::Rejected("writer saturated"))
        }
        RuntimeSubmitOutcomeV1::Fenced { .. } => {
            Err(SqliteEffectPersistenceError::Rejected("writer fenced"))
        }
        RuntimeSubmitOutcomeV1::DeadlineExceededBeforeCommit { .. } => Err(
            SqliteEffectPersistenceError::Rejected("deadline before commit"),
        ),
        RuntimeSubmitOutcomeV1::CancelledBeforeCommit { .. } => Err(
            SqliteEffectPersistenceError::Rejected("cancelled before commit"),
        ),
        RuntimeSubmitOutcomeV1::Unavailable { .. } => {
            Err(SqliteEffectPersistenceError::Rejected("writer unavailable"))
        }
    }
}

fn effect_request(
    binding: &StoreRuntimeBindingV1,
    identity: &tracedecay_store::EffectIdentityV1,
    payload: RepositoryWritePayloadV1,
    stage: &'static str,
    generation: i64,
    admitted_at_micros: i64,
) -> Result<RuntimeSubmitRequestV1, SqliteEffectPersistenceError> {
    let operation_key = operation_key(identity, stage, generation);
    let metadata = StoreOperationMetadataV1 {
        operation_id: StoreOperationIdV1::new(operation_key.clone())?,
        client_id: StoreClientIdV1::new("runtime.effect.dispatch")?,
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        idempotency: IdempotencyIdentityV1 {
            key: StoreIdempotencyKeyV1::new(operation_key.clone())?,
            command_digest: identity.command_digest.clone(),
        },
        durability: DurabilityClassV1::Full,
        priority: OperationPriorityV1::Background,
        admission_bytes: u64::try_from(operation_key.len())
            .unwrap_or(u64::MAX)
            .max(1),
        admitted_at: serde_json::from_value(serde_json::json!(admitted_at_micros))
            .map_err(|_| SqliteEffectPersistenceError::Clock)?,
    };
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!("transaction.{operation_key}"))?,
        compatibility: RuntimeBatchCompatibilityV1::from_operation(&metadata)?,
        opened_at: metadata.admitted_at,
    };
    let control: RuntimeRequestControlV1 = serde_json::from_value(serde_json::json!({
        "requested_at": metadata.admitted_at,
        "deadline": { "deadline_id": format!("deadline.{operation_key}") },
        "cancellation": {
            "cancellation_id": format!("cancellation.{operation_key}"),
            "generation": 1
        }
    }))
    .map_err(|_| SqliteEffectPersistenceError::Clock)?;
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        control,
    )
    .map_err(Into::into)
}

fn operation_key(
    identity: &tracedecay_store::EffectIdentityV1,
    stage: &'static str,
    generation: i64,
) -> String {
    let mut digest = Sha256::new();
    digest.update(identity.effect_id.as_str().as_bytes());
    digest.update(stage.as_bytes());
    digest.update(generation.to_le_bytes());
    let digest = digest.finalize();
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("effect.{stage}.{digest}")
}

fn transition(
    entry: &mut TransactionalOutboxEntryV1,
    state: OutboxEffectStateV1,
    updated_at_micros: i64,
) -> Result<(), SqliteEffectPersistenceError> {
    let updated_at = serde_json::from_value(serde_json::json!(updated_at_micros))
        .map_err(|_| SqliteEffectPersistenceError::Clock)?;
    entry.transition(state, updated_at).map_err(Into::into)
}

fn acknowledgement(
    commit: &StoreCommitReceiptV1,
    inbox: &TransactionalInboxReceiptV1,
) -> Result<OutboxAcknowledgementReceiptV1, SqliteEffectPersistenceError> {
    serde_json::from_value(serde_json::json!({
        "identity": inbox.identity,
        "inbox_receipt": inbox,
        "source_commit_watermark": {
            "shard_id": commit.shard_id,
            "incarnation": commit.incarnation,
            "authority_epoch": commit.authority_epoch,
            "commit_sequence": commit.commit_sequence,
        },
        "acknowledged_at": commit.committed_at,
    }))
    .map_err(|_| {
        SqliteEffectPersistenceError::Ledger(
            "writer returned invalid outbox acknowledgement".to_owned(),
        )
    })
}

fn enforce_writer_binding(
    writer: &PersistentWriter,
    binding: &StoreRuntimeBindingV1,
) -> Result<(), StorageRuntimeContractErrorV1> {
    if writer.binding() != binding {
        return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
            field: "effect writer binding",
        });
    }
    Ok(())
}

fn validate_source_binding(
    binding: &StoreRuntimeBindingV1,
    identity: &tracedecay_store::EffectIdentityV1,
) -> Result<(), StorageRuntimeContractErrorV1> {
    validate_binding(binding, &identity.source_watermark, "source")
}

fn validate_authority_root(
    identity: &tracedecay_store::EffectIdentityV1,
) -> Result<(), StorageRuntimeContractErrorV1> {
    if identity.source_watermark.shard_id.brain_id != identity.target_watermark.shard_id.brain_id
        || identity.source_watermark.shard_id.profile_id
            != identity.target_watermark.shard_id.profile_id
    {
        return Err(StorageRuntimeContractErrorV1::ShardMismatch {
            field: "effect authority root",
        });
    }
    if identity
        .source_watermark
        .shard_id
        .scope
        .project_id()
        .is_none()
        || identity.source_watermark.shard_id.scope.project_id()
            != identity.target_watermark.shard_id.scope.project_id()
    {
        return Err(StorageRuntimeContractErrorV1::ShardMismatch {
            field: "effect project identity",
        });
    }
    Ok(())
}

fn validate_target_binding(
    binding: &StoreRuntimeBindingV1,
    identity: &tracedecay_store::EffectIdentityV1,
) -> Result<(), StorageRuntimeContractErrorV1> {
    validate_binding(binding, &identity.target_watermark, "target")
}

fn validate_binding(
    binding: &StoreRuntimeBindingV1,
    watermark: &tracedecay_store::ShardWatermarkV1,
    side: &'static str,
) -> Result<(), StorageRuntimeContractErrorV1> {
    if watermark.shard_id != binding.shard_id {
        return Err(StorageRuntimeContractErrorV1::ShardMismatch { field: side });
    }
    if watermark.incarnation != binding.incarnation {
        return Err(StorageRuntimeContractErrorV1::EffectIncarnationMismatch { side });
    }
    if watermark.authority_epoch != binding.authority_epoch {
        return Err(StorageRuntimeContractErrorV1::EffectEpochMismatch { side });
    }
    Ok(())
}

fn monotonic_now(floor: i64) -> Result<i64, SqliteEffectPersistenceError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SqliteEffectPersistenceError::Clock)?;
    let now =
        i64::try_from(elapsed.as_micros()).map_err(|_| SqliteEffectPersistenceError::Clock)?;
    Ok(now.max(floor.saturating_add(1)))
}
