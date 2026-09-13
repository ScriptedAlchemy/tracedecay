use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracedecay_domain::{DomainError, FactEventId, FactId, FactOwnerV1, ProvenanceId, RunId};

use super::super::super::{FactCommitReceipt, FactStoreError, FactStoreResult};
use super::super::ProjectMemoryFactIdV1;
use super::{
    MAX_PROJECT_MEMORY_CURATION_OPERATIONS, MAX_PROJECT_MEMORY_CURATION_TARGETS,
    ProjectMemoryFactAddDispositionV1, ProjectMemoryFactCurationLinkDispositionV1,
    ProjectMemoryFactCurationLinkEffectV1, ProjectMemoryFactCurationOperationEffectV1,
    ProjectMemoryFactCurationRemoveDispositionV1, ProjectMemoryFactMergeOutcomeV1,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactCurationReceiptV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    input_digest: String,
    automation_run_id: Option<RunId>,
    operation_effects: Vec<ProjectMemoryFactCurationOperationEffectV1>,
    replay_fact_id: Option<FactId>,
    replay_event_id: Option<FactEventId>,
    changed_facts: Vec<ProjectMemoryFactIdV1>,
    accepted_operations: u64,
    facts_added: u64,
    facts_updated: u64,
    facts_merged: u64,
    facts_removed: u64,
    normalized_tags: u64,
    facts_linked: u64,
    replayed: bool,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ProjectMemoryFactCurationReceiptRef<'a> {
    owner: &'a FactOwnerV1,
    operation_id: &'a ProvenanceId,
    input_digest: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    automation_run_id: Option<&'a RunId>,
    operation_effects: Vec<ProjectMemoryFactCurationOperationEffectRef<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replay_fact_id: Option<&'a FactId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replay_event_id: Option<&'a FactEventId>,
    changed_fact_ids: Vec<&'a FactId>,
    accepted_operations: u64,
    facts_added: u64,
    facts_updated: u64,
    facts_merged: u64,
    facts_removed: u64,
    normalized_tags: u64,
    facts_linked: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectMemoryFactCurationReceiptWire {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    input_digest: String,
    #[serde(default)]
    automation_run_id: Option<RunId>,
    operation_effects: Vec<ProjectMemoryFactCurationOperationEffectWire>,
    #[serde(default)]
    replay_fact_id: Option<FactId>,
    #[serde(default)]
    replay_event_id: Option<FactEventId>,
    changed_fact_ids: Vec<FactId>,
    accepted_operations: u64,
    facts_added: u64,
    facts_updated: u64,
    facts_merged: u64,
    facts_removed: u64,
    normalized_tags: u64,
    facts_linked: u64,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProjectMemoryFactCurationOperationEffectRef<'a> {
    Add {
        fact_id: &'a FactId,
        disposition: ProjectMemoryFactAddDispositionV1,
        closest_fact_id: Option<&'a FactId>,
        similarity_millionths: Option<u32>,
        commit: Option<&'a FactCommitReceipt>,
    },
    Update {
        fact_id: &'a FactId,
        trust_delta_millionths: i32,
        commit: &'a FactCommitReceipt,
    },
    Merge {
        outcome: &'a ProjectMemoryFactMergeOutcomeV1,
    },
    Remove {
        target_fact_id: &'a FactId,
        disposition: ProjectMemoryFactCurationRemoveDispositionV1,
        remaining_fact_count: u64,
        commit: Option<&'a FactCommitReceipt>,
    },
    NormalizeTags {
        fact_id: &'a FactId,
        commit: &'a FactCommitReceipt,
    },
    LinkFacts {
        relation: &'a ProjectMemoryFactCurationLinkEffectV1,
        disposition: ProjectMemoryFactCurationLinkDispositionV1,
        commit: Option<&'a FactCommitReceipt>,
    },
}

impl<'a> From<&'a ProjectMemoryFactCurationOperationEffectV1>
    for ProjectMemoryFactCurationOperationEffectRef<'a>
{
    fn from(effect: &'a ProjectMemoryFactCurationOperationEffectV1) -> Self {
        match effect {
            ProjectMemoryFactCurationOperationEffectV1::Add {
                fact,
                disposition,
                closest_fact,
                similarity_millionths,
                commit,
            } => Self::Add {
                fact_id: fact.fact_id(),
                disposition: *disposition,
                closest_fact_id: closest_fact.as_ref().map(ProjectMemoryFactIdV1::fact_id),
                similarity_millionths: *similarity_millionths,
                commit: commit.as_ref(),
            },
            ProjectMemoryFactCurationOperationEffectV1::Update {
                fact,
                trust_delta_millionths,
                commit,
            } => Self::Update {
                fact_id: fact.fact_id(),
                trust_delta_millionths: *trust_delta_millionths,
                commit,
            },
            ProjectMemoryFactCurationOperationEffectV1::Merge { outcome } => {
                Self::Merge { outcome }
            }
            ProjectMemoryFactCurationOperationEffectV1::Remove {
                target,
                disposition,
                remaining_fact_count,
                commit,
            } => Self::Remove {
                target_fact_id: target.fact_id(),
                disposition: *disposition,
                remaining_fact_count: *remaining_fact_count,
                commit: commit.as_ref(),
            },
            ProjectMemoryFactCurationOperationEffectV1::NormalizeTags { fact, commit } => {
                Self::NormalizeTags {
                    fact_id: fact.fact_id(),
                    commit,
                }
            }
            ProjectMemoryFactCurationOperationEffectV1::LinkFacts {
                relation,
                disposition,
                commit,
            } => Self::LinkFacts {
                relation,
                disposition: *disposition,
                commit: commit.as_ref(),
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProjectMemoryFactCurationOperationEffectWire {
    Add {
        fact_id: FactId,
        disposition: ProjectMemoryFactAddDispositionV1,
        closest_fact_id: Option<FactId>,
        similarity_millionths: Option<u32>,
        commit: Option<FactCommitReceipt>,
    },
    Update {
        fact_id: FactId,
        trust_delta_millionths: i32,
        commit: FactCommitReceipt,
    },
    Merge {
        outcome: ProjectMemoryFactMergeOutcomeV1,
    },
    Remove {
        target_fact_id: FactId,
        disposition: ProjectMemoryFactCurationRemoveDispositionV1,
        remaining_fact_count: u64,
        commit: Option<FactCommitReceipt>,
    },
    NormalizeTags {
        fact_id: FactId,
        commit: FactCommitReceipt,
    },
    LinkFacts {
        relation: ProjectMemoryFactCurationLinkEffectV1,
        disposition: ProjectMemoryFactCurationLinkDispositionV1,
        commit: Option<FactCommitReceipt>,
    },
}

impl Serialize for ProjectMemoryFactCurationReceiptV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ProjectMemoryFactCurationReceiptRef {
            owner: self.owner(),
            operation_id: self.operation_id(),
            input_digest: self.input_digest(),
            automation_run_id: self.automation_run_id(),
            operation_effects: self.operation_effects().iter().map(Into::into).collect(),
            replay_fact_id: self.replay_fact_id(),
            replay_event_id: self.replay_event_id(),
            changed_fact_ids: self
                .changed_facts()
                .iter()
                .map(ProjectMemoryFactIdV1::fact_id)
                .collect(),
            accepted_operations: self.accepted_operations(),
            facts_added: self.facts_added(),
            facts_updated: self.facts_updated(),
            facts_merged: self.facts_merged(),
            facts_removed: self.facts_removed(),
            normalized_tags: self.normalized_tags(),
            facts_linked: self.facts_linked(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProjectMemoryFactCurationReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ProjectMemoryFactCurationReceiptWire::deserialize(deserializer)?;
        let changed_facts = wire
            .changed_fact_ids
            .into_iter()
            .map(|fact_id| ProjectMemoryFactIdV1::new(wire.owner.clone(), fact_id))
            .collect::<FactStoreResult<Vec<_>>>()
            .map_err(serde::de::Error::custom)?;
        let operation_effects = wire
            .operation_effects
            .into_iter()
            .map(|effect| effect.into_effect(&wire.owner))
            .collect::<FactStoreResult<Vec<_>>>()
            .map_err(serde::de::Error::custom)?;
        let receipt = Self::new(
            wire.owner,
            wire.operation_id,
            wire.input_digest,
            wire.automation_run_id,
            operation_effects,
            changed_facts,
        )
        .map_err(serde::de::Error::custom)?;
        if receipt.replay_fact_id() != wire.replay_fact_id.as_ref()
            || receipt.replay_event_id() != wire.replay_event_id.as_ref()
            || receipt.accepted_operations() != wire.accepted_operations
            || receipt.facts_added() != wire.facts_added
            || receipt.facts_updated() != wire.facts_updated
            || receipt.facts_merged() != wire.facts_merged
            || receipt.facts_removed() != wire.facts_removed
            || receipt.normalized_tags() != wire.normalized_tags
            || receipt.facts_linked() != wire.facts_linked
        {
            return Err(serde::de::Error::custom(
                "curation receipt summary does not match its ordered effects",
            ));
        }
        Ok(receipt)
    }
}

impl ProjectMemoryFactCurationOperationEffectWire {
    fn into_effect(
        self,
        owner: &FactOwnerV1,
    ) -> FactStoreResult<ProjectMemoryFactCurationOperationEffectV1> {
        match self {
            Self::Add {
                fact_id,
                disposition,
                closest_fact_id,
                similarity_millionths,
                commit,
            } => ProjectMemoryFactCurationOperationEffectV1::add_snapshot(
                ProjectMemoryFactIdV1::new(owner.clone(), fact_id)?,
                disposition,
                closest_fact_id
                    .map(|fact_id| ProjectMemoryFactIdV1::new(owner.clone(), fact_id))
                    .transpose()?,
                similarity_millionths,
                commit,
            ),
            Self::Update {
                fact_id,
                trust_delta_millionths,
                commit,
            } => ProjectMemoryFactCurationOperationEffectV1::update_snapshot(
                ProjectMemoryFactIdV1::new(owner.clone(), fact_id)?,
                trust_delta_millionths,
                commit,
            ),
            Self::Merge { outcome } => {
                if outcome.owner() != owner {
                    return Err(FactStoreError::OwnerMismatch);
                }
                Ok(ProjectMemoryFactCurationOperationEffectV1::merge(outcome))
            }
            Self::Remove {
                target_fact_id,
                disposition,
                remaining_fact_count,
                commit,
            } => ProjectMemoryFactCurationOperationEffectV1::remove_snapshot(
                ProjectMemoryFactIdV1::new(owner.clone(), target_fact_id)?,
                disposition,
                remaining_fact_count,
                commit,
            ),
            Self::NormalizeTags { fact_id, commit } => {
                ProjectMemoryFactCurationOperationEffectV1::normalize_tags(
                    ProjectMemoryFactIdV1::new(owner.clone(), fact_id)?,
                    commit,
                )
            }
            Self::LinkFacts {
                relation,
                disposition,
                commit,
            } => ProjectMemoryFactCurationOperationEffectV1::link_facts_snapshot(
                relation,
                disposition,
                commit,
            ),
        }
    }
}

impl ProjectMemoryFactCurationReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        input_digest: String,
        automation_run_id: Option<RunId>,
        operation_effects: Vec<ProjectMemoryFactCurationOperationEffectV1>,
        changed_facts: Vec<ProjectMemoryFactIdV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if let Some(run_id) = &automation_run_id {
            run_id.validate()?;
        }
        validate_digest(&input_digest)?;
        if operation_effects.is_empty()
            || operation_effects.len() > MAX_PROJECT_MEMORY_CURATION_OPERATIONS
        {
            return Err(FactStoreError::Contract(DomainError::Empty {
                field: "curation receipt operation effects",
            }));
        }

        let mut committed_event_ids = BTreeSet::new();
        let mut durable_operation_identities = BTreeSet::new();
        let mut expected_changed = Vec::new();
        let mut expected_changed_ids = BTreeSet::new();
        let mut facts_added = 0_u64;
        let mut facts_updated = 0_u64;
        let mut facts_merged = 0_u64;
        let mut facts_removed = 0_u64;
        let mut normalized_tags = 0_u64;
        let mut facts_linked = 0_u64;

        for effect in &operation_effects {
            if effect
                .durable_operation_identity()?
                .is_some_and(|identity| !durable_operation_identities.insert(identity))
            {
                return Err(FactStoreError::Contract(DomainError::DuplicateId {
                    field: "curation operation identity",
                }));
            }
            for commit in effect.commit_receipts() {
                if commit.owner() != &owner
                    || commit.committed_event_ids().last() != Some(commit.last_event_id())
                {
                    return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                        field: "curation effect commit",
                    }));
                }
                for event_id in commit.committed_event_ids() {
                    if !committed_event_ids.insert(event_id.clone()) {
                        return Err(FactStoreError::Contract(DomainError::DuplicateId {
                            field: "curation receipt committed events",
                        }));
                    }
                }
            }
            for fact in effect.changed_facts()? {
                if expected_changed_ids.insert(fact.fact_id().clone()) {
                    expected_changed.push(fact);
                }
            }
            match effect {
                ProjectMemoryFactCurationOperationEffectV1::Add { commit, .. } => {
                    facts_added += u64::from(commit.is_some());
                }
                ProjectMemoryFactCurationOperationEffectV1::Update { .. } => facts_updated += 1,
                ProjectMemoryFactCurationOperationEffectV1::Merge { outcome } => {
                    let merged = u64::try_from(outcome.deleted_losers().len()).map_err(|_| {
                        FactStoreError::Contract(DomainError::NonCanonical {
                            field: "curation receipt merged fact count",
                        })
                    })?;
                    facts_merged = facts_merged
                        .checked_add(merged)
                        .ok_or_else(summary_overflow)?;
                }
                ProjectMemoryFactCurationOperationEffectV1::Remove { disposition, .. } => {
                    facts_removed += u64::from(
                        *disposition == ProjectMemoryFactCurationRemoveDispositionV1::Removed,
                    );
                }
                ProjectMemoryFactCurationOperationEffectV1::NormalizeTags { .. } => {
                    normalized_tags += 1;
                }
                ProjectMemoryFactCurationOperationEffectV1::LinkFacts { commit, .. } => {
                    facts_linked += u64::from(commit.is_some())
                }
            }
        }

        if changed_facts != expected_changed
            || changed_facts.len() > MAX_PROJECT_MEMORY_CURATION_TARGETS
            || changed_facts.iter().any(|fact| fact.owner() != &owner)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation receipt fact identities",
            }));
        }
        let replay_commit = operation_effects
            .iter()
            .find_map(|effect| effect.commit_receipts().into_iter().next());
        let replay_fact_id = replay_commit.map(|commit| commit.fact_id().clone());
        let replay_event_id = replay_commit.map(|commit| commit.last_event_id().clone());
        let accepted_operations = u64::try_from(operation_effects.len()).map_err(|_| {
            FactStoreError::Contract(DomainError::NonCanonical {
                field: "curation receipt accepted operation count",
            })
        })?;
        Ok(Self {
            owner,
            operation_id,
            input_digest,
            automation_run_id,
            operation_effects,
            replay_fact_id,
            replay_event_id,
            changed_facts,
            accepted_operations,
            facts_added,
            facts_updated,
            facts_merged,
            facts_removed,
            normalized_tags,
            facts_linked,
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

    pub fn automation_run_id(&self) -> Option<&RunId> {
        self.automation_run_id.as_ref()
    }

    pub fn operation_effects(&self) -> &[ProjectMemoryFactCurationOperationEffectV1] {
        &self.operation_effects
    }

    pub fn replay_fact_id(&self) -> Option<&FactId> {
        self.replay_fact_id.as_ref()
    }

    pub fn replay_event_id(&self) -> Option<&FactEventId> {
        self.replay_event_id.as_ref()
    }

    pub fn changed_facts(&self) -> &[ProjectMemoryFactIdV1] {
        &self.changed_facts
    }

    /// Number of policy-valid ordered effects, including truthful no-ops.
    pub fn accepted_operations(&self) -> u64 {
        self.accepted_operations
    }

    pub fn facts_added(&self) -> u64 {
        self.facts_added
    }

    pub fn facts_updated(&self) -> u64 {
        self.facts_updated
    }

    pub fn facts_merged(&self) -> u64 {
        self.facts_merged
    }

    pub fn facts_removed(&self) -> u64 {
        self.facts_removed
    }

    pub fn normalized_tags(&self) -> u64 {
        self.normalized_tags
    }

    pub fn facts_linked(&self) -> u64 {
        self.facts_linked
    }

    pub fn replayed(&self) -> bool {
        self.replayed
    }
}

fn validate_digest(input_digest: &str) -> FactStoreResult<()> {
    if input_digest.len() != 64
        || !input_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(FactStoreError::Contract(DomainError::NonCanonical {
            field: "curation receipt input digest",
        }));
    }
    Ok(())
}

fn summary_overflow() -> FactStoreError {
    FactStoreError::Contract(DomainError::NonCanonical {
        field: "curation receipt summary",
    })
}
