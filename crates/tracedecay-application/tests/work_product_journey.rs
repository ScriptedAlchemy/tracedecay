use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    ApplyWorkProductCommandV1, CancellationContext, CapabilityGrantSnapshot,
    CreateWorkProductCommandV1, Deadline, DisclosureClass, GenerateWorkProposalRequestV1,
    RequestContext, RequestId, ResolvedScope, WorkEvidenceExpansionV1, WorkEvidencePort,
    WorkProductApplicationError, WorkProductCommandV1, WorkProductMutationReceiptV1,
    WorkProductProjectionReadV1, WorkProductService, WorkTopologyCasRequestV1,
    WorkTopologyCommitV1, WorkTopologyPort, WorkTopologyPortError, WorkTopologyReadV1,
};
use tracedecay_domain::{
    AcceptanceCriterionId, ActorId, AttemptId, InitiativeId, ManifestDigest, MilestoneId,
    ProjectId, ProviderId, RepositoryId, RetrievalAnchorId, RunId, TaskEvidenceLinkId,
    TaskEvidenceLinkV1, TaskId, UtcMicros, WorkAcceptanceCriterionV1, WorkAttemptIdentityV1,
    WorkGraphVersionV1, WorkHandoffId, WorkHandoffV1, WorkHierarchyV1, WorkInitiativeV1,
    WorkItemInputV1, WorkItemV1, WorkPlanId, WorkPlanV1, WorkProductGraphV1,
    WorkProposalDispositionV1, WorkProposedChildV1, WorkProviderOutcomeV1, WorkProviderRouteId,
    WorkProviderRouteV1, WorkProviderTerminalV1, WorkRouteDecisionV1, WorkScoreKindV1,
    WorkShapeAssessmentV1, WorkSizingV1, WorkTaskEvidenceCoverageV1, WorkTaskEvidenceV1,
    WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

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

fn context(actor: &str, operations: &[&str]) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.work.product"),
        id::<RepositoryId>("repository.work.product"),
        id::<WorktreeId>("worktree.work.product"),
        None,
    )
    .unwrap();
    let capabilities = operations
        .iter()
        .map(|operation| id::<CapabilityId>(&format!("capability.work.{operation}")))
        .collect();
    let use_cases = operations
        .iter()
        .map(|operation| id::<UseCaseId>(&format!("use-case.work.{operation}")))
        .collect();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.product"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        capabilities,
        use_cases,
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>(actor),
        scope,
        grant,
        RequestId::new(format!("request.{actor}")).unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active(format!("cancel.{actor}")).unwrap(),
    )
    .unwrap()
}

fn route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.codex"),
        id::<WorkProviderRouteId>("route.codex.local"),
    )
    .unwrap()
}

fn attempt(task: &TaskId, suffix: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        task.clone(),
        id::<RunId>(&format!("run.{suffix}")),
        id::<AttemptId>(&format!("attempt.{suffix}")),
    )
    .unwrap()
}

fn graph() -> WorkProductGraphV1 {
    let hierarchy = WorkHierarchyV1::new(
        id::<InitiativeId>("initiative.product"),
        id::<WorkPlanId>("plan.product"),
        id::<MilestoneId>("milestone.product"),
    );
    let item = WorkItemV1::new(WorkItemInputV1 {
        task_id: id::<TaskId>("task.product"),
        hierarchy,
        title: "Deliver the canonical Work product".to_owned(),
        dependencies: BTreeSet::new(),
        informational_relations: BTreeSet::new(),
        causal_candidates: BTreeSet::new(),
        acceptance_criteria: vec![
            WorkAcceptanceCriterionV1::new(
                id::<AcceptanceCriterionId>("criterion.product.review"),
                "Independent review evidence is attached".to_owned(),
                true,
            )
            .unwrap(),
        ],
        effort: 8,
        scheduled_at: None,
        deadline: Some(UtcMicros(8_000)),
        created_at: UtcMicros(10),
        updated_at: UtcMicros(10),
    })
    .unwrap();
    WorkProductGraphV1::new(
        WorkGraphVersionV1::initial(),
        vec![
            WorkInitiativeV1::new(id("initiative.product"), "Product".to_owned(), UtcMicros(1))
                .unwrap(),
        ],
        vec![
            WorkPlanV1::new(
                id("plan.product"),
                id("initiative.product"),
                "Product plan".to_owned(),
                UtcMicros(2),
            )
            .unwrap(),
        ],
        vec![
            tracedecay_domain::WorkMilestoneV1::new(
                id("milestone.product"),
                id("plan.product"),
                "Product milestone".to_owned(),
                UtcMicros(3),
            )
            .unwrap(),
        ],
        vec![item],
    )
    .unwrap()
}

