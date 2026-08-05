//! Versioned Work product graph validation and legal transitions.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkProductRelationV1 {
    InitiativeContainsPlan {
        initiative_id: InitiativeId,
        plan_id: WorkPlanId,
    },
    PlanContainsMilestone {
        plan_id: WorkPlanId,
        milestone_id: MilestoneId,
    },
    MilestoneContainsTask {
        milestone_id: MilestoneId,
        task_id: TaskId,
    },
    Gates {
        dependency: TaskId,
        dependent: TaskId,
    },
    Informational {
        source: TaskId,
        target: TaskId,
    },
    CausalCandidate {
        cause: TaskId,
        effect: TaskId,
    },
    Evidence {
        task_id: TaskId,
        link_id: TaskEvidenceLinkId,
    },
    Attempt {
        task_id: TaskId,
        identity: WorkAttemptIdentityV1,
    },
    Handoff {
        task_id: TaskId,
        handoff_id: WorkHandoffId,
    },
    ProposalDecision {
        task_id: TaskId,
        proposal_id: ProposalId,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkGraphChangeV1 {
    TaskAdded {
        item: WorkItemV1,
    },
    EvidenceLinked {
        task_id: TaskId,
        evidence: TaskEvidenceLinkV1,
    },
    ProposalDecided {
        proposal: WorkProposalV1,
        disposition: WorkProposalDispositionV1,
        decided_at: UtcMicros,
    },
    ProposalAccepted {
        proposal: WorkProposalV1,
        accepted_at: UtcMicros,
    },
    ProviderAdmitted {
        task_id: TaskId,
        proposal_id: ProposalId,
        identity: WorkAttemptIdentityV1,
        route: WorkProviderRouteV1,
        admitted_at: UtcMicros,
    },
    ProviderOutcomeRecorded {
        task_id: TaskId,
        outcome: WorkProviderOutcomeV1,
    },
    TaskAccepted {
        task_id: TaskId,
        evidence_by_criterion: BTreeMap<AcceptanceCriterionId, TaskEvidenceLinkId>,
        accepted_at: UtcMicros,
    },
    HandoffRecorded {
        handoff: WorkHandoffV1,
    },
    AttemptCancelled {
        task_id: TaskId,
        identity: WorkAttemptIdentityV1,
        cancelled_at: UtcMicros,
    },
    AttemptRetried {
        task_id: TaskId,
        prior_identity: WorkAttemptIdentityV1,
        identity: WorkAttemptIdentityV1,
        route: WorkProviderRouteV1,
        admitted_at: UtcMicros,
    },
    AdmissionRolledBack {
        task_id: TaskId,
        identity: WorkAttemptIdentityV1,
        rolled_back_at: UtcMicros,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductGraphV1 {
    version: WorkGraphVersionV1,
    initiatives: Vec<WorkInitiativeV1>,
    plans: Vec<WorkPlanV1>,
    milestones: Vec<WorkMilestoneV1>,
    items: Vec<WorkItemV1>,
    proposal_decisions: Vec<WorkProposalDecisionV1>,
    evidence: Vec<TaskEvidenceLinkV1>,
}

impl WorkProductGraphV1 {
    pub fn new(
        version: WorkGraphVersionV1,
        mut initiatives: Vec<WorkInitiativeV1>,
        mut plans: Vec<WorkPlanV1>,
        mut milestones: Vec<WorkMilestoneV1>,
        mut items: Vec<WorkItemV1>,
    ) -> Result<Self, WorkProductContractError> {
        initiatives.sort_by(|left, right| left.id.cmp(&right.id));
        plans.sort_by(|left, right| left.id.cmp(&right.id));
        milestones.sort_by(|left, right| left.id.cmp(&right.id));
        items.sort_by(|left, right| left.input.task_id.cmp(&right.input.task_id));
        let graph = Self {
            version,
            initiatives,
            plans,
            milestones,
            items,
            proposal_decisions: Vec::new(),
            evidence: Vec::new(),
        };
        graph.validate()?;
        Ok(graph)
    }

    pub const fn version(&self) -> WorkGraphVersionV1 {
        self.version
    }

    pub fn initiatives(&self) -> &[WorkInitiativeV1] {
        &self.initiatives
    }

    pub fn plans(&self) -> &[WorkPlanV1] {
        &self.plans
    }

    pub fn milestones(&self) -> &[WorkMilestoneV1] {
        &self.milestones
    }

    pub fn items(&self) -> &[WorkItemV1] {
        &self.items
    }

    pub fn item(&self, task_id: &TaskId) -> Option<&WorkItemV1> {
        self.items
            .binary_search_by(|item| item.task_id().cmp(task_id))
            .ok()
            .map(|index| &self.items[index])
    }

    pub fn proposal_decisions(&self) -> &[WorkProposalDecisionV1] {
        &self.proposal_decisions
    }

    pub fn evidence(&self) -> &[TaskEvidenceLinkV1] {
        &self.evidence
    }

    pub fn relations(&self) -> Vec<WorkProductRelationV1> {
        let mut relations = Vec::new();
        relations.extend(self.plans.iter().map(|plan| {
            WorkProductRelationV1::InitiativeContainsPlan {
                initiative_id: plan.initiative_id().clone(),
                plan_id: plan.id().clone(),
            }
        }));
        relations.extend(self.milestones.iter().map(|milestone| {
            WorkProductRelationV1::PlanContainsMilestone {
                plan_id: milestone.plan_id().clone(),
                milestone_id: milestone.id().clone(),
            }
        }));
        for item in &self.items {
            relations.push(WorkProductRelationV1::MilestoneContainsTask {
                milestone_id: item.hierarchy().milestone_id().clone(),
                task_id: item.task_id().clone(),
            });
            relations.extend(item.dependencies().iter().map(|dependency| {
                WorkProductRelationV1::Gates {
                    dependency: dependency.clone(),
                    dependent: item.task_id().clone(),
                }
            }));
            relations.extend(item.informational_relations().iter().map(|target| {
                WorkProductRelationV1::Informational {
                    source: item.task_id().clone(),
                    target: target.clone(),
                }
            }));
            relations.extend(item.causal_candidates().iter().map(|cause| {
                WorkProductRelationV1::CausalCandidate {
                    cause: cause.clone(),
                    effect: item.task_id().clone(),
                }
            }));
            if let Some(admission) = item.provider_admission() {
                relations.push(WorkProductRelationV1::Attempt {
                    task_id: item.task_id().clone(),
                    identity: admission.identity().clone(),
                });
            }
            relations.extend(item.handoffs().iter().map(|handoff| {
                WorkProductRelationV1::Handoff {
                    task_id: item.task_id().clone(),
                    handoff_id: handoff.handoff_id().clone(),
                }
            }));
        }
        relations.extend(
            self.evidence
                .iter()
                .map(|evidence| WorkProductRelationV1::Evidence {
                    task_id: evidence.task_id().clone(),
                    link_id: evidence.link_id().clone(),
                }),
        );
        relations.extend(self.proposal_decisions.iter().map(|decision| {
            WorkProductRelationV1::ProposalDecision {
                task_id: decision.proposal().task_id().clone(),
                proposal_id: decision.proposal().proposal_id().clone(),
            }
        }));
        relations.sort();
        relations
    }

    pub fn apply(mut self, change: WorkGraphChangeV1) -> Result<Self, WorkProductContractError> {
        match change {
            WorkGraphChangeV1::TaskAdded { item } => self.items.push(item),
            WorkGraphChangeV1::EvidenceLinked { task_id, evidence } => {
                if evidence.task_id() != &task_id {
                    return Err(WorkProductContractError::EvidenceTaskMismatch);
                }
                let item = self.item_mut(&task_id)?;
                item.evidence_links.insert(evidence.link_id.clone());
                self.evidence.push(evidence);
            }
            WorkGraphChangeV1::ProposalDecided {
                proposal,
                disposition,
                decided_at,
            } => {
                self.validate_proposal(&proposal)?;
                self.proposal_decisions.push(WorkProposalDecisionV1 {
                    proposal,
                    disposition,
                    decided_at,
                });
            }
            WorkGraphChangeV1::ProposalAccepted {
                proposal,
                accepted_at,
            } => {
                self.validate_proposal(&proposal)?;
                let parent = self
                    .item(&proposal.task_id)
                    .cloned()
                    .ok_or(WorkProductContractError::UnknownTask)?;
                for child in &proposal.children {
                    self.items.push(WorkItemV1::new(WorkItemInputV1 {
                        task_id: child.task_id.clone(),
                        hierarchy: parent.input.hierarchy.clone(),
                        title: child.title.clone(),
                        dependencies: child.dependencies.clone(),
                        informational_relations: BTreeSet::new(),
                        causal_candidates: BTreeSet::new(),
                        acceptance_criteria: Vec::new(),
                        effort: child.effort,
                        scheduled_at: None,
                        deadline: parent.input.deadline,
                        created_at: accepted_at,
                        updated_at: accepted_at,
                    })?);
                }
                let item = self.item_mut(&proposal.task_id)?;
                item.accepted_proposal = Some(proposal.proposal_id.clone());
                item.accepted_route = Some(proposal.route.clone());
                item.input.updated_at = accepted_at;
                self.proposal_decisions.push(WorkProposalDecisionV1 {
                    proposal,
                    disposition: WorkProposalDispositionV1::Accepted,
                    decided_at: accepted_at,
                });
            }
            WorkGraphChangeV1::ProviderAdmitted {
                task_id,
                proposal_id,
                identity,
                route,
                admitted_at,
            } => {
                if identity.task_id() != &task_id {
                    return Err(WorkProductContractError::InvalidProviderTransition);
                }
                let item = self.item_mut(&task_id)?;
                if item.accepted_proposal.as_ref() != Some(&proposal_id)
                    || item
                        .accepted_route
                        .as_ref()
                        .and_then(WorkRouteDecisionV1::recommended)
                        != Some(&route)
                    || item.provider_admission.is_some()
                {
                    return Err(WorkProductContractError::RouteNotSelected);
                }
                item.provider_admission = Some(WorkProviderAdmissionV1::Admitted {
                    identity,
                    proposal_id,
                    route,
                    admitted_at,
                });
                item.input.updated_at = admitted_at;
            }
            WorkGraphChangeV1::ProviderOutcomeRecorded { task_id, outcome } => {
                let item = self.item_mut(&task_id)?;
                if item
                    .provider_admission
                    .as_ref()
                    .map(WorkProviderAdmissionV1::identity)
                    != Some(outcome.identity())
                    || item
                        .provider_outcomes
                        .iter()
                        .any(|prior| prior.identity() == outcome.identity())
                {
                    return Err(WorkProductContractError::InvalidProviderTransition);
                }
                item.input.updated_at = outcome.observed_at;
                item.provider_outcomes.push(outcome);
            }
            WorkGraphChangeV1::TaskAccepted {
                task_id,
                evidence_by_criterion,
                accepted_at,
            } => {
                let item = self.item_mut(&task_id)?;
                let required = item
                    .acceptance_criteria()
                    .iter()
                    .filter(|criterion| criterion.evidence_required())
                    .map(|criterion| criterion.criterion_id().clone())
                    .collect::<BTreeSet<_>>();
                if evidence_by_criterion.keys().collect::<BTreeSet<_>>()
                    != required.iter().collect::<BTreeSet<_>>()
                    || evidence_by_criterion
                        .values()
                        .any(|link_id| !item.evidence_links.contains(link_id))
                {
                    return Err(WorkProductContractError::AcceptanceUnsatisfied);
                }
                item.accepted_criteria = evidence_by_criterion;
                item.accepted_at = Some(accepted_at);
                item.input.updated_at = accepted_at;
            }
            WorkGraphChangeV1::HandoffRecorded { handoff } => {
                let handed_off_at = handoff.handed_off_at;
                let item = self.item_mut(handoff.task_id())?;
                item.input.updated_at = handed_off_at;
                item.handoffs.push(handoff);
            }
            WorkGraphChangeV1::AttemptCancelled {
                task_id,
                identity,
                cancelled_at,
            } => {
                let item = self.item_mut(&task_id)?;
                if item
                    .provider_admission
                    .as_ref()
                    .map(WorkProviderAdmissionV1::identity)
                    != Some(&identity)
                    || !item.provider_outcomes.is_empty()
                {
                    return Err(WorkProductContractError::InvalidProviderTransition);
                }
                item.provider_admission = Some(WorkProviderAdmissionV1::Cancelled {
                    identity,
                    cancelled_at,
                });
                item.input.updated_at = cancelled_at;
            }
            WorkGraphChangeV1::AttemptRetried {
                task_id,
                prior_identity,
                identity,
                route,
                admitted_at,
            } => {
                let item = self.item_mut(&task_id)?;
                if identity.task_id() != &task_id
                    || identity == prior_identity
                    || item
                        .provider_admission
                        .as_ref()
                        .map(WorkProviderAdmissionV1::identity)
                        != Some(&prior_identity)
                    || !matches!(
                        item.provider_admission,
                        Some(WorkProviderAdmissionV1::Cancelled { .. })
                    ) && item.provider_outcomes.is_empty()
                {
                    return Err(WorkProductContractError::InvalidProviderTransition);
                }
                let proposal_id = item
                    .accepted_proposal
                    .clone()
                    .ok_or(WorkProductContractError::ProposalMismatch)?;
                if item
                    .accepted_route
                    .as_ref()
                    .and_then(WorkRouteDecisionV1::recommended)
                    != Some(&route)
                {
                    return Err(WorkProductContractError::RouteNotSelected);
                }
                item.provider_admission = Some(WorkProviderAdmissionV1::Admitted {
                    identity,
                    proposal_id,
                    route,
                    admitted_at,
                });
                item.input.updated_at = admitted_at;
            }
            WorkGraphChangeV1::AdmissionRolledBack {
                task_id,
                identity,
                rolled_back_at,
            } => {
                let item = self.item_mut(&task_id)?;
                if item
                    .provider_admission
                    .as_ref()
                    .map(WorkProviderAdmissionV1::identity)
                    != Some(&identity)
                    || !matches!(
                        item.provider_admission,
                        Some(WorkProviderAdmissionV1::Admitted { .. })
                    )
                    || !item.provider_outcomes.is_empty()
                {
                    return Err(WorkProductContractError::InvalidProviderTransition);
                }
                item.provider_admission = Some(WorkProviderAdmissionV1::RolledBack {
                    identity,
                    rolled_back_at,
                });
                item.input.updated_at = rolled_back_at;
            }
        }
        self.version = self.version.next()?;
        self.items
            .sort_by(|left, right| left.task_id().cmp(right.task_id()));
        self.validate()?;
        Ok(self)
    }

    fn item_mut(&mut self, task_id: &TaskId) -> Result<&mut WorkItemV1, WorkProductContractError> {
        self.items
            .iter_mut()
            .find(|item| item.task_id() == task_id)
            .ok_or(WorkProductContractError::UnknownTask)
    }

    fn validate_proposal(&self, proposal: &WorkProposalV1) -> Result<(), WorkProductContractError> {
        if proposal.based_on_version != self.version || self.item(&proposal.task_id).is_none() {
            return Err(WorkProductContractError::ProposalMismatch);
        }
        let existing = self
            .items
            .iter()
            .map(WorkItemV1::task_id)
            .collect::<BTreeSet<_>>();
        let proposed = proposal
            .children
            .iter()
            .map(|child| &child.task_id)
            .collect::<BTreeSet<_>>();
        if proposed.iter().any(|task_id| existing.contains(task_id))
            || proposal.children.iter().any(|child| {
                child.dependencies.iter().any(|dependency| {
                    !existing.contains(dependency) && !proposed.contains(dependency)
                })
            })
        {
            return Err(WorkProductContractError::UnknownTask);
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), WorkProductContractError> {
        if self.items.len() > MAX_WORK_PRODUCT_ITEMS {
            return Err(WorkProductContractError::GraphTooLarge);
        }
        ensure_unique(self.initiatives.iter().map(|value| value.id()))?;
        ensure_unique(self.plans.iter().map(|value| value.id()))?;
        ensure_unique(self.milestones.iter().map(|value| value.id()))?;
        ensure_unique(self.items.iter().map(WorkItemV1::task_id))?;

        let initiatives = self
            .initiatives
            .iter()
            .map(WorkInitiativeV1::id)
            .collect::<BTreeSet<_>>();
        let plans = self
            .plans
            .iter()
            .map(WorkPlanV1::id)
            .collect::<BTreeSet<_>>();
        let milestones = self
            .milestones
            .iter()
            .map(WorkMilestoneV1::id)
            .collect::<BTreeSet<_>>();
        if self
            .plans
            .iter()
            .any(|plan| !initiatives.contains(plan.initiative_id()))
            || self
                .milestones
                .iter()
                .any(|milestone| !plans.contains(milestone.plan_id()))
            || self.items.iter().any(|item| {
                !initiatives.contains(item.hierarchy().initiative_id())
                    || !plans.contains(item.hierarchy().plan_id())
                    || !milestones.contains(item.hierarchy().milestone_id())
            })
        {
            return Err(WorkProductContractError::UnknownHierarchy);
        }
        let tasks = self
            .items
            .iter()
            .map(WorkItemV1::task_id)
            .collect::<BTreeSet<_>>();
        let relations = self.items.iter().try_fold(0usize, |total, item| {
            total
                .checked_add(item.dependencies().len())
                .and_then(|value| value.checked_add(item.informational_relations().len()))
                .and_then(|value| value.checked_add(item.causal_candidates().len()))
        });
        if relations.is_none_or(|count| count > MAX_WORK_PRODUCT_RELATIONS) {
            return Err(WorkProductContractError::GraphTooLarge);
        }
        if self.items.iter().any(|item| {
            item.dependencies()
                .iter()
                .chain(item.informational_relations())
                .chain(item.causal_candidates())
                .any(|related| !tasks.contains(related))
        }) {
            return Err(WorkProductContractError::UnknownTask);
        }
        validate_acyclic(&self.items)
    }
}

fn validate_acyclic(items: &[WorkItemV1]) -> Result<(), WorkProductContractError> {
    let mut indegree = items
        .iter()
        .map(|item| (item.task_id().clone(), item.dependencies().len()))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing = BTreeMap::<TaskId, Vec<TaskId>>::new();
    for item in items {
        for dependency in item.dependencies() {
            outgoing
                .entry(dependency.clone())
                .or_default()
                .push(item.task_id().clone());
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(task_id, count)| (*count == 0).then_some(task_id.clone()))
        .collect::<VecDeque<_>>();
    let mut visited = 0usize;
    while let Some(task_id) = ready.pop_front() {
        visited += 1;
        for dependent in outgoing.get(&task_id).into_iter().flatten() {
            let count = indegree
                .get_mut(dependent)
                .ok_or(WorkProductContractError::UnknownTask)?;
            *count -= 1;
            if *count == 0 {
                ready.push_back(dependent.clone());
            }
        }
    }
    if visited == items.len() {
        Ok(())
    } else {
        Err(WorkProductContractError::DependencyCycle)
    }
}

fn ensure_unique<'a, T: Ord + 'a>(
    values: impl Iterator<Item = &'a T>,
) -> Result<(), WorkProductContractError> {
    let mut seen = BTreeSet::new();
    if values.into_iter().all(|value| seen.insert(value)) {
        Ok(())
    } else {
        Err(WorkProductContractError::DuplicateIdentity)
    }
}
