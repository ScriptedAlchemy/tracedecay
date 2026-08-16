use super::*;

use std::collections::BTreeSet;

use tracedecay_application::{
    PrepareWorkProductMutationRequestV1, StartWorkAttemptCommand, WorkAttemptEvidenceRecordV1,
    WorkAttemptProviderOutcomeV1, WorkAttemptStoragePort, WorkEvidenceExpansionSelectorV1,
    WorkEvidenceRetrieveRequestV1, WorkEvidenceSourceV1, WorkGraphReadRequestV1,
    WorkProductChangeDraftV1, WorkProductMutationRequestV1, WorkProductSelectionScopeV1,
    WorkRelationScopeV1,
};
use tracedecay_domain::{
    AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, InitiativeId,
    MilestoneId, ObservationSourceIdentityV1, PrivacyDomainId, ProposalId, ProviderId, RefId,
    RepositoryId, RunId, SessionId, TaskId, TemporalModeV1, WorkApprovalPolicy,
    WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkCancellationStateV1,
    WorkEffectStateV1, WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFilesystemPolicy,
    WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1, WorkItemV1, WorkMilestoneV1, WorkPlanId,
    WorkPlanV1, WorkProposalDispositionV1, WorkProposalV1, WorkProviderBackendV1,
    WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1, WorkRecoveryStateV1,
    WorkRouteDecisionV1, WorkSandboxPolicy, WorkScoreKindV1, WorkShapeAssessmentV1, WorkSizingV1,
    WorkTerminalEvidenceV1, WorkflowOperationRef, WorktreeId,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("Work evidence journey identity")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("Work evidence journey digest")
}

fn product_task(task_id: TaskId) -> (WorkInitiativeV1, WorkPlanV1, WorkMilestoneV1, WorkItemV1) {
    let created_at = UtcMicros(10);
    let initiative_id = id::<InitiativeId>("initiative.work.evidence-journey");
    let plan_id = id::<WorkPlanId>("plan.work.evidence-journey");
    let milestone_id = id::<MilestoneId>("milestone.work.evidence-journey");
    let initiative = WorkInitiativeV1::new(
        initiative_id.clone(),
        "Work evidence journey".to_owned(),
        created_at,
    )
    .expect("initiative");
    let plan = WorkPlanV1::new(
        plan_id.clone(),
        initiative_id.clone(),
        "Work evidence journey plan".to_owned(),
        created_at,
    )
    .expect("plan");
    let milestone = WorkMilestoneV1::new(
        milestone_id.clone(),
        plan_id.clone(),
        "Work evidence journey milestone".to_owned(),
        created_at,
    )
    .expect("milestone");
    let item = WorkItemV1::new(WorkItemInputV1 {
        task_id,
        hierarchy: WorkHierarchyV1::new(initiative_id, plan_id, milestone_id),
        title: "Hydrate the owning provider session".to_owned(),
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
    .expect("Work item");
    (initiative, plan, milestone, item)
}

fn provider_route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.codex-cli"),
        id::<WorkProviderRouteId>("route.work.evidence-codex.v1"),
    )
    .expect("provider route")
}

fn execution_snapshot() -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>(
            "configuration-revision.work.evidence",
        ),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>(
            "configuration-snapshot.work.evidence",
        ),
        effective_behavior_digest: digest('1'),
        resolution_provenance_digest: digest('2'),
        route: provider_route(),
        backend: WorkProviderBackendV1::CodexCli,
        protocol: WorkProviderProtocol::CodexExecJson,
        model: "gpt-5.6".to_owned(),
        executable: WorkExecutableReference::new("executable.codex".to_owned(), digest('3'))
            .expect("executable"),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1)
            .expect("execution limits"),
        deadline: UtcMicros(1_000_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
    .expect("execution snapshot")
}

fn seal_attempt(
    storage: &impl WorkAttemptStoragePort,
    authority: &WorkAuthority,
    admitted: WorkAttemptV1,
    provider_session: ObservationSourceIdentityV1,
) {
    let observed_at = current_micros();
    let running = admitted
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(provider_route()),
            None,
            admitted.lease().clone(),
        )
        .expect("running attempt");
    storage
        .update(
            authority,
            admitted.lease(),
            WorkAttemptStateV1::Leased,
            &running,
            None,
        )
        .expect("persist running attempt");
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: running.identity().clone(),
        requested_route: running.requested_route().clone(),
        actual_route: Some(provider_route()),
        outcome: WorkAttemptProviderOutcomeV1::Exited { code: 0 },
        stdout: None,
        stderr: None,
        provider_session: Some(provider_session),
        provider_fallback: None,
        observed_at,
    };
    let terminal = WorkTerminalEvidenceV1::succeeded(
        evidence.digest().expect("attempt evidence digest"),
        observed_at,
    )
    .expect("terminal evidence");
    let closed = running
        .transition(
            WorkAttemptStateV1::Succeeded,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(provider_route()),
            Some(terminal),
            running.lease().clone(),
        )
        .expect("closed attempt");
    storage
        .update(
            authority,
            running.lease(),
            WorkAttemptStateV1::Running,
            &closed,
            Some(&evidence),
        )
        .expect("seal attempt evidence");
}

