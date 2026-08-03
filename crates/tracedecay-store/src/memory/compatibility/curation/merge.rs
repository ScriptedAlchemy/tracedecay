use tracedecay_domain::{ActorId, DomainError, FactOwnerV1, ProvenanceId};

use super::super::super::{FactStoreError, FactStoreResult};
use super::super::{
    CompatibilityFactMappingV1, CompatibilityFactTargetV1, validate_compatibility_text,
};
use super::MAX_COMPATIBILITY_CURATION_TARGETS;
use super::validate::validate_curation_fact_target;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactMergeCommandV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    winner: CompatibilityFactTargetV1,
    losers: Vec<CompatibilityFactTargetV1>,
    merged_content: Option<String>,
    actor: Option<ActorId>,
}

impl CompatibilityFactMergeCommandV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        winner: CompatibilityFactTargetV1,
        losers: Vec<CompatibilityFactTargetV1>,
        merged_content: Option<String>,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        validate_curation_fact_target(&owner, &winner)?;
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        if let Some(content) = &merged_content {
            validate_compatibility_text(content, "compatibility merge content")?;
        }
        if losers.is_empty() || losers.len() > MAX_COMPATIBILITY_CURATION_TARGETS {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: losers.len(),
                max: MAX_COMPATIBILITY_CURATION_TARGETS,
            });
        }
        for (index, loser) in losers.iter().enumerate() {
            validate_curation_fact_target(&owner, loser)?;
            if loser == &winner || losers[..index].iter().any(|previous| previous == loser) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility merge targets",
                }));
            }
        }
        Ok(Self {
            owner,
            operation_id,
            winner,
            losers,
            merged_content,
            actor,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn winner(&self) -> &CompatibilityFactTargetV1 {
        &self.winner
    }

    pub fn losers(&self) -> &[CompatibilityFactTargetV1] {
        &self.losers
    }

    pub fn merged_content(&self) -> Option<&str> {
        self.merged_content.as_deref()
    }

    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactMergeOutcomeV1 {
    owner: FactOwnerV1,
    winner: CompatibilityFactMappingV1,
    content_updated: bool,
    deleted_losers: Vec<CompatibilityFactMappingV1>,
}

impl CompatibilityFactMergeOutcomeV1 {
    pub fn new(
        owner: FactOwnerV1,
        winner: CompatibilityFactMappingV1,
        content_updated: bool,
        deleted_losers: Vec<CompatibilityFactMappingV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if winner.owner() != &owner
            || deleted_losers.len() > MAX_COMPATIBILITY_CURATION_TARGETS
            || deleted_losers
                .iter()
                .any(|mapping| mapping.owner() != &owner)
            || deleted_losers
                .iter()
                .any(|mapping| mapping.fact_id() == winner.fact_id())
            || deleted_losers.iter().enumerate().any(|(index, mapping)| {
                deleted_losers[..index]
                    .iter()
                    .any(|previous| previous.fact_id() == mapping.fact_id())
            })
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility merge outcome mappings",
            }));
        }
        Ok(Self {
            owner,
            winner,
            content_updated,
            deleted_losers,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn winner(&self) -> &CompatibilityFactMappingV1 {
        &self.winner
    }

    pub fn content_updated(&self) -> bool {
        self.content_updated
    }

    pub fn deleted_losers(&self) -> &[CompatibilityFactMappingV1] {
        &self.deleted_losers
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityMemoryRepairCommandV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
}

impl CompatibilityMemoryRepairCommandV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            owner,
            operation_id,
            actor,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}
