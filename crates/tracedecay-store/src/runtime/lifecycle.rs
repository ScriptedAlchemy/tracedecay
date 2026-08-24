use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use super::{
    DurabilityClassV1, OperationPriorityV1, ReaderHealthLeaseIdV1, ReaderLaneV1, RuntimeLeaseIdV1,
    RuntimeMaintenanceStateV1, RuntimeMaintenanceTransitionIdV1, RuntimeOperationPermitIdV1,
    RuntimePublicationIdV1, RuntimeTransactionIdV1, StorageRuntimeContractErrorV1, StoreClientIdV1,
    StoreOperationIdV1, StoreOperationMetadataV1, StoreRuntimeBindingV1,
};

/// Canonical registry entry published after the daemon opens a runtime.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreRuntimeRegistryPublicationV1 {
    pub publication_id: RuntimePublicationIdV1,
    pub binding: StoreRuntimeBindingV1,
    pub published_at: UtcMicros,
}

/// Bounded lease protecting one published runtime from eviction or replacement.
///
/// Its interval is runtime resource ownership, not an application request
/// `Deadline`; caller deadline and cancellation-token identity remain owned by
/// the application layer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "RuntimeLeaseWireV1")]
#[serde(deny_unknown_fields)]
pub struct RuntimeLeaseV1 {
    pub lease_id: RuntimeLeaseIdV1,
    pub binding: StoreRuntimeBindingV1,
    pub holder: StoreClientIdV1,
    pub acquired_at: UtcMicros,
    pub expires_at: UtcMicros,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeLeaseWireV1 {
    lease_id: RuntimeLeaseIdV1,
    binding: StoreRuntimeBindingV1,
    holder: StoreClientIdV1,
    acquired_at: UtcMicros,
    expires_at: UtcMicros,
}

impl RuntimeLeaseV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.expires_at <= self.acquired_at {
            return Err(StorageRuntimeContractErrorV1::InvalidLeaseInterval {
                field: "runtime lease",
            });
        }
        Ok(())
    }

    pub fn is_expired_at(&self, now: UtcMicros) -> bool {
        now >= self.expires_at
    }
}

impl TryFrom<RuntimeLeaseWireV1> for RuntimeLeaseV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(wire: RuntimeLeaseWireV1) -> Result<Self, Self::Error> {
        let lease = Self {
            lease_id: wire.lease_id,
            binding: wire.binding,
            holder: wire.holder,
            acquired_at: wire.acquired_at,
            expires_at: wire.expires_at,
        };
        lease.validate()?;
        Ok(lease)
    }
}

/// Lease for the reader reserved to health checks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "ReaderHealthLeaseWireV1")]
#[serde(deny_unknown_fields)]
pub struct ReaderHealthLeaseV1 {
    pub lease_id: ReaderHealthLeaseIdV1,
    pub binding: StoreRuntimeBindingV1,
    pub holder: StoreClientIdV1,
    pub lane: ReaderLaneV1,
    pub acquired_at: UtcMicros,
    pub expires_at: UtcMicros,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReaderHealthLeaseWireV1 {
    lease_id: ReaderHealthLeaseIdV1,
    binding: StoreRuntimeBindingV1,
    holder: StoreClientIdV1,
    lane: ReaderLaneV1,
    acquired_at: UtcMicros,
    expires_at: UtcMicros,
}

impl ReaderHealthLeaseV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if self.lane != ReaderLaneV1::ReservedHealth {
            return Err(StorageRuntimeContractErrorV1::ReaderHealthLaneRequired);
        }
        if self.expires_at <= self.acquired_at {
            return Err(StorageRuntimeContractErrorV1::InvalidLeaseInterval {
                field: "reader health lease",
            });
        }
        Ok(())
    }

    pub fn is_expired_at(&self, now: UtcMicros) -> bool {
        now >= self.expires_at
    }
}

impl TryFrom<ReaderHealthLeaseWireV1> for ReaderHealthLeaseV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(wire: ReaderHealthLeaseWireV1) -> Result<Self, Self::Error> {
        let lease = Self {
            lease_id: wire.lease_id,
            binding: wire.binding,
            holder: wire.holder,
            lane: wire.lane,
            acquired_at: wire.acquired_at,
            expires_at: wire.expires_at,
        };
        lease.validate()?;
        Ok(lease)
    }
}

