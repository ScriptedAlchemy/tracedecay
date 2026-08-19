//! Direct coverage for the live provider process path: real spawned
//! executables, bounded stream capture, the cancellation ladder, and the
//! typed availability states that refuse to spawn at all.
//!
//! Framing note: the `(backend, protocol)` pair selects argv and the single
//! provider-owned session-start event whose identity is sealed into Work —
//! `--print --output-format stream-json --verbose` for `ClaudeStreamJson`,
//! `exec --json -` for `CodexExecJson` — the attempt instructions are written
//! to the child's stdin, and both streams are captured as bounded opaque bytes
//! summarized by true byte length plus the sha256 of the retained prefix. No
//! assistant content is parsed or reinterpreted.
//!
//! Gate note: the app-server preference gate is exercised against a *real*
//! `PinnedWorkExecutableBindingResolver` over real on-disk executables, not a
//! stub. That is deliberate — the gate's availability detection is exactly the
//! binding probe (capability admission, path canonicalization, byte-exact
//! digest match), so a stubbed resolver would prove nothing about whether the
//! detection is truthful.

use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationValueV1, SettingKey, TopologyConcurrencyPolicyV1,
    WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1, WorkExecutableCapabilityV1,
};

use crate::config::registry::ConfigurationRegistry;
use crate::config::resolver::{ConfigurationLayerV1, resolve_configuration};
use crate::config::{PinnedRuntimeConfiguration, RuntimeConfigurationTarget};

use tracedecay_application::{
    CancelWorkAttemptCommand, CancellationContext, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
    RequestId, ResolvedScope, WorkAttemptAdmissionKind, WorkAttemptCapacityV1,
    WorkAttemptCapacityVerdictV1, WorkAttemptInsertOutcome, WorkAttemptListPageV1,
    WorkAttemptService, WorkAttemptStatusRequestV1, WorkAttemptStorageError,
    WorkAttemptStoragePort, WorkAttemptStreamChannelV1, WorkAttemptStreamSummaryV1,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ConfigurationSnapshotId,
    EffectReconciliationOutcomeV1, ManifestDigest, NoProgressEscalationV1, ObservabilityPayloadV1,
    ObservabilityTerminalResultV1, OperationActivationOutcomeV1, OperationStageV1, ProjectId,
    ProposalId, ProviderId, RefId, RepositoryId, RunId, SessionId, TaskId, UtcMicros,
    WorkApprovalPolicy, WorkAttemptIdentityV1, WorkAttemptProjectionBindingV1, WorkAttemptStateV1,
    WorkAttemptV1, WorkAuthority, WorkCancellationStateV1, WorkEffectStateV1, WorkEgressPolicy,
    WorkExecutableReference, WorkExecutionEnvelopeV1, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1, WorkFilesystemPolicy,
    WorkGraphVersionV1, WorkLeaseFenceV1, WorkLeaseId, WorkProductEventSequenceV1,
    WorkProductSourceWatermarkV1, WorkProviderRouteId, WorkProviderRouteV1, WorkRecoveryStateV1,
    WorkSandboxPolicy, WorkflowOperationRef, WorkflowStageClassV1, WorktreeId, canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::observability::{
    ObservabilityProducerIdentityV1, RegisteredObservabilityPortV1,
};

/// argv the module maps onto each admitted `(backend, protocol)` pair. These
/// literals live in `provider_arguments`; the spawn tests assert the child
/// actually observed them.
const CLAUDE_STREAM_JSON_ARGV: [&str; 4] =
    ["--print", "--output-format", "stream-json", "--verbose"];
const CODEX_EXEC_JSON_ARGV: [&str; 3] = ["exec", "--json", "-"];

// ---------------------------------------------------------------------------
// In-memory attempt authority
// ---------------------------------------------------------------------------

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

fn sha256_digest(bytes: &[u8]) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(bytes)))).unwrap()
}

type AttemptKey = (WorkAuthority, String);

fn attempt_key(authority: &WorkAuthority, identity: &WorkAttemptIdentityV1) -> AttemptKey {
    (
        authority.clone(),
        WorkAttemptProcessRegistryV1::key(None, identity),
    )
}

#[derive(Default)]
struct AttemptRows {
    fences: BTreeMap<WorkAuthority, u64>,
    rows: BTreeMap<AttemptKey, String>,
    evidence: BTreeMap<AttemptKey, String>,
    /// Every state this attempt was durably moved through, in order. The
    /// cancellation ladder is only truthful if the escalation rung shows up
    /// here, so the tests read the ladder from the durable trail rather than
    /// from the terminal row alone.
    observed_states: Vec<WorkAttemptStateV1>,
}

/// In-memory attempt rows with the same fenced compare-and-swap semantics as
/// the registered `SQLite` store.
#[derive(Clone, Default)]
struct AttemptStore {
    inner: Arc<Mutex<AttemptRows>>,
}

impl AttemptStore {
    fn observed_states(&self) -> Vec<WorkAttemptStateV1> {
        self.inner.lock().unwrap().observed_states.clone()
    }

    fn sealed_evidence(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Option<WorkAttemptEvidenceRecordV1> {
        let inner = self.inner.lock().unwrap();
        let payload = inner.evidence.get(&attempt_key(authority, identity))?;
        Some(serde_json::from_str(payload).unwrap())
    }
}

fn attempt_capacity(
    rows: &AttemptRows,
    authority: &WorkAuthority,
    task_id: &TaskId,
    concurrency: &TopologyConcurrencyPolicyV1,
) -> Result<WorkAttemptCapacityV1, WorkAttemptStorageError> {
    let mut global_active = 0_u64;
    let mut repository_active = 0_u64;
    let mut task_active = 0_u64;
    for ((row_authority, _), payload) in &rows.rows {
        if row_authority.project_id() != authority.project_id() {
            continue;
        }
        let attempt: WorkAttemptV1 =
            serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if attempt.is_terminal() {
            continue;
        }
        global_active += 1;
        if row_authority.repository_id() == authority.repository_id() {
            repository_active += 1;
            if attempt.identity().task_id() == task_id {
                task_active += 1;
            }
        }
    }
    Ok(WorkAttemptCapacityV1::new(
        global_active,
        repository_active,
        task_active,
        concurrency.clone(),
    ))
}

impl WorkAttemptStoragePort for AttemptStore {
    fn next_fence_epoch(&self, authority: &WorkAuthority) -> Result<u64, WorkAttemptStorageError> {
        let mut inner = self.inner.lock().unwrap();
        let epoch = inner.fences.entry(authority.clone()).or_insert(0);
        *epoch += 1;
        Ok(*epoch)
    }

    fn insert(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
    ) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError> {
        let payload =
            serde_json::to_string(attempt).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let mut inner = self.inner.lock().unwrap();
        let key = attempt_key(authority, attempt.identity());
        if let Some(existing) = inner.rows.get(&key) {
            return if *existing == payload {
                serde_json::from_str(existing)
                    .map(WorkAttemptInsertOutcome::Replayed)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)
            } else {
                Err(WorkAttemptStorageError::AttemptConflict)
            };
        }
        inner.rows.insert(key, payload);
        inner.observed_states.push(attempt.state());
        Ok(WorkAttemptInsertOutcome::Inserted)
    }

