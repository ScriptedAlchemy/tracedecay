use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use super::{
    ConsistencyModeV1, FrozenWatermarkCoverageV1, ReaderHealthLeaseIdV1, ReaderHealthLeaseV1,
    ShardWatermarkV1, SnapshotLeaseIdV1, SnapshotLeaseV1, StorageRuntimeContractErrorV1,
    StorageRuntimeErrorV1, StoreCommitReceiptV1, StoreRuntimeBindingV1,
};
use super::{MaintenanceTelemetryV1, RepositoryOperationEnvelopeV1};

/// A validated, closed write request for the daemon-owned runtime.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct RuntimeSubmitRequestV1 {
    envelope: RepositoryOperationEnvelopeV1,
}

impl RuntimeSubmitRequestV1 {
    pub fn new(
        envelope: RepositoryOperationEnvelopeV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        envelope.validate()?;
        Ok(Self { envelope })
    }

    pub fn envelope(&self) -> &RepositoryOperationEnvelopeV1 {
        &self.envelope
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.envelope.validate()
    }
}

impl<'de> Deserialize<'de> for RuntimeSubmitRequestV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(RepositoryOperationEnvelopeV1::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

/// Idempotent write result. Conflict is a typed business outcome rather than a
/// transport or driver error.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeSubmitOutcomeV1 {
    Committed {
        receipt: StoreCommitReceiptV1,
    },
    Replayed {
        receipt: StoreCommitReceiptV1,
    },
    Conflict {
        existing_receipt: StoreCommitReceiptV1,
    },
}

impl RuntimeSubmitOutcomeV1 {
    pub fn validate_for(
        &self,
        request: &RuntimeSubmitRequestV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        request.validate()?;
        let metadata = &request.envelope().metadata;
        match self {
            Self::Committed { receipt } => receipt.validate_for(metadata),
            Self::Replayed { receipt } => receipt.validate_replay_for(metadata),
            Self::Conflict { existing_receipt } => {
                existing_receipt.validate()?;
                if existing_receipt.shard_id != metadata.shard_id {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "conflict receipt shard id",
                    });
                }
                if existing_receipt.incarnation != metadata.incarnation {
                    return Err(StorageRuntimeContractErrorV1::IncarnationMismatch {
                        field: "conflict receipt incarnation",
                        expected: metadata.incarnation,
                        actual: existing_receipt.incarnation,
                    });
                }
                if existing_receipt.authority_epoch != metadata.authority_epoch {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "conflict receipt authority epoch",
                    });
                }
                if existing_receipt.idempotency.key != metadata.idempotency.key {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "conflict receipt idempotency key",
                    });
                }
                if existing_receipt.idempotency.command_digest
                    == metadata.idempotency.command_digest
                {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "conflict receipt command digest",
                    });
                }
                Ok(())
            }
        }
    }
}

/// Closed read operations admitted by the storage runtime. No operation carries
/// a driver query or a physical locator.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeReadOperationV1 {
    CurrentWatermark,
    SnapshotLease { lease_id: SnapshotLeaseIdV1 },
    FrozenCoverage,
    MaintenanceTelemetry,
    ReaderHealthLease { lease_id: ReaderHealthLeaseIdV1 },
}

/// A validated one-runtime read request. Frozen consistency still names a
/// primary binding, while the requested vector describes cross-runtime
/// coverage to be returned with the read.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RuntimeReadRequestV1 {
    binding: StoreRuntimeBindingV1,
    consistency: ConsistencyModeV1,
    operation: RuntimeReadOperationV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeReadRequestWireV1 {
    binding: StoreRuntimeBindingV1,
    consistency: ConsistencyModeV1,
    operation: RuntimeReadOperationV1,
}

impl RuntimeReadRequestV1 {
    pub fn new(
        binding: StoreRuntimeBindingV1,
        consistency: ConsistencyModeV1,
        operation: RuntimeReadOperationV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let request = Self {
            binding,
            consistency,
            operation,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub fn consistency(&self) -> &ConsistencyModeV1 {
        &self.consistency
    }

    pub fn operation(&self) -> &RuntimeReadOperationV1 {
        &self.operation
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        match &self.consistency {
            ConsistencyModeV1::ExactSnapshot { lease } => {
                lease.validate()?;
                if !binding_matches_watermark(&self.binding, &lease.watermark) {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "snapshot lease runtime binding",
                    });
                }
                if let RuntimeReadOperationV1::SnapshotLease { lease_id } = &self.operation
                    && *lease_id != lease.lease_id
                {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "snapshot lease request id",
                    });
                }
            }
            ConsistencyModeV1::FrozenWatermarkVector { vector }
                if vector.get(&self.binding.shard_id).is_none_or(|watermark| {
                    !binding_matches_watermark(&self.binding, watermark)
                }) =>
            {
                return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                    field: "frozen watermark runtime binding",
                });
            }
            _ => {}
        }
        if self.operation == RuntimeReadOperationV1::FrozenCoverage
            && !matches!(
                self.consistency,
                ConsistencyModeV1::FrozenWatermarkVector { .. }
            )
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "frozen coverage consistency",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RuntimeReadRequestV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RuntimeReadRequestWireV1::deserialize(deserializer)?;
        Self::new(wire.binding, wire.consistency, wire.operation).map_err(serde::de::Error::custom)
    }
}

/// Typed runtime read results. The default read-port boundary validates the
/// returned variant against its request before returning it to callers.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeReadOutcomeV1 {
    CurrentWatermark { watermark: ShardWatermarkV1 },
    SnapshotLease { lease: Option<SnapshotLeaseV1> },
    FrozenCoverage { coverage: FrozenWatermarkCoverageV1 },
    MaintenanceTelemetry { telemetry: MaintenanceTelemetryV1 },
    ReaderHealthLease { lease: Option<ReaderHealthLeaseV1> },
}

