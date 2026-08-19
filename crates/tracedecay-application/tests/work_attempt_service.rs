//! Admitted-provider attempt authority contract: lease admission and denial,
//! idempotent starts, the cancellation ladder, restart fencing, staleness
//! refusal, and typed provider-availability terminal journeys.

mod common;

use std::collections::BTreeSet;
use std::ops::Deref;

use common::{fixture_abs_root, id, work_attempt_context, work_digest};

use tracedecay_application::{
    ApplicationProblem, ApplicationProblemKind, CancelWorkAttemptCommand,
    MAX_WORK_ATTEMPT_LIST_PAGE_SIZE, RequestContext, ResumeWorkAttemptsCommand,
    StartWorkAttemptCommand, WorkAttemptCapacityScopeV1, WorkAttemptCapacityVerdictV1,
    WorkAttemptEvidenceRecordV1, WorkAttemptListCoverageV1, WorkAttemptListCursorV1,
    WorkAttemptListRequestV1, WorkAttemptListV1, WorkAttemptProviderOutcomeV1, WorkAttemptService,
    WorkAttemptStatusRequestV1, WorkAttemptTopologyBindingV1, WorkAttemptTopologyStateV1,
    WorkProductAttemptServiceV1,
};
use tracedecay_domain::{
    CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ProviderId, RefId, TaskId,
    UtcMicros, WorkApprovalPolicy, WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1,
    WorkEffectStateV1, WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFilesystemPolicy,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1,
    WorkSandboxPolicy, WorkflowOperationRef,
};

type Store = common::WorkProductAttemptStore;

struct AttemptServices {
    lifecycle: WorkAttemptService<Store>,
    product: WorkProductAttemptServiceV1<Store>,
}

impl Deref for AttemptServices {
    type Target = WorkAttemptService<Store>;

    fn deref(&self) -> &Self::Target {
        &self.lifecycle
    }
}

impl AttemptServices {
    fn start(
        &self,
        context: &RequestContext,
        command: StartWorkAttemptCommand,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        self.start_against_registered_topology(
            context,
            &tracedecay_domain::safe_work_topology_policy_v1(),
            command,
        )
    }

    fn start_against_registered_topology(
        &self,
        context: &RequestContext,
        topology: &tracedecay_domain::configuration::WorkTopologyPolicyV1,
        command: StartWorkAttemptCommand,
    ) -> Result<WorkAttemptV1, ApplicationProblem> {
        self.product.start_against_registered_topology(
            context,
            &common::work_product_binding(),
            &common::work_product_revisions(context),
            topology,
            command,
        )
    }
}

type Fixture = (AttemptServices, Store, RequestContext);

fn fixture(project: &str) -> Fixture {
    let store = Store::default();
    let context = work_attempt_context(project, "actor.attempt.owner");
    (
        AttemptServices {
            lifecycle: WorkAttemptService::new(store.clone()),
            product: WorkProductAttemptServiceV1::new(store.clone()),
        },
        store,
        context,
    )
}

fn requested_route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.attempt.claude-code.v1"),
    )
    .unwrap()
}

fn execution_snapshot() -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.att.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.att.1"),
        effective_behavior_digest: work_digest('c'),
        resolution_provenance_digest: work_digest('d'),
        route: requested_route(),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "claude-test".to_owned(),
        executable: WorkExecutableReference::new(
            "executable.claude.code-cli".to_owned(),
            work_digest('e'),
        )
        .unwrap(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1).unwrap(),
        deadline: UtcMicros(1_000_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
    .unwrap()
}

fn admit_work(work: &Store, context: &RequestContext, task: &str) {
    work.seed_task(context, id::<TaskId>(task), true);
}

fn start_command(task: &str, attempt: &str) -> StartWorkAttemptCommand {
    StartWorkAttemptCommand {
        task_id: id(task),
        run_id: id(&format!("run.{task}")),
        attempt_id: id(attempt),
        operation: id::<WorkflowOperationRef>("operation.attempt.execute-provider"),
        execution_snapshot: execution_snapshot(),
        worktree_root: fixture_abs_root("/tmp/attempt-fixture"),
        reference: Some(id::<RefId>("refs/heads/attempt-fixture")),
        commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        instructions: "Execute the admitted provider step.".to_owned(),
        effect_state: WorkEffectStateV1::Observational,
        occurred_at: UtcMicros(40),
    }
}

