//! Canonical Plan 24 product graph contracts.
//!
//! These values contain no persistence or provider behavior. The owning daemon
//! stores them through its injected shared graph handle. Runtime execution
//! remains external; this graph retains only exact accepted-attempt evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{
    ActorId, ManifestDigest, ProposalId, RetrievalAnchorId, TaskId, UtcMicros,
    WorkAttemptIdentityV1, WorkProductAuthorizedRelationScopeV1, WorkProviderRouteV1,
};

pub const MAX_WORK_PRODUCT_TEXT_BYTES: usize = 4_096;
pub const MAX_WORK_PRODUCT_ITEMS: usize = 10_000;
pub const MAX_WORK_PRODUCT_RELATIONS: usize = 50_000;
pub const MAX_WORK_PRODUCT_EVIDENCE: usize = 1_024;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProductContractError {
    #[error("Work product identity is not canonical")]
    InvalidIdentity,
    #[error("Work product version must be non-zero")]
    InvalidVersion,
    #[error("Work product version overflowed")]
    VersionOverflow,
    #[error("Work product text is not canonical or exceeds its bound")]
    InvalidText,
    #[error("Work product score or estimate is invalid")]
    InvalidScore,
    #[error("Work product hierarchy is missing or inconsistent")]
    UnknownHierarchy,
    #[error("Work product graph repeats an identity")]
    DuplicateIdentity,
    #[error("Work product graph references an unknown task")]
    UnknownTask,
    #[error("Work product gating dependencies contain a cycle")]
    DependencyCycle,
    #[error("Work product graph exceeds its item or relation bound")]
    GraphTooLarge,
    #[error("Work product time range is invalid")]
    InvalidTime,
    #[error("Work proposal does not match the selected graph or task")]
    ProposalMismatch,
    #[error("Work provider route was not explicitly selected")]
    RouteNotSelected,
    #[error("Work acceptance criteria are not satisfied")]
    AcceptanceUnsatisfied,
    #[error("Task evidence is not rooted in the selected task")]
    EvidenceTaskMismatch,
    #[error("Task evidence coverage is inconsistent")]
    InvalidEvidenceCoverage,
    #[error("Work graph change is not legal in the current state")]
    IllegalTransition,
}

macro_rules! work_product_id {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(
            Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, WorkProductContractError> {
                let value = value.into();
                if !crate::canonical_text::is_canonical_text_within(
                    &value,
                    crate::canonical_text::CANONICAL_TEXT_MAX_BYTES,
                ) {
                    return Err(WorkProductContractError::InvalidIdentity);
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?)
                    .map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = WorkProductContractError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )+};
}

work_product_id!(
    InitiativeId,
    WorkPlanId,
    MilestoneId,
    AcceptanceCriterionId,
    TaskEvidenceLinkId,
    WorkHandoffId,
);

#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorkGraphVersionV1(u64);

impl WorkGraphVersionV1 {
    pub fn new(value: u64) -> Result<Self, WorkProductContractError> {
        if value == 0 {
            return Err(WorkProductContractError::InvalidVersion);
        }
        Ok(Self(value))
    }

    pub const fn initial() -> Self {
        Self(1)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Result<Self, WorkProductContractError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(WorkProductContractError::VersionOverflow)
    }
}

impl<'de> Deserialize<'de> for WorkGraphVersionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// The exact owner-relative relation set selected for a canonical Work graph.
///
/// This identity lives in the domain because Plan 32 attempt admission must
/// retain it byte-for-byte through provider settlement. Reconstructing it
/// from a project context would conflate explicit no-Git work with a scoped
/// repository relation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum WorkProductSelectionScopeV1 {
    ProfileOwnedNoGit,
    Relations {
        relation_scopes: BTreeSet<WorkProductAuthorizedRelationScopeV1>,
    },
}

