#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use tracedecay_application::stack_coordinator::*;
use tracedecay_contracts::{
    AuthorizedScopeSetAuthority, CancellationContext, CancellationSignal, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, NativeIntegrationPreflightOutcomeV1,
    NativeIntegrationPreflightRequestV1, NativeIntegrationSelectionBindingV1,
    NativeIntegrationStackResolutionRequestV1, RequestContext, RequestId, ResolvedScope,
};
use tracedecay_domain::{
    BranchStackEdgeV1, BranchStackId, BranchStackNodeV1, BranchStackRevisionId,
    BranchStackRevisionV1, BranchStackSourceV1, CodeGenerationId, CommitId, ContentDigest,
    FrozenIndependentBranchSelectionV1, GitHeadStateV1, GitObjectFormatV1, GitOidV1,
    GitOperationStateV1, MechanicalIntegrationModeV1, NativeIntegrationAnalysisCoverageV1,
    NativeIntegrationAnalysisLaneV1, NativeIntegrationAnalysisReportV1,
    NativeIntegrationDirectionV1, NativeIntegrationGenerationBindingV1,
    NativeIntegrationPreviewDispositionV1, NativeIntegrationPreviewId, NativeIntegrationPreviewV1,
    NativeIntegrationRepositorySnapshotV1, NativeIntegrationSelectionV1, ProjectId, RefId,
    RepositoryId, ScopeSetId, ScopeSetRevision, StackNodeId, UtcMicros, WorktreeId,
    WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{actor, digest};

/// Drives one preflight on this dedicated test thread; the blocking fixture
/// port parks the thread that polls it.
fn run<F: Future>(future: F) -> F::Output {
    tracedecay_runtime_core::native_runtime::block_on(future).expect("native block_on")
}

fn oid(seed: char) -> GitOidV1 {
    GitOidV1::new(seed.to_string().repeat(40)).unwrap()
}

fn complete_analysis(
    snapshot: &NativeIntegrationRepositorySnapshotV1,
) -> NativeIntegrationAnalysisReportV1 {
    let binding = |name: &str, revision: Option<GitOidV1>, tree: GitOidV1| {
        NativeIntegrationGenerationBindingV1 {
            generation_id: CodeGenerationId::new(format!("generation.stack.preview.{name}"))
                .unwrap(),
            project_id: snapshot.project_id.clone(),
            repository_id: snapshot.repository_id.clone(),
            worktree_id: None,
            reference: Some(if name == "source" || name == "merge-base" {
                snapshot.source_ref.clone()
            } else {
                snapshot.destination_ref.clone()
            }),
            snapshot_digest: digest('e'),
            content_identity: ContentDigest::new(digest('f').as_str().to_owned()).unwrap(),
            source_revision: revision,
            source_tree: tree,
            seal_digest: digest('0'),
        }
    };
    let complete = NativeIntegrationAnalysisLaneV1 {
        coverage: NativeIntegrationAnalysisCoverageV1::Complete,
        gaps: Vec::new(),
    };
    NativeIntegrationAnalysisReportV1 {
        merge_base: binding("merge-base", Some(snapshot.merge_base.clone()), oid('a')),
        source: binding(
            "source",
            Some(snapshot.source_tip.clone()),
            snapshot.source_tree.clone(),
        ),
        destination: binding(
            "destination",
            Some(snapshot.destination_tip.clone()),
            snapshot.destination_tree.clone(),
        ),
        candidate: binding("candidate", None, snapshot.source_tree.clone()),
        graph: complete.clone(),
        tests: complete.clone(),
        schema: complete.clone(),
        migrations: complete,
        conflicts: Vec::new(),
        analyzer_revision: "native-stack-preview-test.v1".to_owned(),
        digest: digest('1'),
    }
    .seal()
    .unwrap()
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
    let analysis = complete_analysis(&repository_snapshot);
    NativeIntegrationPreviewV1 {
        preview_id: NativeIntegrationPreviewId::new("preview.stack.complete").unwrap(),
        selection: NativeIntegrationSelectionV1::IndependentBranch(selection),
        repository_snapshot,
        grant_digest: digest('c'),
        policy_digest: digest('d'),
        analysis: Some(analysis),
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
    async fn preflight(
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

struct BlockingPreflight {
    state: Mutex<(usize, bool)>,
    changed: Condvar,
    outcome: Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1>,
}

impl Default for BlockingPreflight {
    fn default() -> Self {
        Self::returning(Ok(NativeIntegrationPreflightOutcomeV1::Unavailable))
    }
}

impl BlockingPreflight {
    fn returning(
        outcome: Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1>,
    ) -> Self {
        Self {
            state: Mutex::new((0, false)),
            changed: Condvar::new(),
            outcome,
        }
    }

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
    async fn preflight(
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
        self.outcome.clone()
    }
}

struct NeverFinishes;

impl OptionalStackPreflightPort for NeverFinishes {
    async fn preflight(
        &self,
        _request: &NativeIntegrationPreflightRequestV1,
        _cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        std::future::pending().await
    }
}

struct EventPreflight<'a> {
    release: &'a tokio::sync::Notify,
    calls: Mutex<usize>,
    outcome: Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1>,
}

impl OptionalStackPreflightPort for EventPreflight<'_> {
    async fn preflight(
        &self,
        _request: &NativeIntegrationPreflightRequestV1,
        _cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorErrorV1> {
        *self.calls.lock().unwrap() += 1;
        self.release.notified().await;
        self.outcome.clone()
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
                declared_revision: Box::new(declared_revision),
                source_node_id,
                destination_node_id,
                direction: NativeIntegrationDirectionV1::PropagateDependencyToDependent,
            },
            grant_digest: digest('e'),
            policy_digest: digest('2'),
            observed_at: UtcMicros(2),
        },
        preview_id: NativeIntegrationPreviewId::new(format!("preview.stack.{index}")).unwrap(),
        preferred_mode: None,
        preview_expires_at: UtcMicros(8_000),
        observed_at: UtcMicros(2),
    }
}

#[tokio::test]
async fn half_open_circuit_rejects_stale_and_denied_but_success_closes() {
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
            .await
            .unwrap(),
        OptionalPreflightV1::Completed(NativeIntegrationPreflightOutcomeV1::Unavailable)
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(11))
            .await
            .unwrap(),
        OptionalPreflightV1::SuppressedOpenCircuit
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(110))
            .await
            .unwrap(),
        OptionalPreflightV1::Completed(NativeIntegrationPreflightOutcomeV1::Stale)
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(111))
            .await
            .unwrap(),
        OptionalPreflightV1::SuppressedOpenCircuit
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(210))
            .await
            .unwrap(),
        OptionalPreflightV1::Completed(NativeIntegrationPreflightOutcomeV1::Denied)
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(211))
            .await
            .unwrap(),
        OptionalPreflightV1::SuppressedOpenCircuit
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(310))
            .await
            .unwrap(),
        OptionalPreflightV1::Completed(NativeIntegrationPreflightOutcomeV1::Preview(Box::new(
            complete_preview()
        )))
    );
    assert_eq!(
        coordinator
            .optional_preflight(&preflight, &request, &cancellation, UtcMicros(311))
            .await
            .unwrap(),
        OptionalPreflightV1::Completed(NativeIntegrationPreflightOutcomeV1::Unavailable)
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
            run(coordinator.optional_preflight(
                preflight.as_ref(),
                &admitted,
                &CancellationSignal::active(format!("cancel.stack.repo-bound.{index}")).unwrap(),
                UtcMicros(10),
            ))
        }));
    }
    preflight.wait_for_active(MAX_REPOSITORY_PREFLIGHTS);
    let mut saturated = request.clone();
    saturated.preview_id = NativeIntegrationPreviewId::new("preview.stack.repo-saturated").unwrap();
    assert_eq!(
        run(coordinator.optional_preflight(
            preflight.as_ref(),
            &saturated,
            &CancellationSignal::active("cancel.stack.repo-saturated").unwrap(),
            UtcMicros(10),
        ))
        .unwrap(),
        OptionalPreflightV1::Saturated
    );
    preflight.release();
    for handle in handles {
        assert_eq!(
            handle.join().unwrap().unwrap(),
            OptionalPreflightV1::Completed(NativeIntegrationPreflightOutcomeV1::Unavailable)
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
            run(coordinator.optional_preflight(
                preflight.as_ref(),
                &request,
                &CancellationSignal::active(format!("cancel.stack.daemon-bound.{index}")).unwrap(),
                UtcMicros(20),
            ))
        }));
    }
    preflight.wait_for_active(MAX_DAEMON_PREFLIGHTS);
    let saturated = preflight_request(100);
    register_policy(&coordinator, &saturated);
    assert_eq!(
        run(coordinator.optional_preflight(
            preflight.as_ref(),
            &saturated,
            &CancellationSignal::active("cancel.stack.daemon-saturated").unwrap(),
            UtcMicros(20),
        ))
        .unwrap(),
        OptionalPreflightV1::Saturated
    );
    preflight.release();
    for handle in handles {
        assert_eq!(
            handle.join().unwrap().unwrap(),
            OptionalPreflightV1::Completed(NativeIntegrationPreflightOutcomeV1::Unavailable)
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
        run(owner_coordinator.optional_preflight(
            owner_preflight.as_ref(),
            &owner_request,
            &CancellationSignal::active("cancel.stack.join-owner").unwrap(),
            UtcMicros(30),
        ))
    });
    preflight.wait_for_active(1);
    let cancelled = CancellationSignal::active("cancel.stack.joiner").unwrap();
    cancelled.cancel(UtcMicros(31));
    assert_eq!(
        run(coordinator.optional_preflight(
            preflight.as_ref(),
            &request,
            &cancelled,
            UtcMicros(30)
        )),
        Err(StackCoordinatorErrorV1::Cancelled)
    );
    preflight.release();
    assert_eq!(
        owner.join().unwrap().unwrap(),
        OptionalPreflightV1::Completed(NativeIntegrationPreflightOutcomeV1::Unavailable)
    );
}

