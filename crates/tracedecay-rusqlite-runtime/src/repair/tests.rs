use std::fmt::Debug;

use super::*;
use tracedecay_store::{
    BrainId, ProjectId, StoreAuthorityEpochV1, StoreIncarnationV1, StoreOperationIdV1,
    StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId,
};

struct Probe(Result<CorruptionEvidence, RepairFault>);

impl CorruptionProbe for Probe {
    fn evidence(&self) -> Result<CorruptionEvidence, RepairFault> {
        self.0.clone()
    }
}

struct Authorization(StoreRuntimeBindingV1);

impl MaintenanceAuthorization for Authorization {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.0
    }
}

#[derive(Default)]
struct Driver {
    repair: Option<RepairReceipt>,
    quarantine: Option<QuarantineReceipt>,
    rebuilt: usize,
    quarantined: usize,
    fail_rebuild: bool,
}

impl RepairDriver for Driver {
    fn lookup_repair_receipt(
        &self,
        _diagnosis: &CorruptionDiagnosis,
        _receipt_id: &StoreOperationIdV1,
    ) -> Result<Option<RepairReceipt>, RepairFault> {
        Ok(self.repair.clone())
    }

    fn lookup_quarantine_receipt(
        &self,
        _diagnosis: &CorruptionDiagnosis,
        _receipt_id: &StoreOperationIdV1,
    ) -> Result<Option<QuarantineReceipt>, RepairFault> {
        Ok(self.quarantine.clone())
    }

    fn rebuild_derived_fts(
        &mut self,
        _authorization: &dyn MaintenanceAuthorization,
        _diagnosis: &CorruptionDiagnosis,
        receipt: &RepairReceipt,
    ) -> Result<(), RepairFault> {
        self.rebuilt += 1;
        if self.fail_rebuild {
            Err(RepairFault::new("rebuild_failed", "injected"))
        } else {
            self.repair = Some(receipt.clone());
            Ok(())
        }
    }

    fn quarantine_authoritative(
        &mut self,
        _authorization: &dyn MaintenanceAuthorization,
        diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<QuarantineReceipt, RepairFault> {
        self.quarantined += 1;
        let receipt = QuarantineReceipt {
            receipt_id: receipt_id.clone(),
            evidence_id: diagnosis.evidence_id.clone(),
            binding: diagnosis.binding.clone(),
            evidence_reference: "quarantine://fixture".to_owned(),
        };
        self.quarantine = Some(receipt.clone());
        Ok(receipt)
    }
}

fn evidence(class: CorruptionClass) -> CorruptionEvidence {
    let observations = match class {
        CorruptionClass::DerivedFtsOnly => vec![CorruptionObservation::DerivedFts],
        CorruptionClass::Authoritative => vec![CorruptionObservation::Authoritative],
        CorruptionClass::Indeterminate => vec![CorruptionObservation::Unclassified],
        CorruptionClass::Healthy => vec![],
    };
    CorruptionEvidence {
        binding: store_binding(),
        evidence_id: operation_id("evidence.fixture"),
        observations,
    }
}

fn coordinator() -> RepairCoordinator {
    RepairCoordinator::new(operation_id("receipt.fixture"))
}

fn authorization() -> Authorization {
    Authorization(store_binding())
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn operation_id(value: &str) -> StoreOperationIdV1 {
    id(value)
}

fn store_binding() -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            id::<BrainId>("brain.repair"),
            id::<UserProfileId>("profile.repair"),
            id::<ProjectId>("project.repair"),
        ),
        StoreIncarnationV1::new(7).unwrap(),
        StoreAuthorityEpochV1::new(19).unwrap(),
    )
}

#[test]
fn diagnosis_is_read_only_and_coordinator_owned() {
    let diagnosis = coordinator()
        .diagnose(&Probe(Ok(evidence(CorruptionClass::DerivedFtsOnly))))
        .unwrap();

    assert_eq!(diagnosis.class, CorruptionClass::DerivedFtsOnly);
    assert_eq!(diagnosis.binding, store_binding());
    assert_eq!(diagnosis.evidence_id, operation_id("evidence.fixture"));
}

#[test]
fn fts_only_corruption_rebuilds_with_an_atomic_receipt() {
    let mut driver = Driver::default();
    let outcome = coordinator().coordinate(
        &Probe(Ok(evidence(CorruptionClass::DerivedFtsOnly))),
        &mut driver,
        Some(&authorization()),
    );

    let receipt = match outcome {
        RepairOutcome::Completed {
            receipt,
            replayed: false,
        } => receipt,
        other => panic!("unexpected outcome: {other:?}"),
    };
    assert_eq!(receipt.receipt_id, operation_id("receipt.fixture"));
    assert_eq!(receipt.evidence_id, operation_id("evidence.fixture"));
    assert_eq!(driver.repair, Some(receipt));
    assert_eq!(driver.rebuilt, 1);
}

