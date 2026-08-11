#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use tracedecay_application::{
    AuthorizedScopeSetAuthority, CancellationContext, CancellationSignal, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, NativeIntegrationEvidenceRevisionsV1,
    NativeIntegrationPreflightOutcomeV1, NativeIntegrationPreflightRequestV1,
    NativeIntegrationSelectionBindingV1, NativeIntegrationStackResolutionRequestV1, RequestContext,
    RequestId, ResolvedScope,
};
use tracedecay_domain::{
    BranchStackEdgeV1, BranchStackId, BranchStackNodeV1, BranchStackRevisionId,
    BranchStackRevisionV1, BranchStackSourceV1, CommitId, FrozenIndependentBranchSelectionV1,
    GitHeadStateV1, GitObjectFormatV1, GitOidV1, GitOperationStateV1, MechanicalIntegrationModeV1,
    NativeIntegrationDirectionV1, NativeIntegrationPreviewDispositionV1,
    NativeIntegrationPreviewId, NativeIntegrationPreviewV1, NativeIntegrationRepositorySnapshotV1,
    NativeIntegrationSelectionV1, ProjectId, RefId, RepositoryId, ScopeSetId, ScopeSetRevision,
    StackNodeId, UtcMicros, WorktreeId, WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{actor, digest};
use tracedecay_usecases::stack_coordinator::*;

fn oid(seed: char) -> GitOidV1 {
    GitOidV1::new(seed.to_string().repeat(40)).unwrap()
}

fn complete_preview() -> NativeIntegrationPreviewV1 {
    let project = ProjectId::new("project.stack.preview").unwrap();
    let repository = RepositoryId::new("repository.stack.preview").unwrap();
    let source_ref = RefId::new("refs/heads/source").unwrap();
    let destination_ref = RefId::new("refs/heads/destination").unwrap();
    let selection = FrozenIndependentBranchSelectionV1::new(
        project.clone(),
        repository.clone(),
        WorktreeInventorySnapshotId::new("inventory.stack.preview").unwrap(),
        WorktreeInventoryEpoch::new(1).unwrap(),
        None,
        None,
        source_ref.clone(),
        destination_ref.clone(),
        oid('1'),
        oid('2'),
        digest('3'),
        UtcMicros(5),
    )
    .unwrap();
    let repository_snapshot = NativeIntegrationRepositorySnapshotV1 {
        project_id: project,
        repository_id: repository,
        source_worktree_id: None,
        destination_worktree_id: None,
        source_ref,
        destination_ref,
        source_tip: oid('1'),
        destination_tip: oid('2'),
        source_tree: oid('4'),
        destination_tree: oid('5'),
        merge_base: oid('6'),
        dependency_commits: vec![oid('1')],
        destination_head: GitHeadStateV1::Detached { commit: oid('2') },
        refs_digest: digest('7'),
        index_digest: digest('8'),
        worktree_digest: digest('9'),
        attributes_digest: digest('a'),
        operation_state: GitOperationStateV1::None,
        clean: true,
        object_format: GitObjectFormatV1::Sha1,
        adapter_revision: "gix-stack-preview.v1".to_owned(),
        captured_at: UtcMicros(6),
        digest: digest('b'),
    }
    .seal()
    .unwrap();
    NativeIntegrationPreviewV1 {
        preview_id: NativeIntegrationPreviewId::new("preview.stack.complete").unwrap(),
        selection: NativeIntegrationSelectionV1::IndependentBranch(selection),
        repository_snapshot,
        grant_digest: digest('c'),
        policy_digest: digest('d'),
        graph_revision_digest: digest('e'),
        test_revision_digest: digest('f'),
        schema_revision_digest: digest('0'),
        migration_revision_digest: digest('1'),
        disposition: NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
            MechanicalIntegrationModeV1::TwoParentMerge,
        ),
        candidate_tree: Some(oid('4')),
        ordered_commits: vec![oid('1')],
        created_at: UtcMicros(10),
        expires_at: UtcMicros(1_000),
        preview_digest: digest('2'),
    }
    .seal()
    .unwrap()
}

struct PreflightOutcomes {
    outcomes: Mutex<VecDeque<Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1>>>,
    calls: Mutex<usize>,
}

