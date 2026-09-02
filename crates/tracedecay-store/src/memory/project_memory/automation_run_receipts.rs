use tracedecay_domain::{DomainError, FactOwnerV1, RunId};

use super::{
    MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS, ProjectMemoryAutomaticFactApplyDispositionV1,
    ProjectMemoryAutomaticFactApplyResultV1, ProjectMemoryAutomaticFactReceiptV1,
    ProjectMemoryAutomaticFactStateV1, ProjectMemoryFactCurationReceiptV1,
};
use crate::memory::{FactStoreError, FactStoreResult};

/// Canonical receipt material already committed by one memory-automation run.
///
/// This is a read projection over the immutable curation and automatic-fact
/// receipts. An empty value therefore proves that the exact owner and run have
/// no committed memory effect in the queried authority; it is not a fallback
/// synthesized by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryAutomationRunReceiptsV1 {
    owner: FactOwnerV1,
    run_id: RunId,
    curation_receipt: Option<ProjectMemoryFactCurationReceiptV1>,
    automatic_fact_receipts: Vec<ProjectMemoryAutomaticFactReceiptV1>,
}

impl ProjectMemoryAutomationRunReceiptsV1 {
    pub fn new(
        owner: FactOwnerV1,
        run_id: RunId,
        curation_receipt: Option<ProjectMemoryFactCurationReceiptV1>,
        automatic_fact_receipts: Vec<ProjectMemoryAutomaticFactReceiptV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        run_id.validate()?;
        if curation_receipt.as_ref().is_some_and(|receipt| {
            receipt.owner() != &owner || receipt.automation_run_id() != Some(&run_id)
        }) {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "memory automation curation receipt identity",
            }));
        }
        if automatic_fact_receipts.len() > MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS {
            return Err(FactStoreError::BatchLimitExceeded {
                field: "memory automation automatic fact receipts",
                count: automatic_fact_receipts.len(),
                max: MAX_PROJECT_MEMORY_AUTOMATIC_FACT_RECEIPTS,
            });
        }
        let mut previous: Option<&ProjectMemoryAutomaticFactReceiptV1> = None;
        for receipt in &automatic_fact_receipts {
            if receipt.owner() != &owner || receipt.automation_run_id() != Some(run_id.as_str()) {
                return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                    field: "memory automation automatic fact receipt identity",
                }));
            }
            if previous.is_some_and(|previous| {
                previous.recorded_at() > receipt.recorded_at()
                    || (previous.recorded_at() == receipt.recorded_at()
                        && previous.apply_id() >= receipt.apply_id())
            }) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "memory automation automatic fact receipt order",
                }));
            }
            previous = Some(receipt);
        }
        Ok(Self {
            owner,
            run_id,
            curation_receipt,
            automatic_fact_receipts,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn curation_receipt(&self) -> Option<&ProjectMemoryFactCurationReceiptV1> {
        self.curation_receipt.as_ref()
    }

    pub fn automatic_fact_receipts(&self) -> &[ProjectMemoryAutomaticFactReceiptV1] {
        &self.automatic_fact_receipts
    }

    /// Reconstitutes the canonical durable result for each committed receipt.
    /// Recovery does not relabel the persisted effect as an idempotency replay;
    /// `AlreadyApplied` describes a write-call outcome, not durable identity.
    pub fn automatic_fact_results(
        &self,
    ) -> FactStoreResult<Vec<ProjectMemoryAutomaticFactApplyResultV1>> {
        self.automatic_fact_receipts
            .iter()
            .cloned()
            .map(|receipt| {
                let disposition = match receipt.state() {
                    ProjectMemoryAutomaticFactStateV1::Applied => {
                        ProjectMemoryAutomaticFactApplyDispositionV1::Applied
                    }
                    ProjectMemoryAutomaticFactStateV1::Quarantined => {
                        ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined
                    }
                };
                ProjectMemoryAutomaticFactApplyResultV1::new(receipt, disposition)
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.curation_receipt.is_none() && self.automatic_fact_receipts.is_empty()
    }
}
