use super::*;

use std::collections::{BTreeMap, BTreeSet};

use tokio::sync::Mutex;
use tracedecay_contracts::{
    ApplicationOutcome, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, GenerateProposalRequest, PrepareWorkProductMutationRequestV1,
    WorkGraphReadRequestV1, WorkProductAuthorizedRelationScopeV1, WorkProductChangeDraftV1,
    WorkProductMutationRequestV1, WorkProductSelectionScopeV1,
};
use tracedecay_daemon_service::{DaemonInvocationService, *};
use tracedecay_domain::{
    ActorId, InitiativeId, MilestoneId, ProposalId, TaskId, WorkAuthority, WorkHierarchyV1,
    WorkInitiativeV1, WorkItemInputV1, WorkItemV1, WorkMilestoneV1, WorkPlanId, WorkPlanV1,
    WorkProposalDispositionV1, WorkProposalV1, WorkRouteDecisionV1, WorkScoreKindV1,
    WorkShapeAssessmentV1, WorkSizingV1,
};
use tracedecay_lsp::LspSessionRegistry;
use tracedecay_policy::work_loop::WorkProposalReasonV1;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

fn product_task(
    identity: &str,
    task_id: TaskId,
    created_at: UtcMicros,
) -> (WorkInitiativeV1, WorkPlanV1, WorkMilestoneV1, WorkItemV1) {
    let initiative_id =
        InitiativeId::new(format!("initiative.work.{identity}")).expect("fixture initiative id");
    let plan_id = WorkPlanId::new(format!("plan.work.{identity}")).expect("fixture plan id");
    let milestone_id =
        MilestoneId::new(format!("milestone.work.{identity}")).expect("fixture milestone id");
    let initiative = WorkInitiativeV1::new(
        initiative_id.clone(),
        format!("Work initiative {identity}"),
        created_at,
    )
    .expect("fixture initiative");
    let plan = WorkPlanV1::new(
        plan_id.clone(),
        initiative_id.clone(),
        format!("Work plan {identity}"),
        created_at,
    )
    .expect("fixture plan");
    let milestone = WorkMilestoneV1::new(
        milestone_id.clone(),
        plan_id.clone(),
        format!("Work milestone {identity}"),
        created_at,
    )
    .expect("fixture milestone");
    let item = WorkItemV1::new(WorkItemInputV1 {
        task_id,
        hierarchy: WorkHierarchyV1::new(initiative_id, plan_id, milestone_id),
        title: format!("Deliver Work task {identity}"),
        dependencies: BTreeSet::new(),
        informational_relations: BTreeSet::new(),
        causal_candidates: BTreeSet::new(),
        acceptance_criteria: Vec::new(),
        effort: 1,
        scheduled_at: None,
        deadline: None,
        created_at,
        updated_at: created_at,
    })
    .expect("fixture Work item");
    (initiative, plan, milestone, item)
}

