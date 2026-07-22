use super::model::{
    CorruptionClass, CorruptionDiagnosis, CorruptionEvidence, FaultStage, QuarantineReceipt,
    RejectionReason, RepairFault, RepairOutcome, RepairReceipt,
};
use crate::maintenance::ExclusiveMaintenancePermit;
use tracedecay_store::{StoreOperationIdV1, StoreRuntimeBindingV1};

/// Read-only corruption evidence source. Implementations must not repair or mutate.
pub trait CorruptionProbe {
    fn evidence(&self) -> Result<CorruptionEvidence, RepairFault>;
}

/// Narrow capability presented by the daemon's fenced maintenance owner.
///
/// The coordinator depends only on the canonical runtime binding so changes to
/// the concrete maintenance permit remain localized to this implementation.
pub trait MaintenanceAuthorization {
    fn binding(&self) -> &StoreRuntimeBindingV1;
}

impl MaintenanceAuthorization for ExclusiveMaintenancePermit {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        self.binding()
    }
}

/// Store-specific repair capabilities consumed by [`RepairCoordinator`].
///
/// No database-open, arbitrary SQL, or file-deletion capability is present.
/// `rebuild_derived_fts` must commit the rebuild and supplied receipt atomically.
pub trait RepairDriver {
    fn lookup_repair_receipt(
        &self,
        diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<Option<RepairReceipt>, RepairFault>;

    fn lookup_quarantine_receipt(
        &self,
        diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<Option<QuarantineReceipt>, RepairFault>;

    fn rebuild_derived_fts(
        &mut self,
        authorization: &dyn MaintenanceAuthorization,
        diagnosis: &CorruptionDiagnosis,
        receipt: &RepairReceipt,
    ) -> Result<(), RepairFault>;

    /// Atomically preserve suspect authoritative state and its idempotent
    /// receipt, then return opaque evidence for operator escalation.
    /// Implementations must not delete the source files.
    fn quarantine_authoritative(
        &mut self,
        authorization: &dyn MaintenanceAuthorization,
        diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<QuarantineReceipt, RepairFault>;
}

#[derive(Clone, Debug)]
pub struct RepairCoordinator {
    receipt_id: StoreOperationIdV1,
}

impl RepairCoordinator {
    pub fn new(receipt_id: StoreOperationIdV1) -> Self {
        Self { receipt_id }
    }

    /// Classify evidence without acquiring maintenance or invoking a repair driver.
    pub fn diagnose<P: CorruptionProbe + ?Sized>(
        &self,
        probe: &P,
    ) -> Result<CorruptionDiagnosis, RepairFault> {
        probe.evidence().map(CorruptionEvidence::classify)
    }

    pub fn coordinate<P: CorruptionProbe + ?Sized, D: RepairDriver>(
        &self,
        probe: &P,
        driver: &mut D,
        authorization: Option<&dyn MaintenanceAuthorization>,
    ) -> RepairOutcome {
        let diagnosis = match self.diagnose(probe) {
            Ok(diagnosis) => diagnosis,
            Err(fault) => {
                return RepairOutcome::Faulted {
                    diagnosis: None,
                    stage: FaultStage::Diagnosis,
                    fault,
                };
            }
        };

        match diagnosis.class {
            CorruptionClass::Healthy => rejected(diagnosis, RejectionReason::Healthy, None),
            CorruptionClass::Indeterminate => rejected(
                diagnosis,
                RejectionReason::IndeterminateEscalationRequired,
                None,
            ),
            CorruptionClass::Authoritative => {
                self.quarantine_authoritative(driver, authorization, diagnosis)
            }
            CorruptionClass::DerivedFtsOnly => {
                self.rebuild_derived_fts(driver, authorization, diagnosis)
            }
        }
    }

    fn quarantine_authoritative<D: RepairDriver>(
        &self,
        driver: &mut D,
        authorization: Option<&dyn MaintenanceAuthorization>,
        diagnosis: CorruptionDiagnosis,
    ) -> RepairOutcome {
        let existing = match driver.lookup_quarantine_receipt(&diagnosis, &self.receipt_id) {
            Ok(existing) => existing,
            Err(fault) => return faulted(diagnosis, FaultStage::ReceiptLookup, fault),
        };
        if let Some(receipt) = existing {
            return quarantine_outcome(diagnosis, &self.receipt_id, receipt);
        }
        let authorization = match authorize(authorization, &diagnosis) {
            Ok(authorization) => authorization,
            Err(fault) => {
                return faulted(diagnosis, FaultStage::MaintenanceAuthorization, fault);
            }
        };
        match driver.quarantine_authoritative(authorization, &diagnosis, &self.receipt_id) {
            Ok(receipt) => quarantine_outcome(diagnosis, &self.receipt_id, receipt),
            Err(fault) => faulted(diagnosis, FaultStage::Quarantine, fault),
        }
    }

    fn rebuild_derived_fts<D: RepairDriver>(
        &self,
        driver: &mut D,
        authorization: Option<&dyn MaintenanceAuthorization>,
        diagnosis: CorruptionDiagnosis,
    ) -> RepairOutcome {
        let existing = match driver.lookup_repair_receipt(&diagnosis, &self.receipt_id) {
            Ok(existing) => existing,
            Err(fault) => return faulted(diagnosis, FaultStage::ReceiptLookup, fault),
        };
        if let Some(receipt) = existing {
            return if repair_receipt_matches(&receipt, &diagnosis, &self.receipt_id) {
                RepairOutcome::Completed {
                    receipt,
                    replayed: true,
                }
            } else {
                faulted(
                    diagnosis,
                    FaultStage::ReceiptLookup,
                    RepairFault::new(
                        "receipt_binding_mismatch",
                        "stored repair receipt belongs to different evidence",
                    ),
                )
            };
        }
        let authorization = match authorize(authorization, &diagnosis) {
            Ok(authorization) => authorization,
            Err(fault) => {
                return faulted(diagnosis, FaultStage::MaintenanceAuthorization, fault);
            }
        };
        let receipt = RepairReceipt {
            receipt_id: self.receipt_id.clone(),
            evidence_id: diagnosis.evidence_id.clone(),
            binding: diagnosis.binding.clone(),
        };
        match driver.rebuild_derived_fts(authorization, &diagnosis, &receipt) {
            Ok(()) => RepairOutcome::Completed {
                receipt,
                replayed: false,
            },
            Err(fault) => faulted(diagnosis, FaultStage::Rebuild, fault),
        }
    }
}

fn authorize<'a>(
    authorization: Option<&'a dyn MaintenanceAuthorization>,
    diagnosis: &CorruptionDiagnosis,
) -> Result<&'a dyn MaintenanceAuthorization, RepairFault> {
    let Some(authorization) = authorization else {
        return Err(RepairFault::new(
            "maintenance_authorization_required",
            "repair mutation requires exclusive maintenance authorization",
        ));
    };
    if authorization.binding() != &diagnosis.binding {
        return Err(RepairFault::new(
            "maintenance_authorization_binding_mismatch",
            "maintenance authorization belongs to a different store publication",
        ));
    }
    Ok(authorization)
}

fn repair_receipt_matches(
    receipt: &RepairReceipt,
    diagnosis: &CorruptionDiagnosis,
    receipt_id: &StoreOperationIdV1,
) -> bool {
    &receipt.receipt_id == receipt_id
        && receipt.evidence_id == diagnosis.evidence_id
        && receipt.binding == diagnosis.binding
}

fn quarantine_outcome(
    diagnosis: CorruptionDiagnosis,
    receipt_id: &StoreOperationIdV1,
    receipt: QuarantineReceipt,
) -> RepairOutcome {
    if &receipt.receipt_id == receipt_id
        && receipt.evidence_id == diagnosis.evidence_id
        && receipt.binding == diagnosis.binding
        && !receipt.evidence_reference.is_empty()
    {
        rejected(
            diagnosis,
            RejectionReason::AuthoritativeQuarantined,
            Some(receipt),
        )
    } else {
        faulted(
            diagnosis,
            FaultStage::Quarantine,
            RepairFault::new(
                "quarantine_receipt_mismatch",
                "quarantine receipt did not bind to the diagnosed store evidence",
            ),
        )
    }
}

fn rejected(
    diagnosis: CorruptionDiagnosis,
    reason: RejectionReason,
    quarantine: Option<QuarantineReceipt>,
) -> RepairOutcome {
    RepairOutcome::Rejected {
        diagnosis,
        reason,
        quarantine,
    }
}

fn faulted(diagnosis: CorruptionDiagnosis, stage: FaultStage, fault: RepairFault) -> RepairOutcome {
    RepairOutcome::Faulted {
        diagnosis: Some(diagnosis),
        stage,
        fault,
    }
}
