//! Native-Git journey proof for the mounted Plan 36 daemon authority.
//!
//! The test drives the retained daemon owner, its canonical SQLite-backed
//! store actor, the exact-pair resolver, and the real `gix` adapter against
//! temporary repositories. It deliberately does not substitute a mock
//! transaction port or a test-only Git implementation.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::analysis::DaemonNativeIntegrationAnalysisV1;
use super::registry::DaemonNativeIntegrationServiceRegistry;
use super::stack_signals::signal_from_preflight;
use tracedecay_application::native_integration::{
    GixNativeIntegrationAdapter, NativeApplyEffectV1, NativeIntegrationAnalysisPort,
    NativeIntegrationAnalysisRevalidationV1, NativeIntegrationMechanics,
};
use tracedecay_application::source_authorization::ProjectSourceAccessSnapshot;
use tracedecay_application::stack_coordinator::{
    DaemonGitHubStackCoordinatorV1, StackSignalDraftV1, StackSignalV1,
};
use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_code_index_runtime::code_index_scheduler::identity::IndexingIdentityV1;
use tracedecay_contracts::git::{
    GITHUB_STACK_SIGNAL_EXPAND_OPERATION, GitHubStackSignalExpandPort,
    GitHubStackSignalExpandPortError, GitHubStackSignalExpandSurfaceRequest,
    GitHubStackSignalExpandSurfaceResultV1,
};
use tracedecay_contracts::{
    AuthorizedRootAdmission, AuthorizedScopeSet, AuthorizedScopeSetAuthority, CancellationContext,
    CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    NativeIntegrationApplyRequestV1, NativeIntegrationPreflightOutcomeV1,
    NativeIntegrationPreflightRequestV1, NativeIntegrationSelectionBindingV1,
    NativeIntegrationStackResolutionOutcomeV1, NativeIntegrationStackResolutionRequestV1,
    RegisteredRootLocatorV1, RequestContext, RequestId, ResolvedScope, SharedProfileStoreLocatorV1,
    native_integration_surface_operation,
};
use tracedecay_domain::{
    ActorId, AuthorityRef, BranchStackEdgeV1, BranchStackId, BranchStackNodeV1,
    BranchStackRevisionId, BranchStackRevisionV1, BranchStackSourceV1, CapabilityId, CommitId,
    ConfigurationRevisionId, FrozenBranchStackSnapshotV1, LocatorDigest, ManifestDigest,
    MechanicalIntegrationModeV1, NativeIntegrationAnalysisCoverageV1,
    NativeIntegrationAnalysisGapV1, NativeIntegrationApprovalId, NativeIntegrationApprovalV1,
    NativeIntegrationDirectionV1, NativeIntegrationPreviewDispositionV1,
    NativeIntegrationPreviewId, NativeIntegrationSelectionV1, NativeIntegrationTerminalOutcomeV1,
    NativeIntegrationTransactionId, ProjectId, RefId, RepositoryId, ScopeSetId, ScopeSetRevision,
    ScopeSourceBinding, SourceBindingId, SourceKindV1, StackNodeId, StackSignalKindV1, UtcMicros,
    WorktreeId, WorktreeInventoryEpoch, WorktreeInventorySnapshotId, canonical_sha256,
};
use tracedecay_global_db::tests::harness::HostAdmissionTestRuntimeV1;
use tracedecay_global_db::{GitHubStackDeliveryStateV1, RegisteredGlobalDbLeaseV1};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::git::try_git_program;
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_store::NativeIntegrationStore;

const OBSERVED_AT: UtcMicros = UtcMicros(100);
const EXPIRES_AT: UtcMicros = UtcMicros(10_000);

struct UnexpectedAnalysis;

impl NativeIntegrationAnalysisPort for UnexpectedAnalysis {
    fn analyze(
        &self,
        _selection: &NativeIntegrationSelectionV1,
        _native: &tracedecay_runtime_core::git_repository::GitNativePreflight,
        _candidate: &tracedecay_runtime_core::git_repository::GitNativeCandidateTreeV1<'_>,
        _deadline: &Deadline,
        _cancellation_signal: &CancellationSignal,
        _cancellation: &CancellationToken,
    ) -> Result<
        tracedecay_domain::NativeIntegrationAnalysisReportV1,
        tracedecay_contracts::NativeIntegrationPortError,
    > {
        panic!("semantic analysis must not run for a native conflict")
    }