#[derive(Clone, Default)]
struct MemoryTopology {
    state: Arc<Mutex<MemoryTopologyState>>,
}

#[derive(Default)]
struct MemoryTopologyState {
    graph: Option<WorkProductGraphV1>,
    commands:
        BTreeMap<tracedecay_domain::WorkCommandId, (ManifestDigest, WorkProductMutationReceiptV1)>,
    calls: usize,
}

impl MemoryTopology {
    fn calls(&self) -> usize {
        self.state.lock().unwrap().calls
    }
}

impl WorkTopologyPort for MemoryTopology {
    fn read(
        &self,
        _authority: &tracedecay_domain::WorkAuthority,
    ) -> Result<WorkTopologyReadV1, WorkTopologyPortError> {
        let mut state = self.state.lock().unwrap();
        state.calls += 1;
        state
            .graph
            .clone()
            .map(WorkTopologyReadV1::Current)
            .ok_or(WorkTopologyPortError::NotFoundOrNotAuthorized)
    }

    fn compare_and_swap(
        &self,
        request: &WorkTopologyCasRequestV1,
    ) -> Result<WorkTopologyCommitV1, WorkTopologyPortError> {
        let mut state = self.state.lock().unwrap();
        state.calls += 1;
        if let Some((digest, receipt)) = state.commands.get(request.command_id()) {
            return if digest == request.input_digest() {
                Ok(WorkTopologyCommitV1::Replayed(receipt.clone()))
            } else {
                Err(WorkTopologyPortError::IdempotencyConflict)
            };
        }
        if state.graph.as_ref().map(WorkProductGraphV1::version) != request.expected_version() {
            return Err(WorkTopologyPortError::VersionConflict);
        }
        let receipt = WorkProductMutationReceiptV1::new(
            request.replacement().clone(),
            false,
            request.command_id().clone(),
        )
        .unwrap();
        state.graph = Some(request.replacement().clone());
        state.commands.insert(
            request.command_id().clone(),
            (request.input_digest().clone(), receipt.clone()),
        );
        Ok(WorkTopologyCommitV1::Committed(receipt))
    }

    fn replay(
        &self,
        _authority: &tracedecay_domain::WorkAuthority,
        command_id: &tracedecay_domain::WorkCommandId,
        input_digest: &ManifestDigest,
    ) -> Result<Option<WorkProductMutationReceiptV1>, WorkTopologyPortError> {
        let mut state = self.state.lock().unwrap();
        state.calls += 1;
        match state.commands.get(command_id) {
            Some((digest, receipt)) if digest == input_digest => Ok(Some(receipt.clone())),
            Some(_) => Err(WorkTopologyPortError::IdempotencyConflict),
            None => Ok(None),
        }
    }
}

#[derive(Clone)]
struct MemoryEvidence {
    evidence: WorkTaskEvidenceV1,
}

impl WorkEvidencePort for MemoryEvidence {
    fn task_evidence(
        &self,
        _authority: &tracedecay_domain::WorkAuthority,
        task_id: &TaskId,
        graph_version: WorkGraphVersionV1,
        _limit: u32,
    ) -> Result<WorkTaskEvidenceV1, WorkProductApplicationError> {
        if self.evidence.task_id() != task_id {
            return Err(WorkProductApplicationError::EvidenceUnavailable);
        }
        WorkTaskEvidenceV1::new(
            task_id.clone(),
            graph_version,
            self.evidence.links().to_vec(),
            WorkTaskEvidenceCoverageV1::Complete {
                returned: self.evidence.links().len() as u32,
                available: self.evidence.links().len() as u32,
            },
        )
        .map_err(|_| WorkProductApplicationError::EvidenceUnavailable)
    }

    fn expand(
        &self,
        _authority: &tracedecay_domain::WorkAuthority,
        task_id: &TaskId,
        link_id: &TaskEvidenceLinkId,
    ) -> Result<WorkEvidenceExpansionV1, WorkProductApplicationError> {
        let link = self
            .evidence
            .links()
            .iter()
            .find(|link| link.task_id() == task_id && link.link_id() == link_id)
            .cloned()
            .ok_or(WorkProductApplicationError::EvidenceUnavailable)?;
        WorkEvidenceExpansionV1::new(link, "evidence-handle:review".to_owned(), false)
    }
}

