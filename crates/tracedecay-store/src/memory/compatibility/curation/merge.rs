use tracedecay_domain::{ActorId, DomainError, FactOwnerV1, ProvenanceId};

use super::super::super::{FactLineageError, FactLineageResult};
use super::super::{FactMapping, FactTarget, validate_text};
use super::MAX_FACT_CURATION_TARGETS;
use super::validate::validate_curation_fact_target;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactMergeCommand {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    winner: FactTarget,
    losers: Vec<FactTarget>,
    merged_content: Option<String>,
    actor: Option<ActorId>,
}

impl FactMergeCommand {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        winner: FactTarget,
        losers: Vec<FactTarget>,
        merged_content: Option<String>,
        actor: Option<ActorId>,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        validate_curation_fact_target(&owner, &winner)?;
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        if let Some(content) = &merged_content {
            validate_text(content, "compatibility merge content")?;
        }
        if losers.is_empty() || losers.len() > MAX_FACT_CURATION_TARGETS {
            return Err(FactLineageError::InvalidQueryLimit {
                limit: losers.len(),
                max: MAX_FACT_CURATION_TARGETS,
            });
        }
        for (index, loser) in losers.iter().enumerate() {
            validate_curation_fact_target(&owner, loser)?;
            if loser == &winner || losers[..index].iter().any(|previous| previous == loser) {
                return Err(FactLineageError::Contract(DomainError::NonCanonical {
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

    pub fn winner(&self) -> &FactTarget {
        &self.winner
    }

    pub fn losers(&self) -> &[FactTarget] {
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
pub struct FactMergeOutcome {
    owner: FactOwnerV1,
    winner: FactMapping,
    content_updated: bool,
    deleted_losers: Vec<FactMapping>,
}

impl FactMergeOutcome {
    pub fn new(
        owner: FactOwnerV1,
        winner: FactMapping,
        content_updated: bool,
        deleted_losers: Vec<FactMapping>,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        if winner.owner() != &owner
            || deleted_losers.len() > MAX_FACT_CURATION_TARGETS
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
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
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

    pub fn winner(&self) -> &FactMapping {
        &self.winner
    }

    pub fn content_updated(&self) -> bool {
        self.content_updated
    }

    pub fn deleted_losers(&self) -> &[FactMapping] {
        &self.deleted_losers
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryRepairCommand {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
}

impl MemoryRepairCommand {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        actor: Option<ActorId>,
    ) -> FactLineageResult<Self> {
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
