use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use tracedecay_domain::{
    ActorId, AttemptId, BrainId, InitiativeId, ManifestDigest, MilestoneId,
    ObservationSourceIdentityV1, ProjectId, ProviderId, RepositoryId, RetrievalAnchorId, RunId,
    SessionId, SourceStoreId, TaskEvidenceLinkId, TaskEvidenceLinkV1, TaskId, UserProfileId,
    UtcMicros, WorkAcceptanceCriterionV1, WorkAttemptIdentityV1, WorkGraphChangeV1,
    WorkGraphVersionV1, WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1, WorkItemV1,
    WorkMilestoneV1, WorkPlanId, WorkPlanV1, WorkProductEventSequenceV1, WorkProductGraphV1,
    WorkProductSourceWatermarkV1, WorkProposalV1, WorkProviderRouteId, WorkProviderRouteV1,
    WorkRouteDecisionV1, WorkScoreKindV1, WorkShapeAssessmentV1, WorkSizingV1, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::*;
use crate::{
    AuthorizedWorkProductScopeV1, CancellationContext, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, RequestId, ResolvedScope, WorkAttemptProviderOutcomeV1,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn selection() -> WorkProductSelectionScopeV1 {
    WorkProductSelectionScopeV1::relations(BTreeSet::from([
        tracedecay_domain::WorkProductAuthorizedRelationScopeV1::Repository {
            project_id: id("project.work-evidence"),
            repository_id: id("repository.work-evidence"),
        },
    ]))
    .unwrap()
}

fn binding() -> WorkProductBindingV1 {
    WorkProductBindingV1::new(
        CapabilityId::new("capability.work.evidence.read").unwrap(),
        UseCaseId::new("use-case.work.evidence.read").unwrap(),
    )
}

fn context() -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.work-evidence"),
        id::<RepositoryId>("repository.work-evidence"),
        id::<WorktreeId>("worktree.work-evidence"),
        None,
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work-evidence"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.work.evidence.read").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.work.evidence.read").unwrap()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.requester"),
        scope,
        grant,
        RequestId::new("request.work-evidence").unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active("cancel.work-evidence").unwrap(),
    )
    .unwrap()
}

fn attempt(task_id: &TaskId) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        task_id.clone(),
        id::<RunId>("run.work-evidence"),
        id::<AttemptId>("attempt.work-evidence"),
    )
    .unwrap()
}