#[tokio::test]
async fn registered_work_services_dispatch_the_core_lifecycle() {
    let profile = tempfile::tempdir().expect("profile root");
    let project = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.work.core-invocation").expect("project id");
    let host =
        tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
            profile.path(),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered project runtime");
    let database = host
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
        .expect("registered project database");
    let actor = ActorId::new("actor.work.core-invocation").expect("actor id");
    let scope = ResolvedScope::new(
        project_id,
        tracedecay_domain::RepositoryId::new("repository.work.core-invocation")
            .expect("repository id"),
        tracedecay_domain::WorktreeId::new("worktree.work.core-invocation").expect("worktree id"),
        None,
    )
    .expect("resolved scope");
    let grant_digest =
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("grant digest");
    let capabilities = tracedecay_contracts::WORK_APPLICATION_OPERATION_IDS_V1
        .iter()
        .map(|(_, capability, _)| CapabilityId::new(*capability).expect("capability"))
        .collect();
    let use_cases = tracedecay_contracts::WORK_APPLICATION_OPERATION_IDS_V1
        .iter()
        .map(|(_, _, use_case)| UseCaseId::new(*use_case).expect("use case"))
        .collect();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.work.core-invocation").expect("grant id"),
        1,
        grant_digest.clone(),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        capabilities,
        use_cases,
        DisclosureClass::Sensitive,
    )
    .expect("Work grant");
    let authority = WorkAuthority::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        actor.clone(),
        grant_digest,
    )
    .expect("Work authority");
    let _registration_grant = grant.clone();
    let _policy_digest =
        ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).expect("policy digest");
    let _configuration_digest =
        ManifestDigest::new(format!("sha256:{}", "f".repeat(64))).expect("configuration digest");
    let service = DaemonInvocationService::default();
    let (proposal_routing, configuration_digest) =
        super::work_evidence_journey_tests::configured_work_proposal_routing(
            project.path(),
            scope.clone(),
            &grant,
        );
    let policy_digest = mount_test_work_observability(
        &service,
        project.path(),
        database.clone(),
        &scope,
        &configuration_digest,
    )
    .await;
    DaemonWorkRuntimeRegistrar::new(&service)
        .register(
            project.path().to_path_buf(),
            database,
            authority,
            actor,
            grant,
            policy_digest,
            configuration_digest.clone(),
            tracedecay_domain::configuration::safe_work_topology_policy_v1(),
            proposal_routing,
            denied_work_evidence_retrieval(),
        )
        .await
        .expect("registered Work runtime");
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let other_project = tempfile::tempdir().expect("other project root");
    let unavailable = service
        .invoke(
            &registry,
            Some(other_project.path()),
            None,
            None,
            None,
            DaemonInvocationRequest::work_application(
                "request.work.other-project",
                WorkApplicationInvocationV1::Views(WorkGraphReadRequestV1::current(
                    WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                    UtcMicros(100),
                )),
                UtcMicros(100),
                Deadline::new(UtcMicros(1_000)).expect("deadline"),
                CancellationContext::active("cancel.request.work.other-project")
                    .expect("cancellation"),
            ),
        )
        .await;
    assert!(matches!(
        unavailable.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::Unavailable
        }
    ));
    let task_id = tracedecay_domain::TaskId::new("task.work.core-invocation").expect("task id");
    let proposal_digest =
        ManifestDigest::new(format!("sha256:{}", "1".repeat(64))).expect("proposal digest");

    macro_rules! invoke {
        ($request_id:literal, $request:expr) => {
            service
                .invoke(
                    &registry,
                    Some(project.path()),
                    None,
                    None,
                    None,
                    DaemonInvocationRequest::work_application(
                        $request_id,
                        $request,
                        UtcMicros(100),
                        Deadline::new(UtcMicros(1_000)).expect("deadline"),
                        CancellationContext::active(concat!("cancel.", $request_id))
                            .expect("cancellation"),
                    ),
                )
                .await
                .outcome
        };
    }

    let product_selection = WorkProductSelectionScopeV1::relations(
        [WorkProductAuthorizedRelationScopeV1::Repository {
            project_id: scope.project_id.clone(),
            repository_id: scope.repository_id.clone(),
        }]
        .into_iter()
        .collect(),
    )
    .expect("product selection");
    let (initiative, plan, milestone, item) =
        product_task("core-invocation", task_id.clone(), UtcMicros(10));
    let prepared_create = invoke!(
        "request.work.prepare-create",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: product_selection.clone(),
            change: WorkProductChangeDraftV1::CreateTask {
                initiative,
                plan,
                milestone,
                item: Box::new(item),
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared_create
    else {
        panic!("product task preparation must return Work evidence: {prepared_create:?}");
    };
    let prepared_create = packet.payload.expect("prepared product task mutation");
    let created = invoke!(
        "request.work.create",
        WorkApplicationInvocationV1::MutateGraph(prepared_create.clone())
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::MutateGraph(ApplicationOutcome::Effect(effect)),
        ..
    } = created
    else {
        panic!("product task creation must return a Work effect: {created:?}");
    };
    let created = effect.payload.expect("created product task receipt");
    assert!(!created.replayed());

    let replayed = invoke!(
        "request.work.create-replay",
        WorkApplicationInvocationV1::MutateGraph(prepared_create)
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::MutateGraph(ApplicationOutcome::Effect(effect)),
        ..
    } = replayed
    else {
        panic!("replayed product task mutation must return a Work effect: {replayed:?}");
    };
    let replayed = effect.payload.expect("replayed product task receipt");
    assert!(replayed.replayed());
    assert_eq!(replayed.event(), created.event());

    let generated = invoke!(
        "request.work.generate-proposal",
        WorkApplicationInvocationV1::GenerateProposal(GenerateProposalRequest {
            selection: product_selection.clone(),
            task_id: task_id.clone(),
            proposal_id: ProposalId::new("proposal.work.generated-routing")
                .expect("generated proposal id"),
            live_git_evidence: None,
            occurred_at: UtcMicros(100),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::GenerateProposal(ApplicationOutcome::Evidence(packet)),
        ..
    } = generated
    else {
        panic!("proposal generation must return Work evidence: {generated:?}");
    };
    let generated = packet.payload.expect("generated proposal evidence");
    assert_eq!(
        generated.verified_graph_version.graph_version(),
        created.verified_graph_version().graph_version(),
        "proposal generation must bind the exact current Work graph"
    );
    let route_plan = generated
        .decision
        .route_plan
        .as_ref()
        .expect("the configured route remains in the explained decision");
    assert_ne!(route_plan.ranked.len(), 0);
    assert!(
        !generated
            .decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::NoEligibleRoutes)
    );
    assert_eq!(
        generated.calibration.provenance.configuration_digest,
        configuration_digest
    );
    assert_eq!(
        generated
            .calibration
            .provenance
            .configuration_revision
            .as_ref()
            .map(tracedecay_domain::ConfigurationRevisionId::as_str),
        Some("configuration.revision.work-evidence-routing")
    );

    let proposal = WorkProposalV1::new(
        ProposalId::new("proposal.work.core-invocation").expect("proposal id"),
        task_id.clone(),
        created.verified_graph_version().graph_version(),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1).expect("proposal shape"),
        WorkSizingV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, "explicit fixture work")
            .expect("proposal sizing"),
        Vec::new(),
        WorkRouteDecisionV1::selected(
            super::work_evidence_journey_tests::provider_route(),
            Vec::new(),
            BTreeSet::new(),
            "no fallback in the core lifecycle".to_owned(),
        )
        .expect("proposal route"),
        "Admit the explicitly declared Work task".to_owned(),
        proposal_digest,
        configuration_digest.clone(),
    )
    .expect("proposal");
    let prepared_accept = invoke!(
        "request.work.prepare-accept-proposal",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: product_selection.clone(),
            change: WorkProductChangeDraftV1::DecideProposal {
                proposal,
                disposition: WorkProposalDispositionV1::Accepted,
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared_accept
    else {
        panic!("proposal preparation must return Work evidence: {prepared_accept:?}");
    };
    let accepted = invoke!(
        "request.work.accept-proposal",
        WorkApplicationInvocationV1::MutateGraph(
            packet.payload.expect("prepared proposal mutation")
        )
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::MutateGraph(ApplicationOutcome::Effect(effect)),
        ..
    } = accepted
    else {
        panic!("proposal acceptance must return a Work effect: {accepted:?}");
    };
    let accepted = effect.payload.expect("accepted proposal receipt");

    let prepared_admission = invoke!(
        "request.work.prepare-admit",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: product_selection.clone(),
            change: WorkProductChangeDraftV1::AdmitExecution {
                task_id: task_id.clone(),
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared_admission
    else {
        panic!("execution admission preparation must return Work evidence: {prepared_admission:?}");
    };
    let WorkProductMutationRequestV1::AdmitExecution(admission) =
        packet.payload.expect("prepared execution admission")
    else {
        panic!("preparation must produce the canonical execution admission request");
    };
    assert_eq!(
        admission.based_on_version,
        accepted.verified_graph_version().graph_version().clone(),
        "admission must fence the graph version that accepted its proposal"
    );
    let admitted = invoke!(
        "request.work.admit",
        WorkApplicationInvocationV1::AdmitExecution(admission.clone())
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::AdmitExecution(ApplicationOutcome::Effect(effect)),
        ..
    } = admitted
    else {
        panic!("execution admission must return a product mutation effect: {admitted:?}");
    };
    let admitted = effect.payload.expect("execution admission receipt");
    assert!(!admitted.mutation.replayed());

    // Committing the admission advances the Work head past the version the
    // admission command itself pinned. A byte-identical resubmission is the
    // same command, so it owes the retained receipt and the same licensed
    // execution snapshot; answering `stale` there would make the admission
    // non-idempotent and strand a caller that lost the first response.
    let replayed_admission = invoke!(
        "request.work.admit-replay",
        WorkApplicationInvocationV1::AdmitExecution(admission)
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::AdmitExecution(ApplicationOutcome::Effect(effect)),
        ..
    } = replayed_admission
    else {
        panic!(
            "replayed execution admission must return a product mutation effect: \
             {replayed_admission:?}"
        );
    };
    let replayed_admission = effect
        .payload
        .expect("replayed execution admission receipt");
    assert!(replayed_admission.mutation.replayed());
    assert_eq!(
        replayed_admission.mutation.event(),
        admitted.mutation.event()
    );
    assert_eq!(
        replayed_admission.execution_snapshot, admitted.execution_snapshot,
        "the replayed admission must license the identical execution snapshot"
    );

    let prepared_task_acceptance = invoke!(
        "request.work.prepare-accept-task",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: product_selection.clone(),
            change: WorkProductChangeDraftV1::AcceptTask {
                task_id: task_id.clone(),
                evidence_by_criterion: BTreeMap::new(),
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared_task_acceptance
    else {
        panic!(
            "task acceptance preparation must return Work evidence: {prepared_task_acceptance:?}"
        );
    };
    let accepted_task = invoke!(
        "request.work.accept-task",
        WorkApplicationInvocationV1::MutateGraph(packet.payload.expect("prepared task acceptance"))
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::MutateGraph(ApplicationOutcome::Effect(effect)),
        ..
    } = accepted_task
    else {
        panic!("task acceptance must return a product mutation effect: {accepted_task:?}");
    };
    let accepted_task = effect.payload.expect("task acceptance receipt");

    let read = invoke!(
        "request.work.product-view",
        WorkApplicationInvocationV1::Views(WorkGraphReadRequestV1::current(
            product_selection.clone(),
            UtcMicros(100),
        ))
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::Views(ApplicationOutcome::Evidence(packet)),
        ..
    } = read
    else {
        panic!("product graph read must return Work evidence: {read:?}");
    };
    let graph = packet.payload.expect("product graph payload");
    assert_eq!(
        graph
            .entries()
            .last()
            .expect("current product graph entry")
            .verified_version(),
        accepted_task.verified_graph_version(),
        "the view must expose the exact version that accepted the task"
    );
    let item = graph
        .entries()
        .last()
        .expect("current product graph entry")
        .graph()
        .items()
        .iter()
        .find(|item| item.task_id() == &task_id)
        .expect("created task remains in the product graph");
    assert!(item.is_execution_admitted());
    assert!(item.is_accepted());

    // An accepted proposal that abstained names no provider route, so there is
    // no execution snapshot to admit. The refusal must say the request cannot
    // be satisfied, not that the graph authority is unavailable after a delay:
    // every authority here is mounted, and no wait produces a route the
    // proposal never recommended.
    let unrouted_task_id =
        tracedecay_domain::TaskId::new("task.work.core-invocation-unrouted").expect("task id");
    let (initiative, plan, milestone, item) = product_task(
        "core-invocation-unrouted",
        unrouted_task_id.clone(),
        UtcMicros(10),
    );
    let prepared_unrouted = invoke!(
        "request.work.prepare-create-unrouted",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: product_selection.clone(),
            change: WorkProductChangeDraftV1::CreateTask {
                initiative,
                plan,
                milestone,
                item: Box::new(item),
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared_unrouted
    else {
        panic!("unrouted task preparation must return Work evidence: {prepared_unrouted:?}");
    };
    let created_unrouted = invoke!(
        "request.work.create-unrouted",
        WorkApplicationInvocationV1::MutateGraph(packet.payload.expect("prepared unrouted task"))
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::MutateGraph(ApplicationOutcome::Effect(effect)),
        ..
    } = created_unrouted
    else {
        panic!("unrouted task creation must return a Work effect: {created_unrouted:?}");
    };
    let created_unrouted = effect.payload.expect("created unrouted task receipt");

    let unrouted_proposal = WorkProposalV1::new(
        ProposalId::new("proposal.work.core-invocation-unrouted").expect("proposal id"),
        unrouted_task_id.clone(),
        created_unrouted.verified_graph_version().graph_version(),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1).expect("proposal shape"),
        WorkSizingV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, "unrouted fixture work")
            .expect("proposal sizing"),
        Vec::new(),
        WorkRouteDecisionV1::abstain("no provider route is eligible for this task")
            .expect("proposal route"),
        "Accept a task no configured route can execute".to_owned(),
        ManifestDigest::new(format!("sha256:{}", "2".repeat(64))).expect("proposal digest"),
        configuration_digest.clone(),
    )
    .expect("unrouted proposal");
    let prepared_unrouted_accept = invoke!(
        "request.work.prepare-accept-unrouted",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: product_selection.clone(),
            change: WorkProductChangeDraftV1::DecideProposal {
                proposal: unrouted_proposal,
                disposition: WorkProposalDispositionV1::Accepted,
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared_unrouted_accept
    else {
        panic!(
            "unrouted proposal preparation must return Work evidence: \
             {prepared_unrouted_accept:?}"
        );
    };
    let accepted_unrouted = invoke!(
        "request.work.accept-unrouted",
        WorkApplicationInvocationV1::MutateGraph(
            packet.payload.expect("prepared unrouted proposal")
        )
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::MutateGraph(ApplicationOutcome::Effect(_)),
        ..
    } = accepted_unrouted
    else {
        panic!("unrouted proposal acceptance must return a Work effect: {accepted_unrouted:?}");
    };
    let prepared_unrouted_admission = invoke!(
        "request.work.prepare-admit-unrouted",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: product_selection,
            change: WorkProductChangeDraftV1::AdmitExecution {
                task_id: unrouted_task_id,
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared_unrouted_admission
    else {
        panic!(
            "unrouted admission preparation must return Work evidence: \
             {prepared_unrouted_admission:?}"
        );
    };
    let unrouted_admission = invoke!(
        "request.work.admit-unrouted",
        WorkApplicationInvocationV1::AdmitExecution(
            match packet.payload.expect("prepared unrouted admission") {
                WorkProductMutationRequestV1::AdmitExecution(admission) => admission,
                other => panic!("preparation must produce an execution admission: {other:?}"),
            }
        )
    );
    let DaemonInvocationOutcome::ApplicationProblem { problem } = unrouted_admission else {
        panic!("admitting an abstained proposal must be refused: {unrouted_admission:?}");
    };
    assert_eq!(
        problem,
        tracedecay_contracts::ApplicationProblem::invalid_request(
            "work.invalid_graph_operation",
            "The Work graph request is invalid"
        ),
        "an unroutable admission is a request to correct, not an authority to retry"
    );
}

/// The Task-family activity producer behind the dashboard's `task_activity`
/// stream. A committed Work mutation must raise exactly one Task pulse against
/// the registered project, and a projection read must raise none, the
/// dispatcher's read arms never reach the effect path that publishes.
#[tokio::test]
async fn committed_work_mutations_publish_task_activity_and_reads_do_not() {
    let fixture = RegisteredWorkFixture::start("task-activity").await;
    let (database, project_id, scope) = (
        fixture.database.clone(),
        fixture.scope.project_id.clone(),
        fixture.scope.clone(),
    );

    macro_rules! invoke {
        ($request_id:literal, $request:expr) => {
            fixture.invoke($request_id, $request).await
        };
    }

    let product_selection = WorkProductSelectionScopeV1::relations(
        [WorkProductAuthorizedRelationScopeV1::Repository {
            project_id: scope.project_id.clone(),
            repository_id: scope.repository_id.clone(),
        }]
        .into_iter()
        .collect(),
    )
    .expect("product selection");
    let task_id = TaskId::new("task.work.task-activity").expect("task id");
    let (initiative, plan, milestone, item) = product_task("task-activity", task_id, UtcMicros(10));

    // Preparing a product mutation reads the real graph authority but does
    // not commit an event, so the activity lane must remain empty.
    let prepared = invoke!(
        "request.work.activity-prepare-create",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: product_selection.clone(),
            change: WorkProductChangeDraftV1::CreateTask {
                initiative,
                plan,
                milestone,
                item: Box::new(item),
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared
    else {
        panic!("product task preparation must return Work evidence: {prepared:?}");
    };
    let prepared = packet.payload.expect("prepared product task mutation");
    assert!(
        tracedecay_session_memory::event_lane::replay_after(&database, project_id.as_str(), None)
            .await
            .expect("activity replay")
            .expect("activity replay present")
            .records
            .is_empty(),
        "a prepared Work mutation must not publish task activity"
    );

    let created = invoke!(
        "request.work.activity-create",
        WorkApplicationInvocationV1::MutateGraph(prepared)
    );
    assert!(
        matches!(
            created,
            DaemonInvocationOutcome::WorkApplication {
                outcome: WorkApplicationOutcomeV1::MutateGraph(ApplicationOutcome::Effect(_)),
                ..
            }
        ),
        "product task creation must return a Work effect: {created:?}"
    );

    let replay =
        tracedecay_session_memory::event_lane::replay_after(&database, project_id.as_str(), None)
            .await
            .expect("activity replay")
            .expect("activity replay present");
    assert_eq!(
        replay.records.len(),
        1,
        "one committed Work mutation must publish exactly one pulse: {replay:?}"
    );
    let pulse = &replay.records[0].pulse;
    assert_eq!(
        pulse.family,
        tracedecay_session_memory::event_lane::ActivityFamilyV1::Task
    );
    assert_eq!(pulse.project_id.as_deref(), Some(project_id.as_str()));
    assert_eq!(pulse.units, 1);
    // Only attempt mutations carry a detail: the canonical `task` payload
    // admits attempt states and nothing else.
    assert_eq!(pulse.detail, None);

    // A canonical graph read after the mutation must not add a second pulse.
    let view_after = invoke!(
        "request.work.activity-view-after",
        WorkApplicationInvocationV1::Views(WorkGraphReadRequestV1::current(
            product_selection,
            UtcMicros(100),
        ))
    );
    assert!(
        matches!(
            view_after,
            DaemonInvocationOutcome::WorkApplication {
                outcome: WorkApplicationOutcomeV1::Views(ApplicationOutcome::Evidence(_)),
                ..
            }
        ),
        "product graph view must return Work evidence: {view_after:?}"
    );
    assert_eq!(
        tracedecay_session_memory::event_lane::replay_after(&database, project_id.as_str(), None)
            .await
            .expect("activity replay")
            .expect("activity replay present")
            .records
            .len(),
        1,
        "a product graph read must not publish task activity"
    );
}

#[tokio::test]
async fn a_work_dispatch_whose_caller_stops_waiting_still_completes() {
    let fixture = RegisteredWorkFixture::start("dropped-caller").await;
    let project_id = fixture.scope.project_id.clone();
    let task_id = TaskId::new("task.work.dropped-caller").expect("task id");
    let (initiative, plan, milestone, item) =
        product_task("dropped-caller", task_id, UtcMicros(10));
    let prepared = fixture
        .invoke(
            "request.work.dropped-caller-prepare",
            WorkApplicationInvocationV1::PrepareGraphMutation(
                PrepareWorkProductMutationRequestV1 {
                    selection: WorkProductSelectionScopeV1::relations(
                        [WorkProductAuthorizedRelationScopeV1::Repository {
                            project_id: fixture.scope.project_id.clone(),
                            repository_id: fixture.scope.repository_id.clone(),
                        }]
                        .into_iter()
                        .collect(),
                    )
                    .expect("product selection"),
                    change: WorkProductChangeDraftV1::CreateTask {
                        initiative,
                        plan,
                        milestone,
                        item: Box::new(item),
                    },
                    causation_event_id: None,
                    evidence: Vec::new(),
                },
            ),
        )
        .await;
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared
    else {
        panic!("product task preparation must return Work evidence: {prepared:?}");
    };
    let project_root = fixture.project.path().to_path_buf();
    let canonical_root = project_root.canonicalize().expect("canonical project root");
    let registered = fixture
        .service
        .project_runtimes
        .request_runtimes(Some(&project_root), Some(&canonical_root))
        .await
        .work
        .expect("registered Work runtime");
    let observability = fixture
        .service
        .observability_producer(Some(&project_root))
        .await;
    let mut created = Box::pin(execute_work_application(
        registered,
        Arc::clone(&fixture.service.work_attempt_processes),
        observability,
        Some(project_root),
        "request.work.dropped-caller-create".to_owned(),
        WorkApplicationInvocationV1::MutateGraph(packet.payload.expect("prepared mutation")),
        UtcMicros(100),
        Deadline::new(UtcMicros(1_000)).expect("deadline"),
        CancellationContext::active("cancel.request.work.dropped-caller-create")
            .expect("cancellation"),
    ));
    let pending = std::future::poll_fn(|context| {
        std::task::Poll::Ready(created.as_mut().poll(context).is_pending())
    })
    .await;
    assert!(
        pending,
        "the dispatch is still committing when its caller stops waiting"
    );
    drop(created);

    let pulses = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let records = tracedecay_session_memory::event_lane::replay_after(
                &fixture.database,
                project_id.as_str(),
                None,
            )
            .await
            .expect("activity replay")
            .map_or(0, |replay| replay.records.len());
            if records > 0 {
                return records;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the abandoned dispatch must still commit and publish its activity");
    assert_eq!(
        pulses, 1,
        "the committed mutation publishes exactly one pulse"
    );
}

/// A profile that never created Work answers the views read, through the same
/// daemon invocation path MCP, CLI and the dashboard use, with the typed
/// absence under its authorized selection. A selection naming a project the
/// caller is not scoped to is still the concealed denial.
#[tokio::test]
async fn a_views_read_before_any_task_is_absent_and_a_foreign_selection_stays_denied() {
    let fixture = RegisteredWorkFixture::start("absent-view").await;

    let absent = fixture
        .invoke(
            "request.work.absent-view",
            WorkApplicationInvocationV1::Views(WorkGraphReadRequestV1::current(
                WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                UtcMicros(100),
            )),
        )
        .await;
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::Views(ApplicationOutcome::Evidence(packet)),
        ..
    } = absent
    else {
        panic!("an authorized read of no graph must be answered: {absent:?}");
    };
    let mut wire = serde_json::to_value(packet.payload.expect("absent graph payload"))
        .expect("serialize the views answer");
    let owner = wire["authorized_scope"]
        .as_object_mut()
        .expect("authorized scope");
    assert!(
        owner
            .remove("owner_brain_id")
            .is_some_and(|id| id.is_string())
    );
    assert!(
        owner
            .remove("owner_profile_id")
            .is_some_and(|id| id.is_string())
    );
    assert_eq!(
        wire,
        serde_json::json!({
            "mode": "absent",
            "authorized_scope": {"selection": {"selection": "profile_owned_no_git"}},
            "selection_coverage": {"coverage": "complete", "covered_events": 0},
        })
    );

    let foreign = fixture
        .invoke(
            "request.work.foreign-view",
            WorkApplicationInvocationV1::Views(WorkGraphReadRequestV1::current(
                WorkProductSelectionScopeV1::relations(
                    [WorkProductAuthorizedRelationScopeV1::Project {
                        project_id: ProjectId::new("project.work.someone-else")
                            .expect("foreign project id"),
                    }]
                    .into_iter()
                    .collect(),
                )
                .expect("foreign selection"),
                UtcMicros(100),
            )),
        )
        .await;
    let DaemonInvocationOutcome::ApplicationProblem { problem } = foreign else {
        panic!("a foreign selection must stay denied: {foreign:?}");
    };
    assert_eq!(
        problem,
        tracedecay_contracts::ApplicationProblem::not_found_or_not_authorized(
            tracedecay_contracts::RetryDirective::Never
        )
    );
}

/// Project open mints the Work grant once and nothing renews it, so a Work
/// runtime registered one horizon and one hour ago must still admit requests.
#[tokio::test]
async fn work_requests_admit_past_the_registration_horizon() {
    let now = tracedecay_contracts::now_micros();
    let horizon = i64::try_from(GRANT_HORIZON.as_micros()).expect("grant horizon");
    let opened_at = UtcMicros(now.0 - horizon - 3_600_000_000);
    let fixture = RegisteredWorkFixture::start_with_grant(
        "registration-horizon",
        opened_at,
        UtcMicros(opened_at.0 + horizon),
    )
    .await;

    let outcome = invoke_registered(
        fixture.service.clone(),
        Arc::clone(&fixture.registry),
        fixture.project.path().to_path_buf(),
        DaemonInvocationRequest::work_application(
            "request.work.registration-horizon",
            WorkApplicationInvocationV1::Views(WorkGraphReadRequestV1::current(
                WorkProductSelectionScopeV1::ProfileOwnedNoGit,
                now,
            )),
            now,
            Deadline::new(UtcMicros(now.0 + 30_000_000)).expect("deadline"),
            CancellationContext::active("cancel.request.work.registration-horizon")
                .expect("cancellation"),
        ),
    )
    .await;
    assert!(
        matches!(
            outcome,
            DaemonInvocationOutcome::WorkApplication {
                outcome: WorkApplicationOutcomeV1::Views(ApplicationOutcome::Evidence(_)),
                ..
            }
        ),
        "a mounted Work runtime must keep serving past the registration horizon: {outcome:?}"
    );
}

/// One registered project with the Work runtime mounted and an empty proposal
/// route table, invoked through the production daemon invocation service.
struct RegisteredWorkFixture {
    _host: tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1,
    _profile: tempfile::TempDir,
    project: tempfile::TempDir,
    database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    scope: ResolvedScope,
    policy_digest: ManifestDigest,
    configuration_digest: ManifestDigest,
    service: DaemonInvocationService,
    registry: Arc<Mutex<LspSessionRegistry>>,
}

impl RegisteredWorkFixture {
    async fn start(name: &str) -> Self {
        // Workflow dispatch admits at the wall clock, not the request's
        // observed time.
        let expires_at = UtcMicros(tracedecay_contracts::now_micros().0 + 3_600_000_000);
        Self::start_with_grant(name, UtcMicros(1), expires_at).await
    }

    async fn start_with_grant(name: &str, issued_at: UtcMicros, expires_at: UtcMicros) -> Self {
        let profile = tempfile::tempdir().expect("profile root");
        let project = tempfile::tempdir().expect("project root");
        let project_id = ProjectId::new(format!("project.work.{name}")).expect("project id");
        let host =
            tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
                profile.path(),
                project.path(),
                project_id.clone(),
            )
            .await
            .expect("registered project runtime");
        let database = host
            .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
            .expect("registered project database");
        let actor = ActorId::new(format!("actor.work.{name}")).expect("actor id");
        let scope = ResolvedScope::new(
            project_id,
            tracedecay_domain::RepositoryId::new(format!("repository.work.{name}"))
                .expect("repository id"),
            tracedecay_domain::WorktreeId::new(format!("worktree.work.{name}"))
                .expect("worktree id"),
            None,
        )
        .expect("resolved scope");
        let grant_digest =
            ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("grant digest");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new(format!("grant.work.{name}")).expect("grant id"),
            1,
            grant_digest.clone(),
            actor.clone(),
            issued_at,
            expires_at,
            scope.clone(),
            tracedecay_contracts::WORK_APPLICATION_OPERATION_IDS_V1
                .iter()
                .chain(&tracedecay_contracts::WORKFLOW_APPLICATION_OPERATION_IDS)
                .map(|(_, capability, _)| CapabilityId::new(*capability).expect("capability"))
                .collect(),
            tracedecay_contracts::WORK_APPLICATION_OPERATION_IDS_V1
                .iter()
                .chain(&tracedecay_contracts::WORKFLOW_APPLICATION_OPERATION_IDS)
                .map(|(_, _, use_case)| UseCaseId::new(*use_case).expect("use case"))
                .collect(),
            DisclosureClass::Sensitive,
        )
        .expect("Work grant");
        let authority = WorkAuthority::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            actor.clone(),
            grant_digest,
        )
        .expect("Work authority");
        let service = DaemonInvocationService::default();
        let (proposal_routing, configuration_digest) =
            empty_work_proposal_routing(scope.clone(), &grant);
        let policy_digest = mount_test_work_observability(
            &service,
            project.path(),
            database.clone(),
            &scope,
            &configuration_digest,
        )
        .await;
        DaemonWorkRuntimeRegistrar::new(&service)
            .register(
                project.path().to_path_buf(),
                database.clone(),
                authority,
                actor,
                grant,
                policy_digest.clone(),
                configuration_digest.clone(),
                tracedecay_domain::configuration::safe_work_topology_policy_v1(),
                proposal_routing,
                denied_work_evidence_retrieval(),
            )
            .await
            .expect("registered Work runtime");
        Self {
            _host: host,
            _profile: profile,
            project,
            database,
            scope,
            policy_digest,
            configuration_digest,
            service,
            registry: Arc::new(Mutex::new(LspSessionRegistry::default())),
        }
    }

    async fn invoke(
        &self,
        request_id: &'static str,
        request: WorkApplicationInvocationV1,
    ) -> DaemonInvocationOutcome {
        invoke_registered(
            self.service.clone(),
            Arc::clone(&self.registry),
            self.project.path().to_path_buf(),
            DaemonInvocationRequest::work_application(
                request_id,
                request,
                UtcMicros(100),
                Deadline::new(UtcMicros(1_000)).expect("deadline"),
                CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
            ),
        )
        .await
    }
}

async fn invoke_registered(
    service: DaemonInvocationService,
    registry: Arc<Mutex<LspSessionRegistry>>,
    project_root: std::path::PathBuf,
    request: DaemonInvocationRequest,
) -> DaemonInvocationOutcome {
    service
        .invoke(&registry, Some(&project_root), None, None, None, request)
        .await
        .outcome
}

/// Broker-socket connections run on native runtime workers. Work and Workflow
/// dispatch awaits its writer jobs and storage reads, so it never parks one:
/// a parked worker drives no other task.
#[tokio::test]
async fn work_and_workflow_dispatch_never_park_a_native_worker_on_the_writer() {
    let fixture = RegisteredWorkFixture::start("native-writer-waits").await;
    let wall_clock = tracedecay_contracts::now_micros();
    let workflow = |request_id: &str, request| {
        DaemonInvocationRequest::workflow_application(
            request_id,
            request,
            wall_clock,
            Deadline::new(UtcMicros(wall_clock.0 + 600_000_000)).expect("deadline"),
            CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
        )
    };
    let digest = |byte: char| {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    };
    let definition = tracedecay_domain::WorkflowDefinition::new(
        tracedecay_domain::WorkflowDefinitionId::new("workflow.native-writer-waits")
            .expect("definition id"),
        1,
        fixture.scope.project_id.clone(),
        vec![tracedecay_domain::WorkflowStep {
            step_id: tracedecay_domain::WorkflowStepId::new("prepare").expect("step id"),
            operation: tracedecay_domain::WorkflowOperationRef::new("operation.work.start_attempt")
                .expect("operation"),
            predecessors: BTreeSet::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            fan_out: None,
        }],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .expect("workflow definition");
    let requests = vec![
        workflow(
            "request.workflow.native-writer-waits.register",
            tracedecay_daemon_protocol::WorkflowApplicationInvocation::RegisterDefinition(
                tracedecay_contracts::WorkflowDefinitionRegisterRequest { definition },
            ),
        ),
        workflow(
            "request.workflow.native-writer-waits.get-run",
            tracedecay_daemon_protocol::WorkflowApplicationInvocation::GetRun(
                tracedecay_contracts::WorkflowRunGetRequest {
                    run_id: tracedecay_domain::RunId::new("run.native-writer-waits")
                        .expect("run id"),
                },
            ),
        ),
        DaemonInvocationRequest::work_application(
            "request.work.native-writer-waits.list",
            WorkApplicationInvocationV1::ListAttempts(
                tracedecay_contracts::WorkAttemptListRequestV1 {
                    page_size: 10,
                    cursor: None,
                },
            ),
            UtcMicros(100),
            Deadline::new(UtcMicros(1_000)).expect("deadline"),
            CancellationContext::active("cancel.request.work.native-writer-waits.list")
                .expect("cancellation"),
        ),
    ];
    let service = fixture.service.clone();
    let registry = Arc::clone(&fixture.registry);
    let project_root = fixture.project.path().to_path_buf();

    let writer_waits = tracedecay_runtime_core::native_runtime::native_thread_writer_waits();
    let served = tracedecay_runtime_core::native_runtime::NativeTask::spawn(async move {
        let mut outcomes = Vec::new();
        for request in requests {
            outcomes.push(
                invoke_registered(
                    service.clone(),
                    Arc::clone(&registry),
                    project_root.clone(),
                    request,
                )
                .await,
            );
        }
        let binding = service
            .work_fan_out_binding(
                Some(&project_root),
                &tracedecay_domain::WorkAttemptIdentityV1::new(
                    TaskId::new("task.native-writer-waits").expect("task id"),
                    tracedecay_domain::RunId::new("run.native-writer-waits").expect("run id"),
                    tracedecay_domain::AttemptId::new("attempt.native-writer-waits")
                        .expect("attempt id"),
                )
                .expect("attempt identity"),
            )
            .await;
        (outcomes, binding)
    })
    .expect("native worker task")
    .wait()
    .await;
    let tracedecay_runtime_core::native_runtime::NativeTaskOutcome::Completed((outcomes, binding)) =
        served
    else {
        panic!("the native worker must serve every request: {served:?}");
    };
    assert_eq!(
        tracedecay_runtime_core::native_runtime::native_thread_writer_waits(),
        writer_waits,
        "Work and Workflow dispatch must not park a native worker on the writer"
    );

    let [register, get_run, list] = outcomes.as_slice() else {
        panic!("three outcomes: {outcomes:?}");
    };
    assert!(
        matches!(
            register,
            DaemonInvocationOutcome::WorkflowApplication { .. }
        ),
        "registration must reach the workflow effect journal: {register:?}"
    );
    assert!(
        matches!(get_run, DaemonInvocationOutcome::ApplicationProblem { .. }),
        "an unknown run must be read from storage and refused: {get_run:?}"
    );
    assert!(
        matches!(list, DaemonInvocationOutcome::WorkApplication { .. }),
        "the attempt list must be answered: {list:?}"
    );
    assert_eq!(binding, None, "no workflow owns an unknown attempt");
}

fn native_storage_wait_site_hits() -> BTreeMap<(&'static str, String), u64> {
    tracedecay_rusqlite_runtime::exact_sql::native_thread_storage_wait_sites()
        .into_iter()
        .map(|site| ((site.kind, site.call_site), site.hits))
        .collect()
}

#[tokio::test]
async fn a_workflow_fan_out_run_never_parks_a_native_worker_on_storage() {
    let fixture = RegisteredWorkFixture::start("native-fan-out").await;
    let wall_clock = tracedecay_contracts::now_micros();
    let workflow = move |request_id: &str, request| {
        DaemonInvocationRequest::workflow_application(
            request_id,
            request,
            wall_clock,
            Deadline::new(UtcMicros(wall_clock.0 + 600_000_000)).expect("deadline"),
            CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
        )
    };
    let definition_id =
        tracedecay_domain::WorkflowDefinitionId::new("workflow.native-fan-out").expect("id");
    let definition = tracedecay_domain::WorkflowDefinition::new(
        definition_id.clone(),
        1,
        fixture.scope.project_id.clone(),
        vec![tracedecay_domain::WorkflowStep {
            step_id: tracedecay_domain::WorkflowStepId::new("fan-out").expect("step id"),
            operation: tracedecay_domain::WorkflowOperationRef::new("operation.work.start_attempt")
                .expect("operation"),
            predecessors: BTreeSet::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            fan_out: Some(tracedecay_domain::WorkflowFanOut { max_width: 1 }),
        }],
        fixture.policy_digest.clone(),
        fixture.configuration_digest.clone(),
        tracedecay_contracts::work_executable_catalog_digest().expect("catalog digest"),
    )
    .expect("workflow definition");
    let route = tracedecay_domain::WorkProviderRouteV1::new(
        tracedecay_domain::ProviderId::new("provider.work.claude-code-cli").expect("provider"),
        tracedecay_domain::WorkProviderRouteId::new("route.native-fan-out").expect("route"),
    )
    .expect("provider route");
    let execution_snapshot = tracedecay_domain::WorkExecutionSnapshot::new(
        tracedecay_domain::WorkExecutionSnapshotInput {
            configuration_revision_id:
                tracedecay_domain::configuration::ConfigurationRevisionId::new(
                    "configuration.revision.work-empty-routing",
                )
                .expect("configuration revision"),
            configuration_snapshot_id: tracedecay_domain::ConfigurationSnapshotId::new(
                "configuration.snapshot.native-fan-out",
            )
            .expect("configuration snapshot"),
            effective_behavior_digest: fixture.configuration_digest.clone(),
            resolution_provenance_digest: fixture.configuration_digest.clone(),
            route: route.clone(),
            backend: tracedecay_domain::WorkProviderBackendV1::ClaudeCodeCli,
            protocol: tracedecay_domain::WorkProviderProtocol::ClaudeStreamJson,
            model: "fixture-model".to_owned(),
            executable: tracedecay_domain::WorkExecutableReference::new(
                "executable.native-fan-out".to_owned(),
                ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).expect("digest"),
            )
            .expect("executable reference"),
            sandbox: tracedecay_domain::WorkSandboxPolicy::Required,
            approval: tracedecay_domain::WorkApprovalPolicy::Never,
            filesystem: tracedecay_domain::WorkFilesystemPolicy::WorkspaceWrite,
            egress: tracedecay_domain::WorkEgressPolicy::Deny,
            environment_allowlist: BTreeSet::new(),
            credential_references: BTreeSet::new(),
            limits: tracedecay_domain::WorkExecutionLimits::new(
                128_000, 8_192, 16_384, 16_384, 65_536, 2,
            )
            .expect("execution limits"),
            deadline: UtcMicros(wall_clock.0 + 300_000_000),
            fallback: tracedecay_domain::WorkFallbackTopology::Disabled,
            topology: tracedecay_domain::configuration::safe_work_topology_policy_v1(),
        },
    )
    .expect("execution snapshot");
    let task_id = TaskId::new("task.native-fan-out.child").expect("task id");
    let (initiative, plan, milestone, item) =
        product_task("native-fan-out", task_id.clone(), wall_clock);
    let input_digest = ManifestDigest::new(format!("sha256:{}", "f".repeat(64))).expect("digest");
    let proposal = WorkProposalV1::new(
        ProposalId::new("proposal.native-fan-out.child").expect("proposal id"),
        task_id,
        tracedecay_domain::WorkGraphVersionV1::new(1).expect("graph version"),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1).expect("shape"),
        WorkSizingV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, "complete the child").expect("size"),
        Vec::new(),
        WorkRouteDecisionV1::abstain("workflow provider is pinned by admission").expect("route"),
        "Execute the fan-out child".to_owned(),
        input_digest.clone(),
        input_digest.clone(),
    )
    .expect("child proposal");
    let run_id = tracedecay_domain::RunId::new("run.native-fan-out").expect("run id");
    let start = tracedecay_contracts::WorkflowRunStartRequest {
        run_id: run_id.clone(),
        definition_id: definition_id.clone(),
        definition_version: 1,
        provider: tracedecay_contracts::WorkflowProviderRegistration::new(
            route,
            tracedecay_domain::WorkProviderBackendV1::ClaudeCodeCli,
            "fixture-model".to_owned(),
            1,
        )
        .expect("provider registration"),
        fan_out: Some(tracedecay_contracts::WorkflowFanOutStartV1 {
            fence: tracedecay_contracts::WorkflowExecutionFence {
                attempt_id: tracedecay_domain::AttemptId::new("attempt.native-fan-out.controller")
                    .expect("attempt id"),
                lease: tracedecay_domain::WorkLeaseFenceV1::new(
                    tracedecay_domain::WorkLeaseId::new("lease.native-fan-out.controller")
                        .expect("lease id"),
                    tracedecay_domain::WorkFenceEpochV1::new(1).expect("fence epoch"),
                )
                .expect("lease fence"),
            },
            max_parallel: 1,
            failure_policy: tracedecay_contracts::WorkflowFailurePolicy::Collect,
            execution_snapshot,
            reference: None,
            commit: tracedecay_domain::CommitId::new("0123456789abcdef0123456789abcdef01234567")
                .expect("commit"),
            effect_state: tracedecay_domain::WorkEffectStateV1::Observational,
            inputs: vec![tracedecay_contracts::WorkflowFanOutInput {
                instructions: "native fan-out child".to_owned(),
                input_digest,
                initiative,
                plan,
                milestone,
                item,
                proposal,
            }],
        }),
        command_id: tracedecay_domain::WorkCommandId::new("command.native-fan-out.start")
            .expect("command id"),
    };
    let setup = [
        workflow(
            "request.workflow.native-fan-out.register",
            tracedecay_daemon_protocol::WorkflowApplicationInvocation::RegisterDefinition(
                tracedecay_contracts::WorkflowDefinitionRegisterRequest { definition },
            ),
        ),
        workflow(
            "request.workflow.native-fan-out.activate",
            tracedecay_daemon_protocol::WorkflowApplicationInvocation::ActivateDefinition(
                tracedecay_contracts::WorkflowDefinitionActivateRequest {
                    definition_id,
                    definition_version: 1,
                    expected_revision: 1,
                },
            ),
        ),
    ];

    let service = fixture.service.clone();
    let registry = Arc::clone(&fixture.registry);
    let project_root = fixture.project.path().to_path_buf();
    let worktree_id = fixture.scope.worktree_id.clone();
    let get_run = move |attempt: u32| {
        workflow(
            &format!("request.workflow.native-fan-out.get-run-{attempt}"),
            tracedecay_daemon_protocol::WorkflowApplicationInvocation::GetRun(
                tracedecay_contracts::WorkflowRunGetRequest {
                    run_id: run_id.clone(),
                },
            ),
        )
    };
    let start = workflow(
        "request.workflow.native-fan-out.start",
        tracedecay_daemon_protocol::WorkflowApplicationInvocation::StartRun(Box::new(start)),
    );
    let sites = native_storage_wait_site_hits();
    let served = tracedecay_runtime_core::native_runtime::NativeTask::spawn(async move {
        let invoke = |request| {
            invoke_registered(
                service.clone(),
                Arc::clone(&registry),
                project_root.clone(),
                request,
            )
        };
        let mut defined = Vec::new();
        for request in setup {
            defined.push(invoke(request).await);
        }
        let started = invoke(start).await;
        for attempt in 0..600 {
            let outcome = invoke(get_run(attempt)).await;
            if let DaemonInvocationOutcome::WorkflowApplication {
                outcome:
                    tracedecay_daemon_protocol::WorkflowApplicationOutcome::GetRun(
                        ApplicationOutcome::Evidence(packet),
                    ),
                ..
            } = &outcome
                && packet
                    .payload
                    .as_ref()
                    .is_some_and(|run| run.status().is_terminal())
            {
                while service.work_attempt_processes.holds_worktree(&worktree_id) {
                    tracedecay_runtime_core::native_runtime::sleep_until(
                        std::time::Instant::now() + std::time::Duration::from_millis(50),
                    )
                    .await
                    .expect("native timer");
                }
                return (defined, started, outcome);
            }
            tracedecay_runtime_core::native_runtime::sleep_until(
                std::time::Instant::now() + std::time::Duration::from_millis(100),
            )
            .await
            .expect("native timer");
        }
        panic!("the fan-out run must reach a terminal status: {started:?}");
    })
    .expect("native worker task");
    let served = tracedecay_runtime_core::native_runtime::timeout(
        std::time::Duration::from_secs(120),
        served.wait(),
    )
    .await
    .expect("the fan-out run settles and releases its attempt within two minutes");
    let tracedecay_runtime_core::native_runtime::NativeTaskOutcome::Completed((
        defined,
        started,
        settled,
    )) = served
    else {
        panic!("the native worker must serve the fan-out run: {served:?}");
    };
    let parked = native_storage_wait_site_hits()
        .into_iter()
        .filter(|(key, hits)| {
            (key.1
                .contains("tracedecay-daemon-service/src/invocation/work")
                || key.1.contains("tracedecay-rusqlite-runtime/src/work"))
                && sites.get(key) != Some(hits)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        parked,
        Vec::new(),
        "a Workflow fan-out run must not park a native worker on Work or Workflow storage"
    );
    assert!(
        defined
            .iter()
            .all(|outcome| matches!(outcome, DaemonInvocationOutcome::WorkflowApplication { .. })),
        "the definition must register and activate: {defined:?}"
    );
    assert!(
        matches!(
            started,
            DaemonInvocationOutcome::WorkflowApplication {
                outcome: tracedecay_daemon_protocol::WorkflowApplicationOutcome::StartRun(
                    ApplicationOutcome::Effect(_)
                ),
                ..
            }
        ),
        "the fan-out run must start: {started:?}"
    );
    let DaemonInvocationOutcome::WorkflowApplication {
        outcome:
            tracedecay_daemon_protocol::WorkflowApplicationOutcome::GetRun(
                ApplicationOutcome::Evidence(packet),
            ),
        ..
    } = settled
    else {
        panic!("the settled run must be read back: {settled:?}");
    };
    let run = packet.payload.expect("settled run");
    assert_eq!(
        run.settled_fan_out_attempts().len(),
        1,
        "the child attempt must settle into the fan-out: {run:?}"
    );
}
