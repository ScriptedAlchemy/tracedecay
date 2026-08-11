use tracedecay_domain::{Confidence, DomainError, FactOwnerV1};

use super::super::super::{FactStoreError, FactStoreResult};
use super::super::ProjectMemoryFactIdV1;
use super::MAX_PROJECT_MEMORY_CURATION_TARGETS;

pub(super) fn validate_curation_confidence(
    confidence: Confidence,
    min_confidence: Confidence,
) -> FactStoreResult<()> {
    if confidence.as_f64() < min_confidence.as_f64() {
        return Err(FactStoreError::Contract(DomainError::NonCanonical {
            field: "curation confidence",
        }));
    }
    Ok(())
}

pub(super) fn validate_curation_fact_target(
    owner: &FactOwnerV1,
    target: &ProjectMemoryFactIdV1,
) -> FactStoreResult<()> {
    if target.owner() != owner {
        return Err(FactStoreError::OwnerMismatch);
    }
    Ok(())
}

pub(super) fn validate_curation_evidence(
    owner: &FactOwnerV1,
    evidence_facts: &[ProjectMemoryFactIdV1],
) -> FactStoreResult<()> {
    if evidence_facts.is_empty() || evidence_facts.len() > MAX_PROJECT_MEMORY_CURATION_TARGETS {
        return Err(FactStoreError::InvalidQueryLimit {
            limit: evidence_facts.len(),
            max: MAX_PROJECT_MEMORY_CURATION_TARGETS,
        });
    }
    for (index, evidence) in evidence_facts.iter().enumerate() {
        validate_curation_fact_target(owner, evidence)?;
        if evidence_facts[..index]
            .iter()
            .any(|previous| previous == evidence)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation evidence",
            }));
        }
    }
    Ok(())
}