    fn insert_bounded(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
        concurrency: &TopologyConcurrencyPolicyV1,
    ) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError> {
        let payload =
            serde_json::to_string(attempt).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let mut inner = self.inner.lock().unwrap();
        let key = attempt_key(authority, attempt.identity());
        if let Some(existing) = inner.rows.get(&key) {
            return if *existing == payload {
                serde_json::from_str(existing)
                    .map(WorkAttemptInsertOutcome::Replayed)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)
            } else {
                Err(WorkAttemptStorageError::AttemptConflict)
            };
        }
        if matches!(
            attempt_capacity(&inner, authority, attempt.identity().task_id(), concurrency,)?
                .verdict(),
            WorkAttemptCapacityVerdictV1::Exhausted(_)
        ) {
            return Err(WorkAttemptStorageError::CapacityExceeded);
        }
        inner.rows.insert(key, payload);
        inner.observed_states.push(attempt.state());
        Ok(WorkAttemptInsertOutcome::Inserted)
    }

    fn admission_capacities(
        &self,
        authority: &WorkAuthority,
        task_ids: &[TaskId],
        concurrency: &TopologyConcurrencyPolicyV1,
    ) -> Result<BTreeMap<TaskId, WorkAttemptCapacityV1>, WorkAttemptStorageError> {
        let inner = self.inner.lock().unwrap();
        task_ids
            .iter()
            .map(|task_id| {
                attempt_capacity(&inner, authority, task_id, concurrency)
                    .map(|capacity| (task_id.clone(), capacity))
            })
            .collect()
    }

    fn load(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptV1, WorkAttemptStorageError> {
        let inner = self.inner.lock().unwrap();
        let payload = inner
            .rows
            .get(&attempt_key(authority, identity))
            .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)?;
        serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)
    }

    fn load_admission_kind(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptAdmissionKind, WorkAttemptStorageError> {
        self.load(authority, identity)
            .map(|_| WorkAttemptAdmissionKind::Ordinary)
    }

    fn update(
        &self,
        authority: &WorkAuthority,
        expected_fence: &WorkLeaseFenceV1,
        expected_state: WorkAttemptStateV1,
        next: &WorkAttemptV1,
        evidence: Option<&WorkAttemptEvidenceRecordV1>,
    ) -> Result<(), WorkAttemptStorageError> {
        let payload =
            serde_json::to_string(next).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let mut inner = self.inner.lock().unwrap();
        let key = attempt_key(authority, next.identity());
        let Some(existing) = inner.rows.get(&key) else {
            return Err(WorkAttemptStorageError::NotFoundOrNotAuthorized);
        };
        let current: WorkAttemptV1 =
            serde_json::from_str(existing).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if current.lease() != expected_fence || current.state() != expected_state {
            return Err(WorkAttemptStorageError::FenceConflict);
        }
        if let Some(evidence) = evidence {
            let record = serde_json::to_string(evidence)
                .map_err(|_| WorkAttemptStorageError::Unavailable)?;
            inner.evidence.insert(key.clone(), record);
        }
        inner.rows.insert(key, payload);
        inner.observed_states.push(next.state());
        Ok(())
    }

    fn open_attempts(
        &self,
        authority: &WorkAuthority,
    ) -> Result<Vec<WorkAttemptV1>, WorkAttemptStorageError> {
        let inner = self.inner.lock().unwrap();
        inner
            .rows
            .iter()
            .filter(|((row_authority, _), _)| row_authority == authority)
            .map(|(_, payload)| {
                serde_json::from_str::<WorkAttemptV1>(payload)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)
            })
            .filter(|attempt| {
                attempt
                    .as_ref()
                    .map_or(true, |attempt| !attempt.is_terminal())
            })
            .collect()
    }

    fn list(
        &self,
        _authority: &WorkAuthority,
        _start_after: Option<&WorkAttemptIdentityV1>,
        _limit: u32,
    ) -> Result<WorkAttemptListPageV1, WorkAttemptStorageError> {
        Err(WorkAttemptStorageError::Unavailable)
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

const PROJECT: &str = "project.work-attempt-exec";
const TASK: &str = "task.work-attempt-exec";
const RUN: &str = "run.work-attempt-exec";

fn request_context() -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(PROJECT),
        id::<RepositoryId>("repository.work-attempt-exec"),
        id::<WorktreeId>("worktree.work-attempt-exec"),
        None,
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work-attempt-exec"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        // Admission validates the grant window and the context deadline
        // against real `current_micros()` timestamps (the cancellation path
        // observes wall-clock time), so both must sit in the real future.
        deadline_in(3_600),
        scope.clone(),
        BTreeSet::from([CapabilityId::new("capability.work.exec-fixture").unwrap()]),
        BTreeSet::from([UseCaseId::new("use-case.work.exec-fixture").unwrap()]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.work-attempt-exec"),
        scope,
        grant,
        RequestId::new("request.work-attempt-exec").unwrap(),
        Deadline::new(deadline_in(3_600)).unwrap(),
        CancellationContext::active("cancel.work-attempt-exec").unwrap(),
    )
    .unwrap()
}

/// The domain pins a route's provider identity to its backend, so a fixture
/// route is only constructible against the backend's canonical provider ID.
fn requested_route(backend: WorkProviderBackendV1) -> WorkProviderRouteV1 {
    let provider = match backend {
        WorkProviderBackendV1::ClaudeCodeCli => "provider.work.claude-code-cli",
        WorkProviderBackendV1::CodexAppServer => "provider.work.codex-app-server",
        WorkProviderBackendV1::CodexCli => "provider.work.codex-cli",
    };
    WorkProviderRouteV1::new(
        id::<ProviderId>(provider),
        id::<WorkProviderRouteId>("route.work-attempt-exec.v1"),
    )
    .unwrap()
}

/// The protocol the domain pins to each backend. `WorkExecutionSnapshot`
/// refuses any other pairing, so this is the only protocol an attempt for
/// `backend` can ever carry into `resolve_provider`.
fn pinned_protocol(backend: WorkProviderBackendV1) -> WorkProviderProtocol {
    match backend {
        WorkProviderBackendV1::ClaudeCodeCli => WorkProviderProtocol::ClaudeStreamJson,
        WorkProviderBackendV1::CodexAppServer => WorkProviderProtocol::CodexAppServerJsonRpc,
        WorkProviderBackendV1::CodexCli => WorkProviderProtocol::CodexExecJson,
    }
}

/// Deadline far enough ahead that the wall-clock arm of the execution select
/// never fires; every fixture below is expected to finish on its own terms.
fn deadline_in(seconds: i64) -> UtcMicros {
    UtcMicros(current_micros().0.saturating_add(seconds * 1_000_000))
}

struct SnapshotShape {
    backend: WorkProviderBackendV1,
    max_stdout_bytes: u64,
    max_stderr_bytes: u64,
    deadline: UtcMicros,
    environment_allowlist: BTreeSet<String>,
    /// The pinned executable identity the preferred backend resolves through.
    executable: WorkExecutableReference,
    /// The snapshot's own fallback topology. Only this may name a successor
    /// backend; the gate never discovers one.
    fallback: WorkFallbackTopology,
}

impl Default for SnapshotShape {
    fn default() -> Self {
        Self {
            backend: WorkProviderBackendV1::ClaudeCodeCli,
            max_stdout_bytes: 65_536,
            max_stderr_bytes: 65_536,
            deadline: deadline_in(120),
            environment_allowlist: BTreeSet::new(),
            executable: WorkExecutableReference::new(
                "executable.work-attempt-exec".to_owned(),
                digest('e'),
            )
            .unwrap(),
            fallback: WorkFallbackTopology::Disabled,
        }
    }
}

fn execution_snapshot(shape: &SnapshotShape) -> WorkExecutionSnapshot {
    crossed_execution_snapshot(shape, shape.backend, pinned_protocol(shape.backend)).unwrap()
}

