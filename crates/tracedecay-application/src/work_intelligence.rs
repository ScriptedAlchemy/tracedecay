//! Governed, read-only intelligence over the canonical Work product graph.
//!
//! These operations do not learn, rank people, or mutate Work. Experience is
//! a bounded selection of already-authorized, anchored outcomes from the exact
//! graph revision the caller names. Proposal comparison reads two exact
//! verified revisions and returns both sides plus their structural delta.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest, ProposalId, ProviderId,
    TaskEvidenceLinkV1, TaskId, UtcMicros, WorkGraphVersionV1, WorkItemV1, WorkProductRelationV1,
    WorkProposalV1, WorkProviderRouteId, WorkProviderRouteV1, WorkRouteDecisionV1,
    WorkRuntimeProjectionCoverageV1, WorkRuntimeProjectionV1, WorkScoreKindV1,
    WorkShapeAssessmentV1, WorkSizingV1, canonical_sha256,
    configuration::{
        ConfigurationSnapshotV1, ConfigurationValueV1, PROJECT_WORK_EXPERTISE_CONSENT_SETTING_KEY,
        SettingKey, USER_WORK_EXPERTISE_CONSENT_SETTING_KEY, WorkExpertiseCategoryV1,
        WorkExpertiseConsentV1,
    },
};
use tracedecay_policy::work_loop::{
    WorkEvidenceFrontierV1, WorkPriorOutcomeV1, WorkProposalCancellationV1, WorkProposalDecisionV1,
    WorkProposalEvaluator, WorkProposalEvaluatorV1, WorkProposalPolicyInputV1,
    WorkProposalReasonV1, WorkProposalRuntimeCoverageV1, WorkRouteCandidateV1,
};

use crate::{
    CancellationState, RequestAdmission, RequestContext, VerifiedWorkEvidenceRootV1,
    VerifiedWorkGraphVersionV1, WorkEvidenceRootReadErrorV1, WorkEvidenceRootReadPortV1,
    WorkGraphReadModeV1, WorkGraphReadPortErrorV1, WorkGraphReadPortV1, WorkGraphReadRequestV1,
    WorkGraphReadV1, WorkProductApplicationErrorV1, WorkProductBindingV1,
    WorkProductOwnerAuthorizationErrorV1, WorkProductOwnerAuthorizationPortV1,
    WorkProductPortContextV1, WorkProductSelectionScopeV1, WorkRoutingSnapshotErrorV1,
    WorkRoutingSnapshotPortV1,
};

pub const MAX_WORK_EXPERIENCE_CANDIDATES_V1: u32 = 100;

/// Read-only proposal generation over one exact current product graph.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerateProposalRequest {
    pub selection: WorkProductSelectionScopeV1,
    pub task_id: TaskId,
    pub proposal_id: ProposalId,
    #[serde(default)]
    pub live_git_evidence: Option<WorkEvidenceFrontierV1>,
    pub occurred_at: UtcMicros,
}

/// A canonical product proposal and the exact verified graph that licensed it.
///
/// `proposal` can be moved directly into a `DecideWorkProposalRequestV1`;
/// callers use `verified_graph_version` to construct that mutation's CAS pin.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GeneratedWorkProposal {
    pub proposal: WorkProposalV1,
    pub verified_graph_version: VerifiedWorkGraphVersionV1,
    pub decision: WorkProposalDecisionV1,
    pub calibration: WorkCalibrationEvidenceV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkCalibrationUncertaintyV1 {
    Supported,
    Sparse,
    Stale,
    Incomparable,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkCalibrationProvenanceV1 {
    pub evaluator_id: String,
    pub evaluator_revision: u64,
    pub input_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub configuration_revision: Option<ConfigurationRevisionId>,
    pub local_evidence: Option<WorkEvidenceFrontierV1>,
    pub evaluated_at: UtcMicros,
}

/// Raw calibration values and their exact decision provenance.
///
/// No rate, probability, or composite score is derived here. Consumers see
/// the authority-supplied outcome rows, categorical uncertainty, and exact
/// denominator counts that produced the proposal's existing calibrated sizing.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkCalibrationEvidenceV1 {
    pub cohort_route: Option<String>,
    pub raw_outcomes: Vec<WorkPriorOutcomeV1>,
    pub eligible_route_count: u32,
    pub routes_with_outcomes: u32,
    pub comparable_outcomes: u32,
    pub incomparable_outcomes: u32,
    pub uncertainty: WorkCalibrationUncertaintyV1,
    pub provenance: WorkCalibrationProvenanceV1,
}

