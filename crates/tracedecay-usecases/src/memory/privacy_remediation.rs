//! At-rest privacy remediation over persisted project-memory facts.
//!
//! Ingest sanitizes before persistence, but rows written under an older
//! detector revision (or under legacy paths that predate the hard cut) can
//! hold values the current detector would refuse. This owner re-runs the
//! current in-process detector over every currently served fact, quarantines
//! every detector hit, and settles every mutation through the one canonical
//! curation authority so durable curation receipts record exactly what
//! changed. Quarantine is intentionally terminal: updating only the current
//! projection would leave superseded assertion payloads at rest. Nothing here
//! executes a scanner binary or touches the network.

use tracedecay_domain::Confidence;
use tracedecay_runtime_core::privacy::{
    MEMORY_FACT_SANITIZER_VERSION_V1, MemoryFactSanitizationV1, sanitize_memory_fact_payload,
};
use tracedecay_store::{
    FactReadControl, FactWriteControl, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactListQueryV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactStore,
    ProjectMemoryFactV1,
};

use super::MemoryApplication;
use super::context::MemoryOperationContext;
use super::curation::{ProjectMemoryCurationMutationTarget, ProjectMemoryCurationOperation};
use super::error::{MemoryApplicationError, MemoryMutationError};
use super::sanitize::{fact_payload_wire, sanitize_optional_memory_text};

/// Why an at-rest rescan ran. Recorded on the receipt so operators can see
/// which journey produced it; daemon store adoption is currently the only
/// production trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyRemediationTriggerV1 {
    /// The daemon adopted the store under the current detector revision.
    DetectorRevisionAdoption,
}

/// Truthful outcome of one at-rest rescan. One durable curation receipt is
/// returned for each bounded page that remediated at least one fact; receipt
/// rows are owned by the fact store's curation authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryPrivacyRemediationReceiptV1 {
    pub detector_revision: String,
    pub trigger: PrivacyRemediationTriggerV1,
    pub scanned_facts: u64,
    pub clean_facts: u64,
    pub quarantined_facts: u64,
    pub curation_receipts: Vec<ProjectMemoryFactCurationReceiptV1>,
}

/// One page of currently served facts per authority read.
const RESCAN_PAGE_LIMIT: usize = 64;

enum FactRescanDispositionV1 {
    Clean,
    Quarantine,
}

impl<A: ProjectMemoryFactStore> MemoryApplication<A> {
    /// Rescans every currently served fact under the current detector
    /// revision, remediating hits through the canonical curation authority.
    ///
    /// The rescan fails closed: a fact whose payload cannot be re-evaluated
    /// aborts the run with a typed error instead of skipping it silently.
    pub async fn privacy_remediation_rescan(
        &self,
        trigger: PrivacyRemediationTriggerV1,
        read_control: &FactReadControl,
        write_control: &FactWriteControl,
    ) -> Result<ProjectMemoryPrivacyRemediationReceiptV1, MemoryApplicationError> {
        let confidence = remediation_confidence()?;
        let mut scanned_facts = 0_u64;
        let mut clean_facts = 0_u64;
        let mut quarantined_facts = 0_u64;
        let mut curation_receipts = Vec::new();
        let mut after_fact_id = None;
        loop {
            let query = ProjectMemoryFactListQueryV1::new(
                self.owner.clone(),
                None,
                None,
                after_fact_id.take(),
                RESCAN_PAGE_LIMIT,
            )?;
            let page = self.list_project_memory_facts(query, read_control).await?;
            let mut operations = Vec::new();
            for projection in page.facts() {
                let ProjectMemoryFactProjectionV1::Available(fact) = projection else {
                    // A withheld projection serves no payload, so there is
                    // nothing at rest for this pass to disclose or rewrite.
                    continue;
                };
                scanned_facts = scanned_facts.saturating_add(1);
                let target = ProjectMemoryCurationMutationTarget::new(
                    fact.fact_id().clone(),
                    fact.last_event_id().clone(),
                );
                match rescan_fact(fact)? {
                    FactRescanDispositionV1::Clean => {
                        clean_facts = clean_facts.saturating_add(1);
                    }
                    FactRescanDispositionV1::Quarantine => {
                        quarantined_facts = quarantined_facts.saturating_add(1);
                        operations.push(ProjectMemoryCurationOperation::Remove {
                            target: target.clone(),
                            evidence_facts: vec![target],
                            confidence,
                            reason: "at-rest privacy rescan quarantined this fact".to_owned(),
                        });
                    }
                }
            }
            if !operations.is_empty() {
                let context = MemoryOperationContext::generated(
                    &self.owner,
                    "privacy_remediation_rescan",
                    None,
                )?;
                let receipt = self
                    .apply_project_memory_curation(
                        operations,
                        confidence,
                        context,
                        None,
                        write_control,
                    )
                    .await
                    .map_err(|error| match error {
                        MemoryMutationError::Application(error) => error,
                        MemoryMutationError::InvalidAuthorityResult { error, .. } => error,
                    })?;
                curation_receipts.push(receipt);
            }
            match page.next_after_fact_id() {
                Some(next) => after_fact_id = Some(next.clone()),
                None => break,
            }
        }
        Ok(ProjectMemoryPrivacyRemediationReceiptV1 {
            detector_revision: MEMORY_FACT_SANITIZER_VERSION_V1.to_owned(),
            trigger,
            scanned_facts,
            clean_facts,
            quarantined_facts,
            curation_receipts,
        })
    }
}

fn remediation_confidence() -> Result<Confidence, MemoryApplicationError> {
    Confidence::new(1.0).map_err(|_| MemoryApplicationError::InvalidInput {
        invariant: "privacy remediation confidence",
    })
}

/// Re-evaluates one served fact's canonical payload wire under the current
/// detector. The wire mirrors the ingest sanitizer exactly, so an unchanged
/// durable answer proves the persisted row already satisfies the revision.
fn rescan_fact(
    fact: &ProjectMemoryFactV1,
) -> Result<FactRescanDispositionV1, MemoryApplicationError> {
    let Some(source_label) = sanitize_optional_memory_text(fact.source_label().map(str::to_owned))
    else {
        return Ok(FactRescanDispositionV1::Quarantine);
    };
    if source_label.as_deref() != fact.source_label() {
        return Ok(FactRescanDispositionV1::Quarantine);
    }
    let wire = fact_payload_wire(
        fact.content(),
        fact.category(),
        fact.tags(),
        fact.entities(),
        fact.metadata(),
        source_label.as_deref(),
    );
    let sanitized = sanitize_memory_fact_payload(wire.clone()).map_err(|_| {
        MemoryApplicationError::InvalidInput {
            invariant: "at-rest privacy rescan detector evaluation",
        }
    })?;
    let MemoryFactSanitizationV1::Durable { payload, .. } = sanitized else {
        return Ok(FactRescanDispositionV1::Quarantine);
    };
    if payload == wire {
        return Ok(FactRescanDispositionV1::Clean);
    }
    Ok(FactRescanDispositionV1::Quarantine)
}
