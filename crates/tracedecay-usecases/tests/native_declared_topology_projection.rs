//! Native-Git conformance for declared-stack publication through the verified
//! Git topology projection. The fixture creates a real linked worktree; the
//! only graph test seam retains the actual verified Grafeo snapshot produced
//! from the resolver's manifest.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    AuthorizedRootAdmission, AuthorizedScopeSet, AuthorizedScopeSetAuthority, CancellationContext,
    CancellationSignal, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    NativeIntegrationPortError, NativeIntegrationSelectionBindingV1,
    NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationStackResolutionPort,
    NativeIntegrationStackResolutionRequestV1, RegisteredRootLocatorV1, RequestContext, RequestId,
    ResolvedScope,
};
use tracedecay_code_index::git_projection::{
    GitTopologyProjectionError, GitTopologyProjectionStore, git_topology_namespace,
    git_topology_projection_identity,
};
use tracedecay_domain::{
    ActorId, BrainId, BranchStackEdgeV1, BranchStackId, BranchStackNodeV1, BranchStackRevisionId,
    BranchStackRevisionV1, BranchStackSourceV1, CommitId, LocatorDigest, ManifestDigest,
    NativeIntegrationDirectionV1, NativeIntegrationSelectionV1, ProjectId, RefId, RepositoryId,
    ScopeSetId, ScopeSetRevision, StackNodeId, UserProfileId, UtcMicros, WorktreeId,
    WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
};
use tracedecay_global_db::VerifiedGraphRuntimePortV1;
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphGenerationManifest, GraphIdempotencyKey,
    GraphProjectionIdentity, VerifiedGraphSnapshot,
};
use tracedecay_store::{
    FactReadControl, StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1,
    StoreShardIdV1, VerifiedStoreLocatorV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::native_integration::ExactPairNativeIntegrationTopology;

const CAPABILITY: &str = "capability.native-declared-topology";
const USE_CASE: &str = "use-case.native-declared-topology";

struct NeverCancelled;

impl GraphCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Test storage keeps only verified Grafeo snapshots. It does not recreate a
/// topology model: the resolver's manifest is applied by `VerifiedGraphSnapshot::memory`.
struct VerifiedSnapshotRuntime {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    snapshots: Mutex<Vec<VerifiedGraphSnapshot>>,
    cancelled: AtomicBool,
    cancel_on_publish: Mutex<Option<CancellationSignal>>,
}

impl Default for VerifiedSnapshotRuntime {
    fn default() -> Self {
        Self::for_project(ProjectId::new("project.native-declared-topology").expect("project id"))
    }
}

impl VerifiedSnapshotRuntime {
    fn for_project(project_id: ProjectId) -> Self {
        Self::for_scope(
            project_id,
            UserProfileId::new("profile.native-declared-topology").expect("profile id"),
        )
    }

    fn for_scope(project_id: ProjectId, profile_id: UserProfileId) -> Self {
        let shard_id = StoreShardIdV1::project(
            BrainId::new("brain.native-topology-fixture").expect("brain id"),
            profile_id,
            project_id,
        );
        let incarnation = StoreIncarnationV1::new(1).expect("store incarnation");
        Self {
            binding: StoreRuntimeBindingV1::new(
                shard_id.clone(),
                incarnation,
                StoreAuthorityEpochV1::new(1).expect("authority epoch"),
            ),
            locator: VerifiedStoreLocatorV1::new(
                shard_id,
                incarnation,
                LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).expect("locator digest"),
            ),
            snapshots: Mutex::new(Vec::new()),
            cancelled: AtomicBool::new(false),
            cancel_on_publish: Mutex::new(None),
        }
    }

    fn cancel_request_on_publish(&self, cancellation: CancellationSignal) {
        *self
            .cancel_on_publish
            .lock()
            .expect("publish cancellation mutex") = Some(cancellation);
    }

    fn snapshot(&self, projection: &GraphProjectionIdentity) -> VerifiedGraphSnapshot {
        self.snapshots
            .lock()
            .expect("snapshot mutex")
            .iter()
            .find(|snapshot| snapshot.projection() == projection)
            .cloned()
            .expect("published verified snapshot")
    }
}

