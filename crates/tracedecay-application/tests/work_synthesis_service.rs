//! Admitted synthesis over fan-out sibling evidence: source-set sealing,
//! citation completeness, preservation of failures/unknowns/disagreement,
//! and the unsynthesized-set answer when nothing is citable.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;

use common::{
    WorkProductAttemptStore, work_authority, work_product_binding, work_product_revisions,
};

use tracedecay_application::{
    AdmitWorkSynthesisCommand, ApplicationProblemKind, CancellationContext,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope,
    StartWorkAttemptCommand, WorkAttemptStoragePort, WorkProductAttemptServiceV1,
    WorkProductSynthesisAttemptServiceV1, WorkSynthesisAttemptV1, WorkSynthesisRefusalV1,
    WorkSynthesisSourceEnvelopeV1, WorkSynthesisSourceOutcomeV1, WorkSynthesisSourceSetV1,
    admit_work_synthesis_against_registered_topology,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest,
    ProjectId, ProposalId, ProviderId, RefId, RepositoryId, RunId, TaskId, UtcMicros,
    WorkApprovalPolicy, WorkArtifactId, WorkArtifactRefV1, WorkAttemptIdentityV1,
    WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority,
    WorkCancellationStateV1, WorkEffectStateV1, WorkEgressPolicy, WorkExecutableReference,
    WorkExecutionEnvelopeV1, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1, WorkFilesystemPolicy,
    WorkGraphVersionV1, WorkLeaseFenceV1, WorkLeaseId, WorkProductEventSequenceV1,
    WorkProductSourceWatermarkV1, WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId,
    WorkProviderRouteV1, WorkRecoveryStateV1, WorkSandboxPolicy, WorkTerminalEvidenceV1,
    WorkflowOperationRef, WorkflowOutputName, WorktreeId,
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

fn context(project: &str, actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(project),
        id::<RepositoryId>("repository.synthesis.fixture"),
        id::<WorktreeId>("worktree.synthesis.fixture"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.work.fixture").unwrap();
    let use_case = UseCaseId::new("use-case.work.fixture").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.fixture"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>(actor),
        scope,
        grant,
        RequestId::new(format!("request.{project}.{actor}")).unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active(format!("cancel.{project}.{actor}")).unwrap(),
    )
    .unwrap()
}

type Fixture = (
    WorkProductSynthesisAttemptServiceV1<WorkProductAttemptStore>,
    WorkProductAttemptServiceV1<WorkProductAttemptStore>,
    WorkProductAttemptStore,
    RequestContext,
);

fn fixture(project: &str) -> Fixture {
    let attempt_store = WorkProductAttemptStore::default();
    let synthesis = WorkProductSynthesisAttemptServiceV1::new(attempt_store.clone());
    let attempts = WorkProductAttemptServiceV1::new(attempt_store.clone());
    (
        synthesis,
        attempts,
        attempt_store,
        context(project, "actor.synthesis.owner"),
    )
}

fn admit_work(store: &WorkProductAttemptStore, context: &RequestContext, task: &str) {
    store.seed_task(context, id(task), true);
}

fn registered_topology() -> tracedecay_domain::WorkTopologyPolicyV1 {
    tracedecay_domain::safe_work_topology_policy_v1()
}

fn admit_synthesis(
    attempts: &WorkProductSynthesisAttemptServiceV1<WorkProductAttemptStore>,
    context: &RequestContext,
    command: AdmitWorkSynthesisCommand,
) -> Result<WorkSynthesisAttemptV1, tracedecay_application::ApplicationProblem> {
    admit_work_synthesis_against_registered_topology(
        attempts,
        context,
        &work_product_binding(),
        &work_product_revisions(context),
        &registered_topology(),
        command,
    )
}

fn admit_synthesis_with_topology(
    attempts: &WorkProductSynthesisAttemptServiceV1<WorkProductAttemptStore>,
    context: &RequestContext,
    topology: &tracedecay_domain::WorkTopologyPolicyV1,
    command: AdmitWorkSynthesisCommand,
) -> Result<WorkSynthesisAttemptV1, tracedecay_application::ApplicationProblem> {
    admit_work_synthesis_against_registered_topology(
        attempts,
        context,
        &work_product_binding(),
        &work_product_revisions(context),
        topology,
        command,
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
    execution_snapshot_with_topology(tracedecay_domain::safe_work_topology_policy_v1())
}

fn execution_snapshot_with_topology(
    topology: tracedecay_domain::WorkTopologyPolicyV1,
) -> WorkExecutionSnapshot {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.syn.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.syn.1"),
        effective_behavior_digest: digest('c'),
        resolution_provenance_digest: digest('d'),
        route: requested_route(),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "claude-test".to_owned(),
        executable: WorkExecutableReference::new(
            "executable.claude.code-cli".to_owned(),
            digest('e'),
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
        topology,
    })
    .unwrap()
}

fn start_command(task: &str, attempt: &str) -> StartWorkAttemptCommand {
    start_command_with_topology(
        task,
        attempt,
        tracedecay_domain::safe_work_topology_policy_v1(),
    )
}

fn start_command_with_topology(
    task: &str,
    attempt: &str,
    topology: tracedecay_domain::WorkTopologyPolicyV1,
) -> StartWorkAttemptCommand {
    StartWorkAttemptCommand {
        task_id: id(task),
        run_id: id(&format!("run.{task}")),
        attempt_id: id(attempt),
        operation: id::<WorkflowOperationRef>("operation.attempt.execute-provider"),
        execution_snapshot: execution_snapshot_with_topology(topology),
        worktree_root: "/tmp/synthesis-fixture".to_owned(),
        reference: Some(id::<RefId>("refs/heads/synthesis-fixture")),
        commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        instructions: "Synthesize the fan-out sibling evidence.".to_owned(),
        effect_state: WorkEffectStateV1::Observational,
        occurred_at: UtcMicros(40),
    }
}

fn source_identity(task: &str, attempt: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(
        id::<TaskId>(task),
        id::<RunId>(&format!("run.{task}")),
        id::<AttemptId>(attempt),
    )
    .unwrap()
}

fn leased_attempt(identity: WorkAttemptIdentityV1) -> WorkAttemptV1 {
    let binding = WorkAttemptProjectionBindingV1::new(
        WorkGraphVersionV1::new(3).unwrap(),
        WorkProductEventSequenceV1::new(7).unwrap(),
        WorkProductSourceWatermarkV1::new(BTreeMap::new()).unwrap(),
        digest('f'),
        id::<ProposalId>("proposal.synthesis.fixture"),
    )
    .unwrap();
    let envelope = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        binding.clone(),
        id::<WorkflowOperationRef>("operation.attempt.execute-provider"),
        execution_snapshot(),
        id::<ProjectId>("project.synthesis.sources"),
        id::<RepositoryId>("repository.synthesis.fixture"),
        id::<WorktreeId>("worktree.synthesis.fixture"),
        "/tmp/synthesis-fixture".to_owned(),
        Some(id::<RefId>("refs/heads/synthesis-fixture")),
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        "Execute the admitted provider step.".to_owned(),
        1,
        WorkEffectStateV1::Observational,
    )
    .unwrap();
    WorkAttemptV1::new(
        identity,
        binding,
        envelope,
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.synthesis.fixture"),
            WorkFenceEpochV1::new(1).unwrap(),
        )
        .unwrap(),
        WorkAttemptStateV1::Leased,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        requested_route(),
        None,
        None,
    )
    .unwrap()
}