#[test]
fn start_is_denied_without_admitted_execution() {
    let (attempts, work, context) = fixture("project.attempt.denial");
    // A missing task is indistinguishable from an unauthorized one.
    let missing = attempts
        .start(&context, start_command("task.attempt.missing", "attempt.1"))
        .unwrap_err();
    assert_eq!(
        missing.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
    // A product task without execution admission is a typed denial, not a
    // queue: the attempt never reaches the lease store.
    work.seed_task(&context, id("task.attempt.unadmitted"), false);
    let denied = attempts
        .start(
            &context,
            start_command("task.attempt.unadmitted", "attempt.1"),
        )
        .unwrap_err();
    assert_eq!(denied.kind(), ApplicationProblemKind::InvalidRequest);
}

#[test]
fn start_refuses_a_caller_topology_that_differs_from_registered_authority() {
    let (attempts, work, context) = fixture("project.attempt.registered-topology");
    let task = "task.attempt.registered-topology";
    admit_work(&work, &context, task);
    let mut registered = tracedecay_domain::safe_work_topology_policy_v1();
    registered.notifications = tracedecay_domain::TopologyNotificationLevelV1::Verbose;

    let refusal = attempts
        .start_against_registered_topology(&context, &registered, start_command(task, "attempt.1"))
        .expect_err("the caller cannot self-attest a topology that the runtime did not register");
    assert_eq!(refusal.kind(), ApplicationProblemKind::Conflict);
    assert_eq!(
        attempts
            .status(
                &context,
                &WorkAttemptStatusRequestV1 {
                    task_id: id(task),
                    run_id: id(&format!("run.{task}")),
                    attempt_id: id("attempt.1"),
                },
            )
            .expect_err("topology refusal must happen before the provider attempt is leased")
            .kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}

#[test]
fn registered_topology_saturates_parallel_attempt_admission() {
    let (attempts, work, context) = fixture("project.attempt.topology-capacity");
    let task = "task.attempt.topology-capacity";
    admit_work(&work, &context, task);
    let topology = tracedecay_domain::safe_work_topology_policy_v1();
    let first = attempts
        .start_against_registered_topology(&context, &topology, start_command(task, "attempt.1"))
        .unwrap();
    let capacity = attempts
        .admission_capacity_against_registered_topology(&context, &id::<TaskId>(task), &topology)
        .unwrap();
    assert_eq!(capacity.global_active(), 1);
    assert_eq!(capacity.repository_active(), 1);
    assert_eq!(capacity.task_active(), 1);
    assert_eq!(
        capacity.verdict(),
        WorkAttemptCapacityVerdictV1::Exhausted(BTreeSet::from([
            WorkAttemptCapacityScopeV1::Global,
            WorkAttemptCapacityScopeV1::Repository,
            WorkAttemptCapacityScopeV1::Task,
        ]))
    );
    let peer_task = id::<TaskId>("task.attempt.topology-peer");
    let task_ids = [id::<TaskId>(task), peer_task.clone()];
    let batch = attempts
        .admission_capacities_against_registered_topology(&context, &task_ids, &topology)
        .unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[&task_ids[0]].global_active(), 1);
    assert_eq!(batch[&task_ids[0]].repository_active(), 1);
    assert_eq!(batch[&task_ids[0]].task_active(), 1);
    assert_eq!(batch[&peer_task].global_active(), 1);
    assert_eq!(batch[&peer_task].repository_active(), 1);
    assert_eq!(batch[&peer_task].task_active(), 0);
    let invalid = attempts
        .admission_capacities_against_registered_topology(
            &context,
            &[peer_task.clone(), peer_task],
            &topology,
        )
        .unwrap_err();
    assert_eq!(invalid.kind(), ApplicationProblemKind::InvalidRequest);

    let saturated = attempts
        .start_against_registered_topology(&context, &topology, start_command(task, "attempt.2"))
        .expect_err("the registered one-attempt topology must fence a second child");
    assert_eq!(saturated.kind(), ApplicationProblemKind::Saturated);

    let replay = attempts
        .start_against_registered_topology(&context, &topology, start_command(task, "attempt.1"))
        .expect("an identical attempt must replay even while capacity is full");
    assert_eq!(replay, first);
}

#[test]
fn start_leases_once_and_replays_identical_admissions() {
    let (attempts, work, context) = fixture("project.attempt.start");
    admit_work(&work, &context, "task.attempt.start");
    let command = start_command("task.attempt.start", "attempt.1");
    let leased = attempts.start(&context, command.clone()).unwrap();
    assert_eq!(leased.state(), WorkAttemptStateV1::Leased);
    assert_eq!(leased.lease().epoch().get(), 1);
    assert_eq!(
        leased.execution().instructions(),
        "Execute the admitted provider step."
    );
    let replayed = attempts.start(&context, command).unwrap();
    assert_eq!(replayed, leased);
    let status = attempts
        .status(
            &context,
            &WorkAttemptStatusRequestV1 {
                task_id: id("task.attempt.start"),
                run_id: id("run.task.attempt.start"),
                attempt_id: id("attempt.1"),
            },
        )
        .unwrap();
    assert_eq!(status, leased);
}

/// Settling an attempt seals terminal runtime evidence without mutating the
/// product graph. A byte-identical replay must still return the durable
/// attempt and its original product binding.
#[test]
fn start_replays_an_identical_admission_after_the_projection_moves() {
    let (attempts, work, context) = fixture("project.attempt.replay-after-move");
    admit_work(&work, &context, "task.attempt.replay");
    let command = start_command("task.attempt.replay", "attempt.1");
    let leased = attempts.start(&context, command.clone()).unwrap();
    let admitted_binding = leased.projection_binding().clone();
    let graph_after_admission = work.graph_version();

    attempts
        .mark_running(&context, leased.identity(), requested_route())
        .unwrap();
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: leased.identity().clone(),
        requested_route: leased.requested_route().clone(),
        actual_route: Some(requested_route()),
        outcome: WorkAttemptProviderOutcomeV1::Exited { code: 1 },
        stdout: None,
        stderr: None,
        provider_session: None,
        provider_fallback: None,
        observed_at: UtcMicros(50),
    };
    let settled = attempts
        .settle(&context, leased.identity(), &evidence)
        .unwrap();
    assert_eq!(
        work.graph_version(),
        graph_after_admission,
        "terminal evidence must not fabricate a product-graph transition"
    );

    let replayed = attempts.start(&context, command).unwrap();
    assert_eq!(
        replayed, settled,
        "an identical admission must replay the durable attempt, not conflict"
    );
    assert_eq!(
        replayed.projection_binding(),
        &admitted_binding,
        "the replay must return the binding pinned at admission, never a re-pin"
    );
}