impl VerifiedGraphRuntimePortV1 for VerifiedSnapshotRuntime {
    fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }

    fn cancel_reconciliation(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
        cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        if let Some(cancellation) = self
            .cancel_on_publish
            .lock()
            .expect("publish cancellation mutex")
            .take()
        {
            cancellation.cancel(UtcMicros(11));
        }
        if cancelled.load(Ordering::Acquire) || self.cancelled.load(Ordering::Acquire) {
            return Err(GraphDbError::Cancelled);
        }
        let snapshot = VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))?;
        let mut snapshots = self.snapshots.lock().expect("snapshot mutex");
        snapshots.retain(|current| current.projection() != snapshot.projection());
        snapshots.push(snapshot.clone());
        Ok(snapshot)
    }

    fn reconcile_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        _idempotency_key: GraphIdempotencyKey,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(GraphDbError::Cancelled);
        }
        let snapshot = VerifiedGraphSnapshot::memory(manifest.clone(), Arc::new(NeverCancelled))?;
        let mut snapshots = self.snapshots.lock().expect("snapshot mutex");
        snapshots.retain(|current| current.projection() != snapshot.projection());
        snapshots.push(snapshot.clone());
        Ok(snapshot)
    }

    fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        read_control: FactReadControl,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        if read_control.interrupted() || self.cancelled.load(Ordering::Acquire) {
            return Err(GraphDbError::Cancelled);
        }
        Ok(self
            .snapshots
            .lock()
            .expect("snapshot mutex")
            .iter()
            .find(|snapshot| snapshot.projection() == projection)
            .cloned())
    }
}

struct NativeGitFixture {
    repository: tempfile::TempDir,
    _linked_parent: tempfile::TempDir,
    linked: PathBuf,
}

impl NativeGitFixture {
    fn new() -> Self {
        let repository = tempfile::tempdir().expect("temporary repository");
        run_git(repository.path(), &["init", "--quiet", "-b", "main"]);
        run_git(
            repository.path(),
            &["config", "user.name", "TraceDecay Fixture"],
        );
        run_git(
            repository.path(),
            &["config", "user.email", "fixture@tracedecay.invalid"],
        );
        std::fs::write(repository.path().join("base.txt"), "base\n").expect("write base");
        run_git(repository.path(), &["add", "base.txt"]);
        run_git(repository.path(), &["commit", "--quiet", "-m", "base"]);
        run_git(repository.path(), &["branch", "feature"]);

        let linked_parent = tempfile::tempdir().expect("linked worktree parent");
        let linked = linked_parent.path().join("feature");
        run_git(
            repository.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                linked.to_str().expect("UTF-8 linked worktree path"),
                "feature",
            ],
        );
        Self {
            repository,
            _linked_parent: linked_parent,
            linked,
        }
    }

    fn root(&self) -> &Path {
        self.repository.path()
    }

    fn linked_root(&self) -> &Path {
        &self.linked
    }

    fn canonical_root(&self) -> PathBuf {
        self.root()
            .canonicalize()
            .expect("canonical repository root")
    }

    fn canonical_linked_root(&self) -> PathBuf {
        self.linked_root()
            .canonicalize()
            .expect("canonical linked worktree root")
    }

    fn tip(&self, root: &Path, reference: &str) -> CommitId {
        CommitId::new(run_git(root, &["rev-parse", reference])).expect("commit tip")
    }

    fn advance_feature(&self, contents: &str) {
        std::fs::write(self.linked_root().join("feature.txt"), contents)
            .expect("write feature change");
        run_git(self.linked_root(), &["add", "feature.txt"]);
        run_git(
            self.linked_root(),
            &["commit", "--quiet", "-m", "feature advance"],
        );
    }

    fn rollback_feature_to_main(&self) {
        run_git(self.linked_root(), &["reset", "--hard", "refs/heads/main"]);
    }
}