/// Builds a snapshot with an explicitly chosen protocol so the pairing rule
/// itself can be exercised.
fn crossed_execution_snapshot(
    shape: &SnapshotShape,
    backend: WorkProviderBackendV1,
    protocol: WorkProviderProtocol,
) -> Result<WorkExecutionSnapshot, tracedecay_domain::WorkRuntimeContractError> {
    WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: id::<ConfigurationRevisionId>("configuration-revision.exec.1"),
        configuration_snapshot_id: id::<ConfigurationSnapshotId>("configuration-snapshot.exec.1"),
        effective_behavior_digest: digest('c'),
        resolution_provenance_digest: digest('d'),
        route: requested_route(backend),
        backend,
        protocol,
        model: "model.work-attempt-exec".to_owned(),
        executable: shape.executable.clone(),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: shape.environment_allowlist.clone(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(
            128_000,
            8_192,
            shape.max_stdout_bytes,
            shape.max_stderr_bytes,
            65_536,
            1,
        )
        .unwrap(),
        deadline: shape.deadline,
        fallback: shape.fallback.clone(),
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
}

struct Fixture {
    attempts: WorkAttemptService<AttemptStore>,
    rows: AttemptStore,
    context: RequestContext,
    authority: WorkAuthority,
    attempt: WorkAttemptV1,
}

impl Fixture {
    fn identity(&self) -> &WorkAttemptIdentityV1 {
        self.attempt.identity()
    }

    fn state(&self) -> WorkAttemptStateV1 {
        self.current_attempt().state()
    }

    fn current_attempt(&self) -> WorkAttemptV1 {
        self.attempts
            .status(
                &self.context,
                &WorkAttemptStatusRequestV1 {
                    task_id: id(TASK),
                    run_id: id(RUN),
                    attempt_id: id("attempt.1"),
                },
            )
            .unwrap()
    }

    fn sealed_evidence(&self) -> WorkAttemptEvidenceRecordV1 {
        self.rows
            .sealed_evidence(&self.authority, self.identity())
            .expect("terminal evidence is sealed with the attempt row")
    }
}

/// Persists one canonically bound attempt in `Leased`, the state handed to the
/// live provider path. Proposal selection is deliberately explicit here: this
/// provider-runtime fixture does not invent routing evidence through a Work
/// event-store double.
fn leased_attempt(worktree_root: &Path, instructions: &str, shape: &SnapshotShape) -> Fixture {
    let rows = AttemptStore::default();
    let attempts = WorkAttemptService::new(rows.clone());
    let context = request_context();
    let authority = WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .unwrap();
    let identity = WorkAttemptIdentityV1::new(
        id::<TaskId>(TASK),
        id::<RunId>(RUN),
        id::<AttemptId>("attempt.1"),
    )
    .unwrap();
    let projection_binding = WorkAttemptProjectionBindingV1::new(
        WorkGraphVersionV1::new(1).unwrap(),
        WorkProductEventSequenceV1::new(1).unwrap(),
        WorkProductSourceWatermarkV1::new(BTreeMap::new()).unwrap(),
        digest('f'),
        id::<ProposalId>("proposal.work-attempt-exec"),
    )
    .unwrap();
    let snapshot = execution_snapshot(shape);
    let requested_route = snapshot.route().clone();
    let execution = WorkExecutionEnvelopeV1::new(
        identity.clone(),
        projection_binding.clone(),
        id::<WorkflowOperationRef>("operation.work-attempt-exec"),
        snapshot,
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        worktree_root.to_string_lossy().into_owned(),
        Some(id::<RefId>("refs/heads/work-attempt-exec")),
        id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        instructions.to_owned(),
        1,
        WorkEffectStateV1::Observational,
    )
    .unwrap();
    let attempt = WorkAttemptV1::new(
        identity,
        projection_binding,
        execution,
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.work-attempt-exec"),
            WorkFenceEpochV1::new(rows.next_fence_epoch(&authority).unwrap()).unwrap(),
        )
        .unwrap(),
        WorkAttemptStateV1::Leased,
        None,
        Vec::new(),
        WorkCancellationStateV1::None,
        WorkRecoveryStateV1::Fresh,
        requested_route,
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        rows.insert(&authority, &attempt).unwrap(),
        WorkAttemptInsertOutcome::Inserted
    );
    Fixture {
        attempts,
        rows,
        context,
        authority,
        attempt,
    }
}

// ---------------------------------------------------------------------------
// Fake-executable fixtures
// ---------------------------------------------------------------------------

/// Writes an executable shell script into an isolated temp directory. The
/// script text carries absolute marker paths because the spawn path calls
/// `env_clear()` — nothing but the allowlist survives into the child.
#[cfg(unix)]
fn fake_executable(directory: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = directory.join(name);
    std::fs::write(&path, body).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

/// A first-choice selection: the preferred backend resolved, so there is
/// nothing to report.
fn preferred(
    executable: PathBuf,
    protocol: WorkProviderProtocol,
    arguments: &[&'static str],
    actual_route: WorkProviderRouteV1,
) -> ProviderSelection {
    ProviderSelection {
        provider: ResolvedProvider {
            executable,
            protocol,
            arguments: arguments.to_vec(),
        },
        actual_route,
        fallback: None,
    }
}

// ---------------------------------------------------------------------------
// Pinned executable-binding fixtures (the gate's availability probe)
// ---------------------------------------------------------------------------

/// Writes a real executable and returns the pinned reference that names it:
/// the same `(executable_id, sha256-of-bytes)` pair the resolver verifies
/// against the on-disk bytes.
#[cfg(unix)]
fn pinned_executable(
    directory: &Path,
    name: &str,
    executable_id: &str,
    body: &str,
) -> (WorkExecutableReference, PathBuf) {
    let path = fake_executable(directory, name, body)
        .canonicalize()
        .unwrap();
    let reference =
        WorkExecutableReference::new(executable_id.to_owned(), sha256_digest(body.as_bytes()))
            .unwrap();
    (reference, path)
}

#[cfg(unix)]
fn binding(
    reference: &WorkExecutableReference,
    path: &Path,
    capability: WorkExecutableCapabilityV1,
) -> WorkExecutableBindingV1 {
    WorkExecutableBindingV1::new(reference.clone(), path.to_path_buf(), vec![capability]).unwrap()
}

/// A real `PinnedWorkExecutableBindingResolver` over exactly these bindings.
#[cfg(unix)]
fn resolver_over(
    root: &Path,
    bindings: Vec<WorkExecutableBindingV1>,
) -> PinnedWorkExecutableBindingResolver {
    let project_id = id::<ProjectId>(PROJECT);
    let revision_id = id::<ConfigurationRevisionId>("configuration-revision.exec.bindings");
    let key = SettingKey::new(WORK_EXECUTABLE_BINDINGS_SETTING_KEY).unwrap();
    let resolution = resolve_configuration(
        &ConfigurationRegistry::core().unwrap(),
        &[ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            revision_id: revision_id.clone(),
            entries: BTreeMap::from([(
                key,
                ConfigurationValueV1::WorkExecutableBindings(bindings),
            )]),
        }],
    )
    .unwrap();
    let configuration = PinnedRuntimeConfiguration::new(
        RuntimeConfigurationTarget {
            project_id,
            project_root: root.to_path_buf(),
        },
        revision_id,
        resolution.snapshot,
    )
    .unwrap();
    PinnedWorkExecutableBindingResolver::from_configuration(&configuration).unwrap()
}

/// The route the snapshot's Codex CLI fallback topology names.
fn fallback_route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.codex-cli"),
        id::<WorkProviderRouteId>("route.work-attempt-exec.codex-cli-fallback"),
    )
    .unwrap()
}

fn codex_cli_fallback(executable: WorkExecutableReference) -> WorkFallbackTopology {
    WorkFallbackTopology::CodexCli {
        route: fallback_route(),
        executable,
    }
}

/// A shell provider that consumes its stdin and exits cleanly. Used wherever
/// a binding has to be *resolvable* for the gate to have a real choice.
const CLEAN_PROVIDER: &str = "#!/bin/sh\ncat > /dev/null\nexit 0\n";

