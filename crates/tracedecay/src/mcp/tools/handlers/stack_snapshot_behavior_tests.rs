//! Behavior of `tracedecay_stack_snapshot` through the MCP tool dispatcher.
//!
//! The call is the production handler path: argument adaptation, daemon
//! invocation, the project-open native-integration owner, and the enrolled
//! repository. Expected values are the refs, epoch, and typed outcomes a
//! caller observes, not schema text or a digest recomputed by the subject.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracedecay_agent_hosts::native_integration::{
    DaemonNativeIntegrationOwner, DaemonNativeIntegrationServiceRegistry, NativeIntegrationTargetV1,
};
use tracedecay_code_index_runtime::code_index_scheduler::identity::IndexingIdentityV1;
use tracedecay_code_index_runtime::resolved_scope_for_project;
use tracedecay_contracts::{
    AuthorizedScopeSet, AuthorizedScopeSetAuthority, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, NativeIntegrationSelectionDeclarationV1,
    NativeIntegrationStackSnapshotSurfaceRequest, RequestContext, RequestId, ResolvedScope,
    native_integration_surface_operation,
};
use tracedecay_daemon_service::{DaemonConfigurationRuntimeRegistrar, DaemonInvocationService};
use tracedecay_domain::{
    ActorId, CapabilityId, ManifestDigest, ProjectId, RefId, RepositoryId, ScopeSetId,
    ScopeSetRevision, UseCaseId, UtcMicros, WorktreeId, WorktreeInventoryEpoch,
    WorktreeInventorySnapshotId,
};
use tracedecay_runtime_core::config::PinnedUserDataDir;
use tracedecay_runtime_core::git::try_git_program;
use tracedecay_sessions::admission::HostAdmissionScope;

use super::{ToolCallRegistryOptions, handle_tool_call_with_registry_options};
use crate::project::TraceDecay;

const PROJECT_ID: &str = "project.stack-snapshot.proof";
const SOURCE_REF: &str = "refs/heads/source";
const DESTINATION_REF: &str = "refs/heads/destination";
const INVENTORY_SNAPSHOT_ID: &str = "inventory.snapshot.proof";
const INVENTORY_EPOCH: u64 = 7;
const PROPOSAL_DIGEST_BYTE: char = 'c';

struct IdleAnalysis;

impl tracedecay_application::native_integration::NativeIntegrationAnalysisPort for IdleAnalysis {
    fn analyze(
        &self,
        _selection: &tracedecay_domain::NativeIntegrationSelectionV1,
        _native: &tracedecay_runtime_core::git_repository::GitNativePreflight,
        _candidate: &tracedecay_runtime_core::git_repository::GitNativeCandidateTreeV1<'_>,
        _deadline: &tracedecay_contracts::Deadline,
        _cancellation_signal: &tracedecay_contracts::CancellationSignal,
        _cancellation: &tracedecay_runtime_core::cancellation::CancellationToken,
    ) -> Result<
        tracedecay_domain::NativeIntegrationAnalysisReportV1,
        tracedecay_contracts::NativeIntegrationPortError,
    > {
        Err(tracedecay_contracts::NativeIntegrationPortError::Unavailable)
    }

    fn revalidate(
        &self,
        _report: &tracedecay_domain::NativeIntegrationAnalysisReportV1,
        _deadline: &tracedecay_contracts::Deadline,
        _cancellation: &tracedecay_contracts::CancellationSignal,
    ) -> Result<
        tracedecay_application::native_integration::NativeIntegrationAnalysisRevalidationV1,
        tracedecay_contracts::NativeIntegrationPortError,
    > {
        Err(tracedecay_contracts::NativeIntegrationPortError::Unavailable)
    }
}

struct MountedStackSnapshotExecutor {
    service: DaemonInvocationService,
    owner: DaemonNativeIntegrationOwner,
    project_root: PathBuf,
    lsp_registry: Arc<Mutex<tracedecay_lsp::LspSessionRegistry>>,
}