fn run_git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("start git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

fn digest(label: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", label.to_string().repeat(64))).expect("digest")
}

struct DeclaredStackRequest {
    project: ProjectId,
    repository: RepositoryId,
    source: ResolvedScope,
    destination: ResolvedScope,
    scope_set_id: ScopeSetId,
    scope_set_revision: ScopeSetRevision,
    inventory_snapshot_id: WorktreeInventorySnapshotId,
    inventory_epoch: WorktreeInventoryEpoch,
    revision: BranchStackRevisionV1,
    request: NativeIntegrationStackResolutionRequestV1,
}

fn declared_stack_request(fixture: &NativeGitFixture, revision_id: &str) -> DeclaredStackRequest {
    declared_stack_request_for_scope(
        fixture,
        revision_id,
        ScopeSetRevision::new(5).expect("scope revision"),
        "branch-stack.native",
    )
}

fn declared_stack_request_for_scope(
    fixture: &NativeGitFixture,
    revision_id: &str,
    scope_set_revision: ScopeSetRevision,
    stack_id: &str,
) -> DeclaredStackRequest {
    let project = ProjectId::new("project.native-declared-topology").expect("project");
    let repository = RepositoryId::new("repository.native-declared-topology").expect("repository");
    let main_worktree = WorktreeId::new("worktree.native.main").expect("main worktree");
    let feature_worktree = WorktreeId::new("worktree.native.feature").expect("feature worktree");
    let main_ref = RefId::new("refs/heads/main").expect("main ref");
    let feature_ref = RefId::new("refs/heads/feature").expect("feature ref");
    let destination = ResolvedScope::new(
        project.clone(),
        repository.clone(),
        main_worktree.clone(),
        Some(main_ref.clone()),
    )
    .expect("main scope");
    let source = ResolvedScope::new(
        project.clone(),
        repository.clone(),
        feature_worktree.clone(),
        Some(feature_ref.clone()),
    )
    .expect("feature scope");
    let scope_set_id = ScopeSetId::new("scope-set.native-declared-topology").expect("scope set");
    let authorized_scope_set = authorize_registered_roots(
        &project,
        scope_set_id.clone(),
        scope_set_revision,
        source.clone(),
        destination.clone(),
        fixture,
    );
    let inventory_snapshot_id =
        WorktreeInventorySnapshotId::new("worktree-inventory.native.7").expect("inventory");
    let inventory_epoch = WorktreeInventoryEpoch::new(7).expect("inventory epoch");
    let main_node = StackNodeId::new("stack-node.native.main").expect("main node");
    let feature_node = StackNodeId::new("stack-node.native.feature").expect("feature node");
    let revision = BranchStackRevisionV1::new(
        BranchStackId::new(stack_id).expect("stack"),
        BranchStackRevisionId::new(revision_id).expect("revision"),
        inventory_snapshot_id.clone(),
        inventory_epoch,
        BranchStackSourceV1::ExplicitDeclaration,
        vec![
            BranchStackNodeV1 {
                node_id: main_node.clone(),
                project_id: project.clone(),
                repository_id: repository.clone(),
                reference: main_ref,
                tip: fixture.tip(fixture.root(), "refs/heads/main"),
                worktree_id: Some(main_worktree),
            },
            BranchStackNodeV1 {
                node_id: feature_node.clone(),
                project_id: project.clone(),
                repository_id: repository.clone(),
                reference: feature_ref,
                tip: fixture.tip(fixture.linked_root(), "refs/heads/feature"),
                worktree_id: Some(feature_worktree),
            },
        ],
        vec![BranchStackEdgeV1 {
            dependency: main_node.clone(),
            dependent: feature_node.clone(),
        }],
    )
    .expect("declared stack revision");
    let request = NativeIntegrationStackResolutionRequestV1 {
        source: source.clone(),
        destination: destination.clone(),
        authorized_scope_set,
        inventory_snapshot_id: inventory_snapshot_id.clone(),
        inventory_epoch,
        selection: NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
            stack_id: revision.stack_id.clone(),
            revision_id: revision.revision_id.clone(),
            revision_digest: revision.digest.clone(),
            declared_revision: revision.clone(),
            source_node_id: feature_node,
            destination_node_id: main_node,
            direction: NativeIntegrationDirectionV1::LandDependentIntoDependency,
        },
        grant_digest: digest('a'),
        policy_digest: digest('b'),
        observed_at: UtcMicros(10),
    };
    request.validate().expect("exact declared-stack request");
    DeclaredStackRequest {
        project,
        repository,
        source,
        destination,
        scope_set_id,
        scope_set_revision,
        inventory_snapshot_id,
        inventory_epoch,
        revision,
        request,
    }
}