// ---------------------------------------------------------------------------
// 1. Happy path
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn a_clean_provider_run_seals_succeeded_evidence_over_the_captured_stream() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let argv_marker = root.join("argv");
    let stdin_marker = root.join("stdin");
    // Claude `stream-json` framing: one JSON object per line on stdout.
    let stream = concat!(
        r#"{"type":"system","subtype":"init","session_id":"s-1"}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","is_error":false}"#,
        "\n",
    );
    let executable = fake_executable(
        root,
        "claude-code",
        &format!(
            "#!/bin/sh\nprintf '%s' \"$*\" > {argv}\ncat > {stdin}\nprintf '%s' '{stream}'\nexit 0\n",
            argv = argv_marker.display(),
            stdin = stdin_marker.display(),
            stream = stream,
        ),
    );

    let instructions = "Execute the admitted provider step.";
    let fixture = leased_attempt(root, instructions, &SnapshotShape::default());
    let admitted_environment =
        admitted_provider_environment(fixture.attempt.execution().execution_snapshot());
    execute_provider_with_environment(
        &fixture.attempts,
        &fixture.context,
        &fixture.attempt,
        &preferred(
            executable,
            WorkProviderProtocol::ClaudeStreamJson,
            &CLAUDE_STREAM_JSON_ARGV,
            requested_route(WorkProviderBackendV1::ClaudeCodeCli),
        ),
        &admitted_environment,
        Arc::new(Notify::new()),
        None,
        None,
        AttemptAdmissionTimingV1::for_test(),
    )
    .await;

    // The protocol's argv reached the real child, and the attempt
    // instructions were piped to its stdin.
    assert_eq!(
        std::fs::read_to_string(&argv_marker).unwrap(),
        CLAUDE_STREAM_JSON_ARGV.join(" ")
    );
    assert_eq!(
        std::fs::read_to_string(&stdin_marker).unwrap(),
        instructions
    );

    assert_eq!(fixture.state(), WorkAttemptStateV1::Succeeded);
    let settled = fixture.current_attempt();
    assert_eq!(settled.artifacts().len(), 1);
    assert_eq!(
        settled.artifacts()[0].artifact_id().as_str(),
        "artifact.provider.stdout"
    );
    assert_eq!(settled.artifacts()[0].byte_length(), stream.len() as u64);
    let scheduled = std::time::Instant::now();
    let admitted = scheduled + std::time::Duration::from_micros(5);
    let started = scheduled + std::time::Duration::from_micros(10);
    let terminal = scheduled + std::time::Duration::from_micros(40);
    let resource = work_operation_resource_observation(
        &fixture.current_attempt(),
        AttemptAdmissionTimingV1 {
            scheduled,
            admitted,
        },
        started,
        terminal,
        Some("provider-request-1".to_owned()),
    )
    .expect("settled provider timing");
    assert_eq!(resource.scheduled_latency_micros, 5);
    assert_eq!(resource.service_latency_micros, 30);
    assert_eq!(
        resource.provider_request_id.as_deref(),
        Some("provider-request-1")
    );
    assert_eq!(
        resource.activation_outcome,
        Some(OperationActivationOutcomeV1::Committed)
    );
    assert_eq!(
        resource
            .stage_timings
            .iter()
            .map(|timing| (timing.stage, timing.elapsed_micros))
            .collect::<Vec<_>>(),
        vec![
            (OperationStageV1::Scheduled, 0),
            (OperationStageV1::Admitted, 5),
            (OperationStageV1::Started, 10),
            (OperationStageV1::Terminal, 40),
        ]
    );
    let evidence = fixture.sealed_evidence();
    assert_eq!(
        evidence.outcome,
        WorkAttemptProviderOutcomeV1::Exited { code: 0 }
    );
    assert_eq!(evidence.identity, *fixture.identity());
    let route = requested_route(WorkProviderBackendV1::ClaudeCodeCli);
    assert_eq!(evidence.requested_route, route);
    assert_eq!(evidence.actual_route, Some(route));
    assert_eq!(
        evidence.provider_session,
        Some(
            ObservationSourceIdentityV1::for_provider(
                id::<ProviderId>("claude"),
                id::<SessionId>("s-1"),
            )
            .expect("Claude provider session"),
        ),
    );
    // Session correlation does not weaken byte-exact stream accounting.
    let stdout = evidence.stdout.expect("stdout summary");
    assert_eq!(stdout.byte_length, stream.len() as u64);
    assert!(!stdout.truncated);
    assert_eq!(stdout.digest, sha256_digest(stream.as_bytes()));
    let stderr = evidence.stderr.expect("stderr summary");
    assert_eq!(stderr.byte_length, 0);
    assert!(!stderr.truncated);
    // The durable trail runs Leased -> Running -> Succeeded; nothing skips.
    assert_eq!(
        fixture.rows.observed_states(),
        vec![
            WorkAttemptStateV1::Leased,
            WorkAttemptStateV1::Running,
            WorkAttemptStateV1::Succeeded,
        ]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn initial_provider_child_uses_values_captured_for_that_spawn() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let environment_marker = root.join("environment");
    let sentinel = format!("TRACEDECAY_WORK_ADMITTED_SENTINEL_{}", std::process::id());
    let ambient_secret = format!("TRACEDECAY_WORK_AMBIENT_SECRET_{}", std::process::id());
    let prior_sentinel = std::env::var_os(&sentinel);
    let prior_secret = std::env::var_os(&ambient_secret);
    // SAFETY: these unique test keys are restored before the test returns.
    unsafe {
        std::env::set_var(&sentinel, "admitted-sentinel");
        std::env::remove_var(&ambient_secret);
    }
    let executable = fake_executable(
        root,
        "environment-provider",
        &format!(
            "#!/bin/sh\nprintf '%s|%s' \"${{{sentinel}:-missing}}\" \"${{{ambient_secret}:-missing}}\" > {marker}\ncat > /dev/null\nexit 0\n",
            marker = environment_marker.display(),
        ),
    );
    let fixture = leased_attempt(
        root,
        "Observe the admitted provider environment.",
        &SnapshotShape {
            environment_allowlist: BTreeSet::from([sentinel.clone()]),
            ..SnapshotShape::default()
        },
    );
    let admitted_environment =
        admitted_provider_environment(fixture.attempt.execution().execution_snapshot());
    // A later ambient mutation cannot alter this initial child, and an
    // unadmitted secret must never cross the boundary. Recovery resolves the
    // same snapshot allowlist again at its later spawn; it does not persist
    // these plaintext values in the attempt row.
    // SAFETY: these unique test keys are restored below.
    unsafe {
        std::env::set_var(&sentinel, "ambient-replacement");
        std::env::set_var(&ambient_secret, "ambient-secret");
    }
    execute_provider_with_environment(
        &fixture.attempts,
        &fixture.context,
        &fixture.attempt,
        &preferred(
            executable,
            WorkProviderProtocol::ClaudeStreamJson,
            &CLAUDE_STREAM_JSON_ARGV,
            requested_route(WorkProviderBackendV1::ClaudeCodeCli),
        ),
        &admitted_environment,
        Arc::new(Notify::new()),
        None,
        None,
        AttemptAdmissionTimingV1::for_test(),
    )
    .await;
    let observed = std::fs::read_to_string(&environment_marker).unwrap();
    // SAFETY: return the process environment to the state this test found.
    unsafe {
        match prior_sentinel {
            Some(value) => std::env::set_var(&sentinel, value),
            None => std::env::remove_var(&sentinel),
        }
        match prior_secret {
            Some(value) => std::env::set_var(&ambient_secret, value),
            None => std::env::remove_var(&ambient_secret),
        }
    }
    assert_eq!(observed, "admitted-sentinel|missing");
    assert_eq!(fixture.state(), WorkAttemptStateV1::Succeeded);
}