fn rooted_graph() -> (
    WorkProductGraphV1,
    WorkAttemptIdentityV1,
    TaskEvidenceLinkV1,
) {
    let task_id = id::<TaskId>("task.work-evidence");
    let hierarchy = WorkHierarchyV1::new(
        id::<InitiativeId>("initiative.work-evidence"),
        id::<WorkPlanId>("plan.work-evidence"),
        id::<MilestoneId>("milestone.work-evidence"),
    );
    let item = WorkItemV1::new(WorkItemInputV1 {
        task_id: task_id.clone(),
        hierarchy,
        title: "Retrieve exact Work evidence".to_owned(),
        dependencies: BTreeSet::new(),
        informational_relations: BTreeSet::new(),
        causal_candidates: BTreeSet::new(),
        acceptance_criteria: vec![
            WorkAcceptanceCriterionV1::new(
                id("criterion.work-evidence"),
                "Evidence remains exact".to_owned(),
                true,
            )
            .unwrap(),
        ],
        effort: 1,
        scheduled_at: None,
        deadline: Some(UtcMicros(8_000)),
        created_at: UtcMicros(10),
        updated_at: UtcMicros(10),
    })
    .unwrap();
    let graph = WorkProductGraphV1::new(
        WorkGraphVersionV1::initial(),
        vec![
            WorkInitiativeV1::new(
                id("initiative.work-evidence"),
                "Evidence initiative".to_owned(),
                UtcMicros(1),
            )
            .unwrap(),
        ],
        vec![
            WorkPlanV1::new(
                id("plan.work-evidence"),
                id("initiative.work-evidence"),
                "Evidence plan".to_owned(),
                UtcMicros(2),
            )
            .unwrap(),
        ],
        vec![
            WorkMilestoneV1::new(
                id("milestone.work-evidence"),
                id("plan.work-evidence"),
                "Evidence milestone".to_owned(),
                UtcMicros(3),
            )
            .unwrap(),
        ],
        vec![item],
    )
    .unwrap();
    let attempt = attempt(&task_id);
    let link = TaskEvidenceLinkV1::new(
        id::<TaskEvidenceLinkId>("link.work-evidence.attempt"),
        1,
        task_id.clone(),
        id::<RetrievalAnchorId>("anchor.work-evidence.attempt"),
        digest('b'),
        UtcMicros(100),
    )
    .unwrap();
    let graph = graph
        .apply(WorkGraphChangeV1::EvidenceLinked {
            task_id: task_id.clone(),
            evidence: link.clone(),
        })
        .unwrap();
    let proposal = WorkProposalV1::new(
        id("proposal.work-evidence"),
        task_id.clone(),
        graph.version(),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1).unwrap(),
        WorkSizingV1::new(WorkScoreKindV1::Heuristic, 1, 1, 1, "bounded").unwrap(),
        Vec::new(),
        WorkRouteDecisionV1::abstain("execution admission selects the route").unwrap(),
        "Admit the sealed attempt identity".to_owned(),
        digest('d'),
    )
    .unwrap();
    let graph = graph
        .apply(WorkGraphChangeV1::ProposalAccepted {
            proposal,
            accepted_at: UtcMicros(101),
        })
        .unwrap();
    let admitted_based_on_version = graph.version();
    let graph = graph
        .apply(WorkGraphChangeV1::ExecutionAdmitted {
            task_id: task_id.clone(),
            based_on_version: admitted_based_on_version,
            admitted_at: UtcMicros(102),
        })
        .unwrap();
    let attempt_based_on_version = graph.version();
    let graph = graph
        .apply(WorkGraphChangeV1::AcceptedAttemptLinked {
            task_id,
            based_on_version: attempt_based_on_version,
            identity: attempt.clone(),
            linked_at: UtcMicros(110),
        })
        .unwrap();
    (graph, attempt, link)
}

fn verified() -> VerifiedWorkGraphVersionV1 {
    VerifiedWorkGraphVersionV1::new(
        WorkGraphVersionV1::new(5).unwrap(),
        WorkProductEventSequenceV1::new(5).unwrap(),
        WorkProductSourceWatermarkV1::new(BTreeMap::<SourceStoreId, u64>::new()).unwrap(),
        digest('c'),
    )
    .unwrap()
}

#[derive(Clone)]
struct RootPort {
    root: VerifiedWorkEvidenceRootV1,
    reads: Arc<AtomicUsize>,
}

struct MissingRootAuthority;

impl WorkEvidenceRootReadPortV1 for MissingRootAuthority {
    fn read_evidence_root(
        &self,
        _context: &WorkProductPortContextV1,
        _task_id: &TaskId,
        _verified_version: &VerifiedWorkGraphVersionV1,
    ) -> Result<VerifiedWorkEvidenceRootV1, WorkEvidenceRootReadErrorV1> {
        Err(WorkEvidenceRootReadErrorV1::Unavailable)
    }
}

impl WorkEvidenceRootReadPortV1 for RootPort {
    fn read_evidence_root(
        &self,
        _context: &WorkProductPortContextV1,
        task_id: &TaskId,
        version: &VerifiedWorkGraphVersionV1,
    ) -> Result<VerifiedWorkEvidenceRootV1, WorkEvidenceRootReadErrorV1> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.root.item.task_id() != task_id || &self.root.verified_version != version {
            return Err(WorkEvidenceRootReadErrorV1::NotFoundOrNotAuthorized);
        }
        Ok(self.root.clone())
    }
}

struct Owner;