    fn revalidate(
        &self,
        _report: &tracedecay_domain::NativeIntegrationAnalysisReportV1,
        _deadline: &Deadline,
        _cancellation: &CancellationSignal,
    ) -> Result<
        NativeIntegrationAnalysisRevalidationV1,
        tracedecay_contracts::NativeIntegrationPortError,
    > {
        panic!("semantic revalidation must not run without a candidate")
    }
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new(try_git_program().expect("resolve the git program"))
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn write_and_commit(root: &Path, path: &str, contents: &str, message: &str) {
    std::fs::write(root.join(path), contents).expect("write fixture content");
    git(root, &["add", path]);
    git(root, &["commit", "-m", message]);
}

fn initialized_repository(root: &Path) {
    for arguments in [
        ["init", "--initial-branch=main"].as_slice(),
        ["config", "user.email", "fixture@example.com"].as_slice(),
        ["config", "user.name", "Fixture"].as_slice(),
    ] {
        git(root, arguments);
    }
    std::fs::create_dir_all(root.join("src")).expect("fixture source directory");
    write_and_commit(root, "src/lib.rs", "pub fn shared_seed() {}\n", "seed");
}

fn prepare_pair(root: &Path, mode: MechanicalIntegrationModeV1) {
    initialized_repository(root);
    git(root, &["checkout", "-b", "destination"]);
    if mode != MechanicalIntegrationModeV1::FastForward {
        write_and_commit(
            root,
            "src/destination.rs",
            "pub fn destination_value() {}\n",
            "destination",
        );
    }
    git(root, &["checkout", "main"]);
    git(root, &["checkout", "-b", "source"]);
    write_and_commit(
        root,
        "src/source_1.rs",
        "pub fn source_one() {}\n",
        "source one",
    );
    if mode == MechanicalIntegrationModeV1::CherryPickExactCommits {
        write_and_commit(
            root,
            "src/source_2.rs",
            "pub fn source_two() {}\n",
            "source two",
        );
    }
    // Neither selected branch is checked out, so the production adapter can
    // prove that this journey does not materialize a selected worktree.
    git(root, &["checkout", "main"]);
}

fn prepare_generated_only_pair(root: &Path) {
    initialized_repository(root);
    std::fs::create_dir_all(root.join("dist")).expect("generated fixture directory");
    write_and_commit(
        root,
        "dist/generated.js",
        "export const generated = 1;\n",
        "generated base",
    );
    git(root, &["branch", "destination"]);
    git(root, &["checkout", "-b", "source"]);
    write_and_commit(
        root,
        "dist/generated.js",
        "export const generated = 2;\n",
        "generated source",
    );
    git(root, &["checkout", "main"]);
}

fn prepare_checked_out_conflict(root: &Path, source_root: &Path) {
    initialized_repository(root);
    git(root, &["checkout", "-b", "dependent"]);
    std::fs::write(root.join("seed.txt"), "dependent\n").expect("write dependent conflict");
    git(root, &["add", "seed.txt"]);
    git(root, &["commit", "-m", "dependent conflict"]);
    git(root, &["branch", "dependency", "main"]);
    git(
        root,
        &[
            "worktree",
            "add",
            source_root.to_str().expect("source root"),
            "dependency",
        ],
    );
    std::fs::write(source_root.join("seed.txt"), "dependency\n")
        .expect("write dependency conflict");
    git(source_root, &["add", "seed.txt"]);
    git(source_root, &["commit", "-m", "dependency conflict"]);
}

fn exact_pair_scopes(repository_root: &Path) -> (ResolvedScope, ResolvedScope) {
    let project = ProjectId::new("project.native.journey").expect("project id");
    let identity = IndexingIdentityV1::resolve(repository_root).expect("indexing identity");
    let authority = ResolvedScope::new(
        project,
        identity.repository_id().clone(),
        identity.worktree_id().clone(),
        identity.head_ref().cloned(),
    )
    .expect("authority scope");
    (authority.clone(), authority)
}

fn operation_authority(
    operation: &str,
) -> (
    tracedecay_tool_catalog::CapabilityId,
    tracedecay_tool_catalog::UseCaseId,
) {
    let operation = native_integration_surface_operation(operation)
        .expect("canonical operation")
        .expect("declared operation");
    (
        operation.capability_id().clone(),
        operation.use_case_id().clone(),
    )
}

fn context(destination: ResolvedScope, request_id: &str) -> RequestContext {
    let (preflight_capability, preflight_use_case) =
        operation_authority(tracedecay_contracts::NATIVE_INTEGRATION_PREFLIGHT_OPERATION);
    let (apply_capability, apply_use_case) =
        operation_authority(tracedecay_contracts::NATIVE_INTEGRATION_APPLY_OPERATION);
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.native.journey").expect("grant id"),
        1,
        digest('a'),
        ActorId::new("actor.native.issuer").expect("issuer"),
        UtcMicros(1),
        EXPIRES_AT,
        destination.clone(),
        BTreeSet::from([preflight_capability, apply_capability]),
        BTreeSet::from([preflight_use_case, apply_use_case]),
        DisclosureClass::Sensitive,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.native.requester").expect("requester"),
        destination,
        grant,
        RequestId::new(request_id).expect("request id"),
        Deadline::new(UtcMicros(
            tracedecay_contracts::clock::now_micros()
                .0
                .saturating_add(60_000_000),
        ))
        .expect("deadline"),
        CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
    )
    .expect("request context")
}

fn authorized_scope_set(
    source: ResolvedScope,
    destination: ResolvedScope,
    request_id: &str,
) -> AuthorizedScopeSet {
    let (capability, use_case) =
        operation_authority(tracedecay_contracts::NATIVE_INTEGRATION_PREFLIGHT_OPERATION);
    let contexts = if source == destination {
        vec![context(source, &format!("{request_id}.authority"))]
    } else {
        vec![
            context(source, &format!("{request_id}.source")),
            context(destination, &format!("{request_id}.destination")),
        ]
    };
    AuthorizedScopeSetAuthority::authorize(
        ScopeSetId::new(format!("scope-set.native.journey.{request_id}")).expect("scope set id"),
        ScopeSetRevision::new(1).expect("scope set revision"),
        contexts,
        &capability,
        &use_case,
        OBSERVED_AT,
    )
    .expect("authorized scope set")
}

fn preflight_request(
    repository_root: &Path,
    mode: MechanicalIntegrationModeV1,
    request_id: &str,
) -> NativeIntegrationPreflightRequestV1 {
    let (source, destination) = exact_pair_scopes(repository_root);
    let context = context(destination.clone(), request_id);
    let authorized_scope_set =
        authorized_scope_set(source.clone(), destination.clone(), request_id);
    NativeIntegrationPreflightRequestV1 {
        context,
        topology: NativeIntegrationStackResolutionRequestV1 {
            source,
            destination,
            authorized_scope_set,
            inventory_snapshot_id: WorktreeInventorySnapshotId::new("inventory.native.journey")
                .expect("inventory snapshot"),
            inventory_epoch: WorktreeInventoryEpoch::new(1).expect("inventory epoch"),
            selection: NativeIntegrationSelectionBindingV1::IndependentBranch {
                proposal_digest: digest('c'),
                source_ref: RefId::new("refs/heads/source").expect("source ref"),
                destination_ref: RefId::new("refs/heads/destination").expect("destination ref"),
            },
            grant_digest: digest('a'),
            policy_digest: digest('d'),
            observed_at: OBSERVED_AT,
        },
        preview_id: NativeIntegrationPreviewId::new(format!("preview.native.{request_id}"))
            .expect("preview id"),
        preferred_mode: Some(mode),
        preview_expires_at: EXPIRES_AT,
        observed_at: OBSERVED_AT,
    }
}

