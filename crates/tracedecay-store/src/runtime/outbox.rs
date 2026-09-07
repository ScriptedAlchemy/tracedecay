use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::UtcMicros;

use super::{
    CommandDigestV1, ShardWatermarkV1, StorageRuntimeContractErrorV1, StoreAuthorityEpochV1,
    StoreEffectIdV1, StoreEffectOrderingKeyV1,
};

/// Storage-owned identity and fences that make one cross-shard effect replay-safe.
///
/// `StoreEffectIdV1` is the persisted representation of an application effect
/// identity. The store crate cannot import the application crate without
/// reversing dependency direction, so adapters use its validated string
/// conversion rather than treating this as a second application authority.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectIdentityV1 {
    pub effect_id: StoreEffectIdV1,
    pub command_digest: CommandDigestV1,
    pub ordering_key: StoreEffectOrderingKeyV1,
    pub source_watermark: ShardWatermarkV1,
    pub target_watermark: ShardWatermarkV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EffectIdentityWireV1 {
    effect_id: StoreEffectIdV1,
    command_digest: CommandDigestV1,
    ordering_key: StoreEffectOrderingKeyV1,
    source_watermark: ShardWatermarkV1,
    target_watermark: ShardWatermarkV1,
}

impl EffectIdentityV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.source_watermark.shard_id == self.target_watermark.shard_id {
            return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                field: "cross-shard effect target",
            });
        }
        Ok(())
    }

    pub fn enforce_epochs(
        &self,
        source: StoreAuthorityEpochV1,
        target: StoreAuthorityEpochV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.source_watermark.authority_epoch != source {
            return Err(StorageRuntimeContractErrorV1::EffectEpochMismatch { side: "source" });
        }
        if self.target_watermark.authority_epoch != target {
            return Err(StorageRuntimeContractErrorV1::EffectEpochMismatch { side: "target" });
        }
        Ok(())
    }

    pub fn enforce_histories(
        &self,
        source: &ShardWatermarkV1,
        target: &ShardWatermarkV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        validate_watermark_history("source", &self.source_watermark, source)?;
        validate_watermark_history("target", &self.target_watermark, target)?;
        if !source.satisfies(&self.source_watermark) {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "source dispatch watermark",
            });
        }
        if !target.satisfies(&self.target_watermark) {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "target dispatch watermark",
            });
        }
        Ok(())
    }
}

fn validate_watermark_history(
    side: &'static str,
    expected: &ShardWatermarkV1,
    actual: &ShardWatermarkV1,
) -> Result<(), StorageRuntimeContractErrorV1> {
    if actual.shard_id != expected.shard_id {
        return Err(StorageRuntimeContractErrorV1::ShardMismatch { field: side });
    }
    if actual.incarnation != expected.incarnation {
        return Err(StorageRuntimeContractErrorV1::EffectIncarnationMismatch { side });
    }
    if actual.authority_epoch != expected.authority_epoch {
        return Err(StorageRuntimeContractErrorV1::EffectEpochMismatch { side });
    }
    Ok(())
}

impl<'de> Deserialize<'de> for EffectIdentityV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = EffectIdentityWireV1::deserialize(deserializer)?;
        let identity = Self {
            effect_id: wire.effect_id,
            command_digest: wire.command_digest,
            ordering_key: wire.ordering_key,
            source_watermark: wire.source_watermark,
            target_watermark: wire.target_watermark,
        };
        identity.validate().map_err(serde::de::Error::custom)?;
        Ok(identity)
    }
}

/// Closed set of durable effects. Payloads remain in their repository contracts.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryEffectV1 {
    RegisterProject,
    PublishObservation,
    PublishWorkflowTask,
    PublishRemoteCommand,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutboxEffectStateV1 {
    Pending,
    Dispatched,
    /// Dispatch may have committed at the target, but no receipt is available.
    EffectUnknown,
    Acknowledged,
}

