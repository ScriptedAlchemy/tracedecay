use tracedecay_store::{
    OutboxAcknowledgementReceiptV1, StoreEffectIdV1, StoreRuntimeBindingV1,
    TransactionalInboxReceiptV1, TransactionalOutboxEntryV1,
};

/// Result of the origin's fenced, ordering-aware dispatch transaction.
///
/// `Prepared` means the transaction changed `Pending` or `EffectUnknown` to
/// `Dispatched`. `InFlightWithoutReceipt` means a previous process left a
/// dispatched entry with no target receipt, so its result is not knowable yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginDispatchPreparation {
    Prepared(TransactionalOutboxEntryV1),
    InFlightWithoutReceipt(TransactionalOutboxEntryV1),
    Acknowledged(OutboxAcknowledgementReceiptV1),
}

/// Narrow origin-shard transaction port.
///
/// Implementations must fence every mutation by `binding`. `prepare_dispatch`
/// also serializes each ordering key and atomically transitions the selected
/// canonical outbox entry. The domain mutation and initial outbox enqueue are a
/// prior repository transaction and are intentionally not modeled here.
#[allow(async_fn_in_trait)]
pub trait OriginEffectTransactions {
    type Error;

    async fn prepare_dispatch(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        effect_id: &StoreEffectIdV1,
    ) -> Result<Option<OriginDispatchPreparation>, Self::Error>;

    async fn mark_effect_unknown(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        entry: &TransactionalOutboxEntryV1,
    ) -> Result<TransactionalOutboxEntryV1, Self::Error>;

    async fn acknowledge(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        receipt: &TransactionalInboxReceiptV1,
    ) -> Result<OutboxAcknowledgementReceiptV1, Self::Error>;
}

/// Origin-shard recovery scan used by bounded restart replay.
///
/// Implementations return at most `limit` canonical ordering-key heads. The
/// coordinator also truncates defensively so a faulty adapter cannot turn one
/// recovery pass into unbounded work.
#[allow(async_fn_in_trait)]
pub trait OriginEffectReplayTransactions: OriginEffectTransactions {
    async fn replay_candidates(
        &mut self,
        origin_binding: &StoreRuntimeBindingV1,
        target_binding: &StoreRuntimeBindingV1,
        limit: usize,
    ) -> Result<Vec<StoreEffectIdV1>, <Self as OriginEffectTransactions>::Error>;
}

/// Narrow target-shard transaction port.
///
/// One implementation call atomically inserts (or reads) inbox idempotency,
/// applies the closed `RepositoryEffectV1` exactly once, and persists the
/// canonical inbox receipt. It must not interpret a workflow task as permission
/// to execute a workflow.
#[allow(async_fn_in_trait)]
pub trait TargetEffectTransactions {
    type Error;

    async fn apply_once(
        &mut self,
        binding: &StoreRuntimeBindingV1,
        entry: &TransactionalOutboxEntryV1,
    ) -> Result<TransactionalInboxReceiptV1, Self::Error>;
}