impl WorkProductOwnerAuthorizationPortV1 for Owner {
    fn authorize_scope(
        &self,
        _context: &RequestContext,
        selection: &WorkProductSelectionScopeV1,
        _observed_at: UtcMicros,
    ) -> Result<AuthorizedWorkProductScopeV1, WorkProductOwnerAuthorizationErrorV1> {
        AuthorizedWorkProductScopeV1::new(
            id::<BrainId>("brain.work-evidence"),
            id::<UserProfileId>("profile.work-evidence"),
            selection.clone(),
        )
        .map_err(|_| WorkProductOwnerAuthorizationErrorV1::Unavailable)
    }
}

#[derive(Clone)]
struct Receipts {
    receipt: WorkAttemptReceiptV1,
}

impl WorkAttemptReceiptReadPortV1 for Receipts {
    fn attempt_receipt(
        &self,
        _authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptReceiptV1, WorkAttemptReceiptReadErrorV1> {
        if &self.receipt.identity != identity {
            return Err(WorkAttemptReceiptReadErrorV1::NotFoundOrNotAuthorized);
        }
        Ok(self.receipt.clone())
    }
}

#[derive(Default)]
struct Sessions {
    requests: Mutex<Vec<WorkTaskSessionRequestV1>>,
}

impl WorkTaskSessionPortV1 for Sessions {
    fn retrieve_task_session<'a>(
        &'a self,
        context: &'a RequestContext,
        request: WorkTaskSessionRequestV1,
        reauthorization: &'a dyn WorkTaskSessionReauthorizationPortV1,
    ) -> WorkTaskSessionFuture<'a> {
        self.requests.lock().unwrap().push(request.clone());
        Box::pin(async move {
            reauthorization
                .reauthorize_task_session(context, &request)
                .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?;
            Ok(WorkTaskSessionEvidenceV1 {
                task_id: request.task_id,
                verified_version: request.verified_version,
                attempt: request.attempt,
                source: request.source,
                participant_epoch: digest('e'),
                ranked_anchors: vec![WorkTaskSessionRankedAnchorV1 {
                    anchor_id: id("anchor.session.message"),
                    final_ordinal: 0,
                    utility_micros: 900_000,
                    contributions: vec![WorkTaskSessionRankContributionV1 {
                        retriever: tracedecay_domain::RetrieverKind::TaskSession,
                        retriever_revision: id("retriever.task-session.v1"),
                        source_occurrence: id("occurrence.session.message"),
                        ordinal_rank: 0,
                        raw_score_micros: 900_000,
                        score_domain: id("score.task-session.v1"),
                        calibration_profile: id("calibration.task-session.v1"),
                        calibrated_feature_micros: 900_000,
                        weight_micros: 1_000_000,
                        weighted_contribution_micros: 900_000,
                    }],
                }],
                hydrated: vec![WorkTaskSessionHydrationV1 {
                    rank: 0,
                    anchor_id: id("anchor.session.message"),
                    state: WorkTaskSessionHydrationStateV1::Available,
                    content: Some(b"Provider completed the accepted attempt".to_vec()),
                }],
                coverage: WorkEvidenceCoverageStateV1::Complete,
                coverage_counts: WorkTaskSessionCoverageV1 {
                    visible: 1,
                    hidden: 0,
                    unknown: 0,
                    redacted: 0,
                },
                freshness: WorkEvidenceFreshnessV1::Current,
                redacted: false,
                continuation: None,
            })
        })
    }
}

struct Anchors;

impl WorkAnchorHydrationPortV1 for Anchors {
    fn hydrate_anchor<'a>(
        &'a self,
        _context: &'a RequestContext,
        request: WorkAnchorHydrationRequestV1,
    ) -> WorkAnchorHydrationFuture<'a> {
        Box::pin(async move {
            Ok(WorkAnchorHydrationV1 {
                exact_anchors: vec![request.anchor_id.clone()],
                anchor_id: request.anchor_id,
                content: vec!["sealed attempt receipt".to_owned()],
                coverage: WorkEvidenceCoverageStateV1::Complete,
                freshness: WorkEvidenceFreshnessV1::Stale,
                redacted: true,
                continuation: None,
            })
        })
    }
}