fn evidence(version: WorkGraphVersionV1) -> MemoryEvidence {
    let task_id = id::<TaskId>("task.product");
    MemoryEvidence {
        evidence: WorkTaskEvidenceV1::new(
            task_id.clone(),
            version,
            vec![
                TaskEvidenceLinkV1::new(
                    id("evidence.product.review"),
                    1,
                    task_id,
                    id::<RetrievalAnchorId>("anchor.product.review"),
                    digest('e'),
                    UtcMicros(15),
                )
                .unwrap(),
            ],
            WorkTaskEvidenceCoverageV1::Complete {
                returned: 1,
                available: 1,
            },
        )
        .unwrap(),
    }
}

fn proposal_request(proposal: &str) -> GenerateWorkProposalRequestV1 {
    GenerateWorkProposalRequestV1 {
        proposal_id: id(proposal),
        task_id: id("task.product"),
        shape: WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 4, 2, 4, 3).unwrap(),
        sizing: WorkSizingV1::new(WorkScoreKindV1::Heuristic, 5, 8, 13, "bounded evidence")
            .unwrap(),
        children: vec![
            WorkProposedChildV1::new(
                id("task.product.prepare"),
                "Prepare".to_owned(),
                3,
                BTreeSet::new(),
            )
            .unwrap(),
            WorkProposedChildV1::new(
                id("task.product.deliver"),
                "Deliver".to_owned(),
                5,
                BTreeSet::from([id("task.product.prepare")]),
            )
            .unwrap(),
        ],
        route: WorkRouteDecisionV1::selected(
            route(),
            Vec::new(),
            BTreeSet::new(),
            "Serial execution with no auxiliary provider".to_owned(),
        )
        .unwrap(),
        explanation: "Separate preparation from delivery and retain a serial fallback".to_owned(),
        evidence_limit: 32,
    }
}

const ALL_OPERATIONS: &[&str] = &[
    "product_snapshot",
    "product_projections",
    "task_evidence",
    "expand_task_evidence",
    "generate_work_proposal",
    "apply_work_command",
];

#[test]
fn decomposition_route_acceptance_admission_outcome_and_replan_are_separate() {
    let topology = MemoryTopology::default();
    let context = context("actor.owner", ALL_OPERATIONS);
    let service =
        WorkProductService::new(topology.clone(), evidence(WorkGraphVersionV1::initial()));
    let created = service
        .create(
            &context,
            CreateWorkProductCommandV1 {
                graph: graph(),
                command_id: id("command.product.create"),
                occurred_at: UtcMicros(10),
            },
        )
        .unwrap();
    assert_eq!(created.graph().version(), WorkGraphVersionV1::initial());

    let task_id = id::<TaskId>("task.product");
    let task_evidence = service.task_evidence(&context, &task_id, 32).unwrap();
    let expanded = service
        .expand_evidence(&context, &task_id, task_evidence.links()[0].link_id())
        .unwrap();
    assert_eq!(expanded.link(), &task_evidence.links()[0]);
    assert_eq!(expanded.content_handle(), "evidence-handle:review");

    let proposal = service
        .generate_proposal(&context, proposal_request("proposal.product.initial"))
        .unwrap();
    assert_eq!(proposal.based_on_version(), WorkGraphVersionV1::initial());
    assert_eq!(
        service.snapshot(&context).unwrap().graph().version(),
        WorkGraphVersionV1::initial(),
        "proposal generation must not mutate Work"
    );

    let accepted = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: WorkGraphVersionV1::initial(),
                command_id: id("command.product.proposal.accept"),
                occurred_at: UtcMicros(20),
                command: WorkProductCommandV1::DecideProposal {
                    proposal: proposal.clone(),
                    disposition: WorkProposalDispositionV1::Accepted,
                },
            },
        )
        .unwrap();
    assert_eq!(accepted.graph().items().len(), 3);

    let identity = attempt(&task_id, "product.initial");
    let admitted = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: accepted.graph().version(),
                command_id: id("command.product.provider.admit"),
                occurred_at: UtcMicros(30),
                command: WorkProductCommandV1::AdmitProvider {
                    task_id: task_id.clone(),
                    proposal_id: proposal.proposal_id().clone(),
                    identity: identity.clone(),
                    route: route(),
                },
            },
        )
        .unwrap();
    assert_eq!(admitted.graph().version().get(), 3);

    let with_outcome = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: admitted.graph().version(),
                command_id: id("command.product.provider.outcome"),
                occurred_at: UtcMicros(40),
                command: WorkProductCommandV1::RecordProviderOutcome {
                    task_id: task_id.clone(),
                    outcome: WorkProviderOutcomeV1::new(
                        identity,
                        WorkProviderTerminalV1::Completed,
                        digest('f'),
                        UtcMicros(40),
                    ),
                },
            },
        )
        .unwrap();
    assert!(!with_outcome.graph().item(&task_id).unwrap().is_accepted());

    let replan = service
        .generate_proposal(
            &context,
            GenerateWorkProposalRequestV1 {
                children: Vec::new(),
                explanation: "Review a targeted reroute after provider evidence".to_owned(),
                ..proposal_request("proposal.product.replan")
            },
        )
        .unwrap();
    assert_eq!(replan.based_on_version(), with_outcome.graph().version());
    assert_eq!(
        service.snapshot(&context).unwrap().graph().version(),
        with_outcome.graph().version(),
        "live replanning must remain unapplied"
    );

    let linked = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: with_outcome.graph().version(),
                command_id: id("command.product.evidence.link"),
                occurred_at: UtcMicros(50),
                command: WorkProductCommandV1::LinkEvidence {
                    task_id: task_id.clone(),
                    evidence: task_evidence.links()[0].clone(),
                },
            },
        )
        .unwrap();
    let accepted_task = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: linked.graph().version(),
                command_id: id("command.product.task.accept"),
                occurred_at: UtcMicros(60),
                command: WorkProductCommandV1::AcceptTask {
                    task_id: task_id.clone(),
                    evidence_by_criterion: BTreeMap::from([(
                        id("criterion.product.review"),
                        id("evidence.product.review"),
                    )]),
                },
            },
        )
        .unwrap();
    assert!(accepted_task.graph().item(&task_id).unwrap().is_accepted());
}