fn declared_preflight_request(
    repository_root: &Path,
    database: &RegisteredGlobalDbLeaseV1,
    mode: MechanicalIntegrationModeV1,
    request_id: &str,
) -> NativeIntegrationPreflightRequestV1 {
    let project_id = ProjectId::new("project.native.journey").expect("project id");
    let repository_id = IndexingIdentityV1::resolve(repository_root)
        .expect("indexing identity")
        .repository_id()
        .clone();
    let source = ResolvedScope::new(
        project_id.clone(),
        repository_id.clone(),
        WorktreeId::new("worktree.native.source").expect("source worktree id"),
        Some(RefId::new("refs/heads/source").expect("source ref")),
    )
    .expect("source scope");
    let destination = ResolvedScope::new(
        project_id.clone(),
        repository_id.clone(),
        WorktreeId::new("worktree.native.destination").expect("destination worktree id"),
        Some(RefId::new("refs/heads/destination").expect("destination ref")),
    )
    .expect("destination scope");
    let source_node_id = StackNodeId::new("node.native.source").expect("source node");
    let destination_node_id =
        StackNodeId::new("node.native.destination").expect("destination node");
    let inventory_snapshot_id =
        WorktreeInventorySnapshotId::new("inventory.native.journey").expect("inventory snapshot");
    let inventory_epoch = WorktreeInventoryEpoch::new(1).expect("inventory epoch");
    let revision = BranchStackRevisionV1::new(
        BranchStackId::new("stack.native.journey").expect("stack id"),
        BranchStackRevisionId::new("revision.native.journey").expect("revision id"),
        inventory_snapshot_id.clone(),
        inventory_epoch,
        BranchStackSourceV1::ExplicitDeclaration,
        vec![
            BranchStackNodeV1 {
                node_id: source_node_id.clone(),
                project_id: project_id.clone(),
                repository_id: repository_id.clone(),
                reference: RefId::new("refs/heads/source").expect("source ref"),
                tip: CommitId::new(git(repository_root, &["rev-parse", "refs/heads/source"]))
                    .expect("source tip"),
                worktree_id: None,
            },
            BranchStackNodeV1 {
                node_id: destination_node_id.clone(),
                project_id: project_id.clone(),
                repository_id,
                reference: RefId::new("refs/heads/destination").expect("destination ref"),
                tip: CommitId::new(git(
                    repository_root,
                    &["rev-parse", "refs/heads/destination"],
                ))
                .expect("destination tip"),
                worktree_id: None,
            },
        ],
        vec![BranchStackEdgeV1 {
            dependency: source_node_id.clone(),
            dependent: destination_node_id.clone(),
        }],
    )
    .expect("declared revision");
    let shard = &database.binding().shard_id;
    let profile = SharedProfileStoreLocatorV1::new(
        shard.brain_id.clone(),
        shard.profile_id.clone(),
        database.db_path().display().to_string(),
    )
    .expect("registered profile store");
    let (capability, use_case) =
        operation_authority(tracedecay_contracts::NATIVE_INTEGRATION_PREFLIGHT_OPERATION);
    let admissions = [source.clone(), destination.clone()]
        .into_iter()
        .enumerate()
        .map(|(index, scope)| {
            AuthorizedRootAdmission::new(
                context(scope, &format!("{request_id}.root.{index}")),
                RegisteredRootLocatorV1::new(project_id.clone(), profile.clone(), repository_root)
                    .expect("registered repository root"),
            )
            .expect("registered root admission")
        })
        .collect();
    let scope_set = AuthorizedScopeSetAuthority::authorize_registered(
        ScopeSetId::new(format!("scope-set.native.journey.{request_id}")).expect("scope set id"),
        ScopeSetRevision::new(1).expect("scope set revision"),
        admissions,
        &capability,
        &use_case,
        OBSERVED_AT,
    )
    .expect("registered authorized scope set");
    let mut request = preflight_request(repository_root, mode, request_id);
    request.context = context(destination.clone(), request_id);
    request.topology.source = source;
    request.topology.destination = destination;
    request.topology.authorized_scope_set = scope_set;
    request.topology.inventory_snapshot_id = inventory_snapshot_id;
    request.topology.inventory_epoch = inventory_epoch;
    request.topology.selection = NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
        stack_id: revision.stack_id.clone(),
        revision_id: revision.revision_id.clone(),
        revision_digest: revision.digest.clone(),
        declared_revision: Box::new(revision),
        source_node_id,
        destination_node_id,
        direction: NativeIntegrationDirectionV1::PropagateDependencyToDependent,
    };
    request
}

fn approval_for(
    context: &RequestContext,
    preview: &tracedecay_domain::NativeIntegrationPreviewV1,
    request_id: &str,
) -> NativeIntegrationApprovalV1 {
    let (capability, _) =
        operation_authority(tracedecay_contracts::NATIVE_INTEGRATION_APPLY_OPERATION);
    NativeIntegrationApprovalV1 {
        approval_id: NativeIntegrationApprovalId::new(format!("approval.native.{request_id}"))
            .expect("approval id"),
        preview_id: preview.preview_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        principal: context.actor().clone(),
        delegated_agent: None,
        capability: CapabilityId::new(capability.as_str().to_owned()).expect("domain capability"),
        grant_digest: context.grant().digest.clone(),
        issued_at: OBSERVED_AT,
        expires_at: preview.expires_at,
        approval_digest: canonical_sha256(&"pending native journey approval").expect("digest"),
    }
    .seal()
    .expect("approval")
}

