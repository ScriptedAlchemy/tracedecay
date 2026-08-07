//! `work` module test coverage (split from the former monolithic
//! `invocation::tests` module).

use super::*;

use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, AttachRuntimeEvidenceCommand,
    CreateWorkCommand, ReviewProposalRequestV1, WorkProjectionDeltaRequestV1,
    WorkProjectionSnapshotRequestV1,
};

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
        .registered_database_arc(crate::application::host_admission::HostAdmissionScope::Project)
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

/// The Task-family activity producer behind the dashboard's `task_activity`
/// stream. A committed Work mutation must raise exactly one Task pulse against
/// the registered project, and a projection read must raise none — the
/// dispatcher's read arms never reach the effect path that publishes.
#[tokio::test]
async fn committed_work_mutations_publish_task_activity_and_reads_do_not() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project root");
    let project_id = ProjectId::new("project.work.task-activity").expect("project id");
    let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
        crate::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id.clone(),
    )
    .await
    .expect("registered project runtime");
    let database = host
        .registered_database_arc(crate::application::host_admission::HostAdmissionScope::Project)
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
    DaemonWorkRuntimeRegistrar::new(&service)
        .register(
            project.path().to_path_buf(),
            Arc::clone(&database),
            authority,
            actor,
            grant,
            ManifestDigest::new(format!("sha256:{}", "e".repeat(64))).expect("policy digest"),
            ManifestDigest::new(format!("sha256:{}", "f".repeat(64)))
                .expect("configuration digest"),
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

    // A read before any mutation: the projection snapshot must leave the
    // activity lane empty.
    let snapshot = invoke!(
        "request.work.activity-snapshot-before",
        WorkApplicationInvocationV1::Snapshot(WorkProjectionSnapshotRequestV1 { page_size: 100 })
    );
    assert!(
        matches!(
            snapshot,
            DaemonInvocationOutcome::WorkApplication {
                outcome: WorkApplicationOutcomeV1::Snapshot(ApplicationOutcome::Evidence(_)),
                ..
            }
        ),
        "snapshot must return Work evidence: {snapshot:?}"
    );
    assert!(
        crate::application::event_lane::replay_after(&database, project_id.as_str(), None)
            .await
            .expect("activity replay")
            .records
            .is_empty(),
        "a Work projection read must not publish task activity"
    );

    let created = invoke!(
        "request.work.activity-create",
        WorkApplicationInvocationV1::Create(CreateWorkCommand {
            task_id: tracedecay_domain::TaskId::new("task.work.task-activity").expect("task id"),
            title: "Publish task activity for a committed mutation".to_owned(),
            dependencies: std::collections::BTreeSet::new(),
            command_id: tracedecay_domain::WorkCommandId::new("command.work.task-activity")
                .expect("command id"),
            occurred_at: UtcMicros(10),
        })
    );
    assert!(
        matches!(
            created,
            DaemonInvocationOutcome::WorkApplication {
                outcome: WorkApplicationOutcomeV1::Create(ApplicationOutcome::Effect(_)),
                ..
            }
        ),
        "create must return a Work effect: {created:?}"
    );

    let replay = crate::application::event_lane::replay_after(&database, project_id.as_str(), None)
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
        crate::application::event_lane::ActivityFamilyV1::Task
    );
    assert_eq!(pulse.project_id.as_deref(), Some(project_id.as_str()));
    assert_eq!(pulse.units, 1);
    // Only attempt mutations carry a detail: the canonical `task` payload
    // admits attempt states and nothing else.
    assert_eq!(pulse.detail, None);

    // A read after the mutation must not add a second pulse.
    let snapshot_after = invoke!(
        "request.work.activity-snapshot-after",
        WorkApplicationInvocationV1::Snapshot(WorkProjectionSnapshotRequestV1 { page_size: 100 })
    );
    assert!(
        matches!(
            snapshot_after,
            DaemonInvocationOutcome::WorkApplication { .. }
        ),
        "snapshot must succeed: {snapshot_after:?}"
    );
    assert_eq!(
        crate::application::event_lane::replay_after(&database, project_id.as_str(), None)
            .await
            .expect("activity replay")
            .records
            .len(),
        1,
        "a Work projection read must not publish task activity"
    );
}