fn authorize_registered_roots(
    project: &ProjectId,
    scope_set_id: ScopeSetId,
    scope_set_revision: ScopeSetRevision,
    source: ResolvedScope,
    destination: ResolvedScope,
    fixture: &NativeGitFixture,
) -> AuthorizedScopeSet {
    AuthorizedScopeSetAuthority::authorize_registered(
        scope_set_id,
        scope_set_revision,
        vec![
            AuthorizedRootAdmission::new(
                request_context(destination, "main"),
                RegisteredRootLocatorV1::new(
                    project.clone(),
                    UserProfileId::new("profile.native-declared-topology").expect("profile"),
                    "store.native-declared-topology",
                    fixture.canonical_root(),
                )
                .expect("main locator"),
            )
            .expect("main admission"),
            AuthorizedRootAdmission::new(
                request_context(source, "feature"),
                RegisteredRootLocatorV1::new(
                    project.clone(),
                    UserProfileId::new("profile.native-declared-topology").expect("profile"),
                    "store.native-declared-topology",
                    fixture.canonical_linked_root(),
                )
                .expect("feature locator"),
            )
            .expect("feature admission"),
        ],
        &CapabilityId::new(CAPABILITY).expect("capability"),
        &UseCaseId::new(USE_CASE).expect("use case"),
        UtcMicros(10),
    )
    .expect("authorized registered roots")
}