/// A fenced lifecycle transition. Exclusive maintenance must retain the
/// matching runtime lease throughout the transition.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "RuntimeMaintenanceTransitionWireV1")]
#[serde(deny_unknown_fields)]
pub struct RuntimeMaintenanceTransitionV1 {
    pub transition_id: RuntimeMaintenanceTransitionIdV1,
    pub binding: StoreRuntimeBindingV1,
    pub lease: RuntimeLeaseV1,
    pub from: RuntimeMaintenanceStateV1,
    pub to: RuntimeMaintenanceStateV1,
    pub requested_at: UtcMicros,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeMaintenanceTransitionWireV1 {
    transition_id: RuntimeMaintenanceTransitionIdV1,
    binding: StoreRuntimeBindingV1,
    lease: RuntimeLeaseV1,
    from: RuntimeMaintenanceStateV1,
    to: RuntimeMaintenanceStateV1,
    requested_at: UtcMicros,
}

impl RuntimeMaintenanceTransitionV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.lease.validate()?;
        if self.lease.binding != self.binding {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "maintenance transition runtime lease",
            });
        }
        if self.requested_at < self.lease.acquired_at || self.lease.is_expired_at(self.requested_at)
        {
            return Err(StorageRuntimeContractErrorV1::InvalidLeaseInterval {
                field: "maintenance transition runtime lease",
            });
        }
        if !Self::is_allowed(self.from, self.to) {
            return Err(
                StorageRuntimeContractErrorV1::InvalidMaintenanceTransition {
                    from: self.from.name(),
                    to: self.to.name(),
                },
            );
        }
        Ok(())
    }

    pub fn is_allowed(from: RuntimeMaintenanceStateV1, to: RuntimeMaintenanceStateV1) -> bool {
        match from {
            RuntimeMaintenanceStateV1::Closed => {
                matches!(to, RuntimeMaintenanceStateV1::Opening)
            }
            RuntimeMaintenanceStateV1::Opening => matches!(
                to,
                RuntimeMaintenanceStateV1::Ready | RuntimeMaintenanceStateV1::Faulted
            ),
            RuntimeMaintenanceStateV1::Ready => matches!(
                to,
                RuntimeMaintenanceStateV1::Draining | RuntimeMaintenanceStateV1::Faulted
            ),
            RuntimeMaintenanceStateV1::Draining => matches!(
                to,
                RuntimeMaintenanceStateV1::ExclusiveMaintenance
                    | RuntimeMaintenanceStateV1::Closed
                    | RuntimeMaintenanceStateV1::Faulted
            ),
            RuntimeMaintenanceStateV1::ExclusiveMaintenance => matches!(
                to,
                RuntimeMaintenanceStateV1::Reopening | RuntimeMaintenanceStateV1::Faulted
            ),
            RuntimeMaintenanceStateV1::Reopening => matches!(
                to,
                RuntimeMaintenanceStateV1::Ready | RuntimeMaintenanceStateV1::Faulted
            ),
            RuntimeMaintenanceStateV1::Faulted => false,
        }
    }
}

impl TryFrom<RuntimeMaintenanceTransitionWireV1> for RuntimeMaintenanceTransitionV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(wire: RuntimeMaintenanceTransitionWireV1) -> Result<Self, Self::Error> {
        let transition = Self {
            transition_id: wire.transition_id,
            binding: wire.binding,
            lease: wire.lease,
            from: wire.from,
            to: wire.to,
            requested_at: wire.requested_at,
        };
        transition.validate()?;
        Ok(transition)
    }
}

impl RuntimeMaintenanceStateV1 {
    pub fn name(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Opening => "opening",
            Self::Ready => "ready",
            Self::Draining => "draining",
            Self::ExclusiveMaintenance => "exclusive_maintenance",
            Self::Reopening => "reopening",
            Self::Faulted => "faulted",
        }
    }
}

/// Compatibility key for operations that may share one local transaction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "RuntimeBatchCompatibilityWireV1")]
#[serde(deny_unknown_fields)]
pub struct RuntimeBatchCompatibilityV1 {
    pub binding: StoreRuntimeBindingV1,
    pub durability: DurabilityClassV1,
    pub priority: OperationPriorityV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeBatchCompatibilityWireV1 {
    binding: StoreRuntimeBindingV1,
    durability: DurabilityClassV1,
    priority: OperationPriorityV1,
}

impl RuntimeBatchCompatibilityV1 {
    pub fn from_operation(
        metadata: &StoreOperationMetadataV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        metadata.validate()?;
        let compatibility = Self {
            binding: StoreRuntimeBindingV1::new(
                metadata.shard_id.clone(),
                metadata.incarnation,
                metadata.authority_epoch,
            ),
            durability: metadata.durability,
            priority: metadata.priority,
        };
        compatibility.validate()?;
        Ok(compatibility)
    }

    pub fn for_batch<'a>(
        operations: impl IntoIterator<Item = &'a StoreOperationMetadataV1>,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let mut operations = operations.into_iter();
        let Some(first) = operations.next() else {
            return Err(StorageRuntimeContractErrorV1::Empty {
                field: "runtime batch",
            });
        };
        let compatibility = Self::from_operation(first)?;
        for operation in operations {
            compatibility.validate_operation(operation)?;
        }
        Ok(compatibility)
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        if !self.binding.shard_id.is_mutable() {
            return Err(StorageRuntimeContractErrorV1::ImmutableShard {
                operation: "runtime batch",
            });
        }
        Ok(())
    }