/// Drives a fixture attempt to the requested terminal state and inserts it
/// directly into the attempt store, the way the registered store carries
/// settled rows.
fn insert_terminal_source(
    store: &WorkProductAttemptStore,
    authority: &WorkAuthority,
    identity: WorkAttemptIdentityV1,
    state: WorkAttemptStateV1,
    artifacts: Vec<WorkArtifactRefV1>,
    evidence_digest: ManifestDigest,
) -> WorkAttemptV1 {
    let leased = leased_attempt(identity);
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            None,
            leased.lease().clone(),
        )
        .unwrap();
    let terminal = match state {
        WorkAttemptStateV1::Succeeded => {
            WorkTerminalEvidenceV1::succeeded(evidence_digest, UtcMicros(500)).unwrap()
        }
        WorkAttemptStateV1::Failed => {
            WorkTerminalEvidenceV1::failed(evidence_digest, UtcMicros(500)).unwrap()
        }
        state => panic!("fixture only settles succeeded or failed sources, got {state:?}"),
    };
    let settled = running
        .transition(
            state,
            None,
            artifacts,
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            Some(terminal),
            running.lease().clone(),
        )
        .unwrap();
    store.insert(authority, &settled).unwrap();
    settled
}

fn insert_running_source(
    store: &WorkProductAttemptStore,
    authority: &WorkAuthority,
    identity: WorkAttemptIdentityV1,
) -> WorkAttemptV1 {
    let leased = leased_attempt(identity);
    let running = leased
        .transition(
            WorkAttemptStateV1::Running,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            None,
            leased.lease().clone(),
        )
        .unwrap();
    store.insert(authority, &running).unwrap();
    running
}

