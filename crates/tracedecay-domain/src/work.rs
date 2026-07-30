//! Canonical task identity, immutable Work events, and deterministic projections.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    ActorId, ManifestDigest, ProjectId, ProposalId, RepositoryId, RunId, TaskId, UtcMicros,
    WorkCommandId, WorktreeId,
};

pub const MAX_WORK_TITLE_BYTES: usize = 512;
pub const MAX_WORK_DEPENDENCIES: usize = 256;
pub const MAX_WORK_RUNTIME_EVIDENCE: usize = 256;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkContractError {
    #[error("work version must be non-zero")]
    InvalidVersion,
    #[error("work version overflowed")]
    VersionOverflow,
    #[error("work title must be canonical and at most {MAX_WORK_TITLE_BYTES} bytes")]
    InvalidTitle,
    #[error("work dependencies exceed the bound of {MAX_WORK_DEPENDENCIES}")]
    TooManyDependencies,
    #[error("a task cannot depend on itself")]
    SelfDependency,
    #[error("work history must not be empty")]
    EmptyHistory,
    #[error("work history must start with a created event")]
    MissingCreation,
    #[error("work history versions must be contiguous")]
    NonContiguousVersion,
    #[error("work history mixes task or authority identities")]
    MixedAuthority,
    #[error("work event times must be monotonic")]
    NonMonotonicTime,
    #[error("work command identity is duplicated")]
    DuplicateCommand,
    #[error("work event is invalid for the current state")]
    InvalidTransition,
    #[error("runtime evidence exceeds the bound of {MAX_WORK_RUNTIME_EVIDENCE}")]
    TooMuchRuntimeEvidence,
    #[error("runtime evidence repeats a run identity")]
    DuplicateRuntimeEvidence,
    #[error("work projection history does not match its version")]
    InvalidProjectionHistory,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorkVersion(u64);

impl WorkVersion {
    pub fn new(value: u64) -> Result<Self, WorkContractError> {
        if value == 0 {
            return Err(WorkContractError::InvalidVersion);
        }
        Ok(Self(value))
    }

    pub const fn initial() -> Self {
        Self(1)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Result<Self, WorkContractError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(WorkContractError::VersionOverflow)
    }
}

impl<'de> Deserialize<'de> for WorkVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct WorkAuthority {
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    actor_id: ActorId,
    policy_digest: ManifestDigest,
}

impl WorkAuthority {
    pub fn new(
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
        actor_id: ActorId,
        policy_digest: ManifestDigest,
    ) -> Result<Self, WorkContractError> {
        Ok(Self {
            project_id,
            repository_id,
            worktree_id,
            actor_id,
            policy_digest,
        })
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn repository_id(&self) -> &RepositoryId {
        &self.repository_id
    }

    pub fn worktree_id(&self) -> &WorktreeId {
        &self.worktree_id
    }

    pub fn actor_id(&self) -> &ActorId {
        &self.actor_id
    }

    pub fn policy_digest(&self) -> &ManifestDigest {
        &self.policy_digest
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEvidenceRef {
    run_id: RunId,
    evidence_digest: ManifestDigest,
    terminal: bool,
}

impl RuntimeEvidenceRef {
    pub fn new(
        run_id: RunId,
        evidence_digest: ManifestDigest,
        terminal: bool,
    ) -> Result<Self, WorkContractError> {
        Ok(Self {
            run_id,
            evidence_digest,
            terminal,
        })
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn evidence_digest(&self) -> &ManifestDigest {
        &self.evidence_digest
    }

    pub const fn is_terminal(&self) -> bool {
        self.terminal
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkEventKind {
    Created {
        title: String,
        dependencies: BTreeSet<TaskId>,
    },
    DependenciesReplanned {
        dependencies: BTreeSet<TaskId>,
    },
    ProposalAccepted {
        proposal_id: ProposalId,
        proposal_digest: ManifestDigest,
    },
    ProposalRejected {
        proposal_id: ProposalId,
        proposal_digest: ManifestDigest,
    },
    ProposalSuperseded {
        proposal_id: ProposalId,
        proposal_digest: ManifestDigest,
    },
    ExecutionAdmitted,
    RuntimeEvidenceAttached {
        evidence: RuntimeEvidenceRef,
    },
    TaskAccepted,
}

impl WorkEventKind {
    fn validate(&self, task_id: &TaskId) -> Result<(), WorkContractError> {
        let dependencies = match self {
            Self::Created {
                title,
                dependencies,
            } => {
                if title.is_empty()
                    || title.trim() != title
                    || title.len() > MAX_WORK_TITLE_BYTES
                    || title.chars().any(char::is_control)
                {
                    return Err(WorkContractError::InvalidTitle);
                }
                Some(dependencies)
            }
            Self::DependenciesReplanned { dependencies } => Some(dependencies),
            _ => None,
        };

        if let Some(dependencies) = dependencies {
            if dependencies.len() > MAX_WORK_DEPENDENCIES {
                return Err(WorkContractError::TooManyDependencies);
            }
            if dependencies.contains(task_id) {
                return Err(WorkContractError::SelfDependency);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvent {
    task_id: TaskId,
    version: WorkVersion,
    authority: WorkAuthority,
    occurred_at: UtcMicros,
    command_id: WorkCommandId,
    input_digest: ManifestDigest,
    event: WorkEventKind,
}

impl WorkEvent {
    pub fn new(
        task_id: TaskId,
        version: WorkVersion,
        authority: WorkAuthority,
        occurred_at: UtcMicros,
        command_id: WorkCommandId,
        input_digest: ManifestDigest,
        event: WorkEventKind,
    ) -> Result<Self, WorkContractError> {
        event.validate(&task_id)?;
        Ok(Self {
            task_id,
            version,
            authority,
            occurred_at,
            command_id,
            input_digest,
            event,
        })
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub const fn version(&self) -> WorkVersion {
        self.version
    }

    pub fn authority(&self) -> &WorkAuthority {
        &self.authority
    }

    pub const fn occurred_at(&self) -> UtcMicros {
        self.occurred_at
    }

    pub fn command_id(&self) -> &WorkCommandId {
        &self.command_id
    }

    pub fn input_digest(&self) -> &ManifestDigest {
        &self.input_digest
    }

    pub fn event(&self) -> &WorkEventKind {
        &self.event
    }
}

impl<'de> Deserialize<'de> for WorkEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            task_id: TaskId,
            version: WorkVersion,
            authority: WorkAuthority,
            occurred_at: UtcMicros,
            command_id: WorkCommandId,
            input_digest: ManifestDigest,
            event: WorkEventKind,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.task_id,
            wire.version,
            wire.authority,
            wire.occurred_at,
            wire.command_id,
            wire.input_digest,
            wire.event,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProjection {
    task_id: TaskId,
    version: WorkVersion,
    authority: WorkAuthority,
    title: String,
    dependencies: BTreeSet<TaskId>,
    accepted_proposal: Option<ProposalId>,
    execution_admitted: bool,
    runtime_evidence: Vec<RuntimeEvidenceRef>,
    task_accepted: bool,
    history_len: usize,
}

impl WorkProjection {
    pub fn rebuild(history: &[WorkEvent]) -> Result<Self, WorkContractError> {
        let first = history.first().ok_or(WorkContractError::EmptyHistory)?;
        let WorkEventKind::Created {
            title,
            dependencies,
        } = first.event()
        else {
            return Err(WorkContractError::MissingCreation);
        };
        if first.version() != WorkVersion::initial() {
            return Err(WorkContractError::NonContiguousVersion);
        }

        let mut projection = Self {
            task_id: first.task_id().clone(),
            version: first.version(),
            authority: first.authority().clone(),
            title: title.clone(),
            dependencies: dependencies.clone(),
            accepted_proposal: None,
            execution_admitted: false,
            runtime_evidence: Vec::new(),
            task_accepted: false,
            history_len: 0,
        };
        let mut expected_version = WorkVersion::initial();
        let mut previous_time = first.occurred_at();
        let mut commands = BTreeSet::new();
        let mut runs = BTreeSet::new();

        for event in history {
            if event.task_id() != &projection.task_id || event.authority() != &projection.authority
            {
                return Err(WorkContractError::MixedAuthority);
            }
            if event.version() != expected_version {
                return Err(WorkContractError::NonContiguousVersion);
            }
            if event.occurred_at() < previous_time {
                return Err(WorkContractError::NonMonotonicTime);
            }
            if !commands.insert(event.command_id().clone()) {
                return Err(WorkContractError::DuplicateCommand);
            }
            if projection.task_accepted && event.version() != WorkVersion::initial() {
                return Err(WorkContractError::InvalidTransition);
            }

            match event.event() {
                WorkEventKind::Created { .. } if event.version() != WorkVersion::initial() => {
                    return Err(WorkContractError::InvalidTransition);
                }
                WorkEventKind::Created { .. } => {}
                WorkEventKind::DependenciesReplanned { dependencies } => {
                    projection.dependencies = dependencies.clone();
                }
                WorkEventKind::ProposalAccepted { proposal_id, .. } => {
                    projection.accepted_proposal = Some(proposal_id.clone());
                }
                WorkEventKind::ProposalRejected { proposal_id, .. }
                | WorkEventKind::ProposalSuperseded { proposal_id, .. } => {
                    if projection.accepted_proposal.as_ref() == Some(proposal_id) {
                        projection.accepted_proposal = None;
                    }
                }
                WorkEventKind::ExecutionAdmitted => {
                    if projection.accepted_proposal.is_none() {
                        return Err(WorkContractError::InvalidTransition);
                    }
                    projection.execution_admitted = true;
                }
                WorkEventKind::RuntimeEvidenceAttached { evidence } => {
                    if projection.runtime_evidence.len() == MAX_WORK_RUNTIME_EVIDENCE {
                        return Err(WorkContractError::TooMuchRuntimeEvidence);
                    }
                    if !runs.insert(evidence.run_id().clone()) {
                        return Err(WorkContractError::DuplicateRuntimeEvidence);
                    }
                    projection.runtime_evidence.push(evidence.clone());
                }
                WorkEventKind::TaskAccepted => projection.task_accepted = true,
            }

            projection.version = event.version();
            projection.history_len += 1;
            previous_time = event.occurred_at();
            expected_version = event.version().next()?;
        }

        Ok(projection)
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub const fn version(&self) -> WorkVersion {
        self.version
    }

    pub fn authority(&self) -> &WorkAuthority {
        &self.authority
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn dependencies(&self) -> &BTreeSet<TaskId> {
        &self.dependencies
    }

    pub fn accepted_proposal(&self) -> Option<&ProposalId> {
        self.accepted_proposal.as_ref()
    }

    pub const fn is_execution_admitted(&self) -> bool {
        self.execution_admitted
    }

    pub fn runtime_evidence(&self) -> &[RuntimeEvidenceRef] {
        &self.runtime_evidence
    }

    pub const fn is_task_accepted(&self) -> bool {
        self.task_accepted
    }

    pub const fn history_len(&self) -> usize {
        self.history_len
    }
}

impl<'de> Deserialize<'de> for WorkProjection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            task_id: TaskId,
            version: WorkVersion,
            authority: WorkAuthority,
            title: String,
            dependencies: BTreeSet<TaskId>,
            accepted_proposal: Option<ProposalId>,
            execution_admitted: bool,
            runtime_evidence: Vec<RuntimeEvidenceRef>,
            task_accepted: bool,
            history_len: usize,
        }

        let wire = Wire::deserialize(deserializer)?;
        if wire.title.is_empty()
            || wire.title.trim() != wire.title
            || wire.title.len() > MAX_WORK_TITLE_BYTES
            || wire.title.chars().any(char::is_control)
        {
            return Err(serde::de::Error::custom(WorkContractError::InvalidTitle));
        }
        if wire.dependencies.len() > MAX_WORK_DEPENDENCIES {
            return Err(serde::de::Error::custom(
                WorkContractError::TooManyDependencies,
            ));
        }
        if wire.dependencies.contains(&wire.task_id) {
            return Err(serde::de::Error::custom(WorkContractError::SelfDependency));
        }
        if wire.runtime_evidence.len() > MAX_WORK_RUNTIME_EVIDENCE {
            return Err(serde::de::Error::custom(
                WorkContractError::TooMuchRuntimeEvidence,
            ));
        }
        let mut run_ids = BTreeSet::new();
        if wire
            .runtime_evidence
            .iter()
            .any(|evidence| !run_ids.insert(evidence.run_id().clone()))
        {
            return Err(serde::de::Error::custom(
                WorkContractError::DuplicateRuntimeEvidence,
            ));
        }
        if usize::try_from(wire.version.get()).ok() != Some(wire.history_len) {
            return Err(serde::de::Error::custom(
                WorkContractError::InvalidProjectionHistory,
            ));
        }

        Ok(Self {
            task_id: wire.task_id,
            version: wire.version,
            authority: wire.authority,
            title: wire.title,
            dependencies: wire.dependencies,
            accepted_proposal: wire.accepted_proposal,
            execution_admitted: wire.execution_admitted,
            runtime_evidence: wire.runtime_evidence,
            task_accepted: wire.task_accepted,
            history_len: wire.history_len,
        })
    }
}