/// Excluding the server-derived binding generation and sequence from the
/// replay comparison must not weaken conflict detection: the same attempt
/// identity carrying different caller-supplied admission content is still
/// refused as a conflict — including after the projection has moved past the
/// admission snapshot, so the refusal below can only come from the divergent
/// content and never from binding drift.
#[test]
fn start_refuses_a_divergent_admission_after_the_projection_moves() {
    let (attempts, work, context) = fixture("project.attempt.divergent");
    admit_work(&work, &context, "task.attempt.divergent");
    let command = start_command("task.attempt.divergent", "attempt.1");
    let leased = attempts.start(&context, command.clone()).unwrap();

    attempts
        .mark_running(&context, leased.identity(), requested_route())
        .unwrap();
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: leased.identity().clone(),
        requested_route: leased.requested_route().clone(),
        actual_route: Some(requested_route()),
        outcome: WorkAttemptProviderOutcomeV1::Exited { code: 1 },
        stdout: None,
        stderr: None,
        provider_session: None,
        provider_fallback: None,
        observed_at: UtcMicros(50),
    };
    let settled = attempts
        .settle(&context, leased.identity(), &evidence)
        .unwrap();

    let mut divergent = command;
    divergent.instructions = "Execute a different provider step.".to_owned();
    let refused = attempts.start(&context, divergent).unwrap_err();
    assert_eq!(
        refused.kind(),
        ApplicationProblemKind::Conflict,
        "a divergent admission under a used identity is a conflict, never a refresh"
    );

    // The refusal left the durable attempt untouched.
    let status = attempts
        .status(
            &context,
            &WorkAttemptStatusRequestV1 {
                task_id: id("task.attempt.divergent"),
                run_id: id("run.task.attempt.divergent"),
                attempt_id: id("attempt.1"),
            },
        )
        .unwrap();
    assert_eq!(status, settled);
}