impl tracedecay_contracts::ApplicationInvocationExecutor for MountedStackSnapshotExecutor {
    fn invoke(
        &self,
        _invocation: tracedecay_contracts::ApplicationInvocation,
    ) -> tracedecay_contracts::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_contracts::ApplicationResponse,
            tracedecay_contracts::InvocationError,
        >,
    > {
        Box::pin(async { Err(tracedecay_contracts::InvocationError::Unavailable) })
    }
}

impl tracedecay_daemon_protocol::DaemonInvocationExecutor for MountedStackSnapshotExecutor {
    fn invoke_controlled(
        &self,
        request: tracedecay_daemon_protocol::DaemonInvocationRequest,
        deadline: tracedecay_contracts::Deadline,
        cancellation: tracedecay_contracts::CancellationSignal,
        _policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            tracedecay_daemon_protocol::DaemonInvocationResponse,
            tracedecay_daemon_protocol::DaemonInvocationError,
        >,
    > {
        let owner = self.owner.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(
                    tracedecay_daemon_protocol::DaemonInvocationError::Cancelled {
                        stage: tracedecay_contracts::CancellationStage::BeforeAdmission,
                    },
                );
            }
            if tracedecay_daemon_protocol::deadline_remaining(&deadline).is_none() {
                return Err(
                    tracedecay_daemon_protocol::DaemonInvocationError::TimedOut {
                        stage: tracedecay_contracts::CancellationStage::BeforeAdmission,
                    },
                );
            }
            Ok(self
                .service
                .invoke_with_cancellation(
                    &self.lsp_registry,
                    Some(&self.project_root),
                    None,
                    None,
                    Some(owner),
                    request,
                    None,
                )
                .await)
        })
    }

    fn observe_feedback(
        &self,
        _subject_digest: ManifestDigest,
        _observed_at: UtcMicros,
        _event: tracedecay_contracts::feedback::observations::FeedbackSourceEventV1,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        tracedecay_domain::errors::Result<()>,
    > {
        Box::pin(async { Ok(()) })
    }
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
}

fn git(root: &Path, arguments: &[&str]) {
    let status = Command::new(try_git_program().expect("git program"))
        .args(arguments)
        .current_dir(root)
        .status()
        .expect("git command");
    assert!(status.success(), "git {arguments:?} failed");
}

fn prepare_repository(root: &Path) {
    git(root, &["init", "-b", "main"]);
    git(
        root,
        &["config", "user.email", "stack-snapshot@example.com"],
    );
    git(root, &["config", "user.name", "Stack Snapshot"]);
    std::fs::write(root.join("README"), "base\n").expect("readme");
    git(root, &["add", "README"]);
    git(root, &["commit", "-m", "base"]);
    git(root, &["checkout", "-b", "source"]);
    std::fs::write(root.join("source.txt"), "source\n").expect("source file");
    git(root, &["add", "source.txt"]);
    git(root, &["commit", "-m", "source"]);
    git(root, &["checkout", "main"]);
    git(root, &["checkout", "-b", "destination"]);
    std::fs::write(root.join("destination.txt"), "destination\n").expect("destination file");
    git(root, &["add", "destination.txt"]);
    git(root, &["commit", "-m", "destination"]);
}

fn stack_snapshot_capability() -> (CapabilityId, UseCaseId) {
    let operation = native_integration_surface_operation(
        tracedecay_contracts::NATIVE_INTEGRATION_STACK_SNAPSHOT_OPERATION,
    )
    .expect("stack snapshot operation")
    .expect("stack snapshot is declared");
    (
        operation.capability_id().clone(),
        operation.use_case_id().clone(),
    )
}