impl OptionalStackPreflightPort for PreflightOutcomes {
    fn preflight(
        &self,
        _request: &NativeIntegrationPreflightRequestV1,
        _cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        *self.calls.lock().unwrap() += 1;
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(NativeIntegrationPreflightOutcomeV1::Unavailable))
    }
}

#[derive(Default)]
struct BlockingPreflight {
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}

impl BlockingPreflight {
    fn wait_for_active(&self, expected: usize) {
        let mut state = self.state.lock().unwrap();
        while state.0 < expected {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}

impl OptionalStackPreflightPort for BlockingPreflight {
    fn preflight(
        &self,
        _request: &NativeIntegrationPreflightRequestV1,
        _cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        let mut state = self.state.lock().unwrap();
        state.0 += 1;
        self.changed.notify_all();
        while !state.1 {
            state = self.changed.wait(state).unwrap();
        }
        Ok(NativeIntegrationPreflightOutcomeV1::Unavailable)
    }
}

fn request_context(exact_scope: ResolvedScope, suffix: &str) -> RequestContext {
    let capability = CapabilityId::new("capability.stack").unwrap();
    let use_case = UseCaseId::new("usecase.stack").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.stack.{suffix}")).unwrap(),
        1,
        digest('e'),
        actor(0),
        UtcMicros(1),
        UtcMicros(10_000),
        exact_scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        actor(0),
        exact_scope,
        grant,
        RequestId::new(format!("request.stack.{suffix}")).unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active(format!("cancel.stack.{suffix}")).unwrap(),
    )
    .unwrap()
}

fn preflight_request(index: usize) -> NativeIntegrationPreflightRequestV1 {
    let project = ProjectId::new(format!("project.preflight.{index}")).unwrap();
    let repository = RepositoryId::new(format!("repository.preflight.{index}")).unwrap();
    let source = ResolvedScope::new(
        project.clone(),
        repository.clone(),
        WorktreeId::new(format!("worktree.preflight.source.{index}")).unwrap(),
        Some(RefId::new(format!("refs/heads/source-{index}")).unwrap()),
    )
    .unwrap();
    let destination = ResolvedScope::new(
        project.clone(),
        repository.clone(),
        WorktreeId::new(format!("worktree.preflight.destination.{index}")).unwrap(),
        Some(RefId::new(format!("refs/heads/destination-{index}")).unwrap()),
    )
    .unwrap();
    let source_context = request_context(source.clone(), &format!("source.{index}"));
    let destination_context = request_context(destination.clone(), &format!("destination.{index}"));
    let capability = CapabilityId::new("capability.stack").unwrap();
    let use_case = UseCaseId::new("usecase.stack").unwrap();
    let scope_set = AuthorizedScopeSetAuthority::authorize(
        ScopeSetId::new(format!("scope-set.stack.{index}")).unwrap(),
        ScopeSetRevision::new(1).unwrap(),
        vec![source_context, destination_context.clone()],
        &capability,
        &use_case,
        UtcMicros(2),
    )
    .unwrap();
    let inventory_snapshot_id =
        WorktreeInventorySnapshotId::new(format!("inventory.stack.{index}")).unwrap();
    let inventory_epoch = WorktreeInventoryEpoch::new(1).unwrap();
    let source_node_id = StackNodeId::new(format!("node.stack.source.{index}")).unwrap();
    let destination_node_id = StackNodeId::new(format!("node.stack.destination.{index}")).unwrap();
    let declared_revision = BranchStackRevisionV1::new(
        BranchStackId::new(format!("stack.{index}")).unwrap(),
        BranchStackRevisionId::new(format!("revision.stack.{index}")).unwrap(),
        inventory_snapshot_id.clone(),
        inventory_epoch,
        BranchStackSourceV1::ExplicitDeclaration,
        vec![
            BranchStackNodeV1 {
                node_id: source_node_id.clone(),
                project_id: project.clone(),
                repository_id: repository.clone(),
                reference: source.reference.clone().unwrap(),
                tip: CommitId::new(format!("commit.source.{index}")).unwrap(),
                worktree_id: Some(source.worktree_id.clone()),
            },
            BranchStackNodeV1 {
                node_id: destination_node_id.clone(),
                project_id: project,
                repository_id: repository,
                reference: destination.reference.clone().unwrap(),
                tip: CommitId::new(format!("commit.destination.{index}")).unwrap(),
                worktree_id: Some(destination.worktree_id.clone()),
            },
        ],
        vec![BranchStackEdgeV1 {
            dependency: source_node_id.clone(),
            dependent: destination_node_id.clone(),
        }],
    )
    .unwrap();
    NativeIntegrationPreflightRequestV1 {
        context: destination_context,
        topology: NativeIntegrationStackResolutionRequestV1 {
            source,
            destination,
            authorized_scope_set: scope_set,
            inventory_snapshot_id,
            inventory_epoch,
            selection: NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
                stack_id: declared_revision.stack_id.clone(),
                revision_id: declared_revision.revision_id.clone(),
                revision_digest: declared_revision.digest.clone(),
                declared_revision,
                source_node_id,
                destination_node_id,
                direction: NativeIntegrationDirectionV1::PropagateDependencyToDependent,
            },
            grant_digest: digest('e'),
            policy_digest: digest('2'),
            observed_at: UtcMicros(2),
        },
        evidence: NativeIntegrationEvidenceRevisionsV1 {
            graph_revision_digest: digest('3'),
            test_revision_digest: digest('4'),
            schema_revision_digest: digest('5'),
            migration_revision_digest: digest('6'),
        },
        preview_id: NativeIntegrationPreviewId::new(format!("preview.stack.{index}")).unwrap(),
        preferred_mode: None,
        preview_expires_at: UtcMicros(8_000),
        observed_at: UtcMicros(2),
    }
}