    pub fn validate_operation(
        &self,
        metadata: &StoreOperationMetadataV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        self.validate()?;
        metadata.validate()?;
        if self.binding.shard_id != metadata.shard_id {
            return Err(StorageRuntimeContractErrorV1::BatchIncompatible { field: "shard id" });
        }
        if self.binding.incarnation != metadata.incarnation {
            return Err(StorageRuntimeContractErrorV1::BatchIncompatible {
                field: "store incarnation",
            });
        }
        if self.binding.authority_epoch != metadata.authority_epoch {
            return Err(StorageRuntimeContractErrorV1::BatchIncompatible {
                field: "authority epoch",
            });
        }
        if self.durability != metadata.durability {
            return Err(StorageRuntimeContractErrorV1::BatchIncompatible {
                field: "durability",
            });
        }
        if self.priority != metadata.priority {
            return Err(StorageRuntimeContractErrorV1::BatchIncompatible { field: "priority" });
        }
        Ok(())
    }
}

impl TryFrom<RuntimeBatchCompatibilityWireV1> for RuntimeBatchCompatibilityV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(wire: RuntimeBatchCompatibilityWireV1) -> Result<Self, Self::Error> {
        let compatibility = Self {
            binding: wire.binding,
            durability: wire.durability,
            priority: wire.priority,
        };
        compatibility.validate()?;
        Ok(compatibility)
    }
}

/// Scope of a local transaction selected after batch compatibility is checked.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "RuntimeTransactionScopeWireV1")]
#[serde(deny_unknown_fields)]
pub struct RuntimeTransactionScopeV1 {
    pub transaction_id: RuntimeTransactionIdV1,
    pub compatibility: RuntimeBatchCompatibilityV1,
    pub opened_at: UtcMicros,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeTransactionScopeWireV1 {
    transaction_id: RuntimeTransactionIdV1,
    compatibility: RuntimeBatchCompatibilityV1,
    opened_at: UtcMicros,
}

impl RuntimeTransactionScopeV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.compatibility.validate()
    }

    pub fn validate_operation(
        &self,
        metadata: &StoreOperationMetadataV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        self.validate()?;
        self.compatibility.validate_operation(metadata)
    }
}

impl TryFrom<RuntimeTransactionScopeWireV1> for RuntimeTransactionScopeV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(wire: RuntimeTransactionScopeWireV1) -> Result<Self, Self::Error> {
        let scope = Self {
            transaction_id: wire.transaction_id,
            compatibility: wire.compatibility,
            opened_at: wire.opened_at,
        };
        scope.validate()?;
        Ok(scope)
    }
}

/// Opaque admission permit bound to one operation and one transaction scope.
///
/// Permit expiry bounds runtime admission after the application has admitted a
/// request. It is not a second caller deadline authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "RuntimeOperationPermitWireV1")]
#[serde(deny_unknown_fields)]
pub struct RuntimeOperationPermitV1 {
    pub permit_id: RuntimeOperationPermitIdV1,
    pub transaction_scope: RuntimeTransactionScopeV1,
    pub operation_id: StoreOperationIdV1,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeOperationPermitWireV1 {
    permit_id: RuntimeOperationPermitIdV1,
    transaction_scope: RuntimeTransactionScopeV1,
    operation_id: StoreOperationIdV1,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
}

impl RuntimeOperationPermitV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.transaction_scope.validate()?;
        if self.issued_at < self.transaction_scope.opened_at || self.expires_at <= self.issued_at {
            return Err(StorageRuntimeContractErrorV1::InvalidLeaseInterval {
                field: "runtime operation permit",
            });
        }
        Ok(())
    }

    pub fn validate_for(
        &self,
        metadata: &StoreOperationMetadataV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        self.validate()?;
        if self.operation_id != metadata.operation_id {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "runtime operation permit operation id",
            });
        }
        self.transaction_scope.validate_operation(metadata)
    }

    pub fn is_expired_at(&self, now: UtcMicros) -> bool {
        now >= self.expires_at
    }
}

impl TryFrom<RuntimeOperationPermitWireV1> for RuntimeOperationPermitV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(wire: RuntimeOperationPermitWireV1) -> Result<Self, Self::Error> {
        let permit = Self {
            permit_id: wire.permit_id,
            transaction_scope: wire.transaction_scope,
            operation_id: wire.operation_id,
            issued_at: wire.issued_at,
            expires_at: wire.expires_at,
        };
        permit.validate()?;
        Ok(permit)
    }
}