fn request() -> WorkEvidenceRetrieveRequestV1 {
    WorkEvidenceRetrieveRequestV1 {
        selection: selection(),
        task_id: id("task.work-evidence"),
        verified_version: verified(),
        temporal: TemporalModeV1::Forensic,
        page_size: 10,
        expansion: None,
        continuation: None,
        observed_at: UtcMicros(500),
    }
}

#[test]
fn partial_owning_source_never_reports_complete_outer_coverage() {
    assert_eq!(
        overall_coverage_state(&[], &[], true),
        WorkEvidenceCoverageStateV1::Partial,
    );
    assert_eq!(
        overall_coverage_state(&[], &[], false),
        WorkEvidenceCoverageStateV1::Complete,
    );
}

#[tokio::test]
async fn task_root_reauthorizes_and_delegates_session_identity_without_task_kernel_input() {
    let (graph, attempt, link) = rooted_graph();
    let reads = Arc::new(AtomicUsize::new(0));
    let provider_session = ObservationSourceIdentityV1::for_provider(
        id::<ProviderId>("codex"),
        id::<SessionId>("session.provider.reported"),
    )
    .unwrap();
    let route = WorkProviderRouteV1::new(
        id::<ProviderId>("provider.codex"),
        id::<WorkProviderRouteId>("route.codex.app-server"),
    )
    .unwrap();
    let receipt = WorkAttemptReceiptV1 {
        identity: attempt.clone(),
        artifacts: Vec::new(),
        evidence: Some(WorkAttemptEvidenceRecordV1 {
            identity: attempt.clone(),
            requested_route: route.clone(),
            actual_route: Some(route),
            outcome: WorkAttemptProviderOutcomeV1::Exited { code: 0 },
            stdout: None,
            stderr: None,
            provider_session: Some(provider_session.clone()),
            provider_fallback: None,
            observed_at: UtcMicros(200),
        }),
    };
    let roots = RootPort {
        root: VerifiedWorkEvidenceRootV1 {
            verified_version: verified(),
            item: graph.item(&id("task.work-evidence")).unwrap().clone(),
            relations: graph
                .relations()
                .into_iter()
                .filter(|relation| relation_touches_task(relation, &id("task.work-evidence")))
                .collect(),
            proposal_decisions: Vec::new(),
            relation_replan_decisions: Vec::new(),
            links: vec![link],
        },
        reads: reads.clone(),
    };
    let sessions = Sessions::default();
    let service = WorkEvidenceRetrievalServiceV1::new(
        roots,
        Owner,
        Receipts { receipt },
        &sessions,
        Anchors,
        binding(),
    );

    let result = service.retrieve(&context(), request()).await.unwrap();

    assert_eq!(result.task_id.as_str(), "task.work-evidence");
    assert_eq!(result.coverage.selected, 2);
    assert_eq!(result.coverage.hydrated, 2);
    assert_eq!(result.coverage.state, WorkEvidenceCoverageStateV1::Complete);
    assert_eq!(result.sources.len(), 3);
    assert_eq!(result.freshness, WorkEvidenceFreshnessV1::Stale);
    assert!(result.redacted);
    assert!(result.omissions.is_empty());
    assert!(result.continuations.is_empty());
    assert_eq!(reads.load(Ordering::SeqCst), 5);
    let requests = sessions.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].selection, selection());
    assert_eq!(requests[0].task_id.as_str(), "task.work-evidence");
    assert_eq!(requests[0].verified_version, verified());
    assert_eq!(requests[0].attempt, attempt);
    assert_eq!(
        requests[0].accepted_attempts,
        BTreeSet::from([attempt.clone()])
    );
    assert_eq!(requests[0].source, provider_session);
    assert_eq!(requests[0].temporal, TemporalModeV1::Forensic);
    assert_eq!(requests[0].continuation, None);
}