async fn invoke_work(
    service: &DaemonInvocationService,
    registry: &Arc<Mutex<LspSessionRegistry>>,
    project_root: &std::path::Path,
    request_id: &str,
    request: WorkApplicationInvocationV1,
) -> DaemonInvocationOutcome {
    let observed_at = current_micros();
    service
        .invoke(
            registry,
            Some(project_root),
            None,
            None,
            None,
            DaemonInvocationRequest::work_application(
                request_id,
                request,
                observed_at,
                Deadline::new(UtcMicros(observed_at.0.saturating_add(60_000_000)))
                    .expect("deadline"),
                CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
            ),
        )
        .await
        .outcome
}

async fn invoke_work_without_attempt_spawn(
    service: &DaemonInvocationService,
    project_root: &std::path::Path,
    request_id: &str,
    request: WorkApplicationInvocationV1,
) -> DaemonInvocationOutcome {
    let observed_at = current_micros();
    let canonical_root = project_root.canonicalize().expect("canonical project root");
    let runtimes = service
        .project_runtimes
        .request_runtimes(Some(project_root), Some(&canonical_root))
        .await;
    let registered = runtimes.work.expect("registered Work runtime");
    let observability = service.observability_producer(Some(project_root)).await;
    execute_work_application(
        registered,
        Arc::clone(&service.work_attempt_processes),
        observability,
        None,
        request_id.to_owned(),
        request,
        observed_at,
        Deadline::new(UtcMicros(observed_at.0.saturating_add(60_000_000))).expect("deadline"),
        CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
    )
    .await
    .outcome
}