#[test]
fn cancellation_ladder_reaches_cancelled_and_attaches_evidence() {
    let (attempts, work, context) = fixture("project.attempt.cancel");
    admit_work(&work, &context, "task.attempt.cancel");
    let leased =
        work.persist_leased_attempt(&context, &start_command("task.attempt.cancel", "attempt.1"));
    let identity = leased.identity().clone();
    attempts
        .mark_running(&context, &identity, requested_route())
        .unwrap();
    let requested = attempts
        .request_cancellation(
            &context,
            CancelWorkAttemptCommand {
                task_id: identity.task_id().clone(),
                run_id: identity.run_id().clone(),
                attempt_id: identity.attempt_id().clone(),
                request_id: id("cancellation.attempt.1"),
                occurred_at: UtcMicros(60),
            },
        )
        .unwrap();
    assert_eq!(requested.state(), WorkAttemptStateV1::CancellationRequested);
    // A different concurrent cancellation request is a conflict, not a merge.
    let conflicting = attempts
        .request_cancellation(
            &context,
            CancelWorkAttemptCommand {
                task_id: identity.task_id().clone(),
                run_id: identity.run_id().clone(),
                attempt_id: identity.attempt_id().clone(),
                request_id: id("cancellation.attempt.other"),
                occurred_at: UtcMicros(61),
            },
        )
        .unwrap_err();
    assert_eq!(conflicting.kind(), ApplicationProblemKind::Conflict);

    let acknowledged = attempts
        .acknowledge_cancellation(&context, &identity, UtcMicros(70))
        .unwrap();
    assert_eq!(
        acknowledged.state(),
        WorkAttemptStateV1::CancellationAcknowledged
    );
    let escalated = attempts
        .escalate_cancellation(&context, &identity, UtcMicros(80))
        .unwrap();
    assert_eq!(escalated.state(), WorkAttemptStateV1::CancellationEscalated);
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: identity.clone(),
        requested_route: escalated.requested_route().clone(),
        actual_route: escalated.actual_route().cloned(),
        outcome: WorkAttemptProviderOutcomeV1::Cancelled,
        stdout: None,
        stderr: None,
        provider_session: None,
        provider_fallback: None,
        observed_at: UtcMicros(90),
    };
    let cancelled = attempts.settle(&context, &identity, &evidence).unwrap();
    assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
    assert!(cancelled.is_terminal());
}

#[test]
fn leased_attempt_can_be_cancelled_without_a_provider_route() {
    let (attempts, work, context) = fixture("project.attempt.cancel-before-start");
    admit_work(&work, &context, "task.attempt.cancel-before-start");
    let leased = work.persist_leased_attempt(
        &context,
        &start_command("task.attempt.cancel-before-start", "attempt.1"),
    );
    let requested = attempts
        .request_cancellation(
            &context,
            CancelWorkAttemptCommand {
                task_id: leased.identity().task_id().clone(),
                run_id: leased.identity().run_id().clone(),
                attempt_id: leased.identity().attempt_id().clone(),
                request_id: id("cancellation.attempt.before-start"),
                occurred_at: UtcMicros(50),
            },
        )
        .unwrap();
    assert_eq!(requested.state(), WorkAttemptStateV1::CancellationRequested);
    assert!(requested.actual_route().is_none());
    let acknowledged = attempts
        .acknowledge_cancellation(&context, leased.identity(), UtcMicros(60))
        .unwrap();
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: leased.identity().clone(),
        requested_route: leased.requested_route().clone(),
        actual_route: None,
        outcome: WorkAttemptProviderOutcomeV1::Cancelled,
        stdout: None,
        stderr: None,
        provider_session: None,
        provider_fallback: None,
        observed_at: UtcMicros(70),
    };
    let cancelled = attempts
        .settle(&context, acknowledged.identity(), &evidence)
        .unwrap();
    assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
    assert!(cancelled.actual_route().is_none());
}