// ---------------------------------------------------------------------------
// 2. Output cap
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test]
async fn stdout_past_the_admitted_cap_is_a_typed_overflow_not_a_silent_success() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let argv_marker = root.join("argv");
    // Codex `exec --json` framing: one JSON event per line, repeated well past
    // the admitted stdout ceiling.
    let line = r#"{"type":"item.completed","item":{"type":"agent_message","text":"chunk"}}"#;
    let mut stream = String::new();
    for _ in 0..64 {
        stream.push_str(line);
        stream.push('\n');
    }
    let executable = fake_executable(
        root,
        "codex",
        &format!(
            "#!/bin/sh\nprintf '%s' \"$*\" > {argv}\ncat > /dev/null\ni=0\nwhile [ $i -lt 64 ]; do printf '%s\\n' '{line}'; i=$((i+1)); done\nexit 0\n",
            argv = argv_marker.display(),
            line = line,
        ),
    );

    const CAP: u64 = 128;
    let fixture = leased_attempt(
        root,
        "Overflow the admitted stdout budget.",
        &SnapshotShape {
            backend: WorkProviderBackendV1::CodexCli,
            max_stdout_bytes: CAP,
            ..SnapshotShape::default()
        },
    );
    let admitted_environment =
        admitted_provider_environment(fixture.attempt.execution().execution_snapshot());
    execute_provider_with_environment(
        &fixture.attempts,
        &fixture.context,
        &fixture.attempt,
        &preferred(
            executable,
            WorkProviderProtocol::CodexExecJson,
            &CODEX_EXEC_JSON_ARGV,
            requested_route(WorkProviderBackendV1::CodexCli),
        ),
        &admitted_environment,
        Arc::new(Notify::new()),
        None,
        None,
        AttemptAdmissionTimingV1::for_test(),
    )
    .await;

    assert_eq!(
        std::fs::read_to_string(&argv_marker).unwrap(),
        CODEX_EXEC_JSON_ARGV.join(" ")
    );
    // The child exited 0; the overflow classification must still win, or a
    // truncated transcript would be sealed as an unqualified success.
    let evidence = fixture.sealed_evidence();
    assert_eq!(
        evidence.outcome,
        WorkAttemptProviderOutcomeV1::StreamOverflow {
            channel: WorkAttemptStreamChannelV1::Stdout,
        }
    );
    assert_eq!(fixture.state(), WorkAttemptStateV1::Failed);
    let stdout = evidence.stdout.expect("stdout summary");
    assert!(stdout.truncated);
    // The summary tells the truth twice over: the real produced length, and
    // the digest of exactly the retained prefix.
    assert_eq!(stdout.byte_length, stream.len() as u64);
    assert!(stdout.byte_length > CAP);
    assert_eq!(
        stdout.digest,
        sha256_digest(&stream.as_bytes()[..CAP as usize])
    );
}

#[test]
fn overflow_classification_names_the_channel_and_yields_to_cancellation() {
    let clean = WorkAttemptStreamSummaryV1 {
        byte_length: 4,
        truncated: false,
        digest: digest('1'),
    };
    let truncated = WorkAttemptStreamSummaryV1 {
        byte_length: 4_096,
        truncated: true,
        digest: digest('2'),
    };

    assert_eq!(
        overflow_outcome(
            WorkAttemptProviderOutcomeV1::Exited { code: 0 },
            &Some(clean.clone()),
            &Some(truncated.clone()),
        ),
        WorkAttemptProviderOutcomeV1::StreamOverflow {
            channel: WorkAttemptStreamChannelV1::Stderr,
        }
    );
    // Stdout is named first when both overflowed.
    assert_eq!(
        overflow_outcome(
            WorkAttemptProviderOutcomeV1::Exited { code: 0 },
            &Some(truncated.clone()),
            &Some(truncated.clone()),
        ),
        WorkAttemptProviderOutcomeV1::StreamOverflow {
            channel: WorkAttemptStreamChannelV1::Stdout,
        }
    );
    // A cancelled or timed-out attempt keeps its own truth: the trim is a
    // consequence of the kill, not the reason the attempt ended.
    for outcome in [
        WorkAttemptProviderOutcomeV1::Cancelled,
        WorkAttemptProviderOutcomeV1::TimedOut,
    ] {
        assert_eq!(
            overflow_outcome(outcome, &Some(truncated.clone()), &Some(truncated.clone())),
            outcome
        );
    }
    // A clean run is untouched, and absent streams classify nothing.
    assert_eq!(
        overflow_outcome(
            WorkAttemptProviderOutcomeV1::Exited { code: 3 },
            &Some(clean),
            &None,
        ),
        WorkAttemptProviderOutcomeV1::Exited { code: 3 }
    );
}

// ---------------------------------------------------------------------------
// 3. Cancellation ladder
// ---------------------------------------------------------------------------