fn request_context(scope: ResolvedScope, suffix: &str) -> RequestContext {
    let (capability, use_case) = stack_snapshot_capability();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.stack-snapshot.{suffix}")).expect("grant id"),
        1,
        digest('a'),
        ActorId::new("actor.stack-snapshot.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(1_000_000_000),
        scope.clone(),
        std::collections::BTreeSet::from([capability.clone()]),
        std::collections::BTreeSet::from([use_case.clone()]),
        DisclosureClass::Sensitive,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.stack-snapshot.requester").expect("requester"),
        scope,
        grant,
        RequestId::new(format!("request.stack-snapshot.{suffix}")).expect("request id"),
        Deadline::new(UtcMicros(1_000_000_000)).expect("deadline"),
        CancellationContext::active(format!("cancel.stack-snapshot.{suffix}")).expect("cancel"),
    )
    .expect("request context")
}

fn authorized_scope_set(
    id: &str,
    source: ResolvedScope,
    destination: ResolvedScope,
) -> AuthorizedScopeSet {
    let (capability, use_case) = stack_snapshot_capability();
    AuthorizedScopeSetAuthority::authorize(
        ScopeSetId::new(id).expect("scope set id"),
        ScopeSetRevision::new(1).expect("scope set revision"),
        vec![
            request_context(destination, &format!("{id}.destination")),
            request_context(source, &format!("{id}.source")),
        ],
        &capability,
        &use_case,
        UtcMicros(100),
    )
    .expect("authorized scope set")
}

fn scope(
    project: &ProjectId,
    repository: &RepositoryId,
    worktree: WorktreeId,
    reference: &str,
) -> ResolvedScope {
    ResolvedScope::new(
        project.clone(),
        repository.clone(),
        worktree,
        Some(RefId::new(reference).expect("reference")),
    )
    .expect("resolved scope")
}

fn snapshot_arguments(
    source: &ResolvedScope,
    destination: &ResolvedScope,
    scope_set: &AuthorizedScopeSet,
    source_ref: &str,
    destination_ref: &str,
) -> Value {
    let request = NativeIntegrationStackSnapshotSurfaceRequest {
        source: source.clone(),
        destination: destination.clone(),
        authorized_scope_set_id: scope_set.scope_set_id().clone(),
        authorized_scope_set_revision: scope_set.revision(),
        authorized_scope_set_digest: scope_set.digest().clone(),
        inventory_snapshot_id: WorktreeInventorySnapshotId::new(INVENTORY_SNAPSHOT_ID)
            .expect("inventory snapshot"),
        inventory_epoch: WorktreeInventoryEpoch::new(INVENTORY_EPOCH).expect("inventory epoch"),
        selection: NativeIntegrationSelectionDeclarationV1::IndependentBranch {
            proposal_digest: digest(PROPOSAL_DIGEST_BYTE),
            source_ref: RefId::new(source_ref).expect("source ref"),
            destination_ref: RefId::new(destination_ref).expect("destination ref"),
        },
        grant_digest: digest('a'),
        policy_digest: digest('d'),
    };
    let mut arguments = serde_json::to_value(request).expect("snapshot arguments");
    arguments["format"] = json!("json");
    arguments
}

fn persist_scope_set(
    database: &tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    scope_set: &AuthorizedScopeSet,
) {
    let storage = database
        .authorized_scope_set_storage()
        .expect("scope-set storage");
    let persisted = storage
        .compare_and_swap(None, scope_set)
        .expect("persist scope set");
    assert!(
        format!("{persisted:?}").starts_with("Applied"),
        "scope set was not stored: {persisted:?}"
    );
}

fn tool_payload(result: &tracedecay_mcp::ToolResult) -> Value {
    let text = result.value["content"]
        .as_array()
        .and_then(|items| {
            items.iter().find_map(|item| {
                let text = item.get("text")?.as_str()?;
                let start = text.find('{')?;
                Some(text[start..].to_owned())
            })
        })
        .unwrap_or_else(|| panic!("tool result has no JSON content: {}", result.value));
    let envelope: Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("tool result is not JSON: {error}\n{text}"));
    envelope
        .pointer("/outcome/value/payload")
        .cloned()
        .unwrap_or_else(|| panic!("tool result has no evidence payload: {envelope}"))
}