#[test]
fn resume_fences_open_attempts_and_completes_lost_cancellations() {
    let (attempts, work, context) = fixture("project.attempt.resume");
    admit_work(&work, &context, "task.attempt.resume");
    let leased =
        work.persist_leased_attempt(&context, &start_command("task.attempt.resume", "attempt.1"));
    let running_identity = {
        let command = StartWorkAttemptCommand {
            attempt_id: id("attempt.2"),
            ..start_command("task.attempt.resume", "attempt.2")
        };
        let attempt = work.persist_leased_attempt(&context, &command);
        attempts
            .mark_running(&context, attempt.identity(), requested_route())
            .unwrap();
        attempt.identity().clone()
    };
    let cancelling_identity = {
        let command = StartWorkAttemptCommand {
            attempt_id: id("attempt.3"),
            ..start_command("task.attempt.resume", "attempt.3")
        };
        let attempt = work.persist_leased_attempt(&context, &command);
        attempts
            .mark_running(&context, attempt.identity(), requested_route())
            .unwrap();
        attempts
            .request_cancellation(
                &context,
                CancelWorkAttemptCommand {
                    task_id: attempt.identity().task_id().clone(),
                    run_id: attempt.identity().run_id().clone(),
                    attempt_id: attempt.identity().attempt_id().clone(),
                    request_id: id("cancellation.attempt.lost"),
                    occurred_at: UtcMicros(50),
                },
            )
            .unwrap();
        attempt.identity().clone()
    };

    let report = attempts
        .resume(
            &context,
            &ResumeWorkAttemptsCommand {
                occurred_at: UtcMicros(100),
            },
        )
        .unwrap();
    assert_eq!(report.recovery_required.len(), 2);
    assert_eq!(report.cancelled.len(), 1);
    for fenced in &report.recovery_required {
        assert_eq!(fenced.state(), WorkAttemptStateV1::RecoveryRequired);
        assert!(fenced.lease().epoch().get() > leased.lease().epoch().get());
    }
    assert!(
        report
            .recovery_required
            .iter()
            .any(|attempt| attempt.identity() == &running_identity)
    );
    let cancelled = &report.cancelled[0];
    assert_eq!(cancelled.identity(), &cancelling_identity);
    assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
    assert!(cancelled.is_terminal());

    // The old fence can no longer advance a fenced attempt: settling with
    // evidence prepared under the lost epoch is refused.
    let stale = attempts
        .settle(
            &context,
            &running_identity,
            &WorkAttemptEvidenceRecordV1 {
                identity: running_identity.clone(),
                requested_route: requested_route(),
                actual_route: Some(requested_route()),
                outcome: WorkAttemptProviderOutcomeV1::Exited { code: 0 },
                stdout: None,
                stderr: None,
                provider_session: None,
                provider_fallback: None,
                observed_at: UtcMicros(110),
            },
        )
        .unwrap_err();
    assert_eq!(stale.kind(), ApplicationProblemKind::InvalidRequest);

    // Recovery execution restarts the fenced attempt under the new fence.
    let restarted = attempts
        .mark_running(&context, &running_identity, requested_route())
        .unwrap();
    assert_eq!(restarted.state(), WorkAttemptStateV1::Running);
}