#[derive(Clone)]
struct PartialTopology {
    graph: WorkProductGraphV1,
}

impl WorkTopologyPort for PartialTopology {
    fn read(
        &self,
        _authority: &tracedecay_domain::WorkAuthority,
    ) -> Result<WorkTopologyReadV1, WorkTopologyPortError> {
        Ok(WorkTopologyReadV1::Partial {
            graph: self.graph.clone(),
            unknowns: vec!["provider capacity unavailable".to_owned()],
        })
    }

    fn compare_and_swap(
        &self,
        _request: &WorkTopologyCasRequestV1,
    ) -> Result<WorkTopologyCommitV1, WorkTopologyPortError> {
        Err(WorkTopologyPortError::Unavailable)
    }

    fn replay(
        &self,
        _authority: &tracedecay_domain::WorkAuthority,
        _command_id: &tracedecay_domain::WorkCommandId,
        _input_digest: &ManifestDigest,
    ) -> Result<Option<WorkProductMutationReceiptV1>, WorkTopologyPortError> {
        Ok(None)
    }
}

#[test]
fn projection_reads_preserve_partial_topology_and_mutations_fail_closed() {
    let topology = PartialTopology { graph: graph() };
    let context = context("actor.partial", ALL_OPERATIONS);
    let service = WorkProductService::new(topology, evidence(WorkGraphVersionV1::initial()));

    let WorkTopologyReadV1::Partial { graph, unknowns } = service.snapshot(&context).unwrap()
    else {
        panic!("partial topology must remain partial");
    };
    assert_eq!(graph.version(), WorkGraphVersionV1::initial());
    assert_eq!(unknowns, vec!["provider capacity unavailable"]);

    let WorkProductProjectionReadV1::Partial {
        projections,
        unknowns,
    } = service.projections(&context).unwrap()
    else {
        panic!("partial topology must remain partial");
    };
    assert_eq!(projections.graph_version(), WorkGraphVersionV1::initial());
    assert_eq!(unknowns, vec!["provider capacity unavailable"]);
    assert_eq!(
        service
            .task_evidence(&context, &id("task.product"), 32)
            .unwrap_err(),
        WorkProductApplicationError::TopologyUnavailable
    );

    let refused = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: WorkGraphVersionV1::initial(),
                command_id: id("command.partial.refused"),
                occurred_at: UtcMicros(20),
                command: WorkProductCommandV1::AcceptTask {
                    task_id: id("task.product"),
                    evidence_by_criterion: BTreeMap::new(),
                },
            },
        )
        .unwrap_err();
    assert_eq!(refused, WorkProductApplicationError::TopologyUnavailable);
}

