use tracedecay_domain::{Confidence, DomainError, FactOwnerV1};

use super::super::super::{FactLineageError, FactLineageResult};
use super::super::FactTarget;
use super::{FactEntityTarget, MAX_FACT_CURATION_TARGETS};

pub(super) fn validate_curation_confidence(
    confidence: Confidence,
    min_confidence: Confidence,
) -> FactLineageResult<()> {
    if confidence.as_f64() < min_confidence.as_f64() {
        return Err(FactLineageError::Contract(DomainError::NonCanonical {
            field: "compatibility curation confidence",
        }));
    }
    Ok(())
}

pub(super) fn validate_curation_fact_target(
    owner: &FactOwnerV1,
    target: &FactTarget,
) -> FactLineageResult<()> {
    if target.owner() != owner {
        return Err(FactLineageError::OwnerMismatch);
    }
    Ok(())
}

pub(super) fn validate_curation_entity_target(
    owner: &FactOwnerV1,
    target: &FactEntityTarget,
) -> FactLineageResult<()> {
    if target.owner() != owner {
        return Err(FactLineageError::OwnerMismatch);
    }
    Ok(())
}

pub(super) fn validate_curation_evidence(
    owner: &FactOwnerV1,
    evidence_facts: &[FactTarget],
) -> FactLineageResult<()> {
    if evidence_facts.is_empty() || evidence_facts.len() > MAX_FACT_CURATION_TARGETS {
        return Err(FactLineageError::InvalidQueryLimit {
            limit: evidence_facts.len(),
            max: MAX_FACT_CURATION_TARGETS,
        });
    }
    for (index, evidence) in evidence_facts.iter().enumerate() {
        validate_curation_fact_target(owner, evidence)?;
        if evidence_facts[..index]
            .iter()
            .any(|previous| previous == evidence)
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility curation evidence",
            }));
        }
    }
    Ok(())
}