impl RuntimeReadOutcomeV1 {
    pub fn validate_for(
        &self,
        request: &RuntimeReadRequestV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        request.validate()?;
        match (request.operation(), self) {
            (RuntimeReadOperationV1::CurrentWatermark, Self::CurrentWatermark { watermark })
                if watermark_satisfies_consistency(request, watermark) =>
            {
                Ok(())
            }
            (RuntimeReadOperationV1::SnapshotLease { lease_id }, Self::SnapshotLease { lease }) => {
                if lease.is_none()
                    && matches!(
                        request.consistency(),
                        ConsistencyModeV1::ExactSnapshot { .. }
                    )
                {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "exact snapshot lease read result",
                    });
                }
                if let Some(lease) = lease {
                    lease.validate()?;
                    if lease.lease_id != *lease_id
                        || !watermark_satisfies_consistency(request, &lease.watermark)
                    {
                        return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                            field: "snapshot lease read result",
                        });
                    }
                }
                Ok(())
            }
            (RuntimeReadOperationV1::FrozenCoverage, Self::FrozenCoverage { coverage }) => {
                coverage.validate()?;
                let ConsistencyModeV1::FrozenWatermarkVector { vector } = request.consistency()
                else {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "frozen coverage consistency",
                    });
                };
                if &coverage.required != vector {
                    return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                        field: "frozen coverage required vector",
                    });
                }
                Ok(())
            }
            (
                RuntimeReadOperationV1::MaintenanceTelemetry,
                Self::MaintenanceTelemetry { telemetry },
            ) if telemetry.shard_id == request.binding().shard_id
                && telemetry.incarnation == request.binding().incarnation
                && telemetry.authority_epoch == request.binding().authority_epoch =>
            {
                Ok(())
            }
            (
                RuntimeReadOperationV1::ReaderHealthLease { lease_id },
                Self::ReaderHealthLease { lease },
            ) => {
                if let Some(lease) = lease {
                    lease.validate()?;
                    if lease.lease_id != *lease_id || lease.binding != *request.binding() {
                        return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                            field: "reader health lease read result",
                        });
                    }
                }
                Ok(())
            }
            _ => Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "runtime read outcome operation",
            }),
        }
    }
}

fn binding_matches_watermark(
    binding: &StoreRuntimeBindingV1,
    watermark: &ShardWatermarkV1,
) -> bool {
    binding.shard_id == watermark.shard_id
        && binding.incarnation == watermark.incarnation
        && binding.authority_epoch == watermark.authority_epoch
}

fn watermark_satisfies_consistency(
    request: &RuntimeReadRequestV1,
    watermark: &ShardWatermarkV1,
) -> bool {
    if !binding_matches_watermark(request.binding(), watermark) {
        return false;
    }
    match request.consistency() {
        ConsistencyModeV1::LatestAvailable => true,
        ConsistencyModeV1::AtLeast { commit_sequence } => {
            watermark.commit_sequence >= *commit_sequence
        }
        ConsistencyModeV1::ExactSnapshot { lease } => watermark == &lease.watermark,
        ConsistencyModeV1::FrozenWatermarkVector { vector } => vector
            .get(&request.binding().shard_id)
            .is_some_and(|required| watermark.satisfies(required)),
    }
}

#[derive(Debug, Error)]
pub enum StorageRuntimePortErrorV1 {
    #[error("invalid storage runtime request: {0}")]
    InvalidRequest(StorageRuntimeContractErrorV1),
    #[error("invalid storage runtime response: {0}")]
    InvalidResponse(StorageRuntimeContractErrorV1),
    #[error(transparent)]
    Runtime(Box<StorageRuntimeErrorV1>),
}

impl From<StorageRuntimeErrorV1> for StorageRuntimePortErrorV1 {
    fn from(error: StorageRuntimeErrorV1) -> Self {
        Self::Runtime(Box::new(error))
    }
}

pub type StorageRuntimePortResultV1<T> = Result<T, StorageRuntimePortErrorV1>;

/// Typed write boundary. Adapters implement only `dispatch_submit`; callers use
/// `submit`, which validates both the request and the receipt-bearing outcome.
pub trait StorageRuntimeSubmitPort: Send + Sync {
    fn dispatch_submit(
        &self,
        request: RuntimeSubmitRequestV1,
    ) -> StorageRuntimePortResultV1<RuntimeSubmitOutcomeV1>;

    fn submit(
        &self,
        request: RuntimeSubmitRequestV1,
    ) -> StorageRuntimePortResultV1<RuntimeSubmitOutcomeV1> {
        request
            .validate()
            .map_err(StorageRuntimePortErrorV1::InvalidRequest)?;
        let outcome = self.dispatch_submit(request.clone())?;
        outcome
            .validate_for(&request)
            .map_err(StorageRuntimePortErrorV1::InvalidResponse)?;
        Ok(outcome)
    }
}

/// Typed read boundary. The only implementation hook receives a validated
/// request; the public method rejects a result whose history or variant does
/// not bind to that request.
pub trait StorageRuntimeReadPort: Send + Sync {
    fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
    ) -> StorageRuntimePortResultV1<RuntimeReadOutcomeV1>;

    fn read(
        &self,
        request: RuntimeReadRequestV1,
    ) -> StorageRuntimePortResultV1<RuntimeReadOutcomeV1> {
        request
            .validate()
            .map_err(StorageRuntimePortErrorV1::InvalidRequest)?;
        let outcome = self.dispatch_read(request.clone())?;
        outcome
            .validate_for(&request)
            .map_err(StorageRuntimePortErrorV1::InvalidResponse)?;
        Ok(outcome)
    }
}