#[test]
fn joined_preflights_share_the_full_result_in_either_consumption_order() {
    let cases = [
        Ok(NativeIntegrationPreflightOutcomeV1::Preview(Box::new(
            complete_preview(),
        ))),
        Ok(NativeIntegrationPreflightOutcomeV1::Partial),
        Ok(NativeIntegrationPreflightOutcomeV1::DurabilityUncertain),
        Ok(NativeIntegrationPreflightOutcomeV1::ResetRequired),
        Ok(NativeIntegrationPreflightOutcomeV1::Stale),
        Ok(NativeIntegrationPreflightOutcomeV1::Denied),
        Ok(NativeIntegrationPreflightOutcomeV1::Cancelled),
        Err(StackCoordinatorErrorV1::Unavailable),
    ];
    for (case, expected) in cases.into_iter().enumerate() {
        for joiner_first in [false, true] {
            let coordinator = DaemonGitHubStackCoordinatorV1::default();
            let release = tokio::sync::Notify::new();
            let preflight = EventPreflight {
                release: &release,
                calls: Mutex::new(0),
                outcome: expected.clone(),
            };
            let request = preflight_request(500 + case);
            register_policy(&coordinator, &request);
            let cancellation = CancellationSignal::active("cancel.stack.shared").unwrap();
            run(async {
                let mut owner = Box::pin(coordinator.optional_preflight(
                    &preflight,
                    &request,
                    &cancellation,
                    UtcMicros(50),
                ));
                let mut joiner = Box::pin(coordinator.optional_preflight(
                    &preflight,
                    &request,
                    &cancellation,
                    UtcMicros(50),
                ));
                std::future::poll_fn(|cx| {
                    assert!(owner.as_mut().poll(cx).is_pending());
                    assert!(joiner.as_mut().poll(cx).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                release.notify_one();
                let published = std::future::poll_fn(|cx| match owner.as_mut().poll(cx) {
                    std::task::Poll::Ready(result) => std::task::Poll::Ready(result),
                    std::task::Poll::Pending => panic!("released owner must finish"),
                })
                .await;
                let expected = expected.clone().map(OptionalPreflightV1::Completed);
                if joiner_first {
                    assert_eq!(joiner.await, expected, "case {case} joiner");
                    assert_eq!(published, expected, "case {case} owner");
                } else {
                    assert_eq!(published, expected, "case {case} owner");
                    assert_eq!(joiner.await, expected, "case {case} joiner");
                }
                assert_eq!(*preflight.calls.lock().unwrap(), 1, "case {case} ran once");
            });
        }
    }
}

#[test]
fn different_request_identities_do_not_share_a_flight() {
    let coordinator = Arc::new(DaemonGitHubStackCoordinatorV1::default());
    let preflight = Arc::new(BlockingPreflight::default());
    let first = preflight_request(600);
    let mut second = first.clone();
    second.preview_id = NativeIntegrationPreviewId::new("preview.stack.other-identity").unwrap();
    let mut other_revision = first.clone();
    let NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
        declared_revision,
        revision_id,
        revision_digest,
        ..
    } = &mut other_revision.topology.selection
    else {
        panic!("declared request");
    };
    **declared_revision = BranchStackRevisionV1::new(
        declared_revision.stack_id.clone(),
        BranchStackRevisionId::new("revision.stack.other").unwrap(),
        declared_revision.inventory_snapshot_id.clone(),
        declared_revision.inventory_epoch,
        declared_revision.source,
        declared_revision.nodes.clone(),
        declared_revision.edges.clone(),
    )
    .unwrap();
    *revision_id = declared_revision.revision_id.clone();
    *revision_digest = declared_revision.digest.clone();
    let mut other_scope = first.clone();
    other_scope.topology.destination = ResolvedScope::new(
        first.topology.destination.project_id.clone(),
        first.topology.destination.repository_id.clone(),
        WorktreeId::new("worktree.stack.other").unwrap(),
        first.topology.destination.reference.clone(),
    )
    .unwrap();
    register_policy(&coordinator, &first);
    register_policy(&coordinator, &other_scope);
    let spawn = |request: NativeIntegrationPreflightRequestV1, suffix: &'static str| {
        let coordinator = Arc::clone(&coordinator);
        let preflight = Arc::clone(&preflight);
        std::thread::spawn(move || {
            run(coordinator.optional_preflight(
                preflight.as_ref(),
                &request,
                &CancellationSignal::active(format!("cancel.stack.identity.{suffix}")).unwrap(),
                UtcMicros(60),
            ))
        })
    };
    let handles = [
        spawn(first, "first"),
        spawn(second, "second"),
        spawn(other_revision, "revision"),
        spawn(other_scope, "scope"),
    ];
    preflight.wait_for_active(4);
    preflight.release();
    for handle in handles {
        assert_eq!(
            handle.join().unwrap(),
            Ok(OptionalPreflightV1::Completed(
                NativeIntegrationPreflightOutcomeV1::Unavailable
            ))
        );
    }
}

#[test]
fn joined_timeout_leaves_the_owner_running_and_preserves_its_result() {
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    let request = preflight_request(650);
    register_policy(&coordinator, &request);
    let cancellation = CancellationSignal::active("cancel.stack.timeout").unwrap();
    let release = tokio::sync::Notify::new();
    let preflight = EventPreflight {
        release: &release,
        calls: Mutex::new(0),
        outcome: Ok(NativeIntegrationPreflightOutcomeV1::Partial),
    };
    run(async {
        let mut owner = Box::pin(coordinator.optional_preflight(
            &preflight,
            &request,
            &cancellation,
            UtcMicros(65),
        ));
        std::future::poll_fn(|cx| {
            assert!(owner.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(
            coordinator
                .optional_preflight(&preflight, &request, &cancellation, UtcMicros(65))
                .await,
            Err(StackCoordinatorErrorV1::Unavailable)
        );
        release.notify_one();
        assert_eq!(
            owner.await,
            Ok(OptionalPreflightV1::Completed(
                NativeIntegrationPreflightOutcomeV1::Partial
            ))
        );
        assert_eq!(*preflight.calls.lock().unwrap(), 1);
        release.notify_one();
        assert_eq!(
            coordinator
                .optional_preflight(&preflight, &request, &cancellation, UtcMicros(66))
                .await,
            Ok(OptionalPreflightV1::Completed(
                NativeIntegrationPreflightOutcomeV1::Partial
            ))
        );
        assert_eq!(*preflight.calls.lock().unwrap(), 2);
    });
}

#[test]
fn abandoned_owner_wakes_its_waiter_and_frees_admission() {
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    let request = preflight_request(700);
    register_policy(&coordinator, &request);
    let cancellation = CancellationSignal::active("cancel.stack.abandoned").unwrap();
    run(async {
        let mut owner = Box::pin(coordinator.optional_preflight(
            &NeverFinishes,
            &request,
            &cancellation,
            UtcMicros(70),
        ));
        let mut joiner = Box::pin(coordinator.optional_preflight(
            &NeverFinishes,
            &request,
            &cancellation,
            UtcMicros(70),
        ));
        std::future::poll_fn(|cx| {
            assert!(owner.as_mut().poll(cx).is_pending());
            assert!(joiner.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;

        drop(owner);

        assert_eq!(
            joiner.await,
            Err(StackCoordinatorErrorV1::Unavailable),
            "the joined waiter is released instead of timing out"
        );
        let requests: Vec<_> = (0..MAX_REPOSITORY_PREFLIGHTS)
            .map(|index| {
                let mut admitted = request.clone();
                admitted.preview_id =
                    NativeIntegrationPreviewId::new(format!("preview.stack.abandoned.{index}"))
                        .unwrap();
                admitted
            })
            .collect();
        let mut held = Vec::new();
        for admitted in &requests {
            let mut abandoned = Box::pin(coordinator.optional_preflight(
                &NeverFinishes,
                admitted,
                &cancellation,
                UtcMicros(71),
            ));
            std::future::poll_fn(|cx| {
                assert!(abandoned.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            held.push(abandoned);
        }
        let mut saturated = request.clone();
        saturated.preview_id = NativeIntegrationPreviewId::new("preview.stack.saturated").unwrap();
        assert_eq!(
            coordinator
                .optional_preflight(&NeverFinishes, &saturated, &cancellation, UtcMicros(71))
                .await,
            Ok(OptionalPreflightV1::Saturated)
        );
        drop(held);
        let preflight = PreflightOutcomes {
            outcomes: Mutex::new(VecDeque::from([Ok(
                NativeIntegrationPreflightOutcomeV1::Partial,
            )])),
            calls: Mutex::new(0),
        };
        assert_eq!(
            coordinator
                .optional_preflight(&preflight, &request, &cancellation, UtcMicros(72))
                .await,
            Ok(OptionalPreflightV1::Completed(
                NativeIntegrationPreflightOutcomeV1::Partial
            )),
            "abandoned flights left no slot or registry entry behind"
        );
    });
}