#[tokio::test]
async fn continuation_must_match_an_exact_reauthorized_expansion_relation() {
    let mut request = request();
    request.expansion = Some(WorkEvidenceExpansionSelectorV1::Anchor {
        link_id: id("link.work-evidence.attempt"),
    });
    request.continuation = Some(WorkEvidenceContinuationV1::TaskSession {
        continuation: WorkTaskSessionContinuationV1 {
            verified_version: verified(),
            attempt: attempt(&id("task.work-evidence")),
            source: ObservationSourceIdentityV1::for_provider(
                id::<ProviderId>("codex"),
                id::<SessionId>("session.provider.reported"),
            )
            .unwrap(),
            participant_epoch: digest('e'),
            temporal_cursor: Some(OpaqueCursor::new("cursor.not-authority").unwrap()),
            ranking_cursor: None,
        },
    });
    assert_eq!(
        validate_request(&request),
        Err(WorkProductApplicationErrorV1::InvalidRequest)
    );
}

#[tokio::test]
async fn missing_root_authority_is_typed_unavailable_before_any_session_read() {
    let sessions = Sessions::default();
    let identity = attempt(&id("task.work-evidence"));
    let route = WorkProviderRouteV1::new(
        id::<ProviderId>("provider.codex"),
        id::<WorkProviderRouteId>("route.codex.app-server"),
    )
    .unwrap();
    let service = WorkEvidenceRetrievalServiceV1::new(
        MissingRootAuthority,
        Owner,
        Receipts {
            receipt: WorkAttemptReceiptV1 {
                identity: identity.clone(),
                artifacts: Vec::new(),
                evidence: Some(WorkAttemptEvidenceRecordV1 {
                    identity,
                    requested_route: route.clone(),
                    actual_route: Some(route),
                    outcome: WorkAttemptProviderOutcomeV1::Exited { code: 0 },
                    stdout: None,
                    stderr: None,
                    provider_session: None,
                    provider_fallback: None,
                    observed_at: UtcMicros(200),
                }),
            },
        },
        &sessions,
        Anchors,
        binding(),
    );

    assert_eq!(
        service.retrieve(&context(), request()).await,
        Err(WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable),
    );
    assert!(sessions.requests.lock().unwrap().is_empty());
}

#[test]
fn provider_collision_cannot_satisfy_a_sealed_session_receipt() {
    let identity = attempt(&id("task.work-evidence"));
    let route = WorkProviderRouteV1::new(
        id::<ProviderId>("provider.codex"),
        id::<WorkProviderRouteId>("route.codex.app-server"),
    )
    .unwrap();
    let receipt = WorkAttemptReceiptV1 {
        identity: identity.clone(),
        artifacts: Vec::new(),
        evidence: Some(WorkAttemptEvidenceRecordV1 {
            identity,
            requested_route: route.clone(),
            actual_route: Some(route),
            outcome: WorkAttemptProviderOutcomeV1::Exited { code: 0 },
            stdout: None,
            stderr: None,
            provider_session: Some(
                ObservationSourceIdentityV1::for_provider(
                    id::<ProviderId>("codex"),
                    id::<SessionId>("session.shared-id"),
                )
                .unwrap(),
            ),
            provider_fallback: None,
            observed_at: UtcMicros(200),
        }),
    };
    let evidence = WorkTaskSessionEvidenceV1 {
        task_id: id("task.work-evidence"),
        verified_version: verified(),
        attempt: receipt.identity.clone(),
        source: ObservationSourceIdentityV1::for_provider(
            id::<ProviderId>("claude"),
            id::<SessionId>("session.shared-id"),
        )
        .unwrap(),
        participant_epoch: digest('e'),
        ranked_anchors: Vec::new(),
        hydrated: Vec::new(),
        coverage: WorkEvidenceCoverageStateV1::Complete,
        coverage_counts: WorkTaskSessionCoverageV1 {
            visible: 0,
            hidden: 0,
            unknown: 0,
            redacted: 0,
        },
        freshness: WorkEvidenceFreshnessV1::Current,
        redacted: false,
        continuation: None,
    };

    assert_eq!(
        validate_task_session(&request(), &receipt, &evidence),
        Err(WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable),
    );
}