fn request_context(scope: ResolvedScope, suffix: &str) -> RequestContext {
    let grant = CapabilityGrantSnapshot::new(
        tracedecay_application::CapabilityGrantId::new(format!("grant.native.{suffix}"))
            .expect("grant"),
        1,
        digest('c'),
        ActorId::new("actor.native.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(100),
        scope.clone(),
        BTreeSet::from([CapabilityId::new(CAPABILITY).expect("capability")]),
        BTreeSet::from([UseCaseId::new(USE_CASE).expect("use case")]),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.native.requester").expect("requester"),
        scope,
        grant,
        RequestId::new(format!("request.native.{suffix}")).expect("request"),
        Deadline::new(UtcMicros(90)).expect("deadline"),
        CancellationContext::active(format!("cancel.native.{suffix}")).expect("cancellation"),
    )
    .expect("request context")
}

fn resolver(
    fixture: &NativeGitFixture,
    request: &DeclaredStackRequest,
    runtime: Arc<VerifiedSnapshotRuntime>,
) -> ExactPairNativeIntegrationTopology {
    let expected_shard = runtime.relational_binding().shard_id.clone();
    ExactPairNativeIntegrationTopology::open_with_graph_runtime(
        request.project.clone(),
        request.repository.clone(),
        fixture.root(),
        expected_shard,
        runtime,
    )
    .expect("open declared-stack resolver")
}

fn projection_store(
    runtime: &VerifiedSnapshotRuntime,
    repository: &RepositoryId,
) -> GitTopologyProjectionStore {
    let identity = git_topology_projection_identity(
        git_topology_namespace(repository).expect("Git topology namespace"),
    )
    .expect("Git topology projection identity");
    GitTopologyProjectionStore::from_verified_snapshot_verified(
        runtime.snapshot(&identity),
        Arc::new(NeverCancelled),
    )
    .expect("verified Git topology store")
}

fn expect_declared_stack(
    outcome: NativeIntegrationStackResolutionOutcomeV1,
    expected: &BranchStackRevisionV1,
) {
    match outcome {
        NativeIntegrationStackResolutionOutcomeV1::Complete(selection) => {
            match selection.as_ref() {
                NativeIntegrationSelectionV1::DeclaredStackEdge(snapshot) => {
                    assert_eq!(&snapshot.revision, expected);
                    assert_eq!(
                        snapshot.direction,
                        NativeIntegrationDirectionV1::LandDependentIntoDependency
                    );
                }
                NativeIntegrationSelectionV1::IndependentBranch(_) => {
                    panic!("declared stack must not resolve as an independent branch")
                }
            }
        }
        other => panic!("declared stack should resolve completely, got {other:?}"),
    }
}

#[test]
fn declared_stack_request_rejects_a_revision_identity_that_disagrees_with_its_authority() {
    let fixture = NativeGitFixture::new();
    let mut declared = declared_stack_request(&fixture, "branch-stack-revision.native.contract");
    let NativeIntegrationSelectionBindingV1::DeclaredStackEdge { revision_id, .. } =
        &mut declared.request.selection
    else {
        panic!("fixture must declare a stack edge");
    };
    *revision_id =
        BranchStackRevisionId::new("branch-stack-revision.native.other").expect("revision");

    assert!(declared.request.validate().is_err());
}

#[test]
fn declared_stack_without_registered_linked_roots_is_unavailable() {
    let fixture = NativeGitFixture::new();
    let mut declared = declared_stack_request(&fixture, "branch-stack-revision.native.unavailable");
    declared.request.authorized_scope_set = AuthorizedScopeSetAuthority::authorize(
        ScopeSetId::new("scope-set.native-declared-topology.unregistered").expect("scope set"),
        ScopeSetRevision::new(1).expect("scope revision"),
        vec![
            request_context(declared.source.clone(), "unregistered.feature"),
            request_context(declared.destination.clone(), "unregistered.main"),
        ],
        &CapabilityId::new(CAPABILITY).expect("capability"),
        &UseCaseId::new(USE_CASE).expect("use case"),
        UtcMicros(10),
    )
    .expect("unregistered scope set");
    declared
        .request
        .validate()
        .expect("declared request remains exact");

    let runtime = Arc::new(VerifiedSnapshotRuntime::default());
    let resolver = resolver(&fixture, &declared, Arc::clone(&runtime));
    let cancellation = CancellationSignal::active("cancel.native-declared-topology.unavailable")
        .expect("cancellation");

    assert_eq!(
        resolver
            .resolve(&declared.request, &cancellation)
            .expect("unavailable declared-stack resolution"),
        NativeIntegrationStackResolutionOutcomeV1::Unavailable
    );
}

#[test]
fn declared_stack_outside_the_enrolled_project_is_denied() {
    let fixture = NativeGitFixture::new();
    let declared = declared_stack_request(&fixture, "branch-stack-revision.native.denied");
    let other_project =
        ProjectId::new("project.native-declared-topology.other").expect("other project");
    let runtime = Arc::new(VerifiedSnapshotRuntime::for_project(other_project.clone()));
    let expected_shard = runtime.relational_binding().shard_id.clone();
    let resolver = ExactPairNativeIntegrationTopology::open_with_graph_runtime(
        other_project,
        declared.repository.clone(),
        fixture.root(),
        expected_shard,
        runtime,
    )
    .expect("open unrelated resolver");
    let cancellation =
        CancellationSignal::active("cancel.native-declared-topology.denied").expect("cancellation");

    assert_eq!(
        resolver
            .resolve(&declared.request, &cancellation)
            .expect("denied declared-stack resolution"),
        NativeIntegrationStackResolutionOutcomeV1::Denied
    );
}

#[test]
fn native_topology_rejects_a_foreign_graph_runtime_binding() {
    let fixture = NativeGitFixture::new();
    let project = ProjectId::new("project.native-declared-topology").expect("project");
    let foreign = Arc::new(VerifiedSnapshotRuntime::for_project(
        ProjectId::new("project.native-declared-topology.foreign").expect("foreign project"),
    ));
    let expected_shard = StoreShardIdV1::project(
        foreign.binding.shard_id.brain_id.clone(),
        foreign.binding.shard_id.profile_id.clone(),
        project.clone(),
    );

    assert_eq!(
        ExactPairNativeIntegrationTopology::open_with_graph_runtime(
            project,
            RepositoryId::new("repository.native-declared-topology").expect("repository"),
            fixture.root(),
            expected_shard,
            foreign,
        )
        .err(),
        Some(NativeIntegrationPortError::Unavailable)
    );
}

#[test]
fn native_topology_rejects_an_incoherent_verified_locator() {
    let fixture = NativeGitFixture::new();
    let project = ProjectId::new("project.native-declared-topology").expect("project");
    let mut incoherent = VerifiedSnapshotRuntime::for_project(project.clone());
    let expected_shard = incoherent.binding.shard_id.clone();
    incoherent.locator = VerifiedStoreLocatorV1::new(
        incoherent.binding.shard_id.clone(),
        StoreIncarnationV1::new(2).expect("different locator incarnation"),
        LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).expect("locator digest"),
    );

    assert_eq!(
        ExactPairNativeIntegrationTopology::open_with_graph_runtime(
            project,
            RepositoryId::new("repository.native-declared-topology").expect("repository"),
            fixture.root(),
            expected_shard,
            Arc::new(incoherent),
        )
        .err(),
        Some(NativeIntegrationPortError::Unavailable)
    );
}