async fn mount(
    database: RegisteredGlobalDbLeaseV1,
    repository_root: std::path::PathBuf,
) -> (
    DaemonNativeIntegrationServiceRegistry,
    super::registry::DaemonNativeIntegrationOwner,
    Arc<DaemonNativeIntegrationAnalysisV1>,
) {
    let project_id = ProjectId::new("project.native.journey").expect("project id");
    let identity = IndexingIdentityV1::resolve(&repository_root).expect("indexing identity");
    let analysis_scope = ResolvedScope::new(
        project_id.clone(),
        identity.repository_id().clone(),
        identity.worktree_id().clone(),
        identity.head_ref().cloned(),
    )
    .expect("analysis scope");
    let schedulers = CodeIndexSchedulerRegistryV1::new(1);
    let index_store = repository_root
        .parent()
        .expect("repository parent")
        .join("native-code-index");
    schedulers
        .mount_worktree(project_id.clone(), &repository_root, index_store, None)
        .await
        .expect("mount canonical code-index scheduler");
    tokio::time::timeout(Duration::from_secs(5), async {
        while schedulers
            .latest_generation_id(&repository_root)
            .await
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial code-index generation seat");
    let analysis = Arc::new(DaemonNativeIntegrationAnalysisV1::new(
        schedulers,
        analysis_scope,
        tokio::runtime::Handle::current(),
    ));
    let registry = DaemonNativeIntegrationServiceRegistry::default();
    let owner = registry
        .ensure(
            database,
            repository_root,
            project_id,
            identity.repository_id().clone(),
            digest('d'),
            OBSERVED_AT,
            analysis.clone(),
        )
        .await
        .expect("mount native integration owner");
    (registry, owner, analysis)
}

async fn preflight(
    owner: super::registry::DaemonNativeIntegrationOwner,
    request: NativeIntegrationPreflightRequestV1,
) -> tracedecay_domain::NativeIntegrationPreviewV1 {
    let signal = CancellationSignal::active("cancel.native.journey.preflight").expect("signal");
    let outcome = tokio::task::spawn_blocking(move || owner.service().preflight(request, &signal))
        .await
        .expect("preflight join")
        .expect("preflight result");
    let NativeIntegrationPreflightOutcomeV1::Preview(preview) = outcome else {
        panic!("fixture must produce an eligible native preview: {outcome:?}");
    };
    preview.as_ref().clone()
}

async fn stack_snapshot(
    owner: super::registry::DaemonNativeIntegrationOwner,
    request: NativeIntegrationStackResolutionRequestV1,
) -> tracedecay_domain::NativeIntegrationSelectionV1 {
    let signal = CancellationSignal::active("cancel.native.journey.snapshot").expect("signal");
    let outcome = tokio::task::spawn_blocking(move || owner.stack_snapshot(request, &signal))
        .await
        .expect("stack snapshot join")
        .expect("stack snapshot result");
    let NativeIntegrationStackResolutionOutcomeV1::Complete(selection) = outcome else {
        panic!("fixture must freeze the exact independent pair: {outcome:?}");
    };
    selection.as_ref().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn checked_out_declared_stack_conflict_enqueues_without_moving_refs() {
    let directory = tempfile::tempdir().expect("temporary project directory");
    let repository_root = directory.path().join("repo");
    let source_root = directory.path().join("dependency");
    std::fs::create_dir_all(&repository_root).expect("repository root");
    prepare_checked_out_conflict(&repository_root, &source_root);

    let project_id = ProjectId::new("project.native.journey").expect("project id");
    let repository_id = RepositoryId::new("repository.native.journey").expect("repository id");
    let source_scope = ResolvedScope::new(
        project_id.clone(),
        repository_id.clone(),
        WorktreeId::new("worktree.native.dependency").expect("source worktree id"),
        Some(RefId::new("refs/heads/dependency").expect("source ref")),
    )
    .expect("source scope");
    let destination_scope = ResolvedScope::new(
        project_id.clone(),
        repository_id.clone(),
        WorktreeId::new("worktree.native.dependent").expect("destination worktree id"),
        Some(RefId::new("refs/heads/dependent").expect("destination ref")),
    )
    .expect("destination scope");
    let source_tip = git(&repository_root, &["rev-parse", "refs/heads/dependency"]);
    let destination_tip = git(&repository_root, &["rev-parse", "refs/heads/dependent"]);
    let source_node_id = StackNodeId::new("node.native.dependency").expect("source node");
    let destination_node_id = StackNodeId::new("node.native.dependent").expect("destination node");
    let inventory_snapshot_id =
        WorktreeInventorySnapshotId::new("inventory.native.declared-conflict")
            .expect("inventory snapshot");
    let inventory_epoch = WorktreeInventoryEpoch::new(1).expect("inventory epoch");
    let revision = BranchStackRevisionV1::new(
        BranchStackId::new("stack.native.declared-conflict").expect("stack id"),
        BranchStackRevisionId::new("revision.native.declared-conflict").expect("revision id"),
        inventory_snapshot_id.clone(),
        inventory_epoch,
        BranchStackSourceV1::ExplicitDeclaration,
        vec![
            BranchStackNodeV1 {
                node_id: source_node_id.clone(),
                project_id: project_id.clone(),
                repository_id: repository_id.clone(),
                reference: source_scope.reference.clone().expect("source reference"),
                tip: CommitId::new(source_tip.clone()).expect("source tip"),
                worktree_id: Some(source_scope.worktree_id.clone()),
            },
            BranchStackNodeV1 {
                node_id: destination_node_id.clone(),
                project_id: project_id.clone(),
                repository_id: repository_id.clone(),
                reference: destination_scope
                    .reference
                    .clone()
                    .expect("destination reference"),
                tip: CommitId::new(destination_tip.clone()).expect("destination tip"),
                worktree_id: Some(destination_scope.worktree_id.clone()),
            },
        ],
        vec![BranchStackEdgeV1 {
            dependency: source_node_id.clone(),
            dependent: destination_node_id.clone(),
        }],
    )
    .expect("declared revision");
    let selection = NativeIntegrationSelectionV1::DeclaredStackEdge(
        FrozenBranchStackSnapshotV1::new(
            revision.clone(),
            source_node_id.clone(),
            destination_node_id.clone(),
            NativeIntegrationDirectionV1::PropagateDependencyToDependent,
            OBSERVED_AT,
        )
        .expect("frozen declared selection"),
    );
    let request = NativeIntegrationPreflightRequestV1 {
        context: context(
            destination_scope.clone(),
            "request.native.journey.declared-conflict",
        ),
        topology: NativeIntegrationStackResolutionRequestV1 {
            source: source_scope.clone(),
            destination: destination_scope.clone(),
            authorized_scope_set: authorized_scope_set(
                source_scope,
                destination_scope.clone(),
                "request.native.journey.declared-conflict",
            ),
            inventory_snapshot_id,
            inventory_epoch,
            selection: NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
                stack_id: revision.stack_id.clone(),
                revision_id: revision.revision_id.clone(),
                revision_digest: revision.digest.clone(),
                declared_revision: Box::new(revision),
                source_node_id,
                destination_node_id,
                direction: NativeIntegrationDirectionV1::PropagateDependencyToDependent,
            },
            grant_digest: digest('a'),
            policy_digest: digest('d'),
            observed_at: OBSERVED_AT,
        },
        preview_id: NativeIntegrationPreviewId::new("preview.native.journey.declared-conflict")
            .expect("preview id"),
        preferred_mode: Some(MechanicalIntegrationModeV1::TwoParentMerge),
        preview_expires_at: EXPIRES_AT,
        observed_at: OBSERVED_AT,
    };
    let adapter = GixNativeIntegrationAdapter::open(
        project_id.clone(),
        repository_id.clone(),
        &repository_root,
        Arc::new(UnexpectedAnalysis),
    )
    .expect("native adapter");
    let preview = adapter
        .preflight(
            &selection,
            &request,
            &CancellationSignal::active("cancel.native.journey.declared-conflict")
                .expect("cancellation signal"),
            &CancellationToken::for_application_request("declared-conflict"),
        )
        .expect("native conflict preview");
    assert!(matches!(
        preview.disposition,
        NativeIntegrationPreviewDispositionV1::NativeConflict { .. }
    ));
    assert_eq!(
        preview.repository_snapshot.destination_worktree_id,
        Some(destination_scope.worktree_id.clone())
    );

    let signal = signal_from_preflight(&destination_scope, &preview)
        .expect("stack signal")
        .expect("actual conflict signal");
    assert_eq!(signal.kind, StackSignalKindV1::ActualConflict);

    let runtime = HostAdmissionTestRuntimeV1::project(
        directory.path().join("profile"),
        &repository_root,
        project_id.clone(),
    )
    .await
    .expect("canonical project test runtime");
    let database = runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .expect("registered project database");
    let (registry, owner, _analysis) = mount(database.clone(), repository_root.clone()).await;
    let now = UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("wall clock")
                .as_micros(),
        )
        .expect("wall clock micros"),
    );
    let expires_at = UtcMicros(now.0.saturating_add(60_000_000));
    let (preflight_capability, preflight_use_case) =
        operation_authority(tracedecay_contracts::NATIVE_INTEGRATION_PREFLIGHT_OPERATION);
    let expand_operation =
        tracedecay_contracts::git::git_surface_operation(GITHUB_STACK_SIGNAL_EXPAND_OPERATION)
            .expect("canonical Git operation")
            .expect("declared Git operation");
    let expand_capability = expand_operation.capability_id().clone();
    let expand_use_case = expand_operation.use_case_id().clone();
    let live_grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.native.declared-conflict").expect("grant id"),
        1,
        digest('a'),
        ActorId::new("actor.native.issuer").expect("issuer"),
        now,
        expires_at,
        destination_scope.clone(),
        BTreeSet::from([preflight_capability.clone(), expand_capability.clone()]),
        BTreeSet::from([preflight_use_case, expand_use_case]),
        DisclosureClass::Sensitive,
    )
    .expect("live grant");
    let requester = ActorId::new("actor.native.requester").expect("requester");
    let live_context = RequestContext::new(
        requester.clone(),
        destination_scope.clone(),
        live_grant.clone(),
        RequestId::new("request.native.journey.declared-conflict.live").expect("request id"),
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.native.journey.declared-conflict.live")
            .expect("cancellation"),
    )
    .expect("live context");
    let access = ProjectSourceAccessSnapshot {
        scope: destination_scope.clone(),
        requester: requester.clone(),
        binding: ScopeSourceBinding::new(
            SourceBindingId::new("binding.native.declared-conflict").expect("binding id"),
            SourceKindV1::GitHub,
            LocatorDigest::new(format!("sha256:{}", "3".repeat(64))).expect("locator digest"),
            AuthorityRef::Project(project_id.clone()),
        )
        .expect("source binding"),
        configuration_revision: ConfigurationRevisionId::new(
            "configuration.native.declared-conflict",
        )
        .expect("configuration revision"),
        configuration_digest: digest('4'),
        configuration_provenance_digest: digest('5'),
        effective_capabilities: BTreeSet::from([preflight_capability, expand_capability]),
        grant_expires_at: expires_at,
    };
    owner
        .store()
        .save_preview(preview.clone())
        .expect("save canonical conflict preview");
    let stack_runtime = owner
        .mount_github_stack_runtime(
            database.clone(),
            destination_scope.clone(),
            access,
            Arc::new(DaemonGitHubStackCoordinatorV1::default()),
        )
        .expect("stack runtime");
    let other_recipient = ActorId::new("actor.native.other").expect("other recipient");
    let other_context = RequestContext::new(
        other_recipient.clone(),
        destination_scope.clone(),
        live_grant,
        RequestId::new("request.native.journey.declared-conflict.other").expect("request id"),
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.native.journey.declared-conflict.other")
            .expect("cancellation"),
    )
    .expect("other context");
    let other_signal = StackSignalV1::seal(
        &destination_scope,
        StackSignalDraftV1 {
            stack_revision_id: signal.stack_revision_id.clone(),
            stack_revision_digest: signal.stack_revision_digest.clone(),
            kind: StackSignalKindV1::ActualConflict,
            state_digest: digest('6'),
            github_stack_digest: None,
            observed_at: UtcMicros(signal.observed_at.0 - 1),
        },
    )
    .expect("other recipient signal");
    stack_runtime
        .enqueue_from_preflight(other_signal.clone(), &other_context)
        .expect("enqueue older other-recipient signal");
    stack_runtime
        .enqueue_from_preflight(signal.clone(), &live_context)
        .expect("enqueue actual conflict");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if stack_runtime
                .pending_host_deliveries()
                .is_ok_and(|pending| pending.len() == 2)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("publish stack signals to host");

    let cancellation = CancellationSignal::active("cancel.native.journey.declared-conflict.expand")
        .expect("expand cancellation");
    let denied = stack_runtime.expand(
        GitHubStackSignalExpandSurfaceRequest {
            signal_id: Some(signal.signal_id.clone()),
            expected_watermark_id: Some(signal.watermark_id.clone()),
        }
        .into_application_request(other_context),
        &cancellation,
    );
    assert_eq!(denied, Err(GitHubStackSignalExpandPortError::Concealed));
    let expanded = stack_runtime
        .expand(
            GitHubStackSignalExpandSurfaceRequest {
                signal_id: None,
                expected_watermark_id: Some(signal.watermark_id.clone()),
            }
            .into_application_request(live_context),
            &cancellation,
        )
        .expect("expand oldest authorized signal");
    let GitHubStackSignalExpandSurfaceResultV1::Expanded { evidence } = expanded else {
        panic!("expected expanded stack conflict evidence");
    };
    assert_eq!(evidence.signal_id, signal.signal_id);
    assert_eq!(evidence.kind, StackSignalKindV1::ActualConflict);
    assert_eq!(evidence.stack_revision_id, signal.stack_revision_id);
    let tracedecay_contracts::git::GitHubStackSignalNativeSourceV1::Preflight {
        preview: expanded_preview,
    } = &evidence.native_source
    else {
        panic!("expected preflight-backed conflict evidence");
    };
    assert_eq!(expanded_preview.preview_id, preview.preview_id);
    assert_eq!(
        expanded_preview.source_ref,
        preview.repository_snapshot.source_ref
    );
    assert_eq!(
        expanded_preview.destination_ref,
        preview.repository_snapshot.destination_ref
    );
    assert!(matches!(
        expanded_preview.disposition,
        NativeIntegrationPreviewDispositionV1::NativeConflict { .. }
    ));
    assert_eq!(
        database
            .github_stack_recipient_state(
                project_id.as_str(),
                evidence.signal_id.as_str(),
                requester.as_str(),
            )
            .await
            .expect("recipient state"),
        Some(GitHubStackDeliveryStateV1::Settled)
    );
    assert_eq!(
        database
            .github_stack_recipient_state(
                project_id.as_str(),
                other_signal.signal_id.as_str(),
                other_recipient.as_str(),
            )
            .await
            .expect("other recipient state"),
        Some(GitHubStackDeliveryStateV1::HostPending)
    );
    let stored = database
        .github_stack_signal(project_id.as_str(), signal.signal_id.as_str())
        .await
        .expect("signal lookup")
        .expect("durable actual conflict");
    let stored_signal: StackSignalV1 =
        serde_json::from_str(&stored.signal_json).expect("stored signal");
    assert_eq!(stored_signal.kind, StackSignalKindV1::ActualConflict);
    assert_eq!(
        git(&repository_root, &["rev-parse", "refs/heads/dependency"]),
        source_tip
    );
    assert_eq!(
        git(&repository_root, &["rev-parse", "refs/heads/dependent"]),
        destination_tip
    );

    drop(stack_runtime);
    registry.shutdown().await.expect("shutdown owner registry");
}