#[test]
fn provider_unavailability_is_a_typed_terminal_journey() {
    let (attempts, work, context) = fixture("project.attempt.unavailable");
    admit_work(&work, &context, "task.attempt.unavailable");
    let leased = work.persist_leased_attempt(
        &context,
        &start_command("task.attempt.unavailable", "attempt.1"),
    );
    let identity = leased.identity().clone();
    let fenced = attempts
        .mark_provider_unavailable(&context, &identity)
        .unwrap();
    assert_eq!(fenced.state(), WorkAttemptStateV1::RecoveryRequired);
    let evidence = WorkAttemptEvidenceRecordV1 {
        identity: identity.clone(),
        requested_route: fenced.requested_route().clone(),
        actual_route: None,
        outcome: WorkAttemptProviderOutcomeV1::ProviderUnavailable {
            state: tracedecay_application::WorkProviderAvailabilityV1::Absent,
        },
        stdout: None,
        stderr: None,
        provider_session: None,
        provider_fallback: None,
        observed_at: UtcMicros(120),
    };
    let failed = attempts
        .fail_recovery(&context, &identity, &evidence)
        .unwrap();
    assert_eq!(failed.state(), WorkAttemptStateV1::Failed);
    assert!(failed.is_terminal());
    // Failing recovery twice replays nothing: the terminal row refuses a
    // second transition.
    let repeated = attempts
        .fail_recovery(&context, &identity, &evidence)
        .unwrap_err();
    assert_eq!(repeated.kind(), ApplicationProblemKind::Conflict);
}

fn verified_topology(generation: &str, task_count: u32) -> WorkAttemptTopologyStateV1 {
    WorkAttemptTopologyStateV1::Verified(WorkAttemptTopologyBindingV1 {
        generation: generation.to_owned(),
        task_count,
    })
}

#[test]
fn list_page_bounds_are_refused_before_any_topology_read() {
    let (attempts, _, context) = fixture("project.attempt.list.bounds");
    for page_size in [0, MAX_WORK_ATTEMPT_LIST_PAGE_SIZE + 1] {
        let refused = attempts
            .list(
                &context,
                &WorkAttemptListRequestV1 {
                    page_size,
                    cursor: None,
                },
                |_| panic!("an out-of-bounds page size must not resolve the topology"),
            )
            .unwrap_err();
        assert_eq!(refused.kind(), ApplicationProblemKind::InvalidRequest);
    }
}

#[test]
fn list_pages_attempts_in_stable_order_and_resumes_from_the_cursor() {
    let (attempts, work, context) = fixture("project.attempt.list.pages");
    admit_work(&work, &context, "task.attempt.list");
    for attempt_id in ["attempt.1", "attempt.2", "attempt.3"] {
        let command = StartWorkAttemptCommand {
            attempt_id: id(attempt_id),
            ..start_command("task.attempt.list", attempt_id)
        };
        work.persist_leased_attempt(&context, &command);
    }

    let first = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 2,
                cursor: None,
            },
            |_| Ok(verified_topology("generation.work.list.1", 1)),
        )
        .unwrap();
    let WorkAttemptListV1::Listed {
        topology,
        attempts: page,
        coverage,
    } = first
    else {
        panic!("an authorized populated scope must list");
    };
    assert_eq!(topology.generation, "generation.work.list.1");
    assert_eq!(topology.task_count, 1);
    assert_eq!(page.len(), 2);
    assert!(page[0].identity() < page[1].identity());
    assert_eq!(page[0].identity().attempt_id().as_str(), "attempt.1");
    assert_eq!(page[1].identity().attempt_id().as_str(), "attempt.2");
    let WorkAttemptListCoverageV1::Capped {
        returned,
        remaining,
        resume,
    } = coverage
    else {
        panic!("a capped page must carry a resume cursor");
    };
    assert_eq!((returned, remaining), (2, 1));
    assert_eq!(resume.generation, "generation.work.list.1");
    assert_eq!(&resume.start_after, page[1].identity());

    let second = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 2,
                cursor: Some(resume),
            },
            |_| Ok(verified_topology("generation.work.list.1", 1)),
        )
        .unwrap();
    let WorkAttemptListV1::Listed {
        attempts: rest,
        coverage,
        ..
    } = second
    else {
        panic!("the resumed page must list");
    };
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].identity().attempt_id().as_str(), "attempt.3");
    assert_eq!(
        coverage,
        WorkAttemptListCoverageV1::Complete { returned: 1 }
    );
}

