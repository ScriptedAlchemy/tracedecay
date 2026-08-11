use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracedecay_domain::{
    ActorId, DomainError, FactId, FactOwnerV1, ProvenanceId, canonical_sha256,
};

use super::super::super::{FactCommitReceipt, FactStoreError, FactStoreResult};
use super::super::{ProjectMemoryFactIdV1, validate_project_memory_text};
use super::MAX_PROJECT_MEMORY_CURATION_TARGETS;
use super::validate::validate_curation_fact_target;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactMergeCommandV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    winner: ProjectMemoryFactIdV1,
    losers: Vec<ProjectMemoryFactIdV1>,
    merged_content: Option<String>,
    actor: Option<ActorId>,
}

impl ProjectMemoryFactMergeCommandV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        winner: ProjectMemoryFactIdV1,
        losers: Vec<ProjectMemoryFactIdV1>,
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
            validate_project_memory_text(content, "merge content")?;
        }
        if losers.is_empty() || losers.len() > MAX_PROJECT_MEMORY_CURATION_TARGETS {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: losers.len(),
                max: MAX_PROJECT_MEMORY_CURATION_TARGETS,
            });
        }
        for (index, loser) in losers.iter().enumerate() {
            validate_curation_fact_target(&owner, loser)?;
            if loser == &winner || losers[..index].iter().any(|previous| previous == loser) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "merge targets",
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

    pub fn winner(&self) -> &ProjectMemoryFactIdV1 {
        &self.winner
    }

    pub fn losers(&self) -> &[ProjectMemoryFactIdV1] {
        &self.losers
    }

    pub fn merged_content(&self) -> Option<&str> {
        self.merged_content.as_deref()
    }

    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }

    pub fn input_digest(&self) -> FactStoreResult<String> {
        let loser_fact_ids = self
            .losers
            .iter()
            .map(ProjectMemoryFactIdV1::fact_id)
            .collect::<Vec<_>>();
        let digest = canonical_sha256(&(
            "tracedecay.project-memory.fact-merge-input.v1",
            &self.owner,
            self.winner.fact_id(),
            loser_fact_ids,
            self.merged_content.as_deref(),
            self.actor.as_ref().map(ActorId::as_str),
        ))?;
        digest
            .as_str()
            .strip_prefix("sha256:")
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                FactStoreError::Contract(DomainError::NonCanonical {
                    field: "project memory fact merge input digest",
                })
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactMergeOutcomeV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    input_digest: String,
    winner: ProjectMemoryFactIdV1,
    content_updated: bool,
    deleted_losers: Vec<ProjectMemoryFactIdV1>,
    commit_receipts: Vec<FactCommitReceipt>,
    // Delivery disposition is excluded from the durable receipt identity.
    replayed: bool,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ProjectMemoryFactMergeOutcomeRef<'a> {
    owner: &'a FactOwnerV1,
    operation_id: &'a ProvenanceId,
    input_digest: &'a str,
    winner_fact_id: &'a FactId,
    content_updated: bool,
    deleted_loser_fact_ids: Vec<&'a FactId>,
    commit_receipts: &'a [FactCommitReceipt],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectMemoryFactMergeOutcomeWire {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    input_digest: String,
    winner_fact_id: FactId,
    content_updated: bool,
    deleted_loser_fact_ids: Vec<FactId>,
    commit_receipts: Vec<FactCommitReceipt>,
}

impl Serialize for ProjectMemoryFactMergeOutcomeV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ProjectMemoryFactMergeOutcomeRef {
            owner: self.owner(),
            operation_id: self.operation_id(),
            input_digest: self.input_digest(),
            winner_fact_id: self.winner().fact_id(),
            content_updated: self.content_updated(),
            deleted_loser_fact_ids: self
                .deleted_losers()
                .iter()
                .map(ProjectMemoryFactIdV1::fact_id)
                .collect(),
            commit_receipts: self.commit_receipts(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProjectMemoryFactMergeOutcomeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ProjectMemoryFactMergeOutcomeWire::deserialize(deserializer)?;
        let winner = ProjectMemoryFactIdV1::new(wire.owner.clone(), wire.winner_fact_id)
            .map_err(serde::de::Error::custom)?;
        let deleted_losers = wire
            .deleted_loser_fact_ids
            .into_iter()
            .map(|fact_id| ProjectMemoryFactIdV1::new(wire.owner.clone(), fact_id))
            .collect::<FactStoreResult<Vec<_>>>()
            .map_err(serde::de::Error::custom)?;
        Self::new(
            wire.owner,
            wire.operation_id,
            wire.input_digest,
            winner,
            wire.content_updated,
            deleted_losers,
            wire.commit_receipts,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl ProjectMemoryFactMergeOutcomeV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        input_digest: String,
        winner: ProjectMemoryFactIdV1,
        content_updated: bool,
        deleted_losers: Vec<ProjectMemoryFactIdV1>,
        commit_receipts: Vec<FactCommitReceipt>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if input_digest.len() != 64
            || !input_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "merge outcome input digest",
            }));
        }
        if winner.owner() != &owner
            || deleted_losers.is_empty()
            || deleted_losers.len() > MAX_PROJECT_MEMORY_CURATION_TARGETS
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
                field: "merge outcome fact identities",
            }));
        }
        let winner_commit_count = usize::from(content_updated);
        if commit_receipts.len() != deleted_losers.len() + winner_commit_count
            || commit_receipts.is_empty()
            || commit_receipts
                .iter()
                .any(|receipt| receipt.owner() != &owner)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "merge outcome commit receipts",
            }));
        }
        if content_updated {
            let winner_commit = &commit_receipts[0];
            if winner_commit.fact_id() != winner.fact_id()
                || winner_commit.committed_event_ids().len() != 2
                || winner_commit.active_assertion_id().is_none()
            {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "merge outcome winner commit receipt",
                }));
            }
        }
        for (loser, commit) in deleted_losers
            .iter()
            .zip(commit_receipts[winner_commit_count..].iter())
        {
            if commit.fact_id() != loser.fact_id()
                || commit.committed_event_ids().len() != 2
                || commit.active_assertion_id().is_some()
            {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "merge outcome loser commit receipts",
                }));
            }
        }
        let mut committed_events = BTreeSet::new();
        for receipt in &commit_receipts {
            for event_id in receipt.committed_event_ids() {
                if !committed_events.insert(event_id) {
                    return Err(FactStoreError::Contract(DomainError::DuplicateId {
                        field: "merge outcome committed events",
                    }));
                }
            }
        }
        Ok(Self {
            owner,
            operation_id,
            input_digest,
            winner,
            content_updated,
            deleted_losers,
            commit_receipts,
            replayed: false,
        })
    }

    pub fn into_replayed(mut self) -> Self {
        self.replayed = true;
        self
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn input_digest(&self) -> &str {
        &self.input_digest
    }

    pub fn winner(&self) -> &ProjectMemoryFactIdV1 {
        &self.winner
    }

    pub fn content_updated(&self) -> bool {
        self.content_updated
    }

    pub fn deleted_losers(&self) -> &[ProjectMemoryFactIdV1] {
        &self.deleted_losers
    }

    pub fn commit_receipts(&self) -> &[FactCommitReceipt] {
        &self.commit_receipts
    }

    pub fn replayed(&self) -> bool {
        self.replayed
    }
}