#[tokio::test(flavor = "multi_thread")]
async fn independent_pair_applies_supported_modes_and_survives_daemon_restart() {
    for (index, mode) in [
        MechanicalIntegrationModeV1::FastForward,
        MechanicalIntegrationModeV1::TwoParentMerge,
        MechanicalIntegrationModeV1::CherryPickExactCommits,
    ]
    .into_iter()
    .enumerate()
    {
        let directory = tempfile::tempdir().expect("temporary project directory");
        let repository_root = directory.path().join("repo");
        std::fs::create_dir_all(&repository_root).expect("repository root");
        prepare_pair(&repository_root, mode);
        let project_id = ProjectId::new("project.native.journey").expect("project id");
        let runtime = HostAdmissionTestRuntimeV1::project(
            directory.path().join("profile"),
            &repository_root,
            project_id,
        )
        .await
        .expect("canonical project test runtime");
        let database = runtime
            .registered_database_lease(HostAdmissionScope::Project)
            .expect("registered project database");

        let (registry, owner, analysis) = mount(database.clone(), repository_root.clone()).await;
        let request_id = format!("request.native.journey.{index}");
        let request = if index == 0 {
            declared_preflight_request(&repository_root, &database, mode, &request_id)
        } else {
            preflight_request(&repository_root, mode, &request_id)
        };
        let context = request.context.clone();
        let destination_scope = request.context.scope().clone();
        let (frozen_selection, preview) = if index == 0 {
            request.validate().expect("declared preflight request");
            let NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
                declared_revision,
                source_node_id,
                destination_node_id,
                direction,
                ..
            } = &request.topology.selection
            else {
                panic!("expected declared stack binding");
            };
            let selection = NativeIntegrationSelectionV1::DeclaredStackEdge(
                FrozenBranchStackSnapshotV1::new(
                    declared_revision.as_ref().clone(),
                    source_node_id.clone(),
                    destination_node_id.clone(),
                    *direction,
                    OBSERVED_AT,
                )
                .expect("declared selection"),
            );
            let adapter = GixNativeIntegrationAdapter::open(
                destination_scope.project_id.clone(),
                destination_scope.repository_id.clone(),
                &repository_root,
                analysis.clone(),
            )
            .expect("native adapter");
            let preflight_selection = selection.clone();
            let preflight_request = request.clone();
            let preview = tokio::task::spawn_blocking(move || {
                adapter.preflight(
                    &preflight_selection,
                    &preflight_request,
                    &CancellationSignal::active("cancel.native.journey.declared-terminal")
                        .expect("cancellation signal"),
                    &CancellationToken::for_application_request("declared-terminal"),
                )
            })
            .await
            .expect("declared preflight join")
            .expect("declared eligible preview");
            (selection, preview)
        } else {
            let selection = stack_snapshot(owner.clone(), request.topology.clone()).await;
            let preview = preflight(owner.clone(), request).await;
            (selection, preview)
        };
        let preview_for_signal = preview.clone();
        assert_eq!(preview.selection, frozen_selection);
        assert_eq!(
            preview.disposition,
            tracedecay_domain::NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
                mode
            ),
            "ordinary independent Rust changes must have complete analysis: {:#?}",
            preview.analysis
        );
        assert!(
            preview
                .repository_snapshot
                .destination_worktree_id
                .is_none()
        );
        let candidate_tree = preview
            .candidate_tree
            .clone()
            .expect("eligible candidate tree");
        let source_tip = preview.repository_snapshot.source_tip.clone();
        let destination_tip = preview.repository_snapshot.destination_tip.clone();
        let ordered_commit_count = preview.ordered_commits.len();
        if index == 0 {
            let late_destination = directory.path().join("late-destination");
            git(
                &repository_root,
                &[
                    "worktree",
                    "add",
                    late_destination.to_str().expect("linked worktree path"),
                    "destination",
                ],
            );
            let linked_authority =
                tracedecay_runtime_core::git_repository::GitRepositoryAuthority::discover(
                    &late_destination,
                )
                .expect("linked worktree authority");
            assert!(
                linked_authority
                    .reference_is_checked_out("refs/heads/main")
                    .expect("complete worktree inventory"),
                "a linked authority must retain the primary checkout"
            );
            let adapter = GixNativeIntegrationAdapter::open(
                destination_scope.project_id.clone(),
                destination_scope.repository_id.clone(),
                &repository_root,
                analysis.clone(),
            )
            .expect("native adapter");
            let refusal_preview = preview.clone();
            let refused = tokio::task::spawn_blocking(move || {
                adapter.apply(
                    &refusal_preview,
                    &CancellationToken::for_application_request("late-destination-refusal"),
                )
            })
            .await
            .expect("linked checkout refusal join");
            assert_eq!(
                refused,
                Ok(NativeApplyEffectV1::FailedNoChange),
                "a newly checked-out destination must terminate without changing Git state"
            );
            assert_eq!(
                git(&repository_root, &["rev-parse", "refs/heads/destination"]),
                destination_tip.as_str()
            );
            git(
                &repository_root,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    late_destination.to_str().expect("linked worktree path"),
                ],
            );
        }
        let approval = approval_for(&context, &preview, &request_id);
        owner
            .store()
            .save_approval(approval.clone())
            .expect("durably issue approval");
        let transaction_id =
            NativeIntegrationTransactionId::new(format!("transaction.native.journey.{index}"))
                .expect("transaction id");
        let status_transaction_id = transaction_id.clone();
        let apply_owner = owner.clone();
        let apply_signal =
            CancellationSignal::active("cancel.native.journey.apply").expect("signal");
        let receipt = tokio::task::spawn_blocking(move || {
            apply_owner.service().apply(
                NativeIntegrationApplyRequestV1 {
                    context,
                    transaction_id: transaction_id.clone(),
                    preview,
                    approval,
                    observed_at: OBSERVED_AT,
                },
                &apply_signal,
            )
        })
        .await
        .expect("apply join")
        .expect("apply result");
        assert_eq!(
            receipt.status.terminal_outcome,
            Some(NativeIntegrationTerminalOutcomeV1::Committed)
        );
        assert_eq!(receipt.final_tree, candidate_tree);
        assert_eq!(
            git(&repository_root, &["rev-parse", "refs/heads/destination"]),
            receipt.final_ref_tip.as_str()
        );
        match mode {
            MechanicalIntegrationModeV1::FastForward => {
                assert_eq!(receipt.final_ref_tip, source_tip);
            }
            MechanicalIntegrationModeV1::TwoParentMerge => {
                let parent_line = git(
                    &repository_root,
                    &["rev-list", "--parents", "-n", "1", "refs/heads/destination"],
                );
                let parents = parent_line.split_whitespace().collect::<Vec<_>>();
                assert_eq!(parents.len(), 3, "merge result must have two parents");
                assert_eq!(parents[1], destination_tip.as_str());
                assert_eq!(parents[2], source_tip.as_str());
            }
            MechanicalIntegrationModeV1::CherryPickExactCommits => {
                let revision_range =
                    format!("{}..refs/heads/destination", destination_tip.as_str());
                let materialized_count = git(
                    &repository_root,
                    &["rev-list", "--count", revision_range.as_str()],
                )
                .parse::<usize>()
                .expect("cherry-pick result count");
                assert_eq!(materialized_count, ordered_commit_count);
                assert_ne!(receipt.final_ref_tip, source_tip);
            }
        }

        if index == 0 {
            let stack_signal = super::stack_signals::signal_from_receipt(
                &destination_scope,
                &preview_for_signal,
                &receipt,
            )
            .expect("terminal stack signal")
            .expect("committed stack signal");
            let now = UtcMicros(
                i64::try_from(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("wall clock")
                        .as_micros(),
                )
                .expect("wall clock micros"),
            );
            let expires_at = UtcMicros(now.0.saturating_add(60_000_000));
            let expand_operation = tracedecay_contracts::git::git_surface_operation(
                GITHUB_STACK_SIGNAL_EXPAND_OPERATION,
            )
            .expect("canonical Git operation")
            .expect("declared Git operation");
            let requester = ActorId::new("actor.native.terminal").expect("requester");
            let grant = CapabilityGrantSnapshot::new(
                CapabilityGrantId::new("grant.native.terminal").expect("grant id"),
                1,
                digest('7'),
                ActorId::new("actor.native.issuer").expect("issuer"),
                now,
                expires_at,
                destination_scope.clone(),
                BTreeSet::from([expand_operation.capability_id().clone()]),
                BTreeSet::from([expand_operation.use_case_id().clone()]),
                DisclosureClass::Sensitive,
            )
            .expect("grant");
            let live_context = RequestContext::new(
                requester.clone(),
                destination_scope.clone(),
                grant,
                RequestId::new("request.native.journey.terminal-expand").expect("request id"),
                Deadline::new(expires_at).expect("deadline"),
                CancellationContext::active("cancel.native.journey.terminal-expand")
                    .expect("cancellation"),
            )
            .expect("live context");
            let access = ProjectSourceAccessSnapshot {
                scope: destination_scope.clone(),
                requester: requester.clone(),
                binding: ScopeSourceBinding::new(
                    SourceBindingId::new("binding.native.terminal").expect("binding id"),
                    SourceKindV1::GitHub,
                    LocatorDigest::new(format!("sha256:{}", "8".repeat(64)))
                        .expect("locator digest"),
                    AuthorityRef::Project(destination_scope.project_id.clone()),
                )
                .expect("source binding"),
                configuration_revision: ConfigurationRevisionId::new(
                    "configuration.native.terminal",
                )
                .expect("configuration revision"),
                configuration_digest: digest('9'),
                configuration_provenance_digest: digest('a'),
                effective_capabilities: BTreeSet::from([expand_operation.capability_id().clone()]),
                grant_expires_at: expires_at,
            };
            let stack_runtime = owner
                .mount_github_stack_runtime(
                    database.clone(),
                    destination_scope.clone(),
                    access,
                    Arc::new(DaemonGitHubStackCoordinatorV1::default()),
                )
                .expect("stack runtime");
            stack_runtime
                .enqueue_from_preflight(stack_signal.clone(), &live_context)
                .expect("enqueue terminal signal");
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if stack_runtime
                        .pending_host_deliveries()
                        .is_ok_and(|pending| pending.len() == 1)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("publish terminal signal");
            let expanded = stack_runtime
                .expand(
                    GitHubStackSignalExpandSurfaceRequest {
                        signal_id: None,
                        expected_watermark_id: Some(stack_signal.watermark_id),
                    }
                    .into_application_request(live_context),
                    &CancellationSignal::active("cancel.native.journey.terminal-consumer")
                        .expect("cancellation"),
                )
                .expect("expand terminal signal");
            let GitHubStackSignalExpandSurfaceResultV1::Expanded { evidence } = expanded else {
                panic!("expected terminal stack evidence");
            };
            let tracedecay_contracts::git::GitHubStackSignalNativeSourceV1::Terminal {
                preview,
                terminal,
            } = evidence.native_source
            else {
                panic!("expected receipt-backed terminal evidence");
            };
            assert_eq!(evidence.kind, StackSignalKindV1::IntegrationCommitted);
            assert_eq!(preview.preview_id, preview_for_signal.preview_id);
            assert_eq!(terminal.receipt_digest, receipt.receipt_digest);
            assert_eq!(terminal.final_ref_tip, receipt.final_ref_tip);
            assert_eq!(terminal.completed_at, receipt.completed_at);
            assert_eq!(
                terminal.outcome,
                NativeIntegrationTerminalOutcomeV1::Committed
            );
            assert_eq!(
                database
                    .github_stack_recipient_state(
                        destination_scope.project_id.as_str(),
                        evidence.signal_id.as_str(),
                        requester.as_str(),
                    )
                    .await
                    .expect("terminal recipient state"),
                Some(GitHubStackDeliveryStateV1::Settled)
            );
            drop(stack_runtime);
        }

        registry.shutdown().await.expect("shutdown owner registry");
        let (restarted_registry, restarted_owner, _analysis) =
            mount(database, repository_root).await;
        let durable = tokio::task::spawn_blocking(move || {
            restarted_owner.service().status(
                tracedecay_contracts::NativeIntegrationStatusRequestV1 {
                    transaction_id: status_transaction_id,
                },
            )
        })
        .await
        .expect("status join")
        .expect("durable status")
        .expect("durable transaction status");
        assert_eq!(
            durable.terminal_outcome,
            Some(NativeIntegrationTerminalOutcomeV1::Committed)
        );
        restarted_registry
            .shutdown()
            .await
            .expect("shutdown restarted owner registry");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn generated_only_change_requires_semantic_review() {
    let directory = tempfile::tempdir().expect("temporary project directory");
    let repository_root = directory.path().join("repo");
    std::fs::create_dir_all(&repository_root).expect("repository root");
    prepare_generated_only_pair(&repository_root);
    let runtime = HostAdmissionTestRuntimeV1::project(
        directory.path().join("profile"),
        &repository_root,
        ProjectId::new("project.native.journey").expect("project id"),
    )
    .await
    .expect("canonical project test runtime");
    let database = runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .expect("registered project database");
    let (registry, owner, _analysis) = mount(database, repository_root.clone()).await;

    let preview = preflight(
        owner,
        preflight_request(
            &repository_root,
            MechanicalIntegrationModeV1::FastForward,
            "request.native.journey.generated-only",
        ),
    )
    .await;

    assert!(matches!(
        preview.disposition,
        NativeIntegrationPreviewDispositionV1::SemanticReviewRequired { .. }
    ));
    let analysis = preview.analysis.expect("semantic analysis");
    assert_eq!(
        analysis.graph.coverage,
        NativeIntegrationAnalysisCoverageV1::Partial
    );
    assert_eq!(
        analysis.graph.gaps,
        vec![NativeIntegrationAnalysisGapV1::WithheldSource]
    );
    registry.shutdown().await.expect("shutdown owner registry");
}