pub(crate) fn calibration_evidence(
    input: &WorkProposalPolicyInputV1,
    decision: &WorkProposalDecisionV1,
) -> Result<WorkCalibrationEvidenceV1, WorkProductApplicationErrorV1> {
    let cohort_route = decision
        .route_plan
        .as_ref()
        .and_then(|plan| plan.ranked.first())
        .map(|route| route.route_id.clone());
    let comparable_outcomes = input
        .prior_outcomes
        .iter()
        .filter(|outcome| outcome.observed_at <= input.evaluated_at)
        .count();
    let incomparable_outcomes = input
        .prior_outcomes
        .len()
        .saturating_sub(comparable_outcomes);
    let routes_with_outcomes = input
        .prior_outcomes
        .iter()
        .filter(|outcome| outcome.observed_at <= input.evaluated_at)
        .map(|outcome| outcome.route_id.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let runtime_coverage_incomparable = decision.ordered_reason_codes.iter().any(|reason| {
        matches!(
            reason,
            WorkProposalReasonV1::RuntimeCoveragePartial
                | WorkProposalReasonV1::RuntimeCoverageUnavailable
        )
    });
    let uncertainty = if incomparable_outcomes > 0 || runtime_coverage_incomparable {
        WorkCalibrationUncertaintyV1::Incomparable
    } else if decision
        .ordered_reason_codes
        .contains(&WorkProposalReasonV1::RouteEvidenceStale)
    {
        WorkCalibrationUncertaintyV1::Stale
    } else if decision
        .ordered_reason_codes
        .contains(&WorkProposalReasonV1::RouteEvidenceSparse)
        || decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::InsufficientCalibrationSupport)
    {
        WorkCalibrationUncertaintyV1::Sparse
    } else {
        WorkCalibrationUncertaintyV1::Supported
    };
    Ok(WorkCalibrationEvidenceV1 {
        cohort_route,
        raw_outcomes: input.prior_outcomes.clone(),
        eligible_route_count: bounded(input.eligible_routes.len())?,
        routes_with_outcomes: bounded(routes_with_outcomes)?,
        comparable_outcomes: bounded(comparable_outcomes)?,
        incomparable_outcomes: bounded(incomparable_outcomes)?,
        uncertainty,
        provenance: WorkCalibrationProvenanceV1 {
            evaluator_id: decision.evaluator_id.as_str().to_owned(),
            evaluator_revision: decision.evaluator_revision,
            input_digest: decision.input_digest.clone(),
            configuration_digest: decision.configuration_digest.clone(),
            configuration_revision: decision.configuration_revision.clone(),
            local_evidence: decision.local_evidence.clone(),
            evaluated_at: input.evaluated_at,
        },
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExperienceRequestV1 {
    pub selection: WorkProductSelectionScopeV1,
    pub task_id: TaskId,
    pub verified_version: VerifiedWorkGraphVersionV1,
    /// Evidence before this owner-supplied horizon is excluded as stale.
    pub evidence_not_before: UtcMicros,
    /// Categories for which the returned context will be used ephemerally.
    pub expertise_categories: BTreeSet<WorkExpertiseCategoryV1>,
    pub limit: u32,
    pub observed_at: UtcMicros,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkExperienceApplicabilityV1 {
    SameAcceptedRoute,
    SameMilestone,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExperienceCandidateV1 {
    pub item: WorkItemV1,
    /// Exact anchored evidence establishing that this is observed experience.
    pub evidence: Vec<TaskEvidenceLinkV1>,
    /// Separate declared applicability facts. This is intentionally not a
    /// score and candidates remain in canonical TaskId order.
    pub applicability: BTreeSet<WorkExperienceApplicabilityV1>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkExperienceCoverageV1 {
    Unavailable,
    Complete {
        returned: u32,
        applicable: u32,
        stale_excluded: u32,
    },
    Partial {
        returned: u32,
        applicable: u32,
        stale_excluded: u32,
        omitted_by_limit: u32,
    },
}

#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkExpertiseUnavailableReasonV1 {
    UserConsentDisabled,
    ProjectConsentDisabled,
    UserConsentNotYetEffective,
    ProjectConsentNotYetEffective,
    UserConsentExpired,
    ProjectConsentExpired,
    RequestedCategoryNotAllowed,
}

#[derive(
    Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkExpertiseLegalActionV1 {
    GrantUserConsent,
    GrantProjectConsent,
    RenewUserConsent,
    RenewProjectConsent,
    AllowRequestedCategories,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkExpertiseContextDurabilityV1 {
    /// Context is returned for this read only and cannot establish evidence,
    /// routing, proposal acceptance, execution admission, or completion.
    EphemeralOnly,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExpertiseConsentPinV1 {
    pub configuration_revision: ConfigurationRevisionId,
    pub configuration_snapshot: ConfigurationSnapshotId,
    pub configuration_digest: ManifestDigest,
    pub provenance_digest: ManifestDigest,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "availability", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkExpertiseAuthorizationV1 {
    Available {
        categories: BTreeSet<WorkExpertiseCategoryV1>,
        expires_at: UtcMicros,
        pin: WorkExpertiseConsentPinV1,
        durability: WorkExpertiseContextDurabilityV1,
    },
    Unavailable {
        reasons: BTreeSet<WorkExpertiseUnavailableReasonV1>,
        legal_actions: BTreeSet<WorkExpertiseLegalActionV1>,
        pin: WorkExpertiseConsentPinV1,
    },
}

/// Exact configuration revision consumed by one Work experience read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkExpertiseConsentSnapshotV1 {
    revision_id: ConfigurationRevisionId,
    snapshot: ConfigurationSnapshotV1,
    user: WorkExpertiseConsentV1,
    project: WorkExpertiseConsentV1,
}

impl WorkExpertiseConsentSnapshotV1 {
    pub fn from_configuration(
        revision_id: ConfigurationRevisionId,
        snapshot: ConfigurationSnapshotV1,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        snapshot
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::RevisionConflict)?;
        let user = expertise_consent_value(&snapshot, USER_WORK_EXPERTISE_CONSENT_SETTING_KEY)?;
        let project =
            expertise_consent_value(&snapshot, PROJECT_WORK_EXPERTISE_CONSENT_SETTING_KEY)?;
        Ok(Self {
            revision_id,
            snapshot,
            user,
            project,
        })
    }

    fn authorization(
        &self,
        categories: &BTreeSet<WorkExpertiseCategoryV1>,
        observed_at: UtcMicros,
    ) -> Result<WorkExpertiseAuthorizationV1, WorkProductApplicationErrorV1> {
        let pin = WorkExpertiseConsentPinV1 {
            configuration_revision: self.revision_id.clone(),
            configuration_snapshot: self.snapshot.snapshot_id.clone(),
            configuration_digest: self.snapshot.effective_behavior_digest.clone(),
            provenance_digest: self.snapshot.resolution_provenance_digest.clone(),
        };
        let mut reasons = BTreeSet::new();
        let mut legal_actions = BTreeSet::new();
        assess_consent(
            &self.user,
            observed_at,
            WorkExpertiseUnavailableReasonV1::UserConsentDisabled,
            WorkExpertiseUnavailableReasonV1::UserConsentNotYetEffective,
            WorkExpertiseUnavailableReasonV1::UserConsentExpired,
            WorkExpertiseLegalActionV1::GrantUserConsent,
            WorkExpertiseLegalActionV1::RenewUserConsent,
            &mut reasons,
            &mut legal_actions,
        );
        assess_consent(
            &self.project,
            observed_at,
            WorkExpertiseUnavailableReasonV1::ProjectConsentDisabled,
            WorkExpertiseUnavailableReasonV1::ProjectConsentNotYetEffective,
            WorkExpertiseUnavailableReasonV1::ProjectConsentExpired,
            WorkExpertiseLegalActionV1::GrantProjectConsent,
            WorkExpertiseLegalActionV1::RenewProjectConsent,
            &mut reasons,
            &mut legal_actions,
        );
        if !categories.is_subset(&self.user.allowed_categories)
            || !categories.is_subset(&self.project.allowed_categories)
        {
            reasons.insert(WorkExpertiseUnavailableReasonV1::RequestedCategoryNotAllowed);
            legal_actions.insert(WorkExpertiseLegalActionV1::AllowRequestedCategories);
        }
        if !reasons.is_empty() {
            return Ok(WorkExpertiseAuthorizationV1::Unavailable {
                reasons,
                legal_actions,
                pin,
            });
        }
        let Some((user_expires_at, project_expires_at)) =
            self.user.expires_at.zip(self.project.expires_at)
        else {
            return Err(WorkProductApplicationErrorV1::RevisionConflict);
        };
        Ok(WorkExpertiseAuthorizationV1::Available {
            categories: categories.clone(),
            expires_at: user_expires_at.min(project_expires_at),
            pin,
            durability: WorkExpertiseContextDurabilityV1::EphemeralOnly,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExperienceV1 {
    pub task_id: TaskId,
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub evidence_not_before: UtcMicros,
    pub observed_at: UtcMicros,
    pub expertise: WorkExpertiseAuthorizationV1,
    pub candidates: Vec<WorkExperienceCandidateV1>,
    pub coverage: WorkExperienceCoverageV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProposalComparisonRequestV1 {
    pub selection: WorkProductSelectionScopeV1,
    pub task_id: TaskId,
    pub old_version: VerifiedWorkGraphVersionV1,
    pub new_version: VerifiedWorkGraphVersionV1,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProposalComparisonEffectV1 {
    /// Comparison is evidence only. There is no apply edge from this result.
    AdvisoryOnly,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProposalComparisonV1 {
    pub task_id: TaskId,
    pub old: VerifiedWorkEvidenceRootV1,
    pub new: VerifiedWorkEvidenceRootV1,
    pub added_relations: Vec<WorkProductRelationV1>,
    pub removed_relations: Vec<WorkProductRelationV1>,
    pub added_evidence: Vec<TaskEvidenceLinkV1>,
    pub removed_evidence: Vec<TaskEvidenceLinkV1>,
    pub item_changed: bool,
    pub effect: WorkProposalComparisonEffectV1,
}

pub struct WorkIntelligenceServiceV1<G, A> {
    graph: G,
    owner_authority: A,
    binding: WorkProductBindingV1,
}

impl<G, A> WorkIntelligenceServiceV1<G, A>
where
    G: WorkGraphReadPortV1,
    A: WorkProductOwnerAuthorizationPortV1,
{
    pub const fn new(graph: G, owner_authority: A, binding: WorkProductBindingV1) -> Self {
        Self {
            graph,
            owner_authority,
            binding,
        }
    }

    pub fn generate_proposal(
        &self,
        context: &RequestContext,
        configuration_digest: ManifestDigest,
        routing_authority: &dyn WorkRoutingSnapshotPortV1,
        request: GenerateProposalRequest,
    ) -> Result<GeneratedWorkProposal, WorkProductApplicationErrorV1> {
        let (authorized_scope, port_context) =
            self.authorize(context, &request.selection, request.occurred_at)?;
        let read = self
            .graph
            .read_graph(
                &port_context,
                &WorkGraphReadRequestV1::current(request.selection.clone(), request.occurred_at),
            )
            .map_err(graph_error)?;
        if read.authorized_scope() != &authorized_scope {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        }
        let WorkGraphReadV1::Current { snapshot, .. } = read else {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        };
        let graph = snapshot.graph();
        let runtime = snapshot.runtime();
        graph
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::GraphAuthorityUnavailable)?;
        runtime
            .validate(graph, snapshot.projected_at())
            .map_err(|_| WorkProductApplicationErrorV1::GraphAuthorityUnavailable)?;
        let item = graph
            .item(&request.task_id)
            .ok_or(WorkProductApplicationErrorV1::NotFoundOrNotAuthorized)?;
        let unresolved_dependency_count = item
            .dependencies()
            .iter()
            .filter(|dependency| {
                graph
                    .item(dependency)
                    .is_none_or(|item| !item.is_accepted())
            })
            .count();
        let runtime_coverage = proposal_runtime_coverage(runtime, &request.task_id)?;
        let routing = routing_authority
            .routing_snapshot(context, &request.task_id)
            .map_err(routing_error)?
            .canonicalize();
        let local_digest = canonical_sha256(&(
            "tracedecay.application.work-product-proposal-local-evidence.v1",
            snapshot.verified_version(),
            graph,
            runtime,
        ))
        .map_err(|_| WorkProductApplicationErrorV1::GraphAuthorityUnavailable)?;
        let input = WorkProposalPolicyInputV1 {
            task_id: request.task_id.clone(),
            based_on_version: graph.version().get(),
            dependency_count: bounded(item.dependencies().len())?,
            unresolved_dependency_count: bounded(unresolved_dependency_count)?,
            accepted_proposal_present: item.accepted_proposal().is_some(),
            execution_admitted: item.is_execution_admitted(),
            task_accepted: item.is_accepted(),
            runtime: runtime_coverage,
            local_evidence: Some(WorkEvidenceFrontierV1 {
                watermark: snapshot.projected_at(),
                digest: local_digest,
            }),
            live_git_evidence: request.live_git_evidence,
            policy_revision: context.grant().revision,
            policy_digest: context.grant().digest.clone(),
            configuration_digest,
            configuration_revision: routing.configuration_revision.clone(),
            deadline: context.deadline().expires_at,
            cancellation: match context.cancellation().state {
                CancellationState::Active => WorkProposalCancellationV1::Active,
                CancellationState::Cancelled { requested_at } => {
                    WorkProposalCancellationV1::Cancelled { requested_at }
                }
            },
            evaluated_at: request.occurred_at,
            eligible_routes: routing.eligible_routes.clone(),
            budget: routing.budget,
            content_location: routing.content_location,
            prior_outcomes: routing.prior_outcomes,
            human_override: routing.human_override,
        };
        let decision = WorkProposalEvaluatorV1::default().evaluate(&input);
        let calibration = calibration_evidence(&input, &decision)?;
        let proposal = canonical_product_proposal(
            request.proposal_id,
            item,
            graph.version(),
            &routing.eligible_routes,
            &decision,
        )?;
        Ok(GeneratedWorkProposal {
            proposal,
            verified_graph_version: snapshot.verified_version().clone(),
            decision,
            calibration,
        })
    }

    pub fn experience(
        &self,
        context: &RequestContext,
        request: WorkExperienceRequestV1,
        consent: WorkExpertiseConsentSnapshotV1,
    ) -> Result<WorkExperienceV1, WorkProductApplicationErrorV1> {
        validate_experience_request(&request)?;
        let (authorized_scope, port_context) =
            self.authorize(context, &request.selection, request.observed_at)?;
        let graph_request = WorkGraphReadRequestV1 {
            selection: request.selection.clone(),
            mode: WorkGraphReadModeV1::Current,
            continuation: None,
            observed_at: request.observed_at,
        };
        let graph_read = self
            .graph
            .read_graph(&port_context, &graph_request)
            .map_err(graph_error)?;
        if graph_read.authorized_scope() != &authorized_scope {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        }
        let WorkGraphReadV1::Current { snapshot, .. } = graph_read else {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        };
        if snapshot.verified_version() != &request.verified_version {
            return Err(WorkProductApplicationErrorV1::VersionConflict);
        }
        let graph = snapshot.graph();
        let target = graph
            .item(&request.task_id)
            .ok_or(WorkProductApplicationErrorV1::NotFoundOrNotAuthorized)?;
        let expertise =
            consent.authorization(&request.expertise_categories, request.observed_at)?;
        if matches!(expertise, WorkExpertiseAuthorizationV1::Unavailable { .. }) {
            return Ok(WorkExperienceV1 {
                task_id: request.task_id,
                verified_version: request.verified_version,
                evidence_not_before: request.evidence_not_before,
                observed_at: request.observed_at,
                expertise,
                candidates: Vec::new(),
                coverage: WorkExperienceCoverageV1::Unavailable,
            });
        }

        let mut candidates = Vec::new();
        let mut stale_excluded = 0u32;
        for item in graph.items() {
            if item.task_id() == target.task_id() || !item.is_accepted() || item.is_archived() {
                continue;
            }
            let mut applicability = BTreeSet::new();
            if target
                .accepted_route()
                .and_then(|route| route.recommended())
                .zip(item.accepted_route().and_then(|route| route.recommended()))
                .is_some_and(|(left, right)| left == right)
            {
                applicability.insert(WorkExperienceApplicabilityV1::SameAcceptedRoute);
            }
            if target.hierarchy().milestone_id() == item.hierarchy().milestone_id() {
                applicability.insert(WorkExperienceApplicabilityV1::SameMilestone);
            }
            if applicability.is_empty() {
                continue;
            }
            let evidence = graph
                .evidence()
                .iter()
                .filter(|link| {
                    link.task_id() == item.task_id()
                        && link.observed_at() >= request.evidence_not_before
                        && link.observed_at() <= request.observed_at
                })
                .cloned()
                .collect::<Vec<_>>();
            if evidence.is_empty() {
                stale_excluded = stale_excluded.saturating_add(1);
                continue;
            }
            candidates.push(WorkExperienceCandidateV1 {
                item: item.clone(),
                evidence,
                applicability,
            });
        }
        candidates.sort_by(|left, right| left.item.task_id().cmp(right.item.task_id()));
        let applicable = bounded(candidates.len())?
            .checked_add(stale_excluded)
            .ok_or(WorkProductApplicationErrorV1::GraphAuthorityUnavailable)?;
        let limit = usize::try_from(request.limit)
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        let omitted = candidates.len().saturating_sub(limit);
        candidates.truncate(limit);
        let returned = bounded(candidates.len())?;
        let coverage = if omitted == 0 {
            WorkExperienceCoverageV1::Complete {
                returned,
                applicable,
                stale_excluded,
            }
        } else {
            WorkExperienceCoverageV1::Partial {
                returned,
                applicable,
                stale_excluded,
                omitted_by_limit: bounded(omitted)?,
            }
        };
        Ok(WorkExperienceV1 {
            task_id: request.task_id,
            verified_version: request.verified_version,
            evidence_not_before: request.evidence_not_before,
            observed_at: request.observed_at,
            expertise,
            candidates,
            coverage,
        })
    }

    pub fn compare_proposal(
        &self,
        context: &RequestContext,
        request: WorkProposalComparisonRequestV1,
    ) -> Result<WorkProposalComparisonV1, WorkProductApplicationErrorV1>
    where
        G: WorkEvidenceRootReadPortV1,
    {
        if request.old_version.graph_version() >= request.new_version.graph_version() {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        let (_, port_context) = self.authorize(context, &request.selection, request.observed_at)?;
        let old = self
            .graph
            .read_evidence_root(&port_context, &request.task_id, &request.old_version)
            .map_err(root_error)?;
        let new = self
            .graph
            .read_evidence_root(&port_context, &request.task_id, &request.new_version)
            .map_err(root_error)?;
        if old.verified_version != request.old_version
            || new.verified_version != request.new_version
            || old.item.task_id() != &request.task_id
            || new.item.task_id() != &request.task_id
        {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        }
        let old_relations = old.relations.iter().cloned().collect::<BTreeSet<_>>();
        let new_relations = new.relations.iter().cloned().collect::<BTreeSet<_>>();
        let added_relations = new_relations.difference(&old_relations).cloned().collect();
        let removed_relations = old_relations.difference(&new_relations).cloned().collect();
        let added_evidence = evidence_difference(&new.links, &old.links);
        let removed_evidence = evidence_difference(&old.links, &new.links);
        let item_changed = old.item != new.item;
        Ok(WorkProposalComparisonV1 {
            task_id: request.task_id,
            old,
            new,
            added_relations,
            removed_relations,
            added_evidence,
            removed_evidence,
            item_changed,
            effect: WorkProposalComparisonEffectV1::AdvisoryOnly,
        })
    }

    fn authorize(
        &self,
        context: &RequestContext,
        selection: &WorkProductSelectionScopeV1,
        observed_at: UtcMicros,
    ) -> Result<
        (
            crate::AuthorizedWorkProductScopeV1,
            WorkProductPortContextV1,
        ),
        WorkProductApplicationErrorV1,
    > {
        if !context.allows(self.binding.capability_id(), self.binding.use_case_id()) {
            return Err(WorkProductApplicationErrorV1::NotAuthorized);
        }
        match context.admission_at(observed_at) {
            RequestAdmission::Admitted => {}
            RequestAdmission::Cancelled => return Err(WorkProductApplicationErrorV1::Cancelled),
            RequestAdmission::TimedOut => return Err(WorkProductApplicationErrorV1::TimedOut),
        }
        selection
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        let scope = self
            .owner_authority
            .authorize_scope(context, selection, observed_at)
            .map_err(|error| match error {
                WorkProductOwnerAuthorizationErrorV1::NotAuthorized => {
                    WorkProductApplicationErrorV1::NotAuthorized
                }
                WorkProductOwnerAuthorizationErrorV1::Unavailable => {
                    WorkProductApplicationErrorV1::GraphAuthorityUnavailable
                }
            })?;
        if scope.selection() != selection {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        }
        let port_context =
            WorkProductPortContextV1::from_request(context, scope.clone(), observed_at);
        Ok((scope, port_context))
    }
}

fn validate_experience_request(
    request: &WorkExperienceRequestV1,
) -> Result<(), WorkProductApplicationErrorV1> {
    if request.limit == 0
        || request.limit > MAX_WORK_EXPERIENCE_CANDIDATES_V1
        || request.evidence_not_before > request.observed_at
        || request.expertise_categories.is_empty()
    {
        return Err(WorkProductApplicationErrorV1::InvalidRequest);
    }
    Ok(())
}

fn expertise_consent_value(
    snapshot: &ConfigurationSnapshotV1,
    key: &'static str,
) -> Result<WorkExpertiseConsentV1, WorkProductApplicationErrorV1> {
    let key = SettingKey::new(key).map_err(|_| WorkProductApplicationErrorV1::RevisionConflict)?;
    let Some(ConfigurationValueV1::WorkExpertiseConsent(consent)) =
        snapshot.effective_values.get(&key)
    else {
        return Err(WorkProductApplicationErrorV1::RevisionConflict);
    };
    consent
        .validate()
        .map_err(|_| WorkProductApplicationErrorV1::RevisionConflict)?;
    Ok(consent.clone())
}

#[allow(clippy::too_many_arguments)]
fn assess_consent(
    consent: &WorkExpertiseConsentV1,
    observed_at: UtcMicros,
    disabled_reason: WorkExpertiseUnavailableReasonV1,
    not_yet_effective_reason: WorkExpertiseUnavailableReasonV1,
    expired_reason: WorkExpertiseUnavailableReasonV1,
    grant_action: WorkExpertiseLegalActionV1,
    renew_action: WorkExpertiseLegalActionV1,
    reasons: &mut BTreeSet<WorkExpertiseUnavailableReasonV1>,
    legal_actions: &mut BTreeSet<WorkExpertiseLegalActionV1>,
) {
    if !consent.enabled {
        reasons.insert(disabled_reason);
        legal_actions.insert(grant_action);
        return;
    }
    if consent
        .granted_at
        .is_some_and(|granted| granted > observed_at)
    {
        reasons.insert(not_yet_effective_reason);
        legal_actions.insert(renew_action);
    }
    if consent
        .expires_at
        .is_none_or(|expires| expires <= observed_at)
    {
        reasons.insert(expired_reason);
        legal_actions.insert(renew_action);
    }
}

fn evidence_difference(
    left: &[TaskEvidenceLinkV1],
    right: &[TaskEvidenceLinkV1],
) -> Vec<TaskEvidenceLinkV1> {
    left.iter()
        .filter(|candidate| !right.iter().any(|existing| existing == *candidate))
        .cloned()
        .collect()
}

fn proposal_runtime_coverage(
    runtime: &WorkRuntimeProjectionV1,
    task_id: &TaskId,
) -> Result<WorkProposalRuntimeCoverageV1, WorkProductApplicationErrorV1> {
    match runtime.coverage() {
        WorkRuntimeProjectionCoverageV1::Complete => {
            let attempts = runtime
                .attempts()
                .iter()
                .filter(|attempt| attempt.identity.task_id() == task_id)
                .collect::<Vec<_>>();
            Ok(WorkProposalRuntimeCoverageV1::Complete {
                attempt_count: bounded(attempts.len())?,
                terminal_attempt_count: bounded(
                    attempts
                        .iter()
                        .filter(|attempt| attempt.state.is_terminal())
                        .count(),
                )?,
            })
        }
        WorkRuntimeProjectionCoverageV1::Partial { .. } => {
            Ok(WorkProposalRuntimeCoverageV1::Partial)
        }
        WorkRuntimeProjectionCoverageV1::Unavailable => {
            Ok(WorkProposalRuntimeCoverageV1::Unavailable)
        }
    }
}

fn canonical_product_proposal(
    proposal_id: ProposalId,
    item: &WorkItemV1,
    based_on_version: WorkGraphVersionV1,
    candidates: &[WorkRouteCandidateV1],
    decision: &WorkProposalDecisionV1,
) -> Result<WorkProposalV1, WorkProductApplicationErrorV1> {
    let shape = WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 0, 0, 0, 0)
        .map_err(|_| WorkProductApplicationErrorV1::ProposalAuthorityUnavailable)?;
    let sizing = WorkSizingV1::new(
        WorkScoreKindV1::Ordinal,
        item.effort(),
        item.effort(),
        item.effort(),
        "declared_work_item_effort",
    )
    .map_err(|_| WorkProductApplicationErrorV1::ProposalAuthorityUnavailable)?;
    let explanation = format!(
        "policy disposition {:?}; reasons {:?}",
        decision.disposition, decision.ordered_reason_codes
    );
    let route = canonical_route_decision(candidates, decision, &explanation)?;
    WorkProposalV1::new(
        proposal_id,
        item.task_id().clone(),
        based_on_version,
        shape,
        sizing,
        Vec::new(),
        route,
        explanation,
        decision.input_digest.clone(),
    )
    .map_err(|_| WorkProductApplicationErrorV1::ProposalAuthorityUnavailable)
}

fn canonical_route_decision(
    candidates: &[WorkRouteCandidateV1],
    decision: &WorkProposalDecisionV1,
    abstention_reason: &str,
) -> Result<WorkRouteDecisionV1, WorkProductApplicationErrorV1> {
    let Some(plan) = decision.route_plan.as_ref() else {
        return WorkRouteDecisionV1::abstain(abstention_reason)
            .map_err(|_| WorkProductApplicationErrorV1::ProposalAuthorityUnavailable);
    };
    let Some(recommended) = plan.ranked.first() else {
        return WorkRouteDecisionV1::abstain(abstention_reason)
            .map_err(|_| WorkProductApplicationErrorV1::ProposalAuthorityUnavailable);
    };
    let recommended_route_id = recommended.route_id.clone();
    let recommended = product_route(candidates, &recommended_route_id)?;
    let alternatives = plan
        .ranked
        .iter()
        .skip(1)
        .map(|ranked| product_route(candidates, &ranked.route_id))
        .collect::<Result<Vec<_>, _>>()?;
    let exclusions = plan
        .exclusions
        .iter()
        .map(|excluded| excluded.route_id.clone())
        .collect();
    let fallback = plan
        .deterministic_baseline
        .clone()
        .unwrap_or(recommended_route_id);
    WorkRouteDecisionV1::selected(recommended, alternatives, exclusions, fallback)
        .map_err(|_| WorkProductApplicationErrorV1::ProposalAuthorityUnavailable)
}

fn product_route(
    candidates: &[WorkRouteCandidateV1],
    route_id: &str,
) -> Result<WorkProviderRouteV1, WorkProductApplicationErrorV1> {
    let candidate = candidates
        .iter()
        .find(|candidate| candidate.route_id == route_id)
        .ok_or(WorkProductApplicationErrorV1::ProposalAuthorityUnavailable)?;
    let provider = ProviderId::new(candidate.provider_capability_id.clone())
        .map_err(|_| WorkProductApplicationErrorV1::ProposalAuthorityUnavailable)?;
    let route = WorkProviderRouteId::new(candidate.route_id.clone())
        .map_err(|_| WorkProductApplicationErrorV1::ProposalAuthorityUnavailable)?;
    WorkProviderRouteV1::new(provider, route)
        .map_err(|_| WorkProductApplicationErrorV1::ProposalAuthorityUnavailable)
}

fn bounded(value: usize) -> Result<u32, WorkProductApplicationErrorV1> {
    u32::try_from(value).map_err(|_| WorkProductApplicationErrorV1::GraphAuthorityUnavailable)
}

fn graph_error(error: WorkGraphReadPortErrorV1) -> WorkProductApplicationErrorV1 {
    error.into()
}

fn routing_error(error: WorkRoutingSnapshotErrorV1) -> WorkProductApplicationErrorV1 {
    match error {
        WorkRoutingSnapshotErrorV1::NotFoundOrNotAuthorized => {
            WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
        }
        WorkRoutingSnapshotErrorV1::Unavailable => {
            WorkProductApplicationErrorV1::ProposalAuthorityUnavailable
        }
    }
}

fn root_error(error: WorkEvidenceRootReadErrorV1) -> WorkProductApplicationErrorV1 {
    match error {
        WorkEvidenceRootReadErrorV1::NotFoundOrNotAuthorized => {
            WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
        }
        WorkEvidenceRootReadErrorV1::Stale => WorkProductApplicationErrorV1::VersionConflict,
        WorkEvidenceRootReadErrorV1::Unavailable => {
            WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable
        }
        WorkEvidenceRootReadErrorV1::Cancelled => WorkProductApplicationErrorV1::Cancelled,
        WorkEvidenceRootReadErrorV1::TimedOut => WorkProductApplicationErrorV1::TimedOut,
    }
}

#[cfg(test)]
mod tests;
