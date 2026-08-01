//! `work` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

#[cfg(unix)]
fn workflow_provider_fixture_path(project: &std::path::Path) -> std::path::PathBuf {
    project.join("codex-workflow-fixture")
}

#[cfg(windows)]
fn workflow_provider_fixture_path(project: &std::path::Path) -> std::path::PathBuf {
    project.join("codex-workflow-fixture.cmd")
}

#[cfg(unix)]
fn write_workflow_provider_fixture(path: &std::path::Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, script).expect("workflow provider fixture");
    let mut permissions = std::fs::metadata(path)
        .expect("workflow provider metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("workflow provider mode");
}

#[cfg(windows)]
fn write_workflow_provider_fixture(path: &std::path::Path, script: &str) {
    let script_path = path.with_extension("py");
    std::fs::write(&script_path, script).expect("workflow provider script");
    let script_name = script_path
        .file_name()
        .expect("workflow provider script name")
        .to_string_lossy();
    std::fs::write(
        path,
        format!("@echo off\r\npython \"%~dp0{script_name}\" %*\r\n"),
    )
    .expect("workflow provider launcher");
}

#[tokio::test]
async fn registered_work_services_dispatch_the_core_lifecycle() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.work.core-invocation").expect("project id");
    let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
        crate::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered project runtime");
    let database = host
        .project_observation_database_arc_for_test()
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
    DaemonWorkRuntimeRegistrar::new(&service)
        .register(
            project.path().to_path_buf(),
            database,
            authority,
            actor,
            grant,
            ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).expect("policy digest"),
            ManifestDigest::new(format!("sha256:{}", "f".repeat(64)))
                .expect("configuration digest"),
            crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                codex_bin: "tracedecay-work-provider-not-used".to_owned(),
                model: None,
                timeout: Duration::from_secs(5),
            },
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
            DaemonInvocationRequest::work_application(
                "request.work.other-project",
                WorkApplicationInvocationV1::Snapshot(WorkProjectionSnapshotRequestV1 {
                    page_size: 100,
                }),
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

    let created = invoke!(
        "request.work.create",
        WorkApplicationInvocationV1::Create(CreateWorkCommand {
            task_id: task_id.clone(),
            title: "Exercise the production Work dispatcher".to_owned(),
            dependencies: std::collections::BTreeSet::new(),
            command_id: tracedecay_domain::WorkCommandId::new("command.work.create")
                .expect("command id"),
            occurred_at: UtcMicros(10),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::Create(ApplicationOutcome::Effect(created_effect)),
        ..
    } = created
    else {
        panic!("create must return a Work effect: {created:?}");
    };
    let created = created_effect.payload.expect("created projection");
    assert_eq!(created.version(), tracedecay_domain::WorkVersion::initial());

    let snapshot = invoke!(
        "request.work.snapshot",
        WorkApplicationInvocationV1::Snapshot(WorkProjectionSnapshotRequestV1 { page_size: 100 })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::Snapshot(ApplicationOutcome::Evidence(snapshot_packet)),
        ..
    } = snapshot
    else {
        panic!("snapshot must return Work evidence: {snapshot:?}");
    };
    let snapshot = snapshot_packet.payload.expect("snapshot payload");
    assert_eq!(snapshot.projections(), std::slice::from_ref(&created));
    let cursor = tracedecay_rusqlite_runtime::work::WorkSqliteStorage::resume_cursor(&snapshot)
        .expect("snapshot cursor");

    let review = ReviewProposalRequestV1 {
        review: tracedecay_application::ReviewProposalCommand {
            task_id: task_id.clone(),
            proposal_id: tracedecay_domain::ProposalId::new("proposal.work.review")
                .expect("proposal id"),
            proposal_digest: proposal_digest.clone(),
            expected_version: created.version(),
            command_id: tracedecay_domain::WorkCommandId::new("command.work.review")
                .expect("command id"),
            occurred_at: UtcMicros(20),
        },
        disposition: tracedecay_application::ReviewProposalDispositionV1::Rejected,
    };
    let reviewed = invoke!(
        "request.work.review",
        WorkApplicationInvocationV1::ReviewProposal(review)
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::ReviewProposal(ApplicationOutcome::Effect(reviewed_effect)),
        ..
    } = reviewed
    else {
        panic!("review must return a Work effect: {reviewed:?}");
    };
    let reviewed = reviewed_effect.payload.expect("reviewed projection");

    let delta = invoke!(
        "request.work.delta",
        WorkApplicationInvocationV1::Delta(WorkProjectionDeltaRequestV1 {
            cursor,
            page_size: 100,
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::Delta(ApplicationOutcome::Evidence(delta_packet)),
        ..
    } = delta
    else {
        panic!("delta must return Work evidence: {delta:?}");
    };
    let delta = delta_packet.payload.expect("delta payload");
    assert_eq!(delta.changed(), std::slice::from_ref(&reviewed));

    let accepted = invoke!(
        "request.work.accept-proposal",
        WorkApplicationInvocationV1::AcceptProposal(AcceptProposalCommand {
            review: tracedecay_application::ReviewProposalCommand {
                task_id: task_id.clone(),
                proposal_id: tracedecay_domain::ProposalId::new("proposal.work.accept")
                    .expect("proposal id"),
                proposal_digest,
                expected_version: reviewed.version(),
                command_id: tracedecay_domain::WorkCommandId::new("command.work.accept-proposal",)
                    .expect("command id"),
                occurred_at: UtcMicros(30),
            },
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::AcceptProposal(ApplicationOutcome::Effect(accepted_effect)),
        ..
    } = accepted
    else {
        panic!("proposal acceptance must return a Work effect: {accepted:?}");
    };
    let accepted = accepted_effect
        .payload
        .expect("accepted proposal projection");

    let admitted = invoke!(
        "request.work.admit",
        WorkApplicationInvocationV1::AdmitExecution(AdmitExecutionCommand {
            task_id: task_id.clone(),
            expected_version: accepted.version(),
            command_id: tracedecay_domain::WorkCommandId::new("command.work.admit")
                .expect("command id"),
            occurred_at: UtcMicros(40),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::AdmitExecution(ApplicationOutcome::Effect(admitted_effect)),
        ..
    } = admitted
    else {
        panic!("execution admission must return a Work effect: {admitted:?}");
    };
    let admitted = admitted_effect.payload.expect("admitted projection");

    let with_evidence = invoke!(
        "request.work.attach-evidence",
        WorkApplicationInvocationV1::AttachRuntimeEvidence(AttachRuntimeEvidenceCommand {
            task_id: task_id.clone(),
            evidence: tracedecay_domain::RuntimeEvidenceRef::new(
                tracedecay_domain::RunId::new("run.work.core-invocation").expect("run id"),
                ManifestDigest::new(format!("sha256:{}", "2".repeat(64))).expect("evidence digest"),
                true,
            )
            .expect("runtime evidence"),
            expected_version: admitted.version(),
            command_id: tracedecay_domain::WorkCommandId::new("command.work.attach-evidence",)
                .expect("command id"),
            occurred_at: UtcMicros(50),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome:
            WorkApplicationOutcomeV1::AttachRuntimeEvidence(ApplicationOutcome::Effect(evidence_effect)),
        ..
    } = with_evidence
    else {
        panic!("runtime evidence must return a Work effect: {with_evidence:?}");
    };
    let with_evidence = evidence_effect.payload.expect("evidence projection");

    let accepted_task = invoke!(
        "request.work.accept-task",
        WorkApplicationInvocationV1::AcceptTask(AcceptTaskCommand {
            task_id,
            expected_version: with_evidence.version(),
            command_id: tracedecay_domain::WorkCommandId::new("command.work.accept-task")
                .expect("command id"),
            occurred_at: UtcMicros(60),
        })
    );
    let DaemonInvocationOutcome::WorkApplication {
        outcome: WorkApplicationOutcomeV1::AcceptTask(ApplicationOutcome::Effect(task_effect)),
        ..
    } = accepted_task
    else {
        panic!("task acceptance must return a Work effect: {accepted_task:?}");
    };
    assert!(
        task_effect
            .payload
            .expect("accepted task projection")
            .is_task_accepted()
    );
}

#[tokio::test]
async fn registered_workflow_services_dispatch_durable_fan_out_and_handoff() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project root");
    let fixture = workflow_provider_fixture_path(project.path());
    let fixture_script = r#"#!/usr/bin/env python3
import json
import sys

for line in sys.stdin:
    message = json.loads(line)
    request_id = message.get("id")
    method = message.get("method")
    if request_id == 0:
        print(json.dumps({"jsonrpc": "2.0", "id": 0, "result": {}}), flush=True)
    elif request_id == 1:
        print(json.dumps({"jsonrpc": "2.0", "id": 1, "result": {"thread": {"id": "thread.workflow.fixture", "model": "gpt-workflow-fixture"}}}), flush=True)
    elif request_id == 2 and method == "turn/start":
        print(json.dumps({"method": "item/completed", "params": {"model": "gpt-workflow-fixture", "item": {"content": [{"type": "output_text", "text": "canonical workflow child completed"}]}}}), flush=True)
        print(json.dumps({"method": "turn/completed"}), flush=True)
"#;
    write_workflow_provider_fixture(&fixture, fixture_script);
    let now = current_micros();
    let project_id = ProjectId::new("project.workflow.invocation").expect("project id");
    let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
        crate::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered project runtime");
    let database = host
        .project_observation_database_arc_for_test()
        .expect("registered project database");
    let actor = ActorId::new("actor.workflow.invocation").expect("actor id");
    let scope = ResolvedScope::new(
        project_id.clone(),
        tracedecay_domain::RepositoryId::new("repository.workflow.invocation")
            .expect("repository id"),
        tracedecay_domain::WorktreeId::new("worktree.workflow.invocation").expect("worktree id"),
        None,
    )
    .expect("resolved scope");
    let grant_digest =
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("grant digest");
    let capabilities = tracedecay_application::WORKFLOW_APPLICATION_OPERATION_IDS_V1
        .iter()
        .chain(tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1.iter())
        .map(|(_, capability, _)| CapabilityId::new(*capability).expect("capability"))
        .collect();
    let use_cases = tracedecay_application::WORKFLOW_APPLICATION_OPERATION_IDS_V1
        .iter()
        .chain(tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1.iter())
        .map(|(_, _, use_case)| UseCaseId::new(*use_case).expect("use case"))
        .collect();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.workflow.invocation").expect("grant id"),
        1,
        grant_digest.clone(),
        actor.clone(),
        UtcMicros(now.0 - 1_000_000),
        UtcMicros(now.0 + 60_000_000),
        scope.clone(),
        capabilities,
        use_cases,
        DisclosureClass::Sensitive,
    )
    .expect("Workflow grant");
    let authority = WorkAuthority::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        actor.clone(),
        grant_digest,
    )
    .expect("Work authority");
    let registration_grant = grant.clone();
    let policy_digest =
        ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).expect("policy digest");
    let configuration_digest =
        ManifestDigest::new(format!("sha256:{}", "f".repeat(64))).expect("configuration digest");
    let service = DaemonInvocationService::default();
    DaemonWorkRuntimeRegistrar::new(&service)
        .register(
            project.path().to_path_buf(),
            Arc::clone(&database),
            authority.clone(),
            actor.clone(),
            grant,
            policy_digest.clone(),
            configuration_digest.clone(),
            crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                codex_bin: fixture.to_string_lossy().into_owned(),
                model: None,
                timeout: Duration::from_secs(5),
            },
        )
        .await
        .expect("registered Work and Workflow runtime");
    let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));

    macro_rules! invoke {
        ($request_id:literal, $request:expr) => {
            service
                .invoke(
                    &registry,
                    Some(project.path()),
                    None,
                    None,
                    DaemonInvocationRequest::workflow_application(
                        $request_id,
                        $request,
                        now,
                        Deadline::new(UtcMicros(now.0 + 30_000_000)).expect("deadline"),
                        CancellationContext::active(concat!("cancel.", $request_id))
                            .expect("cancellation"),
                    ),
                )
                .await
                .outcome
        };
    }

    let digest = |byte: char| {
        ManifestDigest::new(format!(
            "sha256:{}",
            format!("{:02x}", u32::from(byte) & 0xff).repeat(32)
        ))
        .expect("digest")
    };
    let definition = tracedecay_domain::WorkflowDefinitionV1::new(
        tracedecay_domain::WorkflowDefinitionId::new("workflow.definition.invocation")
            .expect("definition id"),
        1,
        project_id,
        vec![tracedecay_domain::WorkflowStepV1 {
            step_id: tracedecay_domain::WorkflowStepId::new("fan-out").expect("step id"),
            operation: tracedecay_domain::WorkflowOperationRef::new(
                tracedecay_application::WORKFLOW_CANONICAL_WORK_OPERATION_V1,
            )
            .expect("operation"),
            predecessors: BTreeSet::default(),
            inputs: Vec::new(),
            outputs: vec![
                tracedecay_domain::WorkflowOutputName::new("created-work").expect("output name"),
            ],
            fan_out: Some(tracedecay_domain::WorkflowFanOutV1 { max_width: 2 }),
        }],
        digest('d'),
        digest('f'),
        digest('c'),
    )
    .expect("definition");

    let registered = invoke!(
        "request.workflow.register",
        WorkflowApplicationInvocationV1::RegisterDefinition(
            tracedecay_application::WorkflowDefinitionRegisterRequestV1 {
                definition: definition.clone(),
            },
        )
    );
    assert!(matches!(
        registered,
        DaemonInvocationOutcome::WorkflowApplication {
            outcome: WorkflowApplicationOutcomeV1::RegisterDefinition(ApplicationOutcome::Effect(
                _
            )),
            ..
        }
    ));
    let activated = invoke!(
        "request.workflow.activate",
        WorkflowApplicationInvocationV1::ActivateDefinition(
            tracedecay_application::WorkflowDefinitionActivateRequestV1 {
                definition_id: definition.definition_id().clone(),
                expected_active_version: None,
                replacement_version: 1,
            },
        )
    );
    assert!(matches!(
        activated,
        DaemonInvocationOutcome::WorkflowApplication {
            outcome: WorkflowApplicationOutcomeV1::ActivateDefinition(ApplicationOutcome::Effect(
                _
            )),
            ..
        }
    ));

    let fan_out = tracedecay_application::WorkflowFanOutRequestV1 {
        definition,
        run_id: tracedecay_domain::RunId::new("run.workflow.invocation").expect("run id"),
        step_id: tracedecay_domain::WorkflowStepId::new("fan-out").expect("step id"),
        fence: tracedecay_application::WorkflowExecutionFenceV1 {
            attempt_id: tracedecay_domain::AttemptId::new("attempt.workflow.invocation")
                .expect("attempt id"),
            lease: tracedecay_domain::WorkLeaseFenceV1::new(
                tracedecay_domain::WorkLeaseId::new("lease.workflow.invocation").expect("lease id"),
                tracedecay_domain::WorkFenceEpochV1::new(1).expect("fence epoch"),
            )
            .expect("lease fence"),
        },
        admitted_at: now,
        cancellation: CancellationContext::active("cancel.workflow.fan-out").expect("cancellation"),
        max_parallel: 1,
        failure_policy: tracedecay_application::WorkflowFailurePolicyV1::Collect,
        provider: tracedecay_application::WorkflowProviderAdmissionV1 {
            route: tracedecay_domain::WorkProviderRouteV1::new(
                tracedecay_domain::ProviderId::new(crate::daemon::work_runtime::CODEX_PROVIDER_ID)
                    .expect("provider id"),
                tracedecay_domain::WorkProviderRouteId::new("route.work.codex-app-server.v1")
                    .expect("route id"),
            )
            .expect("provider route"),
            backend: tracedecay_domain::WorkProviderBackendV1::CodexAppServer,
            model: "gpt-workflow-fixture".to_owned(),
            configuration_digest: digest('f'),
            reference: None,
            commit: tracedecay_domain::CommitId::new("0123456789abcdef0123456789abcdef01234567")
                .expect("commit"),
            deadline: UtcMicros(now.0 + 20_000_000),
            cancellation_generation: 1,
            budget: tracedecay_domain::WorkExecutionBudgetV1::new(16_384, 16_384, 65_536)
                .expect("execution budget"),
            effect_state: tracedecay_domain::WorkEffectStateV1::Observational,
        },
        inputs: vec![tracedecay_application::WorkflowFanOutInputV1 {
            identity: "alpha".to_owned(),
            input_digest: digest('1'),
        }],
    };
    let first = invoke!(
        "request.workflow.execute",
        WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(fan_out.clone()))
    );
    let DaemonInvocationOutcome::WorkflowApplication {
        outcome: WorkflowApplicationOutcomeV1::ExecuteFanOut(ApplicationOutcome::Effect(first)),
        ..
    } = first
    else {
        panic!("fan-out must return a Workflow effect: {first:?}");
    };
    let first = first.payload.expect("fan-out truth");
    let tracedecay_application::WorkflowExecutionTruthV1::Completed {
        checkpoint: ref first_checkpoint,
    } = first
    else {
        panic!("canonical workflow child must complete: {first:?}");
    };
    assert_eq!(first_checkpoint.children.len(), 1);
    let child_attempt = &first_checkpoint.children[0].attempt_identity;
    let stored_attempt = database
        .work_storage()
        .expect("Work storage")
        .execution_attempt(&authority, child_attempt)
        .expect("stored Work attempt")
        .expect("canonical child attempt");
    assert_eq!(
        stored_attempt.state(),
        tracedecay_domain::WorkAttemptStateV1::Succeeded
    );
    assert_eq!(stored_attempt.execution().model(), "gpt-workflow-fixture");
    let mut settled_fan_out = fan_out.clone();
    let mut interrupted_fan_out = fan_out.clone();
    let mut retried_fan_out = fan_out;
    let stopped = service
        .project_runtimes
        .withdraw::<RegisteredWorkRuntime>(project.path())
        .await
        .expect("registered Workflow runtime before restart");
    drop(stopped);
    DaemonWorkRuntimeRegistrar::new(&service)
        .register(
            project.path().to_path_buf(),
            Arc::clone(&database),
            authority.clone(),
            actor.clone(),
            registration_grant,
            policy_digest,
            configuration_digest,
            crate::sessions::codex_app_server::CodexAppServerSummaryConfig {
                codex_bin: fixture.to_string_lossy().into_owned(),
                model: None,
                timeout: Duration::from_secs(5),
            },
        )
        .await
        .expect("restarted Work and Workflow runtime");
    retried_fan_out.fence.attempt_id =
        tracedecay_domain::AttemptId::new("attempt.workflow.invocation.retry")
            .expect("retry attempt id");
    retried_fan_out.fence.lease = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.workflow.invocation").expect("retry lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(2).expect("retry fence epoch"),
    )
    .expect("retry lease fence");
    let replay = invoke!(
        "request.workflow.execute-replay",
        WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(retried_fan_out))
    );
    let DaemonInvocationOutcome::WorkflowApplication {
        outcome: WorkflowApplicationOutcomeV1::ExecuteFanOut(ApplicationOutcome::Effect(replay)),
        ..
    } = replay
    else {
        panic!("fan-out replay must return a Workflow effect: {replay:?}");
    };
    assert_eq!(replay.payload.as_ref(), Some(&first));

    settled_fan_out.run_id =
        tracedecay_domain::RunId::new("run.workflow.invocation.settled-before-checkpoint")
            .expect("settled run id");
    settled_fan_out.fence.attempt_id =
        tracedecay_domain::AttemptId::new("attempt.workflow.invocation.settled")
            .expect("settled attempt id");
    settled_fan_out.fence.lease = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.workflow.invocation.settled")
            .expect("settled lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(1).expect("settled fence epoch"),
    )
    .expect("settled lease fence");
    settled_fan_out.inputs[0].identity = "settled-before-checkpoint".to_owned();
    let settled_plan =
        tracedecay_application::prepare_workflow_fan_out(&settled_fan_out).expect("settled plan");
    crate::daemon::workflow_runtime::crash_after_next_workflow_settlement_for_test();
    let crashed_after_settlement = invoke!(
        "request.workflow.execute-crash-after-settlement",
        WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(settled_fan_out.clone()))
    );
    assert!(matches!(
        crashed_after_settlement,
        DaemonInvocationOutcome::ApplicationProblem { .. }
    ));
    let settled_attempt = database
        .work_storage()
        .expect("Work storage")
        .execution_attempt(&authority, &settled_plan.children[0].attempt_identity)
        .expect("settled attempt read")
        .expect("canonical settlement before Workflow checkpoint");
    assert_eq!(
        settled_attempt.state(),
        tracedecay_domain::WorkAttemptStateV1::Succeeded
    );
    settled_fan_out.fence.attempt_id =
        tracedecay_domain::AttemptId::new("attempt.workflow.invocation.settled.retry")
            .expect("settled retry attempt id");
    settled_fan_out.fence.lease = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.workflow.invocation.settled")
            .expect("settled retry lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(2).expect("settled retry fence epoch"),
    )
    .expect("settled retry lease fence");
    let reconciled = invoke!(
        "request.workflow.execute-reconcile-settlement",
        WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(settled_fan_out))
    );
    let DaemonInvocationOutcome::WorkflowApplication {
        outcome: WorkflowApplicationOutcomeV1::ExecuteFanOut(ApplicationOutcome::Effect(reconciled)),
        ..
    } = reconciled
    else {
        panic!("settled child reconciliation must complete: {reconciled:?}");
    };
    let reconciled = reconciled.payload.expect("reconciled workflow truth");
    let tracedecay_application::WorkflowExecutionTruthV1::Completed { checkpoint } = reconciled
    else {
        panic!("reconciled settlement must be completed: {reconciled:?}");
    };
    assert_eq!(
        checkpoint.children[0].attempt_identity,
        settled_plan.children[0].attempt_identity
    );

    interrupted_fan_out.run_id =
        tracedecay_domain::RunId::new("run.workflow.invocation.interrupted")
            .expect("interrupted run id");
    interrupted_fan_out.fence.attempt_id =
        tracedecay_domain::AttemptId::new("attempt.workflow.invocation.interrupted")
            .expect("interrupted attempt id");
    interrupted_fan_out.fence.lease = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.workflow.invocation.interrupted")
            .expect("interrupted lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(1).expect("interrupted fence epoch"),
    )
    .expect("interrupted lease fence");
    interrupted_fan_out.inputs[0].identity = "interrupted".to_owned();
    let interrupted_plan = tracedecay_application::prepare_workflow_fan_out(&interrupted_fan_out)
        .expect("interrupted plan");
    std::fs::remove_file(&fixture).expect("remove provider before durable intent retry");
    let interrupted = invoke!(
        "request.workflow.execute-interrupted",
        WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(interrupted_fan_out.clone()))
    );
    assert!(matches!(
        interrupted,
        DaemonInvocationOutcome::ApplicationProblem { .. }
    ));
    let interrupted_attempt = database
        .work_storage()
        .expect("Work storage")
        .execution_attempt(&authority, &interrupted_plan.children[0].attempt_identity)
        .expect("interrupted attempt read")
        .expect("durable child intent before provider dispatch");
    assert_eq!(
        interrupted_attempt.identity(),
        &interrupted_plan.children[0].attempt_identity
    );

    write_workflow_provider_fixture(&fixture, fixture_script);
    interrupted_fan_out.fence.attempt_id =
        tracedecay_domain::AttemptId::new("attempt.workflow.invocation.interrupted.retry")
            .expect("interrupted retry attempt id");
    interrupted_fan_out.fence.lease = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.workflow.invocation.interrupted")
            .expect("interrupted retry lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(2).expect("interrupted retry fence epoch"),
    )
    .expect("interrupted retry lease fence");
    let mut cancelled_fan_out = interrupted_fan_out.clone();
    let resumed = invoke!(
        "request.workflow.execute-interrupted-retry",
        WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(interrupted_fan_out))
    );
    let DaemonInvocationOutcome::WorkflowApplication {
        outcome: WorkflowApplicationOutcomeV1::ExecuteFanOut(ApplicationOutcome::Effect(resumed)),
        ..
    } = resumed
    else {
        panic!("durable child intent retry must complete: {resumed:?}");
    };
    let resumed = resumed.payload.expect("resumed workflow truth");
    let tracedecay_application::WorkflowExecutionTruthV1::Completed { checkpoint } = resumed else {
        panic!("resumed workflow child must complete: {resumed:?}");
    };
    assert_eq!(
        checkpoint.children[0].attempt_identity,
        interrupted_plan.children[0].attempt_identity
    );

    let mut failed_fan_out = cancelled_fan_out.clone();
    cancelled_fan_out.run_id = tracedecay_domain::RunId::new("run.workflow.invocation.cancelled")
        .expect("cancelled run id");
    cancelled_fan_out.fence.attempt_id =
        tracedecay_domain::AttemptId::new("attempt.workflow.invocation.cancelled")
            .expect("cancelled attempt id");
    cancelled_fan_out.fence.lease = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.workflow.invocation.cancelled")
            .expect("cancelled lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(1).expect("cancelled fence epoch"),
    )
    .expect("cancelled lease fence");
    cancelled_fan_out.cancellation =
        CancellationContext::cancelled("cancel.workflow.cancelled", now)
            .expect("cancelled workflow context");
    cancelled_fan_out.inputs[0].identity = "cancelled".to_owned();
    let cancelled_plan = tracedecay_application::prepare_workflow_fan_out(&cancelled_fan_out)
        .expect("cancelled plan");
    let cancelled = invoke!(
        "request.workflow.execute-cancelled",
        WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(cancelled_fan_out.clone()))
    );
    let DaemonInvocationOutcome::WorkflowApplication {
        outcome: WorkflowApplicationOutcomeV1::ExecuteFanOut(ApplicationOutcome::Effect(cancelled)),
        ..
    } = cancelled
    else {
        panic!("pre-cancelled workflow must return canonical truth: {cancelled:?}");
    };
    let cancelled_truth = cancelled.payload.expect("pre-cancelled workflow truth");
    assert!(matches!(
        &cancelled_truth,
        tracedecay_application::WorkflowExecutionTruthV1::Cancelled { .. }
    ));
    assert!(
        database
            .work_storage()
            .expect("Work storage")
            .execution_attempt(&authority, &cancelled_plan.children[0].attempt_identity)
            .expect("cancelled attempt read")
            .is_none(),
        "cancellation before durable child intent must not create Work"
    );
    cancelled_fan_out.fence.attempt_id =
        tracedecay_domain::AttemptId::new("attempt.workflow.invocation.cancelled.retry")
            .expect("cancelled retry attempt id");
    cancelled_fan_out.fence.lease = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.workflow.invocation.cancelled")
            .expect("cancelled retry lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(2).expect("cancelled retry fence epoch"),
    )
    .expect("cancelled retry lease fence");
    let cancelled_replay = invoke!(
        "request.workflow.execute-cancelled-replay",
        WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(cancelled_fan_out))
    );
    let DaemonInvocationOutcome::WorkflowApplication {
        outcome:
            WorkflowApplicationOutcomeV1::ExecuteFanOut(ApplicationOutcome::Effect(cancelled_replay)),
        ..
    } = cancelled_replay
    else {
        panic!("pre-cancelled workflow replay must return canonical truth: {cancelled_replay:?}");
    };
    assert_eq!(
        cancelled_replay.payload.as_ref(),
        Some(&cancelled_truth),
        "terminal replay must preserve empty-checkpoint cancellation truth"
    );

    failed_fan_out.run_id =
        tracedecay_domain::RunId::new("run.workflow.invocation.failed").expect("failed run id");
    failed_fan_out.fence.attempt_id =
        tracedecay_domain::AttemptId::new("attempt.workflow.invocation.failed")
            .expect("failed attempt id");
    failed_fan_out.fence.lease = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.workflow.invocation.failed")
            .expect("failed lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(1).expect("failed fence epoch"),
    )
    .expect("failed lease fence");
    failed_fan_out.inputs[0].identity = "provider-failure".to_owned();
    failed_fan_out.failure_policy = tracedecay_application::WorkflowFailurePolicyV1::FailFast;
    failed_fan_out
        .inputs
        .push(tracedecay_application::WorkflowFanOutInputV1 {
            identity: "provider-failure-pending".to_owned(),
            input_digest: digest('2'),
        });
    let failed_plan =
        tracedecay_application::prepare_workflow_fan_out(&failed_fan_out).expect("failed plan");
    write_workflow_provider_fixture(
        &fixture,
        r#"#!/usr/bin/env python3
import json
import sys

for line in sys.stdin:
    message = json.loads(line)
    request_id = message.get("id")
    if request_id == 0:
        print(json.dumps({"jsonrpc": "2.0", "id": 0, "result": {}}), flush=True)
    elif request_id == 1:
        print(json.dumps({"jsonrpc": "2.0", "id": 1, "result": {"thread": {"id": "thread.workflow.failed", "model": "gpt-workflow-fixture"}}}), flush=True)
    elif request_id == 2:
        sys.exit(2)
"#,
    );
    let failed = invoke!(
        "request.workflow.execute-provider-failure",
        WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(failed_fan_out.clone()))
    );
    let DaemonInvocationOutcome::WorkflowApplication {
        outcome: WorkflowApplicationOutcomeV1::ExecuteFanOut(ApplicationOutcome::Effect(failed)),
        ..
    } = failed
    else {
        panic!("provider failure must return canonical workflow truth: {failed:?}");
    };
    let failed_truth = failed.payload.expect("provider failure workflow truth");
    assert!(matches!(
        &failed_truth,
        tracedecay_application::WorkflowExecutionTruthV1::Failed { .. }
    ));
    let failed_attempt = database
        .work_storage()
        .expect("Work storage")
        .execution_attempt(&authority, &failed_plan.children[0].attempt_identity)
        .expect("failed attempt read")
        .expect("failed canonical child attempt");
    assert_eq!(
        failed_attempt.state(),
        tracedecay_domain::WorkAttemptStateV1::Failed
    );
    assert!(
        database
            .work_storage()
            .expect("Work storage")
            .execution_attempt(&authority, &failed_plan.children[1].attempt_identity)
            .expect("pending fail-fast attempt read")
            .is_none(),
        "fail-fast must leave the pending child uncreated"
    );
    failed_fan_out.fence.attempt_id =
        tracedecay_domain::AttemptId::new("attempt.workflow.invocation.failed.retry")
            .expect("failed retry attempt id");
    failed_fan_out.fence.lease = tracedecay_domain::WorkLeaseFenceV1::new(
        tracedecay_domain::WorkLeaseId::new("lease.workflow.invocation.failed")
            .expect("failed retry lease id"),
        tracedecay_domain::WorkFenceEpochV1::new(2).expect("failed retry fence epoch"),
    )
    .expect("failed retry lease fence");
    let failed_replay = invoke!(
        "request.workflow.execute-provider-failure-replay",
        WorkflowApplicationInvocationV1::ExecuteFanOut(Box::new(failed_fan_out))
    );
    let DaemonInvocationOutcome::WorkflowApplication {
        outcome:
            WorkflowApplicationOutcomeV1::ExecuteFanOut(ApplicationOutcome::Effect(failed_replay)),
        ..
    } = failed_replay
    else {
        panic!("fail-fast workflow replay must return canonical truth: {failed_replay:?}");
    };
    assert_eq!(
        failed_replay.payload.as_ref(),
        Some(&failed_truth),
        "terminal replay must preserve partial-checkpoint failure truth"
    );

    let handoff_scope = tracedecay_application::TaskHandoffScopeV1::new(
        scope.project_id,
        scope.repository_id,
        scope.worktree_id,
        tracedecay_domain::WorkflowDefinitionId::new("workflow.definition.invocation")
            .expect("definition id"),
        1,
        tracedecay_domain::WorkflowStepId::new("fan-out").expect("step id"),
        tracedecay_domain::TaskId::new("task.workflow.handoff").expect("task id"),
        tracedecay_domain::ThreadId::new("thread.workflow.handoff").expect("thread id"),
        tracedecay_domain::RunId::new("run.workflow.invocation").expect("run id"),
        actor.clone(),
        actor.clone(),
    )
    .expect("handoff scope");
    let secret = "workflow-handoff-secret-0123456789abcdef".to_owned();
    let issued = invoke!(
        "request.workflow.handoff-issue",
        WorkflowApplicationInvocationV1::HandoffIssue(
            tracedecay_application::TaskHandoffIssueRequestV1 {
                issuer: actor.clone(),
                scope: handoff_scope.clone(),
                secret: secret.clone(),
                issued_at: UtcMicros(100),
                expires_at: UtcMicros(900),
            },
        )
    );
    assert!(matches!(
        issued,
        DaemonInvocationOutcome::WorkflowApplication {
            outcome: WorkflowApplicationOutcomeV1::HandoffIssue(ApplicationOutcome::Effect(_)),
            ..
        }
    ));
    let redeemed = invoke!(
        "request.workflow.handoff-redeem",
        WorkflowApplicationInvocationV1::HandoffRedeem(
            tracedecay_application::TaskHandoffRedeemRequestV1 {
                secret,
                expected_scope: handoff_scope.clone(),
                redeemer: actor,
                consumed_at: UtcMicros(101),
            },
        )
    );
    assert!(matches!(
        redeemed,
        DaemonInvocationOutcome::WorkflowApplication {
            outcome: WorkflowApplicationOutcomeV1::HandoffRedeem(
                ApplicationOutcome::Effect(ref effect)
            ),
            ..
        } if effect.payload.as_ref().is_some_and(|value| value.scope == handoff_scope)
    ));
}