#[test]
fn handoff_cancel_retry_rollback_and_idempotency_preserve_legal_state() {
    let topology = MemoryTopology::default();
    let context = context("actor.owner", ALL_OPERATIONS);
    let service = WorkProductService::new(topology, evidence(WorkGraphVersionV1::initial()));
    service
        .create(
            &context,
            CreateWorkProductCommandV1 {
                graph: graph(),
                command_id: id("command.control.create"),
                occurred_at: UtcMicros(10),
            },
        )
        .unwrap();
    let proposal = service
        .generate_proposal(&context, proposal_request("proposal.control"))
        .unwrap();
    let accepted = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: WorkGraphVersionV1::initial(),
                command_id: id("command.control.accept"),
                occurred_at: UtcMicros(20),
                command: WorkProductCommandV1::DecideProposal {
                    proposal: proposal.clone(),
                    disposition: WorkProposalDispositionV1::Accepted,
                },
            },
        )
        .unwrap();
    let task_id = id::<TaskId>("task.product");
    let first = attempt(&task_id, "control.first");
    let admitted = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: accepted.graph().version(),
                command_id: id("command.control.admit"),
                occurred_at: UtcMicros(30),
                command: WorkProductCommandV1::AdmitProvider {
                    task_id: task_id.clone(),
                    proposal_id: proposal.proposal_id().clone(),
                    identity: first.clone(),
                    route: route(),
                },
            },
        )
        .unwrap();
    let handoff_command = ApplyWorkProductCommandV1 {
        expected_version: admitted.graph().version(),
        command_id: id("command.control.handoff"),
        occurred_at: UtcMicros(35),
        command: WorkProductCommandV1::RecordHandoff {
            handoff: WorkHandoffV1::new(
                id::<WorkHandoffId>("handoff.control"),
                task_id.clone(),
                id("actor.owner"),
                id("actor.reviewer"),
                BTreeSet::from([id("evidence.product.review")]),
                BTreeSet::from(["provider result needs review".to_owned()]),
                UtcMicros(35),
            )
            .unwrap(),
        },
    };
    let handed_off = service.apply(&context, handoff_command.clone()).unwrap();
    let replayed = service.apply(&context, handoff_command).unwrap();
    assert_eq!(replayed.graph(), handed_off.graph());
    assert!(replayed.replayed());

    let cancelled = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: handed_off.graph().version(),
                command_id: id("command.control.cancel"),
                occurred_at: UtcMicros(40),
                command: WorkProductCommandV1::CancelAttempt {
                    task_id: task_id.clone(),
                    identity: first.clone(),
                },
            },
        )
        .unwrap();
    let second = attempt(&task_id, "control.second");
    let retried = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: cancelled.graph().version(),
                command_id: id("command.control.retry"),
                occurred_at: UtcMicros(50),
                command: WorkProductCommandV1::RetryAttempt {
                    task_id: task_id.clone(),
                    prior_identity: first,
                    identity: second.clone(),
                    route: route(),
                },
            },
        )
        .unwrap();
    let rolled_back = service
        .apply(
            &context,
            ApplyWorkProductCommandV1 {
                expected_version: retried.graph().version(),
                command_id: id("command.control.rollback"),
                occurred_at: UtcMicros(60),
                command: WorkProductCommandV1::RollbackAdmission {
                    task_id,
                    identity: second,
                },
            },
        )
        .unwrap();
    assert_eq!(rolled_back.graph().version().get(), 7);
}

#[test]
fn authorization_is_checked_before_topology_or_evidence_access() {
    let topology = MemoryTopology::default();
    let service =
        WorkProductService::new(topology.clone(), evidence(WorkGraphVersionV1::initial()));
    let denied = context("actor.denied", &["product_snapshot"]);

    let error = service
        .create(
            &denied,
            CreateWorkProductCommandV1 {
                graph: graph(),
                command_id: id("command.denied.create"),
                occurred_at: UtcMicros(10),
            },
        )
        .unwrap_err();
    assert_eq!(error, WorkProductApplicationError::NotAuthorized);
    assert_eq!(topology.calls(), 0);
}