/// A provider that traps `SIGINT` and keeps running forces the full ladder:
/// the graceful rung is delivered, ignored, and then escalated to a kill.
/// This test spends the real `CANCELLATION_GRACE` window on purpose — a
/// virtual clock would let the grace expire without proving the child
/// actually survived it.
#[cfg(unix)]
#[tokio::test]
async fn a_provider_that_ignores_interrupt_is_escalated_to_a_kill_on_the_record() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let started_marker = root.join("started");
    let interrupted_marker = root.join("interrupted");
    let executable = fake_executable(
        root,
        "stubborn-provider",
        &format!(
            "#!/bin/sh\ntrap \"printf x >> {interrupted}\" INT\nprintf x > {started}\ni=0\nwhile [ $i -lt 300 ]; do sleep 1; i=$((i+1)); done\n",
            interrupted = interrupted_marker.display(),
            started = started_marker.display(),
        ),
    );

    let fixture = leased_attempt(
        root,
        "Run until cancelled.",
        &SnapshotShape {
            deadline: deadline_in(300),
            ..SnapshotShape::default()
        },
    );
    let cancel = Arc::new(Notify::new());
    let provider = preferred(
        executable,
        WorkProviderProtocol::ClaudeStreamJson,
        &CLAUDE_STREAM_JSON_ARGV,
        requested_route(WorkProviderBackendV1::ClaudeCodeCli),
    );
    let identity = fixture.identity().clone();
    let started_at = std::time::Instant::now();
    let admitted_environment =
        admitted_provider_environment(fixture.attempt.execution().execution_snapshot());

    let driver = async {
        // Wait for the real child to be up and the attempt to be durably
        // Running before a cancellation may even be requested.
        while !started_marker.exists() || fixture.state() != WorkAttemptStateV1::Running {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        fixture
            .attempts
            .request_cancellation(
                &fixture.context,
                CancelWorkAttemptCommand {
                    task_id: identity.task_id().clone(),
                    run_id: identity.run_id().clone(),
                    attempt_id: identity.attempt_id().clone(),
                    request_id: id("cancellation.work-attempt-exec.1"),
                    occurred_at: current_micros(),
                },
            )
            .unwrap();
        // `notify_waiters` only wakes waiters already parked on the channel,
        // so keep signalling until the execution arm observes it.
        loop {
            cancel.notify_waiters();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };

    tokio::select! {
        () = execute_provider_with_environment(
            &fixture.attempts,
            &fixture.context,
            &fixture.attempt,
            &provider,
            &admitted_environment,
            Arc::clone(&cancel),
            None,
            None,
            AttemptAdmissionTimingV1::for_test(),
        ) => {}
        _ = driver => unreachable!("the driver loops until execution settles"),
    }

    // The graceful rung really was delivered to the child, and really was
    // survived: the trap fired and the grace window elapsed in full.
    assert!(
        interrupted_marker.exists(),
        "the provider should have received SIGINT before any kill"
    );
    assert!(started_at.elapsed() >= CANCELLATION_GRACE);

    let states = fixture.rows.observed_states();
    assert!(
        states.contains(&WorkAttemptStateV1::CancellationAcknowledged)
            && states.contains(&WorkAttemptStateV1::CancellationEscalated),
        "the ladder must record both rungs, got {states:?}"
    );
    assert_eq!(*states.last().unwrap(), WorkAttemptStateV1::Cancelled);
    assert_eq!(fixture.state(), WorkAttemptStateV1::Cancelled);
    assert_eq!(
        fixture.sealed_evidence().outcome,
        WorkAttemptProviderOutcomeV1::Cancelled
    );
}

// ---------------------------------------------------------------------------
// 3b. Wall exhaustion (Plan 26 no-progress terminal)
// ---------------------------------------------------------------------------

/// A provider that outlives its envelope deadline is killed and sealed as
/// `TimedOut`, and the kill emits exactly one `operation.no_progress.terminal.v1`
/// owner fact through the mounted producer: the pinned topology-policy digest,
/// a positive armed budget, a measured stall at least that budget, a provably
/// zero frontier, no remaining run budget, the kill escalation, and an unknown
/// effect outcome. This test spends the real two-second wall on purpose — the
/// stall must be a monotonic measurement, not a virtual-clock artifact.
#[cfg(unix)]
#[tokio::test]
async fn a_wall_exhausted_provider_seals_timed_out_and_emits_the_no_progress_terminal() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let runtime = crate::global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        root,
        id::<ProjectId>(PROJECT),
    )
    .await
    .expect("registered runtime");
    let database = runtime.project_database_arc().expect("project database");
    let producer = tracedecay_usecases::observability::BoundedObservabilityProducerV1::start(
        database.clone(),
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: PROJECT.to_owned(),
            process_boot_id: "boot:work-attempt-exec-no-progress".to_owned(),
            producer_revision: "producer.work-attempt-exec.v1".to_owned(),
            configuration_revision: "configuration.work-attempt-exec.v1".to_owned(),
            policy_revision: "policy.work-attempt-exec.v1".to_owned(),
        },
        8,
    )
    .expect("bounded producer");
    let topology_policy_digest = tracedecay_domain::safe_work_topology_policy_v1()
        .compute_digest()
        .expect("topology policy digest");

    let executable = fake_executable(
        root,
        "stalled-provider",
        "#!/bin/sh\ncat > /dev/null\ni=0\nwhile [ $i -lt 300 ]; do sleep 1; i=$((i+1)); done\n",
    );
    // The fixture is built after the database mount so the two-second wall
    // budget covers only the execution itself.
    let deadline = deadline_in(2);
    let fixture = leased_attempt(
        root,
        "Stall past the wall deadline.",
        &SnapshotShape {
            deadline,
            ..SnapshotShape::default()
        },
    );
    let admitted_environment =
        admitted_provider_environment(fixture.attempt.execution().execution_snapshot());
    execute_provider_with_environment(
        &fixture.attempts,
        &fixture.context,
        &fixture.attempt,
        &preferred(
            executable,
            WorkProviderProtocol::ClaudeStreamJson,
            &CLAUDE_STREAM_JSON_ARGV,
            requested_route(WorkProviderBackendV1::ClaudeCodeCli),
        ),
        &admitted_environment,
        Arc::new(Notify::new()),
        Some(&producer),
        Some(&topology_policy_digest),
        AttemptAdmissionTimingV1::for_test(),
    )
    .await;

    // The product path is unchanged by the emission: the kill still seals the
    // typed timeout terminal.
    assert_eq!(fixture.state(), WorkAttemptStateV1::TimedOut);
    assert_eq!(
        fixture.sealed_evidence().outcome,
        WorkAttemptProviderOutcomeV1::TimedOut
    );

    producer.shutdown().await.expect("producer shutdown");
    let page = RegisteredObservabilityPortV1::new(&database)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: PROJECT.to_owned(),
            event_kinds: vec!["operation.no_progress.terminal.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: current_micros().0.saturating_add(1_000_000),
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("no-progress page");
    assert_eq!(page.events.len(), 1, "exactly one no-progress terminal");
    let envelope = &page.events[0];
    assert_eq!(
        envelope.terminal_result,
        Some(ObservabilityTerminalResultV1::TimedOut)
    );
    let ObservabilityPayloadV1::NoProgress(observed) = &envelope.payload else {
        panic!("expected a no-progress payload, got {:?}", envelope.payload);
    };
    let expected_deadline_ref = format!(
        "work-run-deadline:{}",
        canonical_sha256(&(
            "tracedecay.work.run-deadline.v1",
            TASK,
            RUN,
            "attempt.1",
            deadline,
        ))
        .expect("run deadline digest")
        .as_str()
    );
    assert_eq!(observed.run_deadline_ref, expected_deadline_ref);
    assert_eq!(
        observed.concurrency_policy_revision,
        topology_policy_digest.0.as_str()
    );
    assert_eq!(observed.workflow_stage, WorkflowStageClassV1::Execute);
    assert!(
        observed.configured_timeout_micros > 0 && observed.configured_timeout_micros <= 2_000_000,
        "the armed budget is the truthful remaining envelope budget, got {}",
        observed.configured_timeout_micros
    );
    assert!(
        observed.elapsed_stall_micros >= observed.configured_timeout_micros,
        "the stall is measured, not asserted: {} < {}",
        observed.elapsed_stall_micros,
        observed.configured_timeout_micros
    );
    assert_eq!(observed.last_committed_frontier, 0);
    assert_eq!(observed.remaining_run_budget_micros, 0);
    assert_eq!(observed.escalation, NoProgressEscalationV1::Kill);
    assert_eq!(
        observed.effect_outcome,
        EffectReconciliationOutcomeV1::Unknown
    );
}

// ---------------------------------------------------------------------------
// 4. The Codex app-server preference gate (Plan 32)
// ---------------------------------------------------------------------------

/// Plan 32: "Codex-designated work prefers the configured app-server."
///
/// The decisive part of this fixture is that the Codex CLI fallback is *also*
/// configured and *also* resolvable. Preference therefore has to be a real
/// choice: if the gate merely took whatever resolved, this attempt could just
/// as well have landed on the CLI.
#[cfg(unix)]
#[test]
fn a_resolvable_app_server_is_preferred_over_an_equally_resolvable_codex_cli() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let (app_server, app_server_path) =
        pinned_executable(root, "codex-app-server", "codex.app-server", CLEAN_PROVIDER);
    let (cli, cli_path) = pinned_executable(root, "codex-cli", "codex.cli", CLEAN_PROVIDER);
    let resolver = resolver_over(
        root,
        vec![
            binding(
                &app_server,
                &app_server_path,
                WorkExecutableCapabilityV1::CodexAppServerJsonRpc,
            ),
            binding(
                &cli,
                &cli_path,
                WorkExecutableCapabilityV1::CodexCliExecJson,
            ),
        ],
    );

    let fixture = leased_attempt(
        root,
        "Prefer the app-server.",
        &SnapshotShape {
            backend: WorkProviderBackendV1::CodexAppServer,
            executable: app_server,
            fallback: codex_cli_fallback(cli),
            ..SnapshotShape::default()
        },
    );
    let selection = select_with_resolver(&resolver, &fixture.attempt)
        .ok()
        .expect("a resolvable app-server binding wins the gate");

    assert_eq!(
        selection.provider.protocol,
        WorkProviderProtocol::CodexAppServerJsonRpc
    );
    assert_eq!(selection.provider.executable, app_server_path);
    assert_ne!(selection.provider.executable, cli_path);
    assert_eq!(
        selection.actual_route,
        requested_route(WorkProviderBackendV1::CodexAppServer)
    );
    // Nothing was reported because nothing was given up.
    assert!(
        selection.fallback.is_none(),
        "the preferred backend ran; there is no fallback to report"
    );
    // Selection alone starts nothing.
    assert_eq!(fixture.state(), WorkAttemptStateV1::Leased);
}