fn mark_source_succeeded(
    store: &WorkProductAttemptStore,
    authority: &WorkAuthority,
    running: &WorkAttemptV1,
    artifacts: Vec<WorkArtifactRefV1>,
) -> WorkAttemptV1 {
    let terminal = WorkTerminalEvidenceV1::succeeded(digest('0'), UtcMicros(600)).unwrap();
    let succeeded = running
        .transition(
            WorkAttemptStateV1::Succeeded,
            None,
            artifacts,
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            Some(requested_route()),
            Some(terminal),
            running.lease().clone(),
        )
        .unwrap();
    store
        .update(
            authority,
            running.lease(),
            WorkAttemptStateV1::Running,
            &succeeded,
            None,
        )
        .unwrap();
    succeeded
}

fn artifact(name: &str, byte: char) -> WorkArtifactRefV1 {
    WorkArtifactRefV1::new(id::<WorkArtifactId>(name), digest(byte), 128).unwrap()
}

fn attempt_count_for_run(
    store: &WorkProductAttemptStore,
    authority: &WorkAuthority,
    run_id: &RunId,
) -> usize {
    store
        .list(authority, None, 1_000)
        .unwrap()
        .attempts
        .iter()
        .filter(|attempt| attempt.identity().run_id() == run_id)
        .count()
}

fn synthesis_command(sources: Vec<WorkAttemptIdentityV1>) -> AdmitWorkSynthesisCommand {
    AdmitWorkSynthesisCommand {
        start: start_command("task.synthesis", "attempt.synthesis"),
        output_name: id::<WorkflowOutputName>("output.synthesis.fixture"),
        sources,
    }
}

fn synthesis_command_with_topology(
    sources: Vec<WorkAttemptIdentityV1>,
    topology: tracedecay_domain::WorkTopologyPolicyV1,
) -> AdmitWorkSynthesisCommand {
    AdmitWorkSynthesisCommand {
        start: start_command_with_topology("task.synthesis", "attempt.synthesis", topology),
        output_name: id::<WorkflowOutputName>("output.synthesis.fixture"),
        sources,
    }
}