#[tokio::test]
async fn registered_work_evidence_hydrates_the_provider_qualified_task_session() {
    let profile = tempfile::tempdir().expect("profile root");
    let project = profile.path().join("project");
    std::fs::create_dir_all(&project).expect("project root");
    let project_id = id::<ProjectId>("project.work.evidence-journey");
    let repository_id = id::<RepositoryId>("repository.work.evidence-journey");
    let worktree_id = id::<WorktreeId>("worktree.work.evidence-journey");
    let host = crate::host_admission::HostAdmissionTestRuntimeV1::project(
        profile.path(),
        &project,
        project_id.clone(),
    )
    .await
    .expect("registered project runtime");
    let database = host
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Project)
        .expect("registered project database");
    let session_id = id::<SessionId>("session.work.evidence-journey");
    let task_id = id::<TaskId>("task.work.evidence-journey");
    let attempt = WorkAttemptIdentityV1::new(
        task_id.clone(),
        id::<RunId>("run.work.evidence-journey"),
        id::<AttemptId>("attempt.work.evidence-journey"),
    )
    .expect("attempt identity");
    let query_text = format!(
        "{} {}:{} codex {}",
        task_id.as_str(),
        attempt.run_id().as_str(),
        attempt.attempt_id().as_str(),
        session_id.as_str(),
    );
    crate::dashboard::observation_seed::seed_session_message_observation_for_test(
        database.as_ref(),
        crate::dashboard::observation_seed::DashboardSessionMessageSeedV1 {
            project_id: project_id.as_str(),
            provider: "codex",
            session_id: session_id.as_str(),
            message_id: "message.work.evidence-journey",
            role: "assistant",
            content: &format!("{query_text} completed through the registered Work authority"),
            model: Some("gpt-5.6"),
            timestamp: 101,
            ordinal: 1,
        },
    )
    .await
    .expect("seed provider observation");
    crate::dashboard::observation_seed::materialize_session_temporal_refresh_for_test(
        database.as_ref(),
        session_id.as_str(),
    )
    .await
    .expect("materialize provider session");

    let retrieval_root =
        crate::daemon::session_retrieval::DaemonSessionRetrievalRoot::project_identity_for_test(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            project.display().to_string(),
        );
    let scope = retrieval_root
        .identity()
        .session_request_scope()
        .expect("Work scope");
    let retrieval = crate::daemon::session_retrieval::DaemonSessionRetrievalService::new(
        database.clone(),
        retrieval_root,
        None,
    )
    .expect("mounted session retrieval");
    let evidence_retrieval =
        crate::daemon::work_evidence_retrieval::DaemonWorkEvidenceRetrievalV1::new(Arc::new(
            retrieval,
        ))
        .with_federated_authority(Arc::new(
            crate::daemon::work_evidence_retrieval::tests::StaticFederatedAuthority(Arc::new(
                crate::daemon::work_evidence_retrieval::tests::federated_authority(id::<
                    PrivacyDomainId,
                >(
                    "privacy.work.evidence-journey",
                )),
            )),
        ));

    let actor = id::<ActorId>("actor.work.evidence-journey");
    let grant_digest = digest('d');
    let journey_now = current_micros();
    let grant = CapabilityGrantSnapshot::new(
        id::<CapabilityGrantId>("grant.work.evidence-journey"),
        1,
        grant_digest.clone(),
        actor.clone(),
        UtcMicros(journey_now.0.saturating_sub(60_000_000)),
        UtcMicros(journey_now.0.saturating_add(600_000_000)),
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
        project_id,
        repository_id,
        worktree_id,
        actor.clone(),
        grant_digest,
    )
    .expect("Work authority");
    let service = DaemonInvocationService::default();
    let (proposal_routing, configuration_digest) = empty_work_proposal_routing(scope.clone());
    let policy_digest = mount_test_work_observability(
        &service,
        &project,
        database.clone(),
        &scope,
        &configuration_digest,
    )
    .await;
    DaemonWorkRuntimeRegistrar::new(&service)
        .register(
            project.clone(),
            database.clone(),
            authority.clone(),
            actor,
            grant,
            policy_digest,
            configuration_digest,
            tracedecay_domain::configuration::safe_work_topology_policy_v1(),
            proposal_routing,
            evidence_retrieval,
        )
        .await
        .expect("registered Work runtime");
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
    let selection =
        WorkProductSelectionScopeV1::relations(BTreeSet::from([WorkRelationScopeV1::Repository {
            project_id: scope.project_id.clone(),
            repository_id: scope.repository_id.clone(),
        }]))
        .expect("repository Work selection");
    let (initiative, plan, milestone, item) = product_task(task_id.clone());
    let prepared = invoke_work(
        &service,
        &registry,
        &project,
        "request.work.evidence-create-prepare",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: selection.clone(),
            change: WorkProductChangeDraftV1::CreateTask {
                initiative,
                plan,
                milestone,
                item,
            },
            causation_event_id: None,
            evidence: Vec::new(),
        }),
    )
    .await;
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared
    else {
        panic!("create preparation must return evidence: {prepared:?}");
    };
    let created = invoke_work(
        &service,
        &registry,
        &project,
        "request.work.evidence-create",
        WorkApplicationInvocationV1::MutateGraph(packet.payload.expect("prepared create mutation")),
    )
    .await;
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::MutateGraph(ApplicationOutcome::Effect(effect)),
        ..
    } = created
    else {
        panic!("task creation must commit: {created:?}");
    };
    let created = effect.payload.expect("task creation receipt");

    let proposal_id = id::<ProposalId>("proposal.work.evidence-journey");
    let proposal = WorkProposalV1::new(
        proposal_id,
        task_id.clone(),
        created.verified_graph_version().graph_version(),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1).expect("proposal shape"),
        WorkSizingV1::new(
            WorkScoreKindV1::Ordinal,
            1,
            1,
            1,
            "registered Work evidence journey",
        )
        .expect("proposal sizing"),
        Vec::new(),
        WorkRouteDecisionV1::selected(
            provider_route(),
            Vec::new(),
            BTreeSet::new(),
            "no fallback in the exact provider journey".to_owned(),
        )
        .expect("proposal route"),
        "Hydrate the exact provider session.".to_owned(),
        digest('4'),
    )
    .expect("Work proposal");
    let prepared_accept = invoke_work(
        &service,
        &registry,
        &project,
        "request.work.evidence-proposal-prepare",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: selection.clone(),
            change: WorkProductChangeDraftV1::DecideProposal {
                proposal,
                disposition: WorkProposalDispositionV1::Accepted,
            },
            causation_event_id: None,
            evidence: Vec::new(),
        }),
    )
    .await;
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared_accept
    else {
        panic!("proposal acceptance preparation must return evidence: {prepared_accept:?}");
    };
    let accepted = invoke_work(
        &service,
        &registry,
        &project,
        "request.work.evidence-proposal-accept",
        WorkApplicationInvocationV1::MutateGraph(
            packet.payload.expect("prepared proposal acceptance"),
        ),
    )
    .await;
    assert!(matches!(
        accepted,
        DaemonInvocationOutcome::WorkApplication {
            outcome: WorkApplicationOutcomeV1::MutateGraph(ApplicationOutcome::Effect(_)),
            ..
        }
    ));

    let prepared_admission = invoke_work(
        &service,
        &registry,
        &project,
        "request.work.evidence-admission-prepare",
        WorkApplicationInvocationV1::PrepareGraphMutation(PrepareWorkProductMutationRequestV1 {
            selection: selection.clone(),
            change: WorkProductChangeDraftV1::AdmitExecution {
                task_id: task_id.clone(),
            },
            causation_event_id: None,
            evidence: Vec::new(),
        }),
    )
    .await;
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::PrepareGraphMutation(ApplicationOutcome::Evidence(packet)),
        ..
    } = prepared_admission
    else {
        panic!("execution admission preparation must return evidence: {prepared_admission:?}");
    };
    let WorkProductMutationRequestV1::AdmitExecution(admission) =
        packet.payload.expect("prepared execution admission")
    else {
        panic!("preparation must produce an execution admission");
    };
    let admitted = invoke_work(
        &service,
        &registry,
        &project,
        "request.work.evidence-admit",
        WorkApplicationInvocationV1::AdmitExecution(admission),
    )
    .await;
    assert!(matches!(
        admitted,
        DaemonInvocationOutcome::WorkApplication {
            outcome: WorkApplicationOutcomeV1::AdmitExecution(ApplicationOutcome::Effect(_)),
            ..
        }
    ));

    let started = invoke_work_without_attempt_spawn(
        &service,
        &project,
        "request.work.evidence-start",
        WorkApplicationInvocationV1::StartAttempt(StartWorkAttemptCommand {
            task_id: task_id.clone(),
            run_id: attempt.run_id().clone(),
            attempt_id: attempt.attempt_id().clone(),
            operation: id::<WorkflowOperationRef>("operation.work.evidence-provider"),
            execution_snapshot: execution_snapshot(),
            worktree_root: project.display().to_string(),
            reference: Some(id::<RefId>("refs/heads/work-evidence-journey")),
            commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
            instructions: "Hydrate the exact provider session.".to_owned(),
            effect_state: WorkEffectStateV1::Observational,
            occurred_at: current_micros(),
        }),
    )
    .await;
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::StartAttempt(ApplicationOutcome::Effect(effect)),
        ..
    } = started
    else {
        panic!("attempt start must commit: {started:?}");
    };
    let admitted_attempt = effect.payload.expect("admitted Work attempt");

    let provider_session =
        ObservationSourceIdentityV1::for_provider(id::<ProviderId>("codex"), session_id)
            .expect("provider session identity");
    let storage = database.work_storage().expect("Work storage");
    seal_attempt(&storage, &authority, admitted_attempt, provider_session);

    let graph = invoke_work(
        &service,
        &registry,
        &project,
        "request.work.evidence-graph",
        WorkApplicationInvocationV1::Views(WorkGraphReadRequestV1::current(
            selection.clone(),
            current_micros(),
        )),
    )
    .await;
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::Views(ApplicationOutcome::Evidence(packet)),
        ..
    } = graph
    else {
        panic!("Work graph read must return evidence: {graph:?}");
    };
    let graph = packet.payload.expect("Work graph payload");
    let verified_version = graph
        .entries()
        .last()
        .expect("current Work graph")
        .verified_version()
        .clone();

    let retrieved = invoke_work(
        &service,
        &registry,
        &project,
        "request.work.evidence-retrieve",
        WorkApplicationInvocationV1::RetrieveEvidence(WorkEvidenceRetrieveRequestV1 {
            selection,
            task_id,
            verified_version,
            temporal: TemporalModeV1::Forensic,
            page_size: 8,
            expansion: Some(WorkEvidenceExpansionSelectorV1::TaskSession {
                attempt: attempt.clone(),
            }),
            continuation: None,
            observed_at: current_micros(),
        }),
    )
    .await;
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::RetrieveEvidence(ApplicationOutcome::Evidence(packet)),
        ..
    } = retrieved
    else {
        panic!("Work evidence retrieval must return evidence: {retrieved:?}");
    };
    let evidence = packet.payload.expect("Work evidence payload");
    let task_session = evidence.sources.iter().find_map(|source| match source {
        WorkEvidenceSourceV1::TaskSession {
            attempt: source_attempt,
            evidence,
        } if source_attempt == &attempt => Some(evidence),
        _ => None,
    });
    let task_session = task_session
        .unwrap_or_else(|| panic!("provider TaskSession evidence must be present: {evidence:?}"));
    assert!(task_session.hydrated.iter().any(|hydrated| {
        hydrated.content.as_deref().is_some_and(|content| {
            content
                .windows(b"registered Work authority".len())
                .any(|window| window == b"registered Work authority")
        })
    }));
    assert!(evidence.omissions.is_empty(), "{evidence:?}");
}