#[tokio::test(flavor = "multi_thread")]
async fn foreign_destination_ref_drift_terminates_without_mutating_the_foreign_tip() {
    let directory = tempfile::tempdir().expect("temporary project directory");
    let repository_root = directory.path().join("repo");
    std::fs::create_dir_all(&repository_root).expect("repository root");
    prepare_pair(&repository_root, MechanicalIntegrationModeV1::FastForward);
    let project_id = ProjectId::new("project.native.journey").expect("project id");
    let runtime = HostAdmissionTestRuntimeV1::project(
        directory.path().join("profile"),
        &repository_root,
        project_id,
    )
    .await
    .expect("canonical project test runtime");
    let database = runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .expect("registered project database");
    let (registry, owner, _analysis) = mount(database, repository_root.clone()).await;
    let request = preflight_request(
        &repository_root,
        MechanicalIntegrationModeV1::FastForward,
        "request.native.journey.drift",
    );
    let context = request.context.clone();
    let preview = preflight(owner.clone(), request).await;
    let approval = approval_for(&context, &preview, "request.native.journey.drift");
    owner
        .store()
        .save_approval(approval.clone())
        .expect("durably issue approval");

    git(&repository_root, &["checkout", "-b", "foreign"]);
    write_and_commit(&repository_root, "foreign.txt", "foreign\n", "foreign");
    let foreign_tip = git(&repository_root, &["rev-parse", "HEAD"]);
    git(&repository_root, &["checkout", "main"]);
    git(
        &repository_root,
        &["update-ref", "refs/heads/destination", foreign_tip.as_str()],
    );

    let apply_owner = owner.clone();
    let signal = CancellationSignal::active("cancel.native.journey.drift").expect("signal");
    let receipt = tokio::task::spawn_blocking(move || {
        apply_owner.service().apply(
            NativeIntegrationApplyRequestV1 {
                context,
                transaction_id: NativeIntegrationTransactionId::new(
                    "transaction.native.journey.drift",
                )
                .expect("transaction id"),
                preview,
                approval,
                observed_at: OBSERVED_AT,
            },
            &signal,
        )
    })
    .await
    .expect("apply join")
    .expect("foreign drift must terminate without inspection quarantine");
    assert_eq!(
        receipt.status.terminal_outcome,
        Some(NativeIntegrationTerminalOutcomeV1::AbortedNoChange)
    );
    assert_eq!(receipt.final_ref_tip.as_str(), foreign_tip);
    assert_eq!(
        git(&repository_root, &["rev-parse", "refs/heads/destination"]),
        foreign_tip
    );
    // The known pre-commit drift did not poison the repository: a freshly
    // frozen selection can produce a new, mechanically eligible preview.
    let fresh_preview = preflight(
        owner,
        preflight_request(
            &repository_root,
            MechanicalIntegrationModeV1::TwoParentMerge,
            "request.native.journey.after-drift",
        ),
    )
    .await;
    assert_eq!(
        fresh_preview.disposition,
        tracedecay_domain::NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(
            MechanicalIntegrationModeV1::TwoParentMerge
        )
    );
    registry.shutdown().await.expect("shutdown owner registry");
}