#[test]
fn synthesis_refuses_empty_duplicate_and_self_source_sets() {
    let (attempts, _, _, context) = fixture("project.synthesis.refusals");
    let empty = admit_synthesis(&attempts, &context, synthesis_command(Vec::new())).unwrap_err();
    assert_eq!(empty.kind(), ApplicationProblemKind::InvalidRequest);

    let source = source_identity("task.source.a", "attempt.1");
    let duplicated = admit_synthesis(
        &attempts,
        &context,
        synthesis_command(vec![source.clone(), source.clone()]),
    )
    .unwrap_err();
    assert_eq!(duplicated.kind(), ApplicationProblemKind::InvalidRequest);

    let own_identity = source_identity("task.synthesis", "attempt.synthesis");
    let self_citing =
        admit_synthesis(&attempts, &context, synthesis_command(vec![own_identity])).unwrap_err();
    assert_eq!(self_citing.kind(), ApplicationProblemKind::InvalidRequest);
}

#[test]
fn synthesis_refuses_an_unknown_source() {
    let (attempts, _, _, context) = fixture("project.synthesis.unknown-source");
    let missing = admit_synthesis(
        &attempts,
        &context,
        synthesis_command(vec![source_identity("task.source.ghost", "attempt.1")]),
    )
    .unwrap_err();
    assert_eq!(
        missing.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}

#[test]
fn synthesis_returns_the_unsynthesized_set_when_nothing_is_citable() {
    let (attempts, _, attempt_store, context) = fixture("project.synthesis.unsynthesized");
    let mine = work_authority(&context);
    let failed = source_identity("task.source.failed", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        failed.clone(),
        WorkAttemptStateV1::Failed,
        Vec::new(),
        digest('1'),
    );
    let running = source_identity("task.source.running", "attempt.1");
    insert_running_source(&attempt_store, &mine, running.clone());
    let bare = source_identity("task.source.bare", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        bare.clone(),
        WorkAttemptStateV1::Succeeded,
        Vec::new(),
        digest('2'),
    );

    let outcome = admit_synthesis(
        &attempts,
        &context,
        synthesis_command(vec![failed.clone(), running.clone(), bare.clone()]),
    )
    .unwrap();
    let WorkSynthesisAttemptV1::Unsynthesized { sources, refusal } = outcome else {
        panic!("expected the unsynthesized set, got an admission");
    };
    assert_eq!(refusal, WorkSynthesisRefusalV1::NoCitableSources);
    assert!(sources.verified());
    // Every source is preserved verbatim, in the requested order: the
    // failure with its sealed evidence digest, the unknown as an unknown,
    // and the artifact-less success with nothing fabricated for it.
    assert_eq!(
        sources.sources,
        vec![
            WorkSynthesisSourceEnvelopeV1 {
                source: failed,
                outcome: WorkSynthesisSourceOutcomeV1::Failed {
                    evidence: digest('1'),
                },
            },
            WorkSynthesisSourceEnvelopeV1 {
                source: running,
                outcome: WorkSynthesisSourceOutcomeV1::Unknown {
                    state: WorkAttemptStateV1::Running,
                },
            },
            WorkSynthesisSourceEnvelopeV1 {
                source: bare,
                outcome: WorkSynthesisSourceOutcomeV1::Succeeded {
                    artifacts: Vec::new(),
                },
            },
        ]
    );
    // No synthesis attempt was admitted.
    let unadmitted = attempts
        .status(
            &context,
            &source_identity("task.synthesis", "attempt.synthesis"),
        )
        .unwrap_err();
    assert_eq!(
        unadmitted.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}

#[test]
fn synthesis_admits_citing_every_citable_source_and_preserves_the_rest() {
    let (attempts, _, attempt_store, context) = fixture("project.synthesis.admission");
    let mine = work_authority(&context);
    admit_work(&attempt_store, &context, "task.synthesis");

    // Two sources agree on the same artifact pair, one dissents with a
    // different artifact, and one failed outright.
    let agree_a = source_identity("task.source.agree-a", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        agree_a.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![
            artifact("artifact.log", '3'),
            artifact("artifact.patch", '4'),
        ],
        digest('5'),
    );
    let agree_b = source_identity("task.source.agree-b", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        agree_b.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![
            artifact("artifact.log", '3'),
            artifact("artifact.patch", '4'),
        ],
        digest('6'),
    );
    let dissent = source_identity("task.source.dissent", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        dissent.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![artifact("artifact.patch", '7')],
        digest('8'),
    );
    let failed = source_identity("task.source.failed", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        failed.clone(),
        WorkAttemptStateV1::Failed,
        Vec::new(),
        digest('9'),
    );

    let outcome = admit_synthesis(
        &attempts,
        &context,
        synthesis_command(vec![
            agree_a.clone(),
            agree_b.clone(),
            dissent.clone(),
            failed.clone(),
        ]),
    )
    .unwrap();
    let WorkSynthesisAttemptV1::Admitted(admission) = outcome else {
        panic!("expected an admitted synthesis attempt");
    };
    // The synthesis attempt went through the standard admission and holds a
    // lease under the standard fence.
    assert_eq!(admission.attempt.state(), WorkAttemptStateV1::Leased);
    assert_eq!(
        admission.attempt.identity(),
        &source_identity("task.synthesis", "attempt.synthesis")
    );
    // The citation obligation is complete by construction: every citable
    // digest, including the minority evidence, is cited.
    assert_eq!(
        admission.draft.cited_source_digests,
        BTreeSet::from([digest('3'), digest('4'), digest('7')])
    );
    assert_eq!(
        admission.draft.synthesis_attempt,
        source_identity("task.synthesis", "attempt.synthesis")
    );
    // Disagreement is preserved as structure: the concurring pair first,
    // the dissenting minority second, nobody resolved by fiat.
    assert_eq!(admission.groups.len(), 2);
    assert_eq!(admission.groups[0].sources, vec![agree_a, agree_b]);
    assert_eq!(admission.groups[1].sources, vec![dissent]);
    // The failure is preserved uncited rather than dropped.
    assert_eq!(admission.uncited, vec![failed.clone()]);
    assert!(admission.source_set.verified());
    assert_eq!(admission.source_set.sources.len(), 4);
    assert_eq!(
        admission.source_set.sources[3].outcome,
        WorkSynthesisSourceOutcomeV1::Failed {
            evidence: digest('9'),
        }
    );
}

#[test]
fn identical_synthesis_replay_returns_the_byte_stable_admitted_result() {
    let (attempts, _, attempt_store, context) = fixture("project.synthesis.replay");
    let mine = work_authority(&context);
    admit_work(&attempt_store, &context, "task.synthesis");

    let citable = source_identity("task.source.citable", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        citable.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![artifact("artifact.initial", '1')],
        digest('2'),
    );
    let mutable = source_identity("task.source.mutable", "attempt.1");
    let running = insert_running_source(&attempt_store, &mine, mutable.clone());
    let mut topology = registered_topology();
    topology.concurrency.maximum_active_per_repository = NonZeroU16::new(2).unwrap();
    topology.concurrency.maximum_global_active = NonZeroU16::new(2).unwrap();
    topology.validate().unwrap();
    let command = synthesis_command_with_topology(vec![citable, mutable], topology.clone());

    let first =
        admit_synthesis_with_topology(&attempts, &context, &topology, command.clone()).unwrap();
    let WorkSynthesisAttemptV1::Admitted(first_admission) = &first else {
        panic!("expected an admitted synthesis attempt");
    };
    assert_eq!(
        attempt_store.graph_version(),
        Some(WorkGraphVersionV1::new(4).unwrap()),
        "atomic admission must advance the canonical graph when it links the attempt",
    );
    assert_eq!(
        first_admission.source_set.sources[1].outcome,
        WorkSynthesisSourceOutcomeV1::Unknown {
            state: WorkAttemptStateV1::Running,
        }
    );
    mark_source_succeeded(
        &attempt_store,
        &mine,
        &running,
        vec![artifact("artifact.late", '3')],
    );
    let replay = admit_synthesis_with_topology(&attempts, &context, &topology, command).unwrap();

    assert_eq!(
        serde_json::to_vec(&replay).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
    assert_eq!(
        attempt_count_for_run(&attempt_store, &mine, &id("run.task.synthesis")),
        1
    );
}

#[test]
fn changed_synthesis_request_conflicts_without_mutating_the_admitted_result() {
    let (attempts, _, attempt_store, context) = fixture("project.synthesis.conflict");
    let mine = work_authority(&context);
    admit_work(&attempt_store, &context, "task.synthesis");

    let source = source_identity("task.source.citable", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        source.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![artifact("artifact.initial", '4')],
        digest('5'),
    );
    let command = synthesis_command(vec![source]);
    let first = admit_synthesis(&attempts, &context, command.clone()).unwrap();

    let mut changed = command.clone();
    changed.output_name = id("output.synthesis.changed");
    let conflict = admit_synthesis(&attempts, &context, changed).unwrap_err();
    assert_eq!(conflict.kind(), ApplicationProblemKind::Conflict);

    let replay = admit_synthesis(&attempts, &context, command).unwrap();
    assert_eq!(
        serde_json::to_vec(&replay).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
    assert_eq!(
        attempt_count_for_run(&attempt_store, &mine, &id("run.task.synthesis")),
        1
    );
}

#[test]
fn ordinary_start_conflicts_with_an_existing_synthesis_identity() {
    let (attempts, ordinary_attempts, attempt_store, context) =
        fixture("project.synthesis.cross-mode");
    let mine = work_authority(&context);
    admit_work(&attempt_store, &context, "task.synthesis");

    let source = source_identity("task.source.citable", "attempt.1");
    insert_terminal_source(
        &attempt_store,
        &mine,
        source.clone(),
        WorkAttemptStateV1::Succeeded,
        vec![artifact("artifact.initial", '6')],
        digest('7'),
    );
    let command = synthesis_command(vec![source]);
    let first = admit_synthesis(&attempts, &context, command.clone()).unwrap();

    let conflict = ordinary_attempts
        .start_against_registered_topology(
            &context,
            &work_product_binding(),
            &work_product_revisions(&context),
            &registered_topology(),
            command.start.clone(),
        )
        .unwrap_err();
    assert_eq!(conflict.kind(), ApplicationProblemKind::Conflict);

    let replay = admit_synthesis(&attempts, &context, command).unwrap();
    assert_eq!(
        serde_json::to_vec(&replay).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
    assert_eq!(
        attempt_count_for_run(&attempt_store, &mine, &id("run.task.synthesis")),
        1
    );
}

#[test]
fn synthesis_source_sets_are_order_and_content_sealed() {
    let first = WorkSynthesisSourceEnvelopeV1 {
        source: source_identity("task.source.a", "attempt.1"),
        outcome: WorkSynthesisSourceOutcomeV1::Succeeded {
            artifacts: vec![digest('3')],
        },
    };
    let second = WorkSynthesisSourceEnvelopeV1 {
        source: source_identity("task.source.b", "attempt.1"),
        outcome: WorkSynthesisSourceOutcomeV1::Failed {
            evidence: digest('4'),
        },
    };
    let forward = WorkSynthesisSourceSetV1::seal(vec![first.clone(), second.clone()]).unwrap();
    let reversed = WorkSynthesisSourceSetV1::seal(vec![second, first]).unwrap();
    // Order is part of the identity of the set.
    assert_ne!(forward.set_digest, reversed.set_digest);
    assert!(forward.verified());
    // Any mutation after sealing is detectable.
    let mut tampered = forward;
    tampered.sources[0].outcome = WorkSynthesisSourceOutcomeV1::Succeeded {
        artifacts: vec![digest('5')],
    };
    assert!(!tampered.verified());
}