impl OutboxEffectStateV1 {
    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (Self::Pending, Self::Dispatched)
                    | (Self::Dispatched, Self::EffectUnknown)
                    | (Self::Dispatched, Self::Acknowledged)
                    | (Self::EffectUnknown, Self::Dispatched)
                    | (Self::EffectUnknown, Self::Acknowledged)
            )
    }

    fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatched => "dispatched",
            Self::EffectUnknown => "effect_unknown",
            Self::Acknowledged => "acknowledged",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InboxEffectDispositionV1 {
    Applied,
    Replayed,
}

/// Target receipt persisted atomically with an effect and keyed by its storage
/// effect identity and fenced target commit position. It deliberately does not
/// mint a receipt ID parallel to domain-specific receipt authorities.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransactionalInboxReceiptV1 {
    pub identity: EffectIdentityV1,
    pub disposition: InboxEffectDispositionV1,
    pub target_commit_watermark: ShardWatermarkV1,
    pub committed_at: UtcMicros,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransactionalInboxReceiptWireV1 {
    identity: EffectIdentityV1,
    disposition: InboxEffectDispositionV1,
    target_commit_watermark: ShardWatermarkV1,
    committed_at: UtcMicros,
}

impl TransactionalInboxReceiptV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.validate_for(&self.identity)
    }

    pub fn validate_for(
        &self,
        identity: &EffectIdentityV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        self.identity.validate()?;
        identity.validate()?;
        if self.identity != *identity {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "inbox receipt effect identity",
            });
        }
        validate_watermark_history(
            "target",
            &identity.target_watermark,
            &self.target_commit_watermark,
        )?;
        if self.target_commit_watermark.commit_sequence <= identity.target_watermark.commit_sequence
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "inbox receipt target watermark",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for TransactionalInboxReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TransactionalInboxReceiptWireV1::deserialize(deserializer)?;
        let receipt = Self {
            identity: wire.identity,
            disposition: wire.disposition,
            target_commit_watermark: wire.target_commit_watermark,
            committed_at: wire.committed_at,
        };
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}

/// Source-side durable evidence that an outbox entry was acknowledged by the
/// exact target receipt. This is the only contract that may acknowledge an
/// outbox entry.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutboxAcknowledgementReceiptV1 {
    pub identity: EffectIdentityV1,
    pub inbox_receipt: TransactionalInboxReceiptV1,
    pub source_commit_watermark: ShardWatermarkV1,
    pub acknowledged_at: UtcMicros,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutboxAcknowledgementReceiptWireV1 {
    identity: EffectIdentityV1,
    inbox_receipt: TransactionalInboxReceiptV1,
    source_commit_watermark: ShardWatermarkV1,
    acknowledged_at: UtcMicros,
}

