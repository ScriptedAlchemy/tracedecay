//! At-rest privacy remediation over persisted project-memory facts.
//!
//! Ingest sanitizes before persistence, but rows written under an older
//! detector revision (or under legacy paths that predate the hard cut) can
//! hold values the current detector would refuse. This owner re-runs the
//! current in-process detector over every currently served fact, redacts what
//! the detector can make safe, quarantines what it cannot, and settles every
//! mutation through the one canonical curation authority so the durable
//! curation receipt records exactly what changed. Nothing here executes a
//! scanner binary or touches the network, and no unsanitized payload is ever
//! persisted back.

use serde::Deserialize;
use serde_json::{Value, json};
use tracedecay_domain::{Confidence, FactCategoryV1, UtcMicros};
use tracedecay_runtime_core::privacy::{
    MEMORY_FACT_SANITIZER_VERSION_V1, MemoryFactSanitizationV1, sanitize_memory_fact_payload,
};
use tracedecay_store::{
    FactReadControl, FactWriteControl, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactListQueryV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactStore,
    ProjectMemoryFactUpdatePatchV1, ProjectMemoryFactV1,
};

use super::MemoryApplication;
use super::context::MemoryOperationContext;
use super::curation::{ProjectMemoryCurationMutationTarget, ProjectMemoryCurationOperation};
use super::error::{MemoryApplicationError, MemoryMutationError};

/// Why an at-rest rescan ran. Recorded on the receipt so operators can
/// distinguish daemon-adopted maintenance from an explicit request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyRemediationTriggerV1 {
    /// The daemon adopted the store under the current detector revision.
    DetectorRevisionAdoption,
    /// An operator or Doctor explicitly requested a rescan.
    OperatorRequest,
}

/// Truthful outcome of one at-rest rescan. `curation_receipt` is present
/// exactly when the rescan remediated at least one fact; the durable receipt
/// row is owned by the fact store's curation authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryPrivacyRemediationReceiptV1 {
    pub detector_revision: String,
    pub trigger: PrivacyRemediationTriggerV1,
    pub scanned_facts: u64,
    pub clean_facts: u64,
    pub redacted_facts: u64,
    pub quarantined_facts: u64,
    pub curation_receipt: Option<ProjectMemoryFactCurationReceiptV1>,
    pub started_at: UtcMicros,
    pub finished_at: UtcMicros,
}

/// One page of currently served facts per authority read.
const RESCAN_PAGE_LIMIT: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SanitizedFactPayloadWire {
    content: String,
    category: FactCategoryV1,
    tags: Vec<String>,
    entities: Vec<String>,
    metadata: Value,
    #[serde(default)]
    source_label: Option<String>,
}

enum FactRescanDispositionV1 {
    Clean,
    Redact(ProjectMemoryFactUpdatePatchV1),
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
        started_at: UtcMicros,
        finished_at: impl Fn() -> UtcMicros,
        read_control: &FactReadControl,
        write_control: &FactWriteControl,
    ) -> Result<ProjectMemoryPrivacyRemediationReceiptV1, MemoryApplicationError> {
        let mut scanned_facts = 0_u64;
        let mut clean_facts = 0_u64;
        let mut operations = Vec::new();
        let mut redacted_facts = 0_u64;
        let mut quarantined_facts = 0_u64;
        let mut after_fact_id = None;
        loop {
            let query = ProjectMemoryFactListQueryV1::new(
                self.owner.clone(),
                None,
                None,
                after_fact_id.take(),
                RESCAN_PAGE_LIMIT,
            )?;
            let page = self
                .list_project_memory_facts(query, read_control)
                .await?;
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
                    FactRescanDispositionV1::Redact(patch) => {
                        redacted_facts = redacted_facts.saturating_add(1);
                        operations.push(ProjectMemoryCurationOperation::Update {
                            target: target.clone(),
                            patch,
                            evidence_facts: vec![target],
                            confidence: remediation_confidence()?,
                            reason: "at-rest privacy rescan redacted detector findings".to_owned(),
                        });
                    }
                    FactRescanDispositionV1::Quarantine => {
                        quarantined_facts = quarantined_facts.saturating_add(1);
                        operations.push(ProjectMemoryCurationOperation::Remove {
                            target: target.clone(),
                            evidence_facts: vec![target],
                            confidence: remediation_confidence()?,
                            reason: "at-rest privacy rescan quarantined this fact".to_owned(),
                        });
                    }
                }
            }
            match page.next_after_fact_id() {
                Some(next) => after_fact_id = Some(next.clone()),
                None => break,
            }
        }
        let curation_receipt = if operations.is_empty() {
            None
        } else {
            let context = MemoryOperationContext::generated(
                &self.owner,
                "privacy_remediation_rescan",
                None,
            )?;
            let receipt = self
                .apply_project_memory_curation(
                    operations,
                    remediation_confidence()?,
                    context,
                    None,
                    write_control,
                )
                .await
                .map_err(|error| match error {
                    MemoryMutationError::Application(error) => error,
                    MemoryMutationError::InvalidAuthorityResult { error, .. } => error,
                })?;
            Some(receipt)
        };
        Ok(ProjectMemoryPrivacyRemediationReceiptV1 {
            detector_revision: MEMORY_FACT_SANITIZER_VERSION_V1.to_owned(),
            trigger,
            scanned_facts,
            clean_facts,
            redacted_facts,
            quarantined_facts,
            curation_receipt,
            started_at,
            finished_at: finished_at(),
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
fn rescan_fact(fact: &ProjectMemoryFactV1) -> Result<FactRescanDispositionV1, MemoryApplicationError> {
    let mut wire = json!({
        "content": fact.content(),
        "category": fact.category(),
        "tags": fact.tags(),
        "entities": fact.entities(),
        "metadata": fact.metadata(),
    });
    if let Some(source_label) = fact.source_label()
        && let Value::Object(object) = &mut wire
    {
        object.insert(
            "source_label".to_owned(),
            Value::String(source_label.to_owned()),
        );
    }
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
    let sanitized = serde_json::from_value::<SanitizedFactPayloadWire>(payload).map_err(|_| {
        MemoryApplicationError::InvalidInput {
            invariant: "at-rest privacy rescan sanitized payload",
        }
    })?;
    let patch = ProjectMemoryFactUpdatePatchV1::new(
        Some(sanitized.content),
        Some(sanitized.category),
        Some(sanitized.source_label),
        Some(sanitized.tags),
        Some(sanitized.entities),
        Some(sanitized.metadata),
        None,
    )?;
    Ok(FactRescanDispositionV1::Redact(patch))
}
