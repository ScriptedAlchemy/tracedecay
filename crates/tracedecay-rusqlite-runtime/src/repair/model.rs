use std::fmt;

use serde::{Deserialize, Serialize};
use tracedecay_store::{StoreOperationIdV1, StoreRuntimeBindingV1};

/// A typed observation emitted by a store-specific, read-only probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorruptionObservation {
    DerivedFts,
    Authoritative,
    Unclassified,
}

/// Read-only evidence tied directly to the canonical runtime publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorruptionEvidence {
    pub binding: StoreRuntimeBindingV1,
    pub evidence_id: StoreOperationIdV1,
    pub observations: Vec<CorruptionObservation>,
}

impl CorruptionEvidence {
    pub(crate) fn classify(self) -> CorruptionDiagnosis {
        let class = if self
            .observations
            .contains(&CorruptionObservation::Authoritative)
        {
            CorruptionClass::Authoritative
        } else if self
            .observations
            .contains(&CorruptionObservation::Unclassified)
        {
            CorruptionClass::Indeterminate
        } else if self.observations.is_empty() {
            CorruptionClass::Healthy
        } else {
            CorruptionClass::DerivedFtsOnly
        };
        CorruptionDiagnosis {
            binding: self.binding,
            evidence_id: self.evidence_id,
            class,
            observations: self.observations,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorruptionClass {
    Healthy,
    DerivedFtsOnly,
    Authoritative,
    Indeterminate,
}

/// Coordinator-owned classification of read-only corruption evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorruptionDiagnosis {
    pub binding: StoreRuntimeBindingV1,
    pub evidence_id: StoreOperationIdV1,
    pub class: CorruptionClass,
    pub observations: Vec<CorruptionObservation>,
}

/// Idempotent receipt for an atomic derived-FTS rebuild.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairReceipt {
    pub receipt_id: StoreOperationIdV1,
    pub evidence_id: StoreOperationIdV1,
    pub binding: StoreRuntimeBindingV1,
}

/// Idempotent receipt for preserved authoritative corruption evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QuarantineReceipt {
    pub receipt_id: StoreOperationIdV1,
    pub evidence_id: StoreOperationIdV1,
    pub binding: StoreRuntimeBindingV1,
    /// Opaque adapter-owned reference to preserved evidence.
    pub evidence_reference: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultStage {
    Diagnosis,
    ReceiptLookup,
    MaintenanceAuthorization,
    Rebuild,
    Quarantine,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairFault {
    pub code: &'static str,
    pub detail: String,
}

impl RepairFault {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for RepairFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for RepairFault {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectionReason {
    Healthy,
    IndeterminateEscalationRequired,
    AuthoritativeQuarantined,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepairOutcome {
    Completed {
        receipt: RepairReceipt,
        replayed: bool,
    },
    Rejected {
        diagnosis: CorruptionDiagnosis,
        reason: RejectionReason,
        quarantine: Option<QuarantineReceipt>,
    },
    Faulted {
        diagnosis: Option<CorruptionDiagnosis>,
        stage: FaultStage,
        fault: RepairFault,
    },
}