impl WorkProductSelectionScopeV1 {
    pub fn relations(
        relation_scopes: BTreeSet<WorkProductAuthorizedRelationScopeV1>,
    ) -> Result<Self, WorkProductContractError> {
        if relation_scopes.is_empty() {
            return Err(WorkProductContractError::InvalidIdentity);
        }
        Ok(Self::Relations { relation_scopes })
    }

    pub const fn relation_scopes(&self) -> Option<&BTreeSet<WorkProductAuthorizedRelationScopeV1>> {
        match self {
            Self::ProfileOwnedNoGit => None,
            Self::Relations { relation_scopes } => Some(relation_scopes),
        }
    }

    pub fn validate(&self) -> Result<(), WorkProductContractError> {
        if matches!(
            self,
            Self::Relations { relation_scopes } if relation_scopes.is_empty()
        ) {
            return Err(WorkProductContractError::InvalidIdentity);
        }
        Ok(())
    }
}

macro_rules! work_container {
    ($name:ident, $id:ident $(, $parent:ident : $parent_ty:ident)?) => {
        #[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            id: $id,
            $($parent: $parent_ty,)?
            title: String,
            created_at: UtcMicros,
        }

        impl $name {
            pub fn new(
                id: $id,
                $($parent: $parent_ty,)?
                title: String,
                created_at: UtcMicros,
            ) -> Result<Self, WorkProductContractError> {
                validate_text(&title)?;
                Ok(Self { id, $($parent,)? title, created_at })
            }

            pub fn id(&self) -> &$id {
                &self.id
            }

            $(pub fn $parent(&self) -> &$parent_ty {
                &self.$parent
            })?

            pub fn title(&self) -> &str {
                &self.title
            }

            pub const fn created_at(&self) -> UtcMicros {
                self.created_at
            }
        }
    };
}

work_container!(WorkInitiativeV1, InitiativeId);
work_container!(WorkPlanV1, WorkPlanId, initiative_id: InitiativeId);
work_container!(WorkMilestoneV1, MilestoneId, plan_id: WorkPlanId);

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkHierarchyV1 {
    initiative_id: InitiativeId,
    plan_id: WorkPlanId,
    milestone_id: MilestoneId,
}

impl WorkHierarchyV1 {
    pub fn new(
        initiative_id: InitiativeId,
        plan_id: WorkPlanId,
        milestone_id: MilestoneId,
    ) -> Self {
        Self {
            initiative_id,
            plan_id,
            milestone_id,
        }
    }

    pub fn initiative_id(&self) -> &InitiativeId {
        &self.initiative_id
    }

    pub fn plan_id(&self) -> &WorkPlanId {
        &self.plan_id
    }