/// Plan 32: the Codex CLI is eligible "only when app-server is unsupported or
/// absent before session start and the pinned Plan 20 snapshot explicitly
/// allows that fallback", and the plan index requires the fallback to be
/// "reported rather than hidden".
///
/// Here the app-server executable exists on disk but its bytes no longer match
/// the pinned digest — the probe is a real file read, so this is a
/// `DigestMismatch`, not a configuration guess. The CLI takes over and the
/// handover survives all the way into the sealed terminal evidence.
#[cfg(unix)]
#[tokio::test]
async fn a_disqualified_app_server_falls_back_to_codex_cli_and_says_so_in_the_evidence() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let (app_server, app_server_path) =
        pinned_executable(root, "codex-app-server", "codex.app-server", CLEAN_PROVIDER);
    let argv_marker = root.join("argv");
    let (cli, cli_path) = pinned_executable(
        root,
        "codex-cli",
        "codex.cli",
        &format!(
            "#!/bin/sh\nprintf '%s' \"$*\" > {argv}\ncat > /dev/null\nprintf '%s\\n' '{{\"type\":\"thread.started\",\"thread_id\":\"thread-codex-fallback\"}}'\nexit 0\n",
            argv = argv_marker.display(),
        ),
    );
    let resolver = resolver_over(
        root,
        vec![
            binding(
                &app_server,
                &app_server_path,
                WorkExecutableCapabilityV1::CodexAppServerJsonRpc,
            ),
            binding(
                &cli,
                &cli_path,
                WorkExecutableCapabilityV1::CodexCliExecJson,
            ),
        ],
    );
    // The app-server binary is replaced after pinning. Configuration still
    // names it; only reading the bytes can tell.
    std::fs::write(&app_server_path, "#!/bin/sh\nexit 0\n").unwrap();

    let fixture = leased_attempt(
        root,
        "Fall back to the Codex CLI.",
        &SnapshotShape {
            backend: WorkProviderBackendV1::CodexAppServer,
            executable: app_server,
            fallback: codex_cli_fallback(cli),
            ..SnapshotShape::default()
        },
    );
    let selection = select_with_resolver(&resolver, &fixture.attempt)
        .ok()
        .expect("the configured fallback is eligible");

    assert_eq!(
        selection.provider.protocol,
        WorkProviderProtocol::CodexExecJson
    );
    assert_eq!(selection.provider.executable, cli_path);
    assert_eq!(selection.actual_route, fallback_route());
    let report = selection
        .fallback
        .clone()
        .expect("a fallback that is not reported is a hidden fallback");
    assert_eq!(
        report,
        WorkProviderFallbackRecordV1 {
            preferred_backend: WorkProviderBackendV1::CodexAppServer,
            preferred_route: requested_route(WorkProviderBackendV1::CodexAppServer),
            preferred_state: WorkProviderAvailabilityV1::DigestMismatch,
            fallback_backend: WorkProviderBackendV1::CodexCli,
            fallback_route: fallback_route(),
            fallback_state: None,
        }
    );

    let admitted_environment =
        admitted_provider_environment(fixture.attempt.execution().execution_snapshot());
    execute_provider_with_environment(
        &fixture.attempts,
        &fixture.context,
        &fixture.attempt,
        &selection,
        &admitted_environment,
        Arc::new(Notify::new()),
        None,
        None,
        AttemptAdmissionTimingV1::for_test(),
    )
    .await;

    // The CLI really ran, on the CLI's own argv.
    assert_eq!(
        std::fs::read_to_string(&argv_marker).unwrap(),
        CODEX_EXEC_JSON_ARGV.join(" ")
    );
    assert_eq!(fixture.state(), WorkAttemptStateV1::Succeeded);
    let evidence = fixture.sealed_evidence();
    assert_eq!(
        evidence.outcome,
        WorkAttemptProviderOutcomeV1::Exited { code: 0 }
    );
    // The record keeps both routes apart: what was asked for, and what ran.
    assert_eq!(
        evidence.requested_route,
        requested_route(WorkProviderBackendV1::CodexAppServer)
    );
    assert_eq!(evidence.actual_route, Some(fallback_route()));
    assert_eq!(evidence.provider_fallback, Some(report));
    assert_eq!(
        evidence.provider_session,
        Some(
            ObservationSourceIdentityV1::for_provider(
                id::<ProviderId>("codex"),
                id::<SessionId>("thread-codex-fallback"),
            )
            .expect("Codex provider session"),
        ),
    );
}

/// A snapshot whose topology is `Disabled` has no successor to name. Losing
/// the app-server must deny the attempt, not reach for an ambient Codex CLI.
#[cfg(unix)]
#[test]
fn a_disabled_topology_denies_instead_of_inventing_a_codex_cli_route() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let (app_server, _) =
        pinned_executable(root, "codex-app-server", "codex.app-server", CLEAN_PROVIDER);
    // A perfectly usable Codex CLI is configured and resolvable — and still
    // unreachable, because this snapshot never named it.
    let (cli, cli_path) = pinned_executable(root, "codex-cli", "codex.cli", CLEAN_PROVIDER);
    let resolver = resolver_over(
        root,
        vec![binding(
            &cli,
            &cli_path,
            WorkExecutableCapabilityV1::CodexCliExecJson,
        )],
    );

    let fixture = leased_attempt(
        root,
        "No configured fallback.",
        &SnapshotShape {
            backend: WorkProviderBackendV1::CodexAppServer,
            executable: app_server,
            fallback: WorkFallbackTopology::Disabled,
            ..SnapshotShape::default()
        },
    );
    let denial = select_with_resolver(&resolver, &fixture.attempt)
        .err()
        .expect("an unbound app-server with no configured fallback is denied");
    assert_eq!(denial.state, WorkProviderAvailabilityV1::Absent);
    assert!(
        denial.fallback.is_none(),
        "no fallback was configured, so there is nothing to report"
    );
    assert_eq!(fixture.state(), WorkAttemptStateV1::Leased);
}

/// When both the preferred backend and its configured fallback are refused,
/// one typed state would erase half the truth. The denial keeps both.
#[cfg(unix)]
#[tokio::test]
async fn a_fallback_that_is_also_refused_keeps_both_denials_on_the_record() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let (app_server, app_server_path) =
        pinned_executable(root, "codex-app-server", "codex.app-server", CLEAN_PROVIDER);
    let (cli, cli_path) = pinned_executable(root, "codex-cli", "codex.cli", CLEAN_PROVIDER);
    // The app-server binding declares only the CLI capability, so it is
    // `Unsupported`; the CLI executable is gone, so it is `Unavailable`.
    let resolver = resolver_over(
        root,
        vec![
            binding(
                &app_server,
                &app_server_path,
                WorkExecutableCapabilityV1::CodexCliExecJson,
            ),
            binding(
                &cli,
                &cli_path,
                WorkExecutableCapabilityV1::CodexCliExecJson,
            ),
        ],
    );
    std::fs::remove_file(&cli_path).unwrap();

    let fixture = leased_attempt(
        root,
        "Neither backend is eligible.",
        &SnapshotShape {
            backend: WorkProviderBackendV1::CodexAppServer,
            executable: app_server,
            fallback: codex_cli_fallback(cli),
            ..SnapshotShape::default()
        },
    );
    let denial = select_with_resolver(&resolver, &fixture.attempt)
        .err()
        .expect("neither backend may start");
    assert_eq!(denial.state, WorkProviderAvailabilityV1::Unsupported);
    assert_eq!(
        denial.fallback,
        Some(WorkProviderFallbackRecordV1 {
            preferred_backend: WorkProviderBackendV1::CodexAppServer,
            preferred_route: requested_route(WorkProviderBackendV1::CodexAppServer),
            preferred_state: WorkProviderAvailabilityV1::Unsupported,
            fallback_backend: WorkProviderBackendV1::CodexCli,
            fallback_route: fallback_route(),
            fallback_state: Some(WorkProviderAvailabilityV1::Unavailable),
        })
    );

    // Sealed exactly as `run_attempt` seals it.
    settle_unstarted(
        &fixture.attempts,
        &fixture.context,
        fixture.identity(),
        &fixture.attempt,
        WorkAttemptProviderOutcomeV1::ProviderUnavailable {
            state: denial.state,
        },
        denial.fallback.clone(),
        None,
    );
    assert_eq!(fixture.state(), WorkAttemptStateV1::Failed);
    let evidence = fixture.sealed_evidence();
    assert_eq!(
        evidence.outcome,
        WorkAttemptProviderOutcomeV1::ProviderUnavailable {
            state: WorkProviderAvailabilityV1::Unsupported,
        }
    );
    assert_eq!(evidence.provider_fallback, denial.fallback);
}