async fn call_stack_snapshot(
    graph: &TraceDecay,
    executor: &MountedStackSnapshotExecutor,
    arguments: Value,
) -> tracedecay_domain::errors::Result<tracedecay_mcp::ToolResult> {
    let mut options = ToolCallRegistryOptions::default().admit_opened_project(graph)?;
    options.application_invocation_executor = Some(executor);
    handle_tool_call_with_registry_options(
        graph,
        "tracedecay_stack_snapshot",
        arguments,
        None,
        None,
        options,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn stack_snapshot_freezes_enrolled_refs_and_refuses_the_other_inputs() {
    let _profile = PinnedUserDataDir::new();
    if tracedecay_code_index::parallelism::installed_worker_status().is_none() {
        tracedecay_code_index::parallelism::install_worker_plan(
            tracedecay_domain::configuration::CodeIndexWorkerSelectionV1::Automatic {},
            8 * 1024 * 1024 * 1024,
        )
        .expect("worker plan");
    }

    let directory = tempfile::tempdir().expect("temporary repository");
    let repository_root = directory.path().join("repo");
    std::fs::create_dir_all(&repository_root).expect("repository directory");
    prepare_repository(&repository_root);
    let repository_root = repository_root
        .canonicalize()
        .expect("canonical repository");

    let (graph, runtime) =
        TraceDecay::init_test_fixture_with_registered_runtime(&repository_root, PROJECT_ID)
            .await
            .expect("registered project");
    let identity = IndexingIdentityV1::resolve(graph.project_root()).expect("indexing identity");
    let project_id = ProjectId::new(PROJECT_ID).expect("project id");
    let repository_id = identity.repository_id().clone();
    let source_scope = scope(
        &project_id,
        &repository_id,
        WorktreeId::new("worktree.stack-snapshot.source").expect("source worktree"),
        SOURCE_REF,
    );
    let destination_scope = scope(
        &project_id,
        &repository_id,
        identity.worktree_id().clone(),
        DESTINATION_REF,
    );
    let enrolled = authorized_scope_set(
        "scope-set.stack-snapshot.proof",
        source_scope.clone(),
        destination_scope.clone(),
    );
    let foreign_project = ProjectId::new("project.stack-snapshot.foreign").expect("foreign");
    let foreign_source = scope(
        &foreign_project,
        &repository_id,
        WorktreeId::new("worktree.stack-snapshot.source").expect("source worktree"),
        SOURCE_REF,
    );
    let foreign_destination = scope(
        &foreign_project,
        &repository_id,
        identity.worktree_id().clone(),
        DESTINATION_REF,
    );
    let foreign = authorized_scope_set(
        "scope-set.stack-snapshot.foreign",
        foreign_source.clone(),
        foreign_destination.clone(),
    );

    let database = runtime
        .registered_database_lease(HostAdmissionScope::Project)
        .expect("project sessions")
        .clone();
    persist_scope_set(&database, &enrolled);
    persist_scope_set(&database, &foreign);

    let policy_digest = digest('d');
    let owner = DaemonNativeIntegrationServiceRegistry::default()
        .ensure(
            database,
            NativeIntegrationTargetV1 {
                repository_root: repository_root.clone(),
                project_id: project_id.clone(),
                repository_id: repository_id.clone(),
                policy_digest: policy_digest.clone(),
            },
            UtcMicros(100),
            Arc::new(IdleAnalysis),
        )
        .await
        .expect("native integration owner");

    let profile_root =
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root");
    let profile_identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
            .expect("profile identity");
    let observed_at = tracedecay_contracts::clock::now_micros();
    let service = DaemonInvocationService::default();
    DaemonConfigurationRuntimeRegistrar::new(&service)
        .register(
            graph.project_root().to_path_buf(),
            Arc::clone(graph.configuration_runtime()),
            resolved_scope_for_project(graph.project_root(), &project_id)
                .expect("configuration scope"),
            profile_identity.profile_id().clone(),
            ActorId::new("actor.stack-snapshot.mcp").expect("configuration actor"),
            UtcMicros(observed_at.0.saturating_add(3_600_000_000)),
            None,
            policy_digest,
        )
        .await
        .expect("configuration runtime");

    let executor = MountedStackSnapshotExecutor {
        service,
        owner,
        project_root: graph.project_root().to_path_buf(),
        lsp_registry: Arc::new(Mutex::new(tracedecay_lsp::LspSessionRegistry::default())),
    };

    let frozen = call_stack_snapshot(
        &graph,
        &executor,
        snapshot_arguments(
            &source_scope,
            &destination_scope,
            &enrolled,
            SOURCE_REF,
            DESTINATION_REF,
        ),
    )
    .await
    .expect("enrolled snapshot call");
    let frozen = tool_payload(&frozen);
    assert_eq!(frozen["outcome"], "stack_snapshot");
    assert_eq!(frozen["selection"]["project_id"], PROJECT_ID);
    assert_eq!(
        frozen["selection"]["repository_id"],
        identity.repository_id().as_str()
    );
    assert_eq!(frozen["selection"]["source_ref"], SOURCE_REF);
    assert_eq!(frozen["selection"]["destination_ref"], DESTINATION_REF);
    assert_eq!(frozen["selection"]["inventory_epoch"], INVENTORY_EPOCH);
    assert_eq!(
        frozen["sealed_snapshot"]["selection"]["kind"],
        "independent_branch"
    );
    assert_eq!(
        frozen["sealed_snapshot"]["selection"]["binding"]["source_ref"],
        SOURCE_REF
    );
    assert_eq!(
        frozen["sealed_snapshot"]["selection"]["binding"]["destination_ref"],
        DESTINATION_REF
    );
    assert_eq!(
        frozen["sealed_snapshot"]["selection"]["binding"]["proposal_digest"],
        format!("sha256:{}", PROPOSAL_DIGEST_BYTE.to_string().repeat(64))
    );
    assert_eq!(
        frozen["sealed_snapshot"]["inventory_snapshot_id"],
        INVENTORY_SNAPSHOT_ID
    );
    assert_eq!(
        frozen["sealed_snapshot"]["inventory_epoch"],
        INVENTORY_EPOCH
    );

    let mut missing_ref = snapshot_arguments(
        &source_scope,
        &destination_scope,
        &enrolled,
        SOURCE_REF,
        DESTINATION_REF,
    );
    missing_ref["selection"]["binding"]["source_ref"] = json!("refs/heads/absent");
    let missing = call_stack_snapshot(&graph, &executor, missing_ref)
        .await
        .expect("missing ref call");
    let missing = tool_payload(&missing);
    assert_eq!(missing["outcome"], "unavailable");
    assert_eq!(missing["reason"], "partial");

    let foreign_result = call_stack_snapshot(
        &graph,
        &executor,
        snapshot_arguments(
            &foreign_source,
            &foreign_destination,
            &foreign,
            SOURCE_REF,
            DESTINATION_REF,
        ),
    )
    .await
    .expect("foreign project call");
    let foreign_result = tool_payload(&foreign_result);
    assert_eq!(foreign_result["outcome"], "unavailable");
    assert_eq!(foreign_result["reason"], "denied");

    let mut path_bearing = snapshot_arguments(
        &source_scope,
        &destination_scope,
        &enrolled,
        SOURCE_REF,
        DESTINATION_REF,
    );
    path_bearing["repository_path"] = json!("/tmp/not-a-repository");
    let rejected = call_stack_snapshot(&graph, &executor, path_bearing)
        .await
        .expect_err("a path field must be rejected before a snapshot is minted");
    let rejected = rejected.to_string();
    assert!(
        rejected.contains("application_surface_invalid_request"),
        "{rejected}"
    );
    assert!(
        !rejected.contains("stack_snapshot"),
        "rejection must not look like a frozen snapshot: {rejected}"
    );
}