#[test]
fn native_topology_rejects_a_same_project_foreign_brain_runtime() {
    let fixture = NativeGitFixture::new();
    let project = ProjectId::new("project.native-declared-topology").expect("project");
    let runtime = Arc::new(VerifiedSnapshotRuntime::for_project(project.clone()));
    let expected_shard = StoreShardIdV1::project(
        BrainId::new("brain.native-topology-expected").expect("expected brain"),
        runtime.binding.shard_id.profile_id.clone(),
        project.clone(),
    );

    assert_eq!(
        ExactPairNativeIntegrationTopology::open_with_graph_runtime(
            project,
            RepositoryId::new("repository.native-declared-topology").expect("repository"),
            fixture.root(),
            expected_shard,
            runtime,
        )
        .err(),
        Some(NativeIntegrationPortError::Unavailable)
    );
}

#[test]
fn declared_stack_with_a_foreign_profile_graph_runtime_is_unavailable() {
    let fixture = NativeGitFixture::new();
    let declared = declared_stack_request(&fixture, "branch-stack-revision.native.profile");
    let runtime = Arc::new(VerifiedSnapshotRuntime::for_scope(
        declared.project.clone(),
        UserProfileId::new("profile.native-declared-topology.foreign").expect("foreign profile"),
    ));
    let resolver = resolver(&fixture, &declared, runtime);
    let cancellation = CancellationSignal::active("cancel.native-declared-topology.profile")
        .expect("cancellation");

    assert_eq!(
        resolver
            .resolve(&declared.request, &cancellation)
            .expect("foreign-profile declared-stack resolution"),
        NativeIntegrationStackResolutionOutcomeV1::Unavailable
    );
}

#[test]
fn declared_stack_cancellation_after_graph_commit_admission_returns_complete() {
    let fixture = NativeGitFixture::new();
    fixture.advance_feature("cancelled feature revision\n");
    let declared = declared_stack_request(&fixture, "branch-stack-revision.native.cancelled");
    let runtime = Arc::new(VerifiedSnapshotRuntime::default());
    let resolver = resolver(&fixture, &declared, Arc::clone(&runtime));
    let cancellation = CancellationSignal::active("cancel.native-declared-topology.publication")
        .expect("cancellation");
    runtime.cancel_request_on_publish(cancellation.clone());

    expect_declared_stack(
        resolver
            .resolve(&declared.request, &cancellation)
            .expect("cancelled declared-stack resolution"),
        &declared.revision,
    );
    assert_eq!(cancellation.cancelled_at(), Some(UtcMicros(11)));
    let published = projection_store(&runtime, &declared.repository);
    assert_eq!(published.repository(), &declared.repository);
}