/// Every backend the domain admits now maps onto a transport. The gate is
/// total over the pinned pairs and refuses only the crossed ones, which the
/// domain already makes unconstructible.
#[test]
fn every_pinned_backend_protocol_pair_maps_onto_a_transport() {
    for backend in [
        WorkProviderBackendV1::ClaudeCodeCli,
        WorkProviderBackendV1::CodexAppServer,
        WorkProviderBackendV1::CodexCli,
    ] {
        assert!(
            provider_arguments(backend, pinned_protocol(backend)).is_some(),
            "backend {backend:?} must have an admitted transport"
        );
    }
    assert_eq!(
        provider_arguments(
            WorkProviderBackendV1::CodexAppServer,
            WorkProviderProtocol::CodexAppServerJsonRpc,
        ),
        Some(Vec::new()),
        "the app-server's `app-server` argument belongs to the session client"
    );
    assert!(
        provider_arguments(
            WorkProviderBackendV1::ClaudeCodeCli,
            WorkProviderProtocol::CodexExecJson,
        )
        .is_none()
    );
}

/// A configuration that cannot be resolved at all denies every backend with a
/// transport-level state, and never with `Unsupported` — `Unsupported` is
/// reserved for a pairing the runtime genuinely does not admit.
#[test]
fn an_unresolvable_configuration_denies_every_backend_without_claiming_unsupported() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let unresolvable_root = root.join("absent-project-root");

    for backend in [
        WorkProviderBackendV1::ClaudeCodeCli,
        WorkProviderBackendV1::CodexAppServer,
        WorkProviderBackendV1::CodexCli,
    ] {
        let fixture = leased_attempt(
            root,
            "Unresolvable configuration.",
            &SnapshotShape {
                backend,
                ..SnapshotShape::default()
            },
        );
        let denial = select_provider(&unresolvable_root, &fixture.attempt)
            .err()
            .expect("a fixture executable binding never resolves");
        assert_ne!(
            denial.state,
            WorkProviderAvailabilityV1::Unsupported,
            "backend {backend:?} is an admitted pairing"
        );
        assert!(denial.fallback.is_none());
        // Nothing was started, so the lease is untouched.
        assert_eq!(fixture.state(), WorkAttemptStateV1::Leased);
        assert_eq!(
            fixture.rows.observed_states(),
            vec![WorkAttemptStateV1::Leased]
        );
    }
}

/// The gate's `provider_arguments` catch-all can only ever see a crossed pair:
/// the domain pins every protocol to exactly one backend, so such a pair
/// cannot be constructed upstream in the first place.
#[test]
fn crossed_backend_protocol_pairs_cannot_be_admitted_upstream() {
    let shape = SnapshotShape::default();
    for (backend, protocol) in [
        (
            WorkProviderBackendV1::ClaudeCodeCli,
            WorkProviderProtocol::CodexExecJson,
        ),
        (
            WorkProviderBackendV1::CodexCli,
            WorkProviderProtocol::ClaudeStreamJson,
        ),
        (
            WorkProviderBackendV1::CodexAppServer,
            WorkProviderProtocol::ClaudeStreamJson,
        ),
    ] {
        assert!(
            crossed_execution_snapshot(&shape, backend, protocol).is_err(),
            "backend {backend:?} must not admit protocol {protocol:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Missing executable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_provider_executable_seals_a_typed_denial_instead_of_panicking() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let absent = root.join("provider-that-was-never-installed");
    assert!(!absent.exists());

    let fixture = leased_attempt(
        root,
        "Spawn a provider that is not there.",
        &SnapshotShape::default(),
    );
    let admitted_environment =
        admitted_provider_environment(fixture.attempt.execution().execution_snapshot());
    execute_provider_with_environment(
        &fixture.attempts,
        &fixture.context,
        &fixture.attempt,
        &preferred(
            absent,
            WorkProviderProtocol::ClaudeStreamJson,
            &CLAUDE_STREAM_JSON_ARGV,
            requested_route(WorkProviderBackendV1::ClaudeCodeCli),
        ),
        &admitted_environment,
        Arc::new(Notify::new()),
        None,
        None,
        AttemptAdmissionTimingV1::for_test(),
    )
    .await;

    let evidence = fixture.sealed_evidence();
    assert_eq!(evidence.outcome, WorkAttemptProviderOutcomeV1::LaunchFailed);
    // Nothing ran, so there is no stream to summarize and no negotiated route.
    assert!(evidence.stdout.is_none());
    assert!(evidence.stderr.is_none());
    assert!(evidence.actual_route.is_none());
    assert_eq!(fixture.state(), WorkAttemptStateV1::Failed);
    // The denial is fenced through recovery; the attempt is never marked
    // Running for a process that does not exist.
    assert_eq!(
        fixture.rows.observed_states(),
        vec![
            WorkAttemptStateV1::Leased,
            WorkAttemptStateV1::RecoveryRequired,
            WorkAttemptStateV1::Failed,
        ]
    );
}

#[tokio::test]
async fn an_unavailable_provider_state_is_sealed_as_denial_evidence() {
    let directory = tempfile::TempDir::new().unwrap();
    let root = directory.path();
    let fixture = leased_attempt(
        root,
        "Provider is unsupported here.",
        &SnapshotShape::default(),
    );

    settle_unstarted(
        &fixture.attempts,
        &fixture.context,
        fixture.identity(),
        &fixture.attempt,
        WorkAttemptProviderOutcomeV1::ProviderUnavailable {
            state: WorkProviderAvailabilityV1::Unsupported,
        },
        None,
        None,
    );

    assert_eq!(fixture.state(), WorkAttemptStateV1::Failed);
    assert_eq!(
        fixture.sealed_evidence().outcome,
        WorkAttemptProviderOutcomeV1::ProviderUnavailable {
            state: WorkProviderAvailabilityV1::Unsupported,
        }
    );
}

// ---------------------------------------------------------------------------
// Live-process ownership registry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_process_registry_admits_one_live_owner_per_attempt() {
    let directory = tempfile::TempDir::new().unwrap();
    let fixture = leased_attempt(
        directory.path(),
        "Registry only.",
        &SnapshotShape::default(),
    );
    let registry = WorkAttemptProcessRegistryV1::default();

    let cancel = registry
        .register(fixture.identity())
        .expect("the first claim owns the attempt");
    assert!(
        registry.register(fixture.identity()).is_none(),
        "a second claim must not displace the live owner"
    );

    // The registered channel really reaches the live owner.
    let waiter = tokio::spawn(async move { cancel.notified().await });
    while !waiter.is_finished() {
        registry.signal_test_cancellation(fixture.identity());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    waiter.await.unwrap();

    // Releasing returns the attempt to the unowned pool, and signalling an
    // attempt this daemon no longer owns is a silent no-op, not a panic.
    registry.release(fixture.identity());
    registry.signal_test_cancellation(fixture.identity());
    assert!(
        registry.register(fixture.identity()).is_some(),
        "release must return the attempt to the unowned pool"
    );
}

#[test]
fn the_process_registry_isolates_equal_attempt_ids_by_worktree() {
    let directory = tempfile::TempDir::new().unwrap();
    let fixture = leased_attempt(
        directory.path(),
        "Registry scope.",
        &SnapshotShape::default(),
    );
    let first_worktree: WorktreeId = id("worktree.registry.first");
    let second_worktree: WorktreeId = id("worktree.registry.second");
    let registry = WorkAttemptProcessRegistryV1::default();

    assert!(
        registry
            .register_for_worktree(fixture.identity(), &first_worktree)
            .is_some()
    );
    assert!(
        registry
            .register_for_worktree(fixture.identity(), &second_worktree)
            .is_some(),
        "another worktree owns a distinct scoped attempt identity"
    );
    assert!(registry.holds_attempt(&first_worktree, fixture.identity()));
    assert!(registry.holds_attempt(&second_worktree, fixture.identity()));

    registry.release_for_worktree(fixture.identity(), &first_worktree);
    assert!(!registry.holds_attempt(&first_worktree, fixture.identity()));
    assert!(registry.holds_attempt(&second_worktree, fixture.identity()));
}