    pub fn milestone_id(&self) -> &MilestoneId {
        &self.milestone_id
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAcceptanceCriterionV1 {
    criterion_id: AcceptanceCriterionId,
    description: String,
    evidence_required: bool,
}

impl WorkAcceptanceCriterionV1 {
    pub fn new(
        criterion_id: AcceptanceCriterionId,
        description: String,
        evidence_required: bool,
    ) -> Result<Self, WorkProductContractError> {
        validate_text(&description)?;
        Ok(Self {
            criterion_id,
            description,
            evidence_required,
        })
    }

    pub fn criterion_id(&self) -> &AcceptanceCriterionId {
        &self.criterion_id
    }

    pub const fn evidence_required(&self) -> bool {
        self.evidence_required
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskEvidenceLinkV1 {
    link_id: TaskEvidenceLinkId,
    revision: u64,
    task_id: TaskId,
    anchor_id: RetrievalAnchorId,
    evidence_digest: ManifestDigest,
    observed_at: UtcMicros,
}

impl TaskEvidenceLinkV1 {
    pub fn new(
        link_id: TaskEvidenceLinkId,
        revision: u64,
        task_id: TaskId,
        anchor_id: RetrievalAnchorId,
        evidence_digest: ManifestDigest,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkProductContractError> {
        if revision == 0 {
            return Err(WorkProductContractError::InvalidVersion);
        }
        Ok(Self {
            link_id,
            revision,
            task_id,
            anchor_id,
            evidence_digest,
            observed_at,
        })
    }

    pub fn link_id(&self) -> &TaskEvidenceLinkId {
        &self.link_id
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn evidence_digest(&self) -> &ManifestDigest {
        &self.evidence_digest
    }

    pub const fn observed_at(&self) -> UtcMicros {
        self.observed_at
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkTaskEvidenceCoverageV1 {
    Complete {
        returned: u32,
        available: u32,
    },
    Partial {
        returned: u32,
        available: u32,
        unknowns: BTreeSet<String>,
    },
}

impl WorkTaskEvidenceCoverageV1 {
    fn validate(&self, count: usize) -> Result<(), WorkProductContractError> {
        let (returned, available, unknowns) = match self {
            Self::Complete {
                returned,
                available,
            } => (*returned, *available, None),
            Self::Partial {
                returned,
                available,
                unknowns,
            } => (*returned, *available, Some(unknowns)),
        };
        if usize::try_from(returned).ok() != Some(count)
            || returned > available
            || matches!(self, Self::Complete { .. }) && returned != available
            || unknowns.is_some_and(BTreeSet::is_empty)
        {
            return Err(WorkProductContractError::InvalidEvidenceCoverage);
        }
        if let Some(unknowns) = unknowns {
            for unknown in unknowns {
                validate_text(unknown)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskEvidenceV1 {
    task_id: TaskId,
    graph_version: WorkGraphVersionV1,
    links: Vec<TaskEvidenceLinkV1>,
    coverage: WorkTaskEvidenceCoverageV1,
}

impl WorkTaskEvidenceV1 {
    pub fn new(
        task_id: TaskId,
        graph_version: WorkGraphVersionV1,
        mut links: Vec<TaskEvidenceLinkV1>,
        coverage: WorkTaskEvidenceCoverageV1,
    ) -> Result<Self, WorkProductContractError> {
        links.sort_by(|left, right| left.link_id.cmp(&right.link_id));
        let evidence = Self {
            task_id,
            graph_version,
            links,
            coverage,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    pub fn validate(&self) -> Result<(), WorkProductContractError> {
        let links = &self.links;
        if links.len() > MAX_WORK_PRODUCT_EVIDENCE {
            return Err(WorkProductContractError::GraphTooLarge);
        }
        if links.iter().any(|link| link.revision() == 0) {
            return Err(WorkProductContractError::InvalidVersion);
        }
        if links.iter().any(|link| link.task_id() != &self.task_id) {
            return Err(WorkProductContractError::EvidenceTaskMismatch);
        }
        if links
            .iter()
            .map(TaskEvidenceLinkV1::link_id)
            .collect::<BTreeSet<_>>()
            .len()
            != links.len()
        {
            return Err(WorkProductContractError::DuplicateIdentity);
        }
        if links
            .windows(2)
            .any(|pair| pair[0].link_id() > pair[1].link_id())
        {
            return Err(WorkProductContractError::IllegalTransition);
        }
        self.coverage.validate(links.len())?;
        Ok(())
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub const fn graph_version(&self) -> WorkGraphVersionV1 {
        self.graph_version
    }

    pub fn links(&self) -> &[TaskEvidenceLinkV1] {
        &self.links
    }

    pub const fn coverage(&self) -> &WorkTaskEvidenceCoverageV1 {
        &self.coverage
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkScoreKindV1 {
    Ordinal,
    Heuristic,
    CalibratedRange,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkShapeAssessmentV1 {
    score_kind: WorkScoreKindV1,
    complexity: u8,
    ambiguity: u8,
    blast_radius: u8,
    integration_overhead: u8,
}

impl WorkShapeAssessmentV1 {
    pub fn new(
        score_kind: WorkScoreKindV1,
        complexity: u8,
        ambiguity: u8,
        blast_radius: u8,
        integration_overhead: u8,
    ) -> Result<Self, WorkProductContractError> {
        if [complexity, ambiguity, blast_radius, integration_overhead]
            .into_iter()
            .any(|score| score > 5)
        {
            return Err(WorkProductContractError::InvalidScore);
        }
        Ok(Self {
            score_kind,
            complexity,
            ambiguity,
            blast_radius,
            integration_overhead,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkSizingV1 {
    score_kind: WorkScoreKindV1,
    low: u32,
    likely: u32,
    high: u32,
    coverage: String,
}

impl WorkSizingV1 {
    pub fn new(
        score_kind: WorkScoreKindV1,
        low: u32,
        likely: u32,
        high: u32,
        coverage: impl Into<String>,
    ) -> Result<Self, WorkProductContractError> {
        let coverage = coverage.into();
        if low == 0 || low > likely || likely > high {
            return Err(WorkProductContractError::InvalidScore);
        }
        validate_text(&coverage)?;
        Ok(Self {
            score_kind,
            low,
            likely,
            high,
            coverage,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum WorkRouteDecisionV1 {
    Selected {
        recommended: WorkProviderRouteV1,
        alternatives: Vec<WorkProviderRouteV1>,
        exclusions: BTreeSet<String>,
        fallback: String,
    },
    Abstained {
        reason: String,
    },
}

impl WorkRouteDecisionV1 {
    pub fn selected(
        recommended: WorkProviderRouteV1,
        alternatives: Vec<WorkProviderRouteV1>,
        exclusions: BTreeSet<String>,
        fallback: String,
    ) -> Result<Self, WorkProductContractError> {
        validate_text(&fallback)?;
        for exclusion in &exclusions {
            validate_text(exclusion)?;
        }
        Ok(Self::Selected {
            recommended,
            alternatives,
            exclusions,
            fallback,
        })
    }

    pub fn abstain(reason: impl Into<String>) -> Result<Self, WorkProductContractError> {
        let reason = reason.into();
        validate_text(&reason)?;
        Ok(Self::Abstained { reason })
    }

    pub const fn recommended(&self) -> Option<&WorkProviderRouteV1> {
        match self {
            Self::Selected { recommended, .. } => Some(recommended),
            Self::Abstained { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProposedChildV1 {
    task_id: TaskId,
    title: String,
    effort: u32,
    dependencies: BTreeSet<TaskId>,
}

impl WorkProposedChildV1 {
    pub fn new(
        task_id: TaskId,
        title: String,
        effort: u32,
        dependencies: BTreeSet<TaskId>,
    ) -> Result<Self, WorkProductContractError> {
        validate_text(&title)?;
        if effort == 0 || dependencies.contains(&task_id) {
            return Err(WorkProductContractError::InvalidScore);
        }
        Ok(Self {
            task_id,
            title,
            effort,
            dependencies,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProposalV1 {
    proposal_id: ProposalId,
    task_id: TaskId,
    based_on_version: WorkGraphVersionV1,
    shape: WorkShapeAssessmentV1,
    sizing: WorkSizingV1,
    children: Vec<WorkProposedChildV1>,
    route: WorkRouteDecisionV1,
    explanation: String,
    evidence_digest: ManifestDigest,
}

impl WorkProposalV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        proposal_id: ProposalId,
        task_id: TaskId,
        based_on_version: WorkGraphVersionV1,
        shape: WorkShapeAssessmentV1,
        sizing: WorkSizingV1,
        mut children: Vec<WorkProposedChildV1>,
        route: WorkRouteDecisionV1,
        explanation: String,
        evidence_digest: ManifestDigest,
    ) -> Result<Self, WorkProductContractError> {
        validate_text(&explanation)?;
        children.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        if children
            .windows(2)
            .any(|pair| pair[0].task_id == pair[1].task_id)
            || children.iter().any(|child| child.task_id == task_id)
        {
            return Err(WorkProductContractError::DuplicateIdentity);
        }
        Ok(Self {
            proposal_id,
            task_id,
            based_on_version,
            shape,
            sizing,
            children,
            route,
            explanation,
            evidence_digest,
        })
    }

    pub fn proposal_id(&self) -> &ProposalId {
        &self.proposal_id
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub const fn based_on_version(&self) -> WorkGraphVersionV1 {
        self.based_on_version
    }

    pub fn route(&self) -> &WorkRouteDecisionV1 {
        &self.route
    }

    pub const fn shape(&self) -> &WorkShapeAssessmentV1 {
        &self.shape
    }

    pub const fn sizing(&self) -> &WorkSizingV1 {
        &self.sizing
    }

    pub fn children(&self) -> &[WorkProposedChildV1] {
        &self.children
    }

    pub fn explanation(&self) -> &str {
        &self.explanation
    }

    pub fn evidence_digest(&self) -> &ManifestDigest {
        &self.evidence_digest
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProposalDispositionV1 {
    Accepted,
    Rejected,
    Superseded,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProposalDecisionV1 {
    proposal: WorkProposalV1,
    disposition: WorkProposalDispositionV1,
    decided_at: UtcMicros,
}

impl WorkProposalDecisionV1 {
    pub const fn proposal(&self) -> &WorkProposalV1 {
        &self.proposal
    }

    pub const fn disposition(&self) -> &WorkProposalDispositionV1 {
        &self.disposition
    }

    pub const fn decided_at(&self) -> UtcMicros {
        self.decided_at
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkHandoffV1 {
    handoff_id: WorkHandoffId,
    task_id: TaskId,
    from_actor: ActorId,
    to_actor: ActorId,
    evidence_frontier: BTreeSet<TaskEvidenceLinkId>,
    unknowns: BTreeSet<String>,
    handed_off_at: UtcMicros,
}

impl WorkHandoffV1 {
    pub fn new(
        handoff_id: WorkHandoffId,
        task_id: TaskId,
        from_actor: ActorId,
        to_actor: ActorId,
        evidence_frontier: BTreeSet<TaskEvidenceLinkId>,
        unknowns: BTreeSet<String>,
        handed_off_at: UtcMicros,
    ) -> Result<Self, WorkProductContractError> {
        for unknown in &unknowns {
            validate_text(unknown)?;
        }
        Ok(Self {
            handoff_id,
            task_id,
            from_actor,
            to_actor,
            evidence_frontier,
            unknowns,
            handed_off_at,
        })
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub fn handoff_id(&self) -> &WorkHandoffId {
        &self.handoff_id
    }

    pub fn from_actor(&self) -> &ActorId {
        &self.from_actor
    }

    pub fn to_actor(&self) -> &ActorId {
        &self.to_actor
    }

    pub fn evidence_frontier(&self) -> &BTreeSet<TaskEvidenceLinkId> {
        &self.evidence_frontier
    }

    pub fn unknowns(&self) -> &BTreeSet<String> {
        &self.unknowns
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkItemInputV1 {
    pub task_id: TaskId,
    pub hierarchy: WorkHierarchyV1,
    pub title: String,
    pub dependencies: BTreeSet<TaskId>,
    pub informational_relations: BTreeSet<TaskId>,
    pub causal_candidates: BTreeSet<TaskId>,
    pub acceptance_criteria: Vec<WorkAcceptanceCriterionV1>,
    pub effort: u32,
    pub scheduled_at: Option<UtcMicros>,
    pub deadline: Option<UtcMicros>,
    pub created_at: UtcMicros,
    pub updated_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkItemV1 {
    input: WorkItemInputV1,
    accepted_proposal: Option<ProposalId>,
    accepted_route: Option<WorkRouteDecisionV1>,
    evidence_links: BTreeSet<TaskEvidenceLinkId>,
    accepted_criteria: BTreeMap<AcceptanceCriterionId, TaskEvidenceLinkId>,
    #[serde(with = "accepted_attempt_wire")]
    #[schemars(with = "Vec<AcceptedAttemptWireV1>")]
    accepted_attempts: BTreeMap<WorkAttemptIdentityV1, TaskEvidenceLinkId>,
    handoffs: Vec<WorkHandoffV1>,
    accepted_at: Option<UtcMicros>,
    archived_at: Option<UtcMicros>,
}

/// JSON-safe entry for the accepted-attempt relation. The domain keeps a map
/// for exact identity lookup, while persisted/wire state uses a sorted array
/// because a structured identity cannot be a JSON object key.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AcceptedAttemptWireV1 {
    identity: WorkAttemptIdentityV1,
    link_id: TaskEvidenceLinkId,
}

mod accepted_attempt_wire;

impl WorkItemV1 {
    pub fn new(input: WorkItemInputV1) -> Result<Self, WorkProductContractError> {
        validate_text(&input.title)?;
        if input.effort == 0
            || input.updated_at < input.created_at
            || input
                .deadline
                .is_some_and(|deadline| deadline < input.created_at)
            || input.dependencies.contains(&input.task_id)
            || input.informational_relations.contains(&input.task_id)
            || input.causal_candidates.contains(&input.task_id)
        {
            return Err(WorkProductContractError::InvalidTime);
        }
        let criterion_ids = input
            .acceptance_criteria
            .iter()
            .map(|criterion| criterion.criterion_id.clone())
            .collect::<BTreeSet<_>>();
        if criterion_ids.len() != input.acceptance_criteria.len() {
            return Err(WorkProductContractError::DuplicateIdentity);
        }
        Ok(Self {
            input,
            accepted_proposal: None,
            accepted_route: None,
            evidence_links: BTreeSet::new(),
            accepted_criteria: BTreeMap::new(),
            accepted_attempts: BTreeMap::new(),
            handoffs: Vec::new(),
            accepted_at: None,
            archived_at: None,
        })
    }

    pub fn task_id(&self) -> &TaskId {
        &self.input.task_id
    }

    pub fn hierarchy(&self) -> &WorkHierarchyV1 {
        &self.input.hierarchy
    }

    pub fn title(&self) -> &str {
        &self.input.title
    }

    pub fn dependencies(&self) -> &BTreeSet<TaskId> {
        &self.input.dependencies
    }

    pub fn informational_relations(&self) -> &BTreeSet<TaskId> {
        &self.input.informational_relations
    }

    pub fn causal_candidates(&self) -> &BTreeSet<TaskId> {
        &self.input.causal_candidates
    }

    pub fn acceptance_criteria(&self) -> &[WorkAcceptanceCriterionV1] {
        &self.input.acceptance_criteria
    }

    pub const fn effort(&self) -> u32 {
        self.input.effort
    }

    pub const fn scheduled_at(&self) -> Option<UtcMicros> {
        self.input.scheduled_at
    }

    pub const fn deadline(&self) -> Option<UtcMicros> {
        self.input.deadline
    }

    pub const fn created_at(&self) -> UtcMicros {
        self.input.created_at
    }

    pub const fn updated_at(&self) -> UtcMicros {
        self.input.updated_at
    }

    pub fn accepted_proposal(&self) -> Option<&ProposalId> {
        self.accepted_proposal.as_ref()
    }

    pub fn accepted_route(&self) -> Option<&WorkRouteDecisionV1> {
        self.accepted_route.as_ref()
    }

    pub fn accepted_attempts(&self) -> &BTreeMap<WorkAttemptIdentityV1, TaskEvidenceLinkId> {
        &self.accepted_attempts
    }

    pub fn evidence_links(&self) -> &BTreeSet<TaskEvidenceLinkId> {
        &self.evidence_links
    }

    pub fn handoffs(&self) -> &[WorkHandoffV1] {
        &self.handoffs
    }

    pub const fn is_accepted(&self) -> bool {
        self.accepted_at.is_some()
    }

    pub const fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

mod graph;
pub use graph::*;
fn validate_text(value: &str) -> Result<(), WorkProductContractError> {
    if crate::canonical_text::is_canonical_text_within(value, MAX_WORK_PRODUCT_TEXT_BYTES) {
        Ok(())
    } else {
        Err(WorkProductContractError::InvalidText)
    }
}