#[test]
fn half_open_circuit_rejects_stale_and_denied_but_success_closes() {
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    let request = preflight_request(1);
    let policy = StackCircuitPolicyV1 {
        revision: 1,
        policy_digest: digest('0'),
        failure_threshold: 1,
        open_micros: 100,
    }
    .seal()
    .unwrap();
    coordinator
        .register_circuit_policy(&request.topology.destination, policy)
        .unwrap();
    let preflight = PreflightOutcomes {
        outcomes: Mutex::new(VecDeque::from([
            Ok(NativeIntegrationPreflightOutcomeV1::Unavailable),
            Ok(NativeIntegrationPreflightOutcomeV1::Stale),
            Ok(NativeIntegrationPreflightOutcomeV1::Denied),
            Ok(NativeIntegrationPreflightOutcomeV1::Preview(Box::new(
                complete_preview(),
            ))),
            Ok(NativeIntegrationPreflightOutcomeV1::Unavailable),
        ])),
        calls: Mutex::new(0),
    };
    let cancellation = CancellationSignal::active("cancel.stack.preflight").unwrap();
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(10))
            .unwrap(),
        OptionalPreflightDispositionV1::Unavailable
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(11))
            .unwrap(),
        OptionalPreflightDispositionV1::SuppressedOpenCircuit
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(110))
            .unwrap(),
        OptionalPreflightDispositionV1::Stale
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(111))
            .unwrap(),
        OptionalPreflightDispositionV1::SuppressedOpenCircuit
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(210))
            .unwrap(),
        OptionalPreflightDispositionV1::Denied
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(211))
            .unwrap(),
        OptionalPreflightDispositionV1::SuppressedOpenCircuit
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(310))
            .unwrap(),
        OptionalPreflightDispositionV1::Complete
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(311))
            .unwrap(),
        OptionalPreflightDispositionV1::Unavailable
    );
    assert_eq!(*preflight.calls.lock().unwrap(), 5);
}

fn register_policy(
    coordinator: &DaemonGitHubStackCoordinatorV1,
    request: &NativeIntegrationPreflightRequestV1,
) {
    coordinator
        .register_circuit_policy(
            &request.topology.destination,
            StackCircuitPolicyV1 {
                revision: 1,
                policy_digest: digest('0'),
                failure_threshold: 10,
                open_micros: 100,
            }
            .seal()
            .unwrap(),
        )
        .unwrap();
}