#[test]
fn declared_stack_projection_conforms_to_native_linked_worktree_state() {
    let fixture = NativeGitFixture::new();
    fixture.advance_feature("first feature revision\n");
    let first = declared_stack_request(&fixture, "branch-stack-revision.native.1");
    let runtime = Arc::new(VerifiedSnapshotRuntime::default());
    let resolver = resolver(&fixture, &first, Arc::clone(&runtime));
    let cancellation = CancellationSignal::active("cancel.native-declared-topology.resolve")
        .expect("cancellation");

    expect_declared_stack(
        resolver
            .resolve(&first.request, &cancellation)
            .expect("native declared-stack resolution"),
        &first.revision,
    );
    let first_store = projection_store(&runtime, &first.repository);
    assert_eq!(first_store.repository(), &first.repository);
    assert_eq!(
        first_store
            .branch_stack_revision_exact(
                &first.project,
                &first.repository,
                &first.scope_set_id,
                first.scope_set_revision,
                first.request.authorized_scope_set.digest(),
                &first.revision.stack_id,
                &first.revision.revision_id,
                &first.revision.digest,
                &first.inventory_snapshot_id,
                first.inventory_epoch,
                Arc::new(NeverCancelled),
            )
            .expect("exact declared revision"),
        Some(first.revision.clone())
    );
    assert_eq!(
        first_store
            .worktree_occupancy_exact(
                &first.project,
                &first.repository,
                &first.scope_set_id,
                first.scope_set_revision,
                first.request.authorized_scope_set.digest(),
                first.source.reference.as_ref().expect("source ref"),
                Arc::new(NeverCancelled),
            )
            .expect("feature occupancy"),
        vec![first.source.worktree_id.clone()]
    );
    assert_eq!(
        first_store
            .worktree_occupancy_exact(
                &first.project,
                &first.repository,
                &first.scope_set_id,
                first.scope_set_revision,
                first.request.authorized_scope_set.digest(),
                first
                    .destination
                    .reference
                    .as_ref()
                    .expect("destination ref"),
                Arc::new(NeverCancelled),
            )
            .expect("main occupancy"),
        vec![first.destination.worktree_id.clone()]
    );

    fixture.advance_feature("second feature revision\n");
    let second = declared_stack_request(&fixture, "branch-stack-revision.native.2");
    expect_declared_stack(
        resolver
            .resolve(&second.request, &cancellation)
            .expect("republished declared-stack resolution"),
        &second.revision,
    );
    let second_store = projection_store(&runtime, &second.repository);
    assert_ne!(first_store.generation(), second_store.generation());
    assert_ne!(first_store.ref_watermark(), second_store.ref_watermark());
}

#[test]
fn declared_scope_revision_change_replaces_the_prior_projected_scope() {
    let fixture = NativeGitFixture::new();
    fixture.advance_feature("first feature revision\n");
    let first = declared_stack_request_for_scope(
        &fixture,
        "branch-stack-revision.native.scope.5",
        ScopeSetRevision::new(5).expect("first scope revision"),
        "branch-stack.native.scope.5",
    );
    let runtime = Arc::new(VerifiedSnapshotRuntime::default());
    let resolver = resolver(&fixture, &first, Arc::clone(&runtime));
    let cancellation = CancellationSignal::active("cancel.native-declared-topology.scope-revision")
        .expect("cancellation");

    expect_declared_stack(
        resolver
            .resolve(&first.request, &cancellation)
            .expect("first declared-stack resolution"),
        &first.revision,
    );

    fixture.advance_feature("second feature revision\n");
    let second = declared_stack_request_for_scope(
        &fixture,
        "branch-stack-revision.native.scope.6",
        ScopeSetRevision::new(6).expect("second scope revision"),
        "branch-stack.native.scope.6",
    );
    expect_declared_stack(
        resolver
            .resolve(&second.request, &cancellation)
            .expect("second declared-stack resolution"),
        &second.revision,
    );

    let store = projection_store(&runtime, &second.repository);
    assert_eq!(store.branch_stacks().len(), 1);
    assert_eq!(store.worktree_occupancies().len(), 2);
    assert_eq!(
        store.branch_stack_revision_exact(
            &first.project,
            &first.repository,
            &first.scope_set_id,
            first.scope_set_revision,
            first.request.authorized_scope_set.digest(),
            &first.revision.stack_id,
            &first.revision.revision_id,
            &first.revision.digest,
            &first.inventory_snapshot_id,
            first.inventory_epoch,
            Arc::new(NeverCancelled),
        ),
        Err(GitTopologyProjectionError::StaleBinding {
            detail: "scope-set revision or digest changed",
        })
    );
    assert_eq!(
        store.worktree_occupancy_exact(
            &first.project,
            &first.repository,
            &first.scope_set_id,
            first.scope_set_revision,
            first.request.authorized_scope_set.digest(),
            first.source.reference.as_ref().expect("source ref"),
            Arc::new(NeverCancelled),
        ),
        Err(GitTopologyProjectionError::StaleBinding {
            detail: "scope-set revision or digest changed",
        })
    );
    assert_eq!(
        store
            .worktree_occupancy_exact(
                &second.project,
                &second.repository,
                &second.scope_set_id,
                second.scope_set_revision,
                second.request.authorized_scope_set.digest(),
                second.source.reference.as_ref().expect("source ref"),
                Arc::new(NeverCancelled),
            )
            .expect("second scope occupancy"),
        vec![second.source.worktree_id.clone()]
    );
}

