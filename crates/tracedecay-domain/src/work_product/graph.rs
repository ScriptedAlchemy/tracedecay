//! Versioned Work product graph validation and legal transitions.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

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
    AcceptedAttempt {
        task_id: TaskId,
        identity: WorkAttemptIdentityV1,
        link_id: TaskEvidenceLinkId,
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
#[serde(deny_unknown_fields)]
pub struct WorkRelationReplanProposalV1 {
    pub proposal_id: ProposalId,
    pub task_id: TaskId,
    pub based_on_version: WorkGraphVersionV1,
    dependencies: BTreeSet<TaskId>,
    informational_relations: BTreeSet<TaskId>,
    causal_candidates: BTreeSet<TaskId>,
    pub payload_digest: ManifestDigest,
}

impl WorkRelationReplanProposalV1 {
    pub fn new(
        proposal_id: ProposalId,
        task_id: TaskId,
        based_on_version: WorkGraphVersionV1,
        dependencies: Vec<TaskId>,
        informational_relations: Vec<TaskId>,
        causal_candidates: Vec<TaskId>,
    ) -> Result<Self, WorkProductContractError> {
        ensure_unique(dependencies.iter())?;
        ensure_unique(informational_relations.iter())?;
        ensure_unique(causal_candidates.iter())?;
        let dependencies = dependencies.into_iter().collect();
        let informational_relations = informational_relations.into_iter().collect();
        let causal_candidates = causal_candidates.into_iter().collect();
        let payload_digest = relation_replan_digest(
            &task_id,
            based_on_version,
            &dependencies,
            &informational_relations,
            &causal_candidates,
        )?;
        let proposal = Self {
            proposal_id,
            task_id,
            based_on_version,
            dependencies,
            informational_relations,
            causal_candidates,
            payload_digest,
        };
        proposal.validate()?;
        Ok(proposal)
    }

    pub fn dependencies(&self) -> &BTreeSet<TaskId> {
        &self.dependencies
    }

    pub fn informational_relations(&self) -> &BTreeSet<TaskId> {
        &self.informational_relations
    }

    pub fn causal_candidates(&self) -> &BTreeSet<TaskId> {
        &self.causal_candidates
    }

    pub(crate) fn validate(&self) -> Result<(), WorkProductContractError> {
        if self.dependencies.contains(&self.task_id) {
            return Err(WorkProductContractError::DependencyCycle);
        }
        if self.informational_relations.contains(&self.task_id)
            || self.causal_candidates.contains(&self.task_id)
        {
            return Err(WorkProductContractError::IllegalTransition);
        }
        if self.payload_digest
            != relation_replan_digest(
                &self.task_id,
                self.based_on_version,
                &self.dependencies,
                &self.informational_relations,
                &self.causal_candidates,
            )?
        {
            return Err(WorkProductContractError::ProposalMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRelationReplanDecisionV1 {
    pub proposal: WorkRelationReplanProposalV1,
    pub disposition: WorkProposalDispositionV1,
    pub decided_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkGraphChangeV1 {
    TaskAdded {
        item: Box<WorkItemV1>,
    },
    RelationReplanDecided {
        proposal: WorkRelationReplanProposalV1,
        disposition: WorkProposalDispositionV1,
        decided_at: UtcMicros,
    },
    TaskRelationsReplanned {
        proposal_id: ProposalId,
        applied_at: UtcMicros,
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
    AcceptedAttemptLinked {
        task_id: TaskId,
        based_on_version: WorkGraphVersionV1,
        identity: WorkAttemptIdentityV1,
        evidence: TaskEvidenceLinkV1,
        linked_at: UtcMicros,
    },
    TaskAccepted {
        task_id: TaskId,
        evidence_by_criterion: BTreeMap<AcceptanceCriterionId, TaskEvidenceLinkId>,
        accepted_at: UtcMicros,
    },
    HandoffRecorded {
        handoff: WorkHandoffV1,
    },
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductGraphV1 {
    version: WorkGraphVersionV1,
    initiatives: Vec<WorkInitiativeV1>,
    plans: Vec<WorkPlanV1>,
    milestones: Vec<WorkMilestoneV1>,
    items: Vec<WorkItemV1>,
    proposal_decisions: Vec<WorkProposalDecisionV1>,
    relation_replan_decisions: Vec<WorkRelationReplanDecisionV1>,
    evidence: Vec<TaskEvidenceLinkV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UncheckedWorkProductGraphV1 {
    version: WorkGraphVersionV1,
    initiatives: Vec<WorkInitiativeV1>,
    plans: Vec<WorkPlanV1>,
    milestones: Vec<WorkMilestoneV1>,
    items: Vec<WorkItemV1>,
    proposal_decisions: Vec<WorkProposalDecisionV1>,
    relation_replan_decisions: Vec<WorkRelationReplanDecisionV1>,
    evidence: Vec<TaskEvidenceLinkV1>,
}

impl<'de> Deserialize<'de> for WorkProductGraphV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let unchecked = UncheckedWorkProductGraphV1::deserialize(deserializer)?;
        Self::from_parts(unchecked).map_err(serde::de::Error::custom)
    }
}

impl WorkProductGraphV1 {
    pub fn new(
        version: WorkGraphVersionV1,
        initiatives: Vec<WorkInitiativeV1>,
        plans: Vec<WorkPlanV1>,
        milestones: Vec<WorkMilestoneV1>,
        items: Vec<WorkItemV1>,
    ) -> Result<Self, WorkProductContractError> {
        Self::from_parts(UncheckedWorkProductGraphV1 {
            version,
            initiatives,
            plans,
            milestones,
            items,
            proposal_decisions: Vec::new(),
            relation_replan_decisions: Vec::new(),
            evidence: Vec::new(),
        })
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
            relations.extend(item.accepted_attempts().iter().map(|(identity, link_id)| {
                WorkProductRelationV1::AcceptedAttempt {
                    task_id: item.task_id().clone(),
                    identity: identity.clone(),
                    link_id: link_id.clone(),
                }
            }));
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
        relations.extend(self.relation_replan_decisions.iter().map(|decision| {
            WorkProductRelationV1::ProposalDecision {
                task_id: decision.proposal.task_id.clone(),
                proposal_id: decision.proposal.proposal_id.clone(),
            }
        }));
        relations.sort();
        relations
    }

    pub fn apply(mut self, change: WorkGraphChangeV1) -> Result<Self, WorkProductContractError> {
        match change {
            WorkGraphChangeV1::TaskAdded { item } => self.items.push(*item),
            WorkGraphChangeV1::RelationReplanDecided {
                proposal,
                disposition,
                decided_at,
            } => {
                if proposal.based_on_version != self.version
                    || self
                        .relation_replan_decisions
                        .iter()
                        .any(|decision| decision.proposal.proposal_id == proposal.proposal_id)
                    || self
                        .proposal_decisions
                        .iter()
                        .any(|decision| decision.proposal().proposal_id() == &proposal.proposal_id)
                {
                    return Err(WorkProductContractError::ProposalMismatch);
                }
                proposal.validate()?;
                let tasks = self
                    .items
                    .iter()
                    .map(WorkItemV1::task_id)
                    .collect::<BTreeSet<_>>();
                if !tasks.contains(&proposal.task_id)
                    || proposal
                        .dependencies()
                        .iter()
                        .chain(proposal.informational_relations())
                        .chain(proposal.causal_candidates())
                        .any(|related| !tasks.contains(related))
                {
                    return Err(WorkProductContractError::UnknownTask);
                }
                if decided_at
                    < self
                        .item(&proposal.task_id)
                        .ok_or(WorkProductContractError::UnknownTask)?
                        .updated_at()
                {
                    return Err(WorkProductContractError::InvalidTime);
                }
                let mut proposed_items = self.items.clone();
                let item = proposed_items
                    .iter_mut()
                    .find(|item| item.task_id() == &proposal.task_id)
                    .ok_or(WorkProductContractError::UnknownTask)?;
                item.input.dependencies = proposal.dependencies.iter().cloned().collect();
                validate_acyclic(&proposed_items)?;
                self.relation_replan_decisions
                    .push(WorkRelationReplanDecisionV1 {
                        proposal,
                        disposition,
                        decided_at,
                    });
            }
            WorkGraphChangeV1::TaskRelationsReplanned {
                proposal_id,
                applied_at,
            } => {
                let proposal = self
                    .relation_replan_decisions
                    .iter()
                    .find(|decision| {
                        decision.proposal.proposal_id == proposal_id
                            && decision.disposition == WorkProposalDispositionV1::Accepted
                            && decision
                                .proposal
                                .based_on_version
                                .next()
                                .ok()
                                .is_some_and(|version| version == self.version)
                    })
                    .map(|decision| &decision.proposal)
                    .cloned()
                    .ok_or(WorkProductContractError::ProposalMismatch)?;
                let item = self.item_mut(&proposal.task_id)?;
                if applied_at < item.updated_at() {
                    return Err(WorkProductContractError::InvalidTime);
                }
                item.input.dependencies = proposal.dependencies;
                item.input.informational_relations = proposal.informational_relations;
                item.input.causal_candidates = proposal.causal_candidates;
                item.input.updated_at = applied_at;
            }
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
                if decided_at
                    < self
                        .item(&proposal.task_id)
                        .ok_or(WorkProductContractError::UnknownTask)?
                        .updated_at()
                {
                    return Err(WorkProductContractError::InvalidTime);
                }
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
                if accepted_at < parent.updated_at() {
                    return Err(WorkProductContractError::InvalidTime);
                }
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
            WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id,
                based_on_version,
                identity,
                evidence,
                linked_at,
            } => {
                if identity.task_id() != &task_id {
                    return Err(WorkProductContractError::IllegalTransition);
                }
                if based_on_version != self.version {
                    return Err(WorkProductContractError::IllegalTransition);
                }
                let evidence = TaskEvidenceLinkV1::new(
                    evidence.link_id().clone(),
                    evidence.revision(),
                    evidence.task_id().clone(),
                    evidence.anchor_id().clone(),
                    evidence.evidence_digest().clone(),
                    evidence.observed_at(),
                )?;
                if evidence.task_id() != &task_id {
                    return Err(WorkProductContractError::EvidenceTaskMismatch);
                }
                if evidence.observed_at() > linked_at {
                    return Err(WorkProductContractError::InvalidTime);
                }
                if self
                    .evidence
                    .iter()
                    .any(|prior| prior.link_id() == evidence.link_id())
                {
                    return Err(WorkProductContractError::DuplicateIdentity);
                }
                let item = self.item_mut(&task_id)?;
                if linked_at < item.updated_at() {
                    return Err(WorkProductContractError::InvalidTime);
                }
                if item.accepted_attempts.contains_key(&identity) {
                    return Err(WorkProductContractError::DuplicateIdentity);
                }
                item.accepted_attempts
                    .insert(identity, evidence.link_id().clone());
                item.evidence_links.insert(evidence.link_id().clone());
                item.input.updated_at = linked_at;
                self.evidence.push(evidence);
            }
            WorkGraphChangeV1::TaskAccepted {
                task_id,
                evidence_by_criterion,
                accepted_at,
            } => {
                let item = self.item_mut(&task_id)?;
                if accepted_at < item.updated_at() {
                    return Err(WorkProductContractError::InvalidTime);
                }
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
                if handed_off_at < item.updated_at() {
                    return Err(WorkProductContractError::InvalidTime);
                }
                item.input.updated_at = handed_off_at;
                item.handoffs.push(handoff);
            }
        }
        self.version = self.version.next()?;
        self.items
            .sort_by(|left, right| left.task_id().cmp(right.task_id()));
        self.evidence
            .sort_by(|left, right| left.link_id().cmp(right.link_id()));
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

    pub fn validate(&self) -> Result<(), WorkProductContractError> {
        if self.items.len() > MAX_WORK_PRODUCT_ITEMS {
            return Err(WorkProductContractError::GraphTooLarge);
        }
        ensure_unique(self.initiatives.iter().map(|value| value.id()))?;
        ensure_unique(self.plans.iter().map(|value| value.id()))?;
        ensure_unique(self.milestones.iter().map(|value| value.id()))?;
        ensure_unique(self.items.iter().map(WorkItemV1::task_id))?;
        ensure_unique(self.evidence.iter().map(TaskEvidenceLinkV1::link_id))?;
        ensure_unique(
            self.proposal_decisions
                .iter()
                .map(|decision| decision.proposal().proposal_id())
                .chain(
                    self.relation_replan_decisions
                        .iter()
                        .map(|decision| &decision.proposal.proposal_id),
                ),
        )?;
        for item in &self.items {
            validate_item_state(item)?;
        }

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
        for decision in &self.relation_replan_decisions {
            let proposal = &decision.proposal;
            proposal.validate()?;
            if proposal.based_on_version >= self.version {
                return Err(WorkProductContractError::ProposalMismatch);
            }
            if !tasks.contains(&proposal.task_id)
                || proposal
                    .dependencies()
                    .iter()
                    .chain(proposal.informational_relations())
                    .chain(proposal.causal_candidates())
                    .any(|related| !tasks.contains(related))
            {
                return Err(WorkProductContractError::UnknownTask);
            }
        }
        if self
            .evidence
            .iter()
            .any(|link| !tasks.contains(link.task_id()))
        {
            return Err(WorkProductContractError::EvidenceTaskMismatch);
        }
        if self.items.iter().any(|item| {
            item.evidence_links.iter().any(|link_id| {
                !self
                    .evidence
                    .iter()
                    .any(|link| link.link_id() == link_id && link.task_id() == item.task_id())
            })
        }) || self.evidence.iter().any(|link| {
            self.item(link.task_id())
                .is_none_or(|item| !item.evidence_links.contains(link.link_id()))
        }) {
            return Err(WorkProductContractError::EvidenceTaskMismatch);
        }
        validate_acyclic(&self.items)
    }

    fn from_parts(parts: UncheckedWorkProductGraphV1) -> Result<Self, WorkProductContractError> {
        let UncheckedWorkProductGraphV1 {
            version,
            mut initiatives,
            mut plans,
            mut milestones,
            mut items,
            mut proposal_decisions,
            mut relation_replan_decisions,
            mut evidence,
        } = parts;
        initiatives.sort_by(|left, right| left.id.cmp(&right.id));
        plans.sort_by(|left, right| left.id.cmp(&right.id));
        milestones.sort_by(|left, right| left.id.cmp(&right.id));
        items.sort_by(|left, right| left.input.task_id.cmp(&right.input.task_id));
        proposal_decisions.sort_by(|left, right| {
            left.proposal
                .proposal_id
                .cmp(&right.proposal.proposal_id)
                .then_with(|| left.decided_at.cmp(&right.decided_at))
        });
        relation_replan_decisions.sort_by(|left, right| {
            left.proposal
                .proposal_id
                .cmp(&right.proposal.proposal_id)
                .then_with(|| left.decided_at.cmp(&right.decided_at))
        });
        evidence.sort_by(|left, right| left.link_id.cmp(&right.link_id));
        let graph = Self {
            version,
            initiatives,
            plans,
            milestones,
            items,
            proposal_decisions,
            relation_replan_decisions,
            evidence,
        };
        graph.validate()?;
        Ok(graph)
    }
}

fn validate_item_state(item: &WorkItemV1) -> Result<(), WorkProductContractError> {
    WorkItemV1::new(item.input.clone())?;
    if item.accepted_proposal.is_some() != item.accepted_route.is_some()
        || item.accepted_attempts.iter().any(|(identity, link_id)| {
            identity.task_id() != item.task_id() || !item.evidence_links.contains(link_id)
        })
        || item
            .handoffs
            .iter()
            .any(|handoff| handoff.task_id() != item.task_id())
    {
        return Err(WorkProductContractError::IllegalTransition);
    }
    let required = item
        .acceptance_criteria()
        .iter()
        .filter(|criterion| criterion.evidence_required())
        .map(WorkAcceptanceCriterionV1::criterion_id)
        .collect::<BTreeSet<_>>();
    let acceptance_is_valid = item.accepted_at.is_some()
        && item.accepted_criteria.keys().collect::<BTreeSet<_>>() == required
        && item
            .accepted_criteria
            .values()
            .all(|link_id| item.evidence_links.contains(link_id));
    if (!item.accepted_criteria.is_empty() || item.accepted_at.is_some()) && !acceptance_is_valid {
        return Err(WorkProductContractError::AcceptanceUnsatisfied);
    }
    Ok(())
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

fn relation_replan_digest(
    task_id: &TaskId,
    version: WorkGraphVersionV1,
    dependencies: &BTreeSet<TaskId>,
    informational: &BTreeSet<TaskId>,
    causal: &BTreeSet<TaskId>,
) -> Result<ManifestDigest, WorkProductContractError> {
    crate::canonical_sha256(&(
        "tracedecay.work-product.relation-replan.v1",
        task_id,
        version,
        dependencies,
        informational,
        causal,
    ))
    .map_err(|_| WorkProductContractError::ProposalMismatch)
}