impl OutboxAcknowledgementReceiptV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.identity.validate()?;
        self.inbox_receipt.validate_for(&self.identity)?;
        validate_watermark_history(
            "source",
            &self.identity.source_watermark,
            &self.source_commit_watermark,
        )?;
        if self.source_commit_watermark.commit_sequence
            <= self.identity.source_watermark.commit_sequence
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "outbox acknowledgement source watermark",
            });
        }
        if self.acknowledged_at < self.inbox_receipt.committed_at {
            return Err(StorageRuntimeContractErrorV1::InvalidLeaseInterval {
                field: "outbox acknowledgement time",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for OutboxAcknowledgementReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = OutboxAcknowledgementReceiptWireV1::deserialize(deserializer)?;
        let receipt = Self {
            identity: wire.identity,
            inbox_receipt: wire.inbox_receipt,
            source_commit_watermark: wire.source_commit_watermark,
            acknowledged_at: wire.acknowledged_at,
        };
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}

/// Record committed atomically with the source-domain mutation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TransactionalOutboxEntryV1 {
    pub identity: EffectIdentityV1,
    pub effect: RepositoryEffectV1,
    pub state: OutboxEffectStateV1,
    pub acknowledgement: Option<OutboxAcknowledgementReceiptV1>,
    pub enqueued_at: UtcMicros,
    pub updated_at: UtcMicros,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransactionalOutboxEntryWireV1 {
    identity: EffectIdentityV1,
    effect: RepositoryEffectV1,
    state: OutboxEffectStateV1,
    acknowledgement: Option<OutboxAcknowledgementReceiptV1>,
    enqueued_at: UtcMicros,
    updated_at: UtcMicros,
}

impl TransactionalOutboxEntryV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.identity.validate()?;
        if self.updated_at < self.enqueued_at {
            return Err(StorageRuntimeContractErrorV1::InvalidLeaseInterval {
                field: "outbox entry time",
            });
        }
        match (&self.state, &self.acknowledgement) {
            (OutboxEffectStateV1::Acknowledged, Some(receipt)) => {
                receipt.validate()?;
                if receipt.identity != self.identity {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "outbox acknowledgement effect identity",
                    });
                }
                if self.updated_at != receipt.acknowledged_at {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "outbox acknowledgement update time",
                    });
                }
            }
            (OutboxEffectStateV1::Acknowledged, None) => {
                return Err(StorageRuntimeContractErrorV1::AcknowledgementReceiptRequired);
            }
            (_, Some(_)) => {
                return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                    field: "outbox acknowledgement state",
                });
            }
            (_, None) => {}
        }
        Ok(())
    }

    pub fn transition(
        &mut self,
        next: OutboxEffectStateV1,
        updated_at: UtcMicros,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        self.validate()?;
        if next == OutboxEffectStateV1::Acknowledged {
            return Err(StorageRuntimeContractErrorV1::AcknowledgementReceiptRequired);
        }
        if !self.state.can_transition_to(next) {
            return Err(StorageRuntimeContractErrorV1::InvalidEffectTransition {
                from: self.state.name(),
                to: next.name(),
            });
        }
        if updated_at < self.updated_at {
            return Err(StorageRuntimeContractErrorV1::InvalidLeaseInterval {
                field: "outbox entry time",
            });
        }
        self.state = next;
        self.acknowledgement = None;
        self.updated_at = updated_at;
        Ok(())
    }

    pub fn acknowledge(
        &mut self,
        receipt: OutboxAcknowledgementReceiptV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        self.validate()?;
        receipt.validate()?;
        if receipt.identity != self.identity {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "outbox acknowledgement effect identity",
            });
        }
        if self.state == OutboxEffectStateV1::Acknowledged {
            if self.acknowledgement.as_ref() == Some(&receipt) {
                return Ok(());
            }
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "outbox acknowledgement receipt",
            });
        }
        if !self
            .state
            .can_transition_to(OutboxEffectStateV1::Acknowledged)
        {
            return Err(StorageRuntimeContractErrorV1::InvalidEffectTransition {
                from: self.state.name(),
                to: OutboxEffectStateV1::Acknowledged.name(),
            });
        }
        if receipt.acknowledged_at < self.updated_at {
            return Err(StorageRuntimeContractErrorV1::InvalidLeaseInterval {
                field: "outbox entry time",
            });
        }
        let acknowledged_at = receipt.acknowledged_at;
        self.state = OutboxEffectStateV1::Acknowledged;
        self.acknowledgement = Some(receipt);
        self.updated_at = acknowledged_at;
        Ok(())
    }
}

impl<'de> Deserialize<'de> for TransactionalOutboxEntryV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TransactionalOutboxEntryWireV1::deserialize(deserializer)?;
        let entry = Self {
            identity: wire.identity,
            effect: wire.effect,
            state: wire.state,
            acknowledgement: wire.acknowledgement,
            enqueued_at: wire.enqueued_at,
            updated_at: wire.updated_at,
        };
        entry.validate().map_err(serde::de::Error::custom)?;
        Ok(entry)
    }
}