#[test]
fn declared_stack_ref_rollback_rejects_stale_binding_without_replacing_verified_generation() {
    let fixture = NativeGitFixture::new();
    fixture.advance_feature("feature revision before rollback\n");
    let declared = declared_stack_request(&fixture, "branch-stack-revision.native.rollback");
    let runtime = Arc::new(VerifiedSnapshotRuntime::default());
    let resolver = resolver(&fixture, &declared, Arc::clone(&runtime));
    let cancellation = CancellationSignal::active("cancel.native-declared-topology.rollback")
        .expect("cancellation");

    expect_declared_stack(
        resolver
            .resolve(&declared.request, &cancellation)
            .expect("initial declared-stack resolution"),
        &declared.revision,
    );
    let published = projection_store(&runtime, &declared.repository);
    let expected_generation = published.generation().clone();
    let stale_inventory_epoch = WorktreeInventoryEpoch::new(8).expect("stale inventory epoch");
    assert_eq!(
        published.branch_stack_revision_exact(
            &declared.project,
            &declared.repository,
            &declared.scope_set_id,
            declared.scope_set_revision,
            declared.request.authorized_scope_set.digest(),
            &declared.revision.stack_id,
            &declared.revision.revision_id,
            &declared.revision.digest,
            &declared.inventory_snapshot_id,
            stale_inventory_epoch,
            Arc::new(NeverCancelled),
        ),
        Err(GitTopologyProjectionError::StaleBinding {
            detail: "worktree inventory fence changed",
        })
    );
    assert_eq!(
        published.worktree_occupancy_exact(
            &declared.project,
            &declared.repository,
            &declared.scope_set_id,
            ScopeSetRevision::new(6).expect("stale scope revision"),
            declared.request.authorized_scope_set.digest(),
            declared.source.reference.as_ref().expect("source ref"),
            Arc::new(NeverCancelled),
        ),
        Err(GitTopologyProjectionError::StaleBinding {
            detail: "scope-set revision or digest changed",
        })
    );
    assert_eq!(
        published.worktree_occupancy_exact(
            &declared.project,
            &declared.repository,
            &ScopeSetId::new("scope-set.native-declared-topology.other").expect("other scope"),
            declared.scope_set_revision,
            declared.request.authorized_scope_set.digest(),
            declared.source.reference.as_ref().expect("source ref"),
            Arc::new(NeverCancelled),
        ),
        Err(GitTopologyProjectionError::Unavailable(
            "exact scope-set topology projection is unavailable".to_owned(),
        ))
    );

    fixture.rollback_feature_to_main();
    assert_eq!(
        resolver
            .resolve(&declared.request, &cancellation)
            .expect("stale declared-stack resolution"),
        NativeIntegrationStackResolutionOutcomeV1::Stale
    );
    assert_eq!(
        projection_store(&runtime, &declared.repository).generation(),
        &expected_generation
    );
}
