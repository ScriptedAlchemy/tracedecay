//! `work` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_application::{
    GenerateProposalRequest, PrepareWorkProductMutationRequestV1, WorkGraphReadRequestV1,
    WorkProductChangeDraftV1, WorkProductMutationRequestV1, WorkProductSelectionScopeV1,
    WorkRelationScopeV1,
};
use tracedecay_domain::{
    InitiativeId, MilestoneId, ProposalId, TaskId, WorkHierarchyV1, WorkInitiativeV1,
    WorkItemInputV1, WorkItemV1, WorkMilestoneV1, WorkPlanId, WorkPlanV1,
    WorkProposalDispositionV1, WorkProposalV1, WorkRouteDecisionV1, WorkScoreKindV1,
    WorkShapeAssessmentV1, WorkSizingV1,
};
use tracedecay_policy::work_loop::WorkProposalReasonV1;

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
    let _pin = crate::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.work.core-invocation").expect("project id");
    let host = crate::host_admission::HostAdmissionTestRuntimeV1::project(
        crate::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered project runtime");
    let database = host
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Project)
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
    let capabilities = tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
        .iter()
        .map(|(_, capability, _)| CapabilityId::new(*capability).expect("capability"))
        .collect();
    let use_cases = tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
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
    let (proposal_routing, configuration_digest) = empty_work_proposal_routing(scope.clone());
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
        [WorkRelationScopeV1::Repository {
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
        .expect("an empty pinned route set remains an explained decision");
    assert!(route_plan.ranked.is_empty());
    assert!(
        generated
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
            .map(|revision| revision.as_str()),
        Some("configuration.revision.work-empty-routing")
    );

    let proposal = WorkProposalV1::new(
        ProposalId::new("proposal.work.core-invocation").expect("proposal id"),
        task_id.clone(),
        created.verified_graph_version().graph_version().clone(),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1).expect("proposal shape"),
        WorkSizingV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, "explicit fixture work")
            .expect("proposal sizing"),
        Vec::new(),
        WorkRouteDecisionV1::abstain("test route has no provider selection")
            .expect("proposal route"),
        "Admit the explicitly declared Work task".to_owned(),
        proposal_digest,
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
        WorkApplicationInvocationV1::AdmitExecution(admission)
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::AdmitExecution(ApplicationOutcome::Effect(effect)),
        ..
    } = admitted
    else {
        panic!("execution admission must return a product mutation effect: {admitted:?}");
    };
    let admitted = effect.payload.expect("execution admission receipt");
    assert!(!admitted.replayed());

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
            product_selection,
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
}

/// The Task-family activity producer behind the dashboard's `task_activity`
/// stream. A committed Work mutation must raise exactly one Task pulse against
/// the registered project, and a projection read must raise none — the
/// dispatcher's read arms never reach the effect path that publishes.
#[tokio::test]
async fn committed_work_mutations_publish_task_activity_and_reads_do_not() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.work.task-activity").expect("project id");
    let host = crate::host_admission::HostAdmissionTestRuntimeV1::project(
        crate::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered project runtime");
    let database = host
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Project)
        .expect("registered project database");
    let actor = ActorId::new("actor.work.task-activity").expect("actor id");
    let scope = ResolvedScope::new(
        project_id.clone(),
        tracedecay_domain::RepositoryId::new("repository.work.task-activity")
            .expect("repository id"),
        tracedecay_domain::WorktreeId::new("worktree.work.task-activity").expect("worktree id"),
        None,
    )
    .expect("resolved scope");
    let grant_digest =
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("grant digest");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.work.task-activity").expect("grant id"),
        1,
        grant_digest.clone(),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
            .iter()
            .map(|(_, capability, _)| CapabilityId::new(*capability).expect("capability"))
            .collect(),
        tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
            .iter()
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
    let (proposal_routing, configuration_digest) = empty_work_proposal_routing(scope.clone());
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
            policy_digest,
            configuration_digest,
            tracedecay_domain::configuration::safe_work_topology_policy_v1(),
            proposal_routing,
            denied_work_evidence_retrieval(),
        )
        .await
        .expect("registered Work runtime");
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));

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
        [WorkRelationScopeV1::Repository {
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
        tracedecay_usecases::event_lane::replay_after(&database, project_id.as_str(), None)
            .await
            .expect("activity replay")
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
        tracedecay_usecases::event_lane::replay_after(&database, project_id.as_str(), None)
            .await
            .expect("activity replay");
    assert_eq!(
        replay.records.len(),
        1,
        "one committed Work mutation must publish exactly one pulse: {replay:?}"
    );
    let pulse = &replay.records[0].pulse;
    assert_eq!(
        pulse.family,
        tracedecay_usecases::event_lane::ActivityFamilyV1::Task
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
        tracedecay_usecases::event_lane::replay_after(&database, project_id.as_str(), None)
            .await
            .expect("activity replay")
            .records
            .len(),
        1,
        "a product graph read must not publish task activity"
    );
}