#[test]
fn list_of_an_authorized_scope_without_attempts_is_an_explicit_zero_complete_page() {
    let (attempts, work, context) = fixture("project.attempt.list.zero");
    admit_work(&work, &context, "task.attempt.list.zero");
    let listed = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 10,
                cursor: None,
            },
            |_| Ok(verified_topology("generation.work.list.zero", 1)),
        )
        .unwrap();
    let WorkAttemptListV1::Listed {
        attempts: page,
        coverage,
        ..
    } = listed
    else {
        panic!("an authorized empty scope must list, not conceal");
    };
    assert!(page.is_empty());
    assert_eq!(
        coverage,
        WorkAttemptListCoverageV1::Complete { returned: 0 }
    );
}

#[test]
fn list_without_any_work_is_a_typed_absent_state() {
    let (attempts, _, context) = fixture("project.attempt.list.absent");
    let listed = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 10,
                cursor: None,
            },
            |_| Ok(WorkAttemptTopologyStateV1::Absent),
        )
        .unwrap();
    assert_eq!(listed, WorkAttemptListV1::Absent);
}

#[test]
fn list_cursor_from_a_superseded_topology_generation_is_stale() {
    let (attempts, work, context) = fixture("project.attempt.list.stale");
    admit_work(&work, &context, "task.attempt.list.stale");
    work.persist_leased_attempt(
        &context,
        &start_command("task.attempt.list.stale", "attempt.1"),
    );
    let cursor = WorkAttemptListCursorV1 {
        generation: "generation.work.list.old".to_owned(),
        start_after: identity_of("task.attempt.list.stale", "attempt.1"),
    };
    // A newer verified generation refuses the old cursor.
    let stale = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 2,
                cursor: Some(cursor.clone()),
            },
            |_| Ok(verified_topology("generation.work.list.new", 1)),
        )
        .unwrap_err();
    assert_eq!(stale.kind(), ApplicationProblemKind::Stale);
    // A scope whose topology no longer exists refuses the cursor the same way.
    let gone = attempts
        .list(
            &context,
            &WorkAttemptListRequestV1 {
                page_size: 2,
                cursor: Some(cursor),
            },
            |_| Ok(WorkAttemptTopologyStateV1::Absent),
        )
        .unwrap_err();
    assert_eq!(gone.kind(), ApplicationProblemKind::Stale);
}

#[test]
fn list_conceals_foreign_scopes_behind_their_own_typed_states() {
    let (attempts, work, owner) = fixture("project.attempt.list.conceal");
    admit_work(&work, &owner, "task.attempt.list.conceal");
    work.persist_leased_attempt(
        &owner,
        &start_command("task.attempt.list.conceal", "attempt.1"),
    );

    // A foreign actor's authority resolves its own topology: absent, exactly
    // like a scope that never had Work.
    let foreign = work_attempt_context("project.attempt.list.conceal", "actor.attempt.foreign");
    let absent = attempts
        .list(
            &foreign,
            &WorkAttemptListRequestV1 {
                page_size: 10,
                cursor: None,
            },
            |_| Ok(WorkAttemptTopologyStateV1::Absent),
        )
        .unwrap();
    assert_eq!(absent, WorkAttemptListV1::Absent);

    // Even against a verified topology, the foreign authority scope holds no
    // rows: nothing owned by another actor ever leaks into the page.
    let empty = attempts
        .list(
            &foreign,
            &WorkAttemptListRequestV1 {
                page_size: 10,
                cursor: None,
            },
            |_| Ok(verified_topology("generation.work.list.conceal", 1)),
        )
        .unwrap();
    let WorkAttemptListV1::Listed {
        attempts: page,
        coverage,
        ..
    } = empty
    else {
        panic!("a foreign authorized scope lists its own (empty) attempt set");
    };
    assert!(page.is_empty());
    assert_eq!(
        coverage,
        WorkAttemptListCoverageV1::Complete { returned: 0 }
    );
}

fn identity_of(task: &str, attempt: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(id(task), id(&format!("run.{task}")), id(attempt)).unwrap()
}