#[test]
fn preflight_admission_enforces_repository_and_daemon_bounds() {
    let coordinator = Arc::new(DaemonGitHubStackCoordinatorV1::default());
    let preflight = Arc::new(BlockingPreflight::default());
    let request = preflight_request(10);
    register_policy(&coordinator, &request);
    let mut handles = Vec::new();
    for index in 0..MAX_REPOSITORY_PREFLIGHTS {
        let mut admitted = request.clone();
        admitted.preview_id =
            NativeIntegrationPreviewId::new(format!("preview.stack.repo-bound.{index}")).unwrap();
        let coordinator = Arc::clone(&coordinator);
        let preflight = Arc::clone(&preflight);
        handles.push(std::thread::spawn(move || {
            coordinator.optional_preflight(
                preflight.as_ref(),
                &admitted,
                &CancellationSignal::active(format!("cancel.stack.repo-bound.{index}")).unwrap(),
                UtcMicros(10),
            )
        }));
    }
    preflight.wait_for_active(MAX_REPOSITORY_PREFLIGHTS);
    let mut saturated = request.clone();
    saturated.preview_id = NativeIntegrationPreviewId::new("preview.stack.repo-saturated").unwrap();
    assert_eq!(
        coordinator
            .optional_preflight(
                preflight.as_ref(),
                &saturated,
                &CancellationSignal::active("cancel.stack.repo-saturated").unwrap(),
                UtcMicros(10),
            )
            .unwrap(),
        OptionalPreflightDispositionV1::Saturated
    );
    preflight.release();
    for handle in handles {
        assert_eq!(
            handle.join().unwrap().unwrap(),
            OptionalPreflightDispositionV1::Unavailable
        );
    }

    let coordinator = Arc::new(DaemonGitHubStackCoordinatorV1::default());
    let preflight = Arc::new(BlockingPreflight::default());
    let mut handles = Vec::new();
    for index in 20..20 + MAX_DAEMON_PREFLIGHTS {
        let request = preflight_request(index);
        register_policy(&coordinator, &request);
        let coordinator = Arc::clone(&coordinator);
        let preflight = Arc::clone(&preflight);
        handles.push(std::thread::spawn(move || {
            coordinator.optional_preflight(
                preflight.as_ref(),
                &request,
                &CancellationSignal::active(format!("cancel.stack.daemon-bound.{index}")).unwrap(),
                UtcMicros(20),
            )
        }));
    }
    preflight.wait_for_active(MAX_DAEMON_PREFLIGHTS);
    let saturated = preflight_request(100);
    register_policy(&coordinator, &saturated);
    assert_eq!(
        coordinator
            .optional_preflight(
                preflight.as_ref(),
                &saturated,
                &CancellationSignal::active("cancel.stack.daemon-saturated").unwrap(),
                UtcMicros(20),
            )
            .unwrap(),
        OptionalPreflightDispositionV1::Saturated
    );
    preflight.release();
    for handle in handles {
        assert_eq!(
            handle.join().unwrap().unwrap(),
            OptionalPreflightDispositionV1::Unavailable
        );
    }
}

#[test]
fn joined_preflight_observes_cancellation_without_waiting_for_owner() {
    let coordinator = Arc::new(DaemonGitHubStackCoordinatorV1::default());
    let preflight = Arc::new(BlockingPreflight::default());
    let request = preflight_request(200);
    register_policy(&coordinator, &request);
    let owner_coordinator = Arc::clone(&coordinator);
    let owner_preflight = Arc::clone(&preflight);
    let owner_request = request.clone();
    let owner = std::thread::spawn(move || {
        owner_coordinator.optional_preflight(
            owner_preflight.as_ref(),
            &owner_request,
            &CancellationSignal::active("cancel.stack.join-owner").unwrap(),
            UtcMicros(30),
        )
    });
    preflight.wait_for_active(1);
    let cancelled = CancellationSignal::active("cancel.stack.joiner").unwrap();
    cancelled.cancel(UtcMicros(31));
    assert_eq!(
        coordinator.optional_preflight(preflight.as_ref(), &request, &cancelled, UtcMicros(30)),
        Err(StackCoordinatorErrorV1::Cancelled)
    );
    preflight.release();
    assert_eq!(
        owner.join().unwrap().unwrap(),
        OptionalPreflightDispositionV1::Unavailable
    );
}