#[test]
fn authoritative_corruption_is_quarantined_and_never_rebuilt() {
    let mut driver = Driver::default();
    let outcome = coordinator().coordinate(
        &Probe(Ok(CorruptionEvidence {
            binding: store_binding(),
            evidence_id: operation_id("evidence.fixture"),
            observations: vec![
                CorruptionObservation::DerivedFts,
                CorruptionObservation::Authoritative,
            ],
        })),
        &mut driver,
        Some(&authorization()),
    );

    assert!(matches!(
        outcome,
        RepairOutcome::Rejected {
            reason: RejectionReason::AuthoritativeQuarantined,
            quarantine: Some(_),
            ..
        }
    ));
    assert_eq!((driver.quarantined, driver.rebuilt), (1, 0));
}

#[test]
fn committed_receipt_replays_without_maintenance_authorization() {
    let probe = Probe(Ok(evidence(CorruptionClass::DerivedFtsOnly)));
    let mut driver = Driver::default();
    let first = coordinator().coordinate(&probe, &mut driver, Some(&authorization()));
    let first_receipt = match first {
        RepairOutcome::Completed { receipt, .. } => receipt,
        other => panic!("unexpected first outcome: {other:?}"),
    };

    assert_eq!(
        coordinator().coordinate(&probe, &mut driver, None),
        RepairOutcome::Completed {
            receipt: first_receipt,
            replayed: true,
        }
    );
    assert_eq!(driver.rebuilt, 1);
}

#[test]
fn mutation_requires_authorization_for_the_diagnosed_binding() {
    let mut driver = Driver::default();
    let missing = coordinator().coordinate(
        &Probe(Ok(evidence(CorruptionClass::DerivedFtsOnly))),
        &mut driver,
        None,
    );
    let wrong = Authorization(StoreRuntimeBindingV1::new(
        store_binding().shard_id,
        StoreIncarnationV1::new(8).unwrap(),
        StoreAuthorityEpochV1::new(20).unwrap(),
    ));
    let mismatched = coordinator().coordinate(
        &Probe(Ok(evidence(CorruptionClass::DerivedFtsOnly))),
        &mut driver,
        Some(&wrong),
    );

    assert!(matches!(
        missing,
        RepairOutcome::Faulted {
            stage: FaultStage::MaintenanceAuthorization,
            ..
        }
    ));
    assert!(matches!(
        mismatched,
        RepairOutcome::Faulted {
            stage: FaultStage::MaintenanceAuthorization,
            ..
        }
    ));
    assert_eq!(driver.rebuilt, 0);
}

#[test]
fn indeterminate_corruption_escalates_without_mutation() {
    let mut driver = Driver::default();
    let outcome = coordinator().coordinate(
        &Probe(Ok(evidence(CorruptionClass::Indeterminate))),
        &mut driver,
        Some(&authorization()),
    );

    assert!(matches!(
        outcome,
        RepairOutcome::Rejected {
            reason: RejectionReason::IndeterminateEscalationRequired,
            quarantine: None,
            ..
        }
    ));
    assert_eq!((driver.rebuilt, driver.quarantined), (0, 0));
}

#[test]
fn diagnosis_failure_has_no_repair_side_effects() {
    let mut driver = Driver::default();
    let outcome = coordinator().coordinate(
        &Probe(Err(RepairFault::new("read_failed", "injected"))),
        &mut driver,
        None,
    );

    assert!(matches!(
        outcome,
        RepairOutcome::Faulted {
            stage: FaultStage::Diagnosis,
            diagnosis: None,
            ..
        }
    ));
    assert_eq!((driver.rebuilt, driver.quarantined), (0, 0));
}

#[test]
fn rebuild_failure_does_not_publish_a_receipt() {
    let mut driver = Driver {
        fail_rebuild: true,
        ..Driver::default()
    };
    let outcome = coordinator().coordinate(
        &Probe(Ok(evidence(CorruptionClass::DerivedFtsOnly))),
        &mut driver,
        Some(&authorization()),
    );

    assert!(matches!(
        outcome,
        RepairOutcome::Faulted {
            stage: FaultStage::Rebuild,
            ..
        }
    ));
    assert!(driver.repair.is_none());
}
