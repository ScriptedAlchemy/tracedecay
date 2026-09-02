use tracedecay_domain::{Confidence, FactOwnerV1};

use super::ProjectMemoryFactCurationReviewRefV1;
use super::validate::validate_curation_evidence;
use super::{
    ProjectMemoryFactAddCommandV1, ProjectMemoryFactMergeCommandV1,
    ProjectMemoryFactRemoveCommandV1, ProjectMemoryFactUpdateCommandV1,
};
use crate::memory::{FactStoreError, FactStoreResult};

/// Evidence and reviewer rationale bound to one automatic curation mutation.
///
/// The canonical command owns mutation identity and compare-and-set material;
/// this value binds the exact reviewed evidence that admitted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactCurationEvidenceV1 {
    facts: Vec<ProjectMemoryFactCurationReviewRefV1>,
    confidence: Confidence,
    reason: String,
}

impl ProjectMemoryFactCurationEvidenceV1 {
    pub fn new(
        owner: &FactOwnerV1,
        facts: Vec<ProjectMemoryFactCurationReviewRefV1>,
        confidence: Confidence,
        reason: String,
    ) -> FactStoreResult<Self> {
        validate_curation_evidence(owner, &facts)?;
        super::super::validate_project_memory_text(&reason, "curation mutation reason")?;
        Ok(Self {
            facts,
            confidence,
            reason,
        })
    }

    pub fn facts(&self) -> &[ProjectMemoryFactCurationReviewRefV1] {
        &self.facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }
}

macro_rules! curation_mutation {
    ($name:ident, $command:ty, $owner:expr) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct $name {
            command: $command,
            evidence: ProjectMemoryFactCurationEvidenceV1,
        }

        impl $name {
            pub fn new(
                command: $command,
                evidence: ProjectMemoryFactCurationEvidenceV1,
            ) -> FactStoreResult<Self> {
                let command_owner: &FactOwnerV1 = ($owner)(&command);
                if evidence
                    .facts()
                    .iter()
                    .any(|fact| fact.fact().owner() != command_owner)
                {
                    return Err(FactStoreError::OwnerMismatch);
                }
                Ok(Self { command, evidence })
            }

            pub fn command(&self) -> &$command {
                &self.command
            }

            pub fn evidence(&self) -> &ProjectMemoryFactCurationEvidenceV1 {
                &self.evidence
            }
        }
    };
}

curation_mutation!(
    ProjectMemoryFactCurationAddV1,
    ProjectMemoryFactAddCommandV1,
    ProjectMemoryFactAddCommandV1::owner
);
curation_mutation!(
    ProjectMemoryFactCurationUpdateV1,
    ProjectMemoryFactUpdateCommandV1,
    update_command_owner
);
curation_mutation!(
    ProjectMemoryFactCurationMergeV1,
    ProjectMemoryFactMergeCommandV1,
    ProjectMemoryFactMergeCommandV1::owner
);
curation_mutation!(
    ProjectMemoryFactCurationRemoveV1,
    ProjectMemoryFactRemoveCommandV1,
    remove_command_owner
);

fn update_command_owner(command: &ProjectMemoryFactUpdateCommandV1) -> &FactOwnerV1 {
    command.target().owner()
}

fn remove_command_owner(command: &ProjectMemoryFactRemoveCommandV1) -> &FactOwnerV1 {
    command.target().owner()
}

impl ProjectMemoryFactCurationUpdateV1 {
    pub(in crate::memory::project_memory) fn validate_review_cas(&self) -> FactStoreResult<()> {
        self.command.expected_last_event_id().ok_or_else(|| {
            FactStoreError::Contract(tracedecay_domain::DomainError::Empty {
                field: "curation update expected event",
            })
        })?;
        Ok(())
    }
}

impl ProjectMemoryFactCurationRemoveV1 {
    pub(in crate::memory::project_memory) fn validate_review_cas(&self) -> FactStoreResult<()> {
        self.command.expected_last_event_id().ok_or_else(|| {
            FactStoreError::Contract(tracedecay_domain::DomainError::Empty {
                field: "curation remove expected event",
            })
        })?;
        Ok(())
    }
}
