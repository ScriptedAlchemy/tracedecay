use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use tracedecay_application::{
    WorkAttemptPersistencePort, WorkExecutionError, WorkExecutionService,
    WorkProviderExecutionError, WorkProviderExecutionPort, WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    AttemptId, ProviderId, UtcMicros, WorkArtifactId, WorkArtifactRefV1, WorkAttemptIdentityV1,
    WorkAttemptProgressV1, WorkAttemptProjectionBindingV1, WorkAttemptStateV1, WorkAttemptV1,
    WorkAuthority, WorkCancellationAcknowledgementV1, WorkCancellationRequestV1,
    WorkCancellationStateV1, WorkLeaseFenceV1, WorkProjection, WorkProjectionSnapshotV1,
    WorkProviderRouteId, WorkProviderRouteV1, WorkRecoveryStateV1, WorkRestartReasonV1,
    WorkTerminalEvidenceV1, canonical_sha256,
};

use crate::application::event_lane::{self, ActivityFamilyV1};
use crate::global_db::RegisteredGlobalDb;
use crate::sessions::codex_app_server::{
    CodexAppServerCancellation, CodexAppServerSummary, CodexAppServerSummaryConfig,
    run_prompt_with_codex_app_server_cancellable,
};

const CODEX_PROVIDER_ID: &str = "provider.work.codex-app-server";
const CODEX_ROUTE_ID: &str = "route.work.codex-app-server.v1";
const CODEX_THREAD_SOURCE: &str = "tracedecay_work";

#[derive(Clone)]
pub(crate) struct CodexAppServerWorkProviderV1<S> {
    storage: S,
    authority: WorkAuthority,
    config: CodexAppServerSummaryConfig,
    executions: Arc<
        Mutex<
            BTreeMap<
                WorkAttemptIdentityV1,
                (
                    CodexAppServerCancellation,
                    Option<JoinHandle<Result<CodexAppServerSummary, String>>>,
                ),
            >,
        >,
    >,
}

impl<S> CodexAppServerWorkProviderV1<S>
where
    S: WorkStoragePort + Clone,
{
    pub(crate) fn new(
        storage: S,
        authority: WorkAuthority,
        config: CodexAppServerSummaryConfig,
    ) -> Self {
        Self {
            storage,
            authority,
            config,
            executions: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub(crate) fn route() -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
        let provider_id = ProviderId::new(CODEX_PROVIDER_ID).map_err(|error| {
            WorkProviderExecutionError::Rejected(format!(
                "canonical Codex provider id is invalid: {error}"
            ))
        })?;
        let route_id = WorkProviderRouteId::new(CODEX_ROUTE_ID).map_err(|error| {
            WorkProviderExecutionError::Rejected(format!(
                "canonical Codex route id is invalid: {error}"
            ))
        })?;
        WorkProviderRouteV1::new(provider_id, route_id).map_err(|error| {
            WorkProviderExecutionError::Rejected(format!(
                "canonical Codex route is invalid: {error}"
            ))
        })
    }

    pub(crate) fn is_ready(&self) -> bool {
        executable_is_resolvable(&self.config.codex_bin)
    }

    fn projection(
        &self,
        attempt: &WorkAttemptV1,
    ) -> Result<WorkProjection, WorkProviderExecutionError> {
        let history =
            WorkStoragePort::load(&self.storage, &self.authority, attempt.identity().task_id())
                .map_err(map_work_storage_error)?;
        let projection = WorkProjection::rebuild(&history).map_err(|error| {
            WorkProviderExecutionError::Rejected(format!(
                "canonical Work projection is invalid: {error}"
            ))
        })?;
        if !projection.is_execution_admitted() {
            return Err(WorkProviderExecutionError::Rejected(
                "Work projection is not admitted for execution".to_owned(),
            ));
        }
        if projection.version() != attempt.projection_binding().work_version() {
            return Err(WorkProviderExecutionError::Rejected(
                "Work projection changed after attempt admission".to_owned(),
            ));
        }
        Ok(projection)
    }

    fn finish(
        &self,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<CodexAppServerSummary>, String> {
        let (cancellation, handle) = {
            let mut executions = self
                .executions
                .lock()
                .map_err(|_| "Codex Work execution registry lock failed".to_owned())?;
            let (cancellation, handle) = executions
                .get_mut(identity)
                .ok_or_else(|| "Codex Work execution is not active".to_owned())?;
            let handle = handle
                .take()
                .ok_or_else(|| "Codex Work execution completion is already claimed".to_owned())?;
            (cancellation.clone(), handle)
        };
        let outcome = handle.join();
        let mut executions = self
            .executions
            .lock()
            .map_err(|_| "Codex Work execution registry lock failed".to_owned())?;
        executions.remove(identity);
        let outcome = outcome.map_err(|_| "Codex Work execution thread panicked".to_owned())?;
        if cancellation.is_cancelled() {
            Ok(None)
        } else {
            outcome.map(Some)
        }
    }

    fn request_stop(
        &self,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<(), WorkProviderExecutionError> {
        let executions = self.executions.lock().map_err(|_| {
            WorkProviderExecutionError::Unavailable(
                "Codex Work execution registry lock failed".to_owned(),
            )
        })?;
        let (cancellation, _) = executions.get(identity).ok_or_else(|| {
            WorkProviderExecutionError::Rejected("Codex Work execution is not active".to_owned())
        })?;
        cancellation.cancel();
        Ok(())
    }
}

impl<S> WorkProviderExecutionPort for CodexAppServerWorkProviderV1<S>
where
    S: WorkStoragePort + Clone + Send + Sync + 'static,
{
    fn start(
        &self,
        attempt: &WorkAttemptV1,
    ) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
        if !self.is_ready() {
            return Err(WorkProviderExecutionError::Unavailable(format!(
                "Codex app-server executable is unavailable: {}",
                self.config.codex_bin
            )));
        }
        let projection = self.projection(attempt)?;
        let prompt = format!(
            "Execute the admitted TraceDecay Work task.\nTask: {}\nTitle: {}\n\
             Use the current project context and return a concise completion report.",
            projection.task_id().as_str(),
            projection.title()
        );
        let mut executions = self.executions.lock().map_err(|_| {
            WorkProviderExecutionError::Unavailable(
                "Codex Work execution registry lock failed".to_owned(),
            )
        })?;
        if executions.contains_key(attempt.identity()) {
            return Err(WorkProviderExecutionError::Rejected(
                "Codex Work execution is already active".to_owned(),
            ));
        }
        let cancellation = CodexAppServerCancellation::default();
        let cancellation_for_run = cancellation.clone();
        let config = self.config.clone();
        let handle = std::thread::spawn(move || {
            run_prompt_with_codex_app_server_cancellable(
                &prompt,
                &config,
                CODEX_THREAD_SOURCE,
                &cancellation_for_run,
            )
            .map_err(|error| error.to_string())
        });
        executions.insert(attempt.identity().clone(), (cancellation, Some(handle)));
        Self::route()
    }

    fn request_cancellation(
        &self,
        attempt: &WorkAttemptV1,
        _request: &WorkCancellationRequestV1,
    ) -> Result<(), WorkProviderExecutionError> {
        self.request_stop(attempt.identity())
    }
}

pub(crate) struct DaemonWorkRuntimeV1<'a, S>
where
    S: WorkAttemptPersistencePort + WorkStoragePort + Clone,
{
    authority: WorkAuthority,
    storage: S,
    provider: CodexAppServerWorkProviderV1<S>,
    execution: WorkExecutionService<S, CodexAppServerWorkProviderV1<S>>,
    observation_db: &'a RegisteredGlobalDb,
    project_root: PathBuf,
}

impl<'a, S> DaemonWorkRuntimeV1<'a, S>
where
    S: WorkAttemptPersistencePort + WorkStoragePort + Clone + Send + Sync + 'static,
{
    pub(crate) fn new(
        authority: WorkAuthority,
        storage: S,
        config: CodexAppServerSummaryConfig,
        observation_db: &'a RegisteredGlobalDb,
        project_root: PathBuf,
    ) -> Self {
        let provider =
            CodexAppServerWorkProviderV1::new(storage.clone(), authority.clone(), config);
        let execution = WorkExecutionService::new(storage.clone(), provider.clone());
        Self {
            authority,
            storage,
            provider,
            execution,
            observation_db,
            project_root,
        }
    }

    pub(crate) fn provider_route(&self) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
        CodexAppServerWorkProviderV1::<S>::route()
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.provider.is_ready() && event_lane::enabled(Some(self.observation_db))
    }

    pub(crate) async fn acquire_lease(
        &self,
        snapshot: &WorkProjectionSnapshotV1,
        identity: WorkAttemptIdentityV1,
        lease: WorkLeaseFenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let projection = snapshot
            .projections()
            .iter()
            .find(|projection| projection.task_id() == identity.task_id())
            .ok_or(WorkExecutionError::NotFound)?;
        let binding = WorkAttemptProjectionBindingV1::new(
            snapshot.generation_id().clone(),
            snapshot.sequence(),
            projection.version(),
        )?;
        let leased = self.execution.acquire_lease(
            &self.authority,
            snapshot,
            identity,
            binding,
            lease,
            self.provider_route()?,
        )?;
        self.publish_activity("leased").await;
        Ok(leased)
    }

    pub(crate) async fn start(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        recovery: WorkRecoveryStateV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let running = match self
            .execution
            .start(&self.authority, identity, lease, recovery)
        {
            Ok(running) => running,
            Err(error) => {
                if self.provider.request_stop(identity).is_ok() {
                    let _ = self.finish_provider(identity).await;
                }
                return Err(error);
            }
        };
        self.publish_activity("running").await;
        Ok(running)
    }

    pub(crate) async fn publish_progress(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        progress: WorkAttemptProgressV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let attempt =
            self.execution
                .publish_progress(&self.authority, identity, lease, progress)?;
        self.publish_activity("progress").await;
        Ok(attempt)
    }

    pub(crate) async fn publish_artifact(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        artifact: WorkArtifactRefV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let attempt =
            self.execution
                .publish_artifact(&self.authority, identity, lease, artifact)?;
        self.publish_activity("artifact").await;
        Ok(attempt)
    }

    pub(crate) async fn finish(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        observed_at: UtcMicros,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let provider = self.provider.clone();
        let identity_for_join = identity.clone();
        let outcome = tokio::task::spawn_blocking(move || provider.finish(&identity_for_join))
            .await
            .map_err(|error| {
                WorkProviderExecutionError::Unavailable(format!(
                    "Codex Work completion task failed: {error}"
                ))
            })?;
        let terminal = match outcome {
            Ok(Some(summary)) => {
                let digest = canonical_sha256(&summary.text).map_err(|error| {
                    WorkProviderExecutionError::Rejected(format!(
                        "Codex Work artifact digest failed: {error}"
                    ))
                })?;
                let artifact_id = artifact_id(identity.attempt_id())?;
                let artifact = WorkArtifactRefV1::new(
                    artifact_id,
                    digest.clone(),
                    u64::try_from(summary.text.len()).map_err(|_| {
                        WorkProviderExecutionError::Rejected(
                            "Codex Work artifact length overflowed".to_owned(),
                        )
                    })?,
                )?;
                self.publish_artifact(identity, lease, artifact).await?;
                self.publish_progress(identity, lease, WorkAttemptProgressV1::new(1, 1)?)
                    .await?;
                WorkTerminalEvidenceV1::succeeded(digest, observed_at)?
            }
            Ok(None) => {
                let current =
                    WorkAttemptPersistencePort::load(&self.storage, &self.authority, identity)
                        .map_err(WorkExecutionError::Persistence)?
                        .ok_or(WorkExecutionError::NotFound)?;
                let WorkCancellationStateV1::Requested(request) = current.cancellation() else {
                    return Err(WorkExecutionError::TerminalConflict);
                };
                self.execution.acknowledge_cancellation(
                    &self.authority,
                    identity,
                    lease,
                    WorkCancellationAcknowledgementV1::new(request.clone(), observed_at)?,
                )?;
                self.publish_activity("cancellation_acknowledged").await;
                let digest = canonical_sha256(&(
                    "tracedecay.work.codex.cancelled.v1",
                    identity,
                    observed_at,
                ))
                .map_err(|error| {
                    WorkProviderExecutionError::Rejected(format!(
                        "Codex Work cancellation evidence failed: {error}"
                    ))
                })?;
                WorkTerminalEvidenceV1::cancelled(digest, observed_at)?
            }
            Err(message) => {
                let digest = canonical_sha256(&(
                    "tracedecay.work.codex.failed.v1",
                    identity,
                    message.as_str(),
                    observed_at,
                ))
                .map_err(|error| {
                    WorkProviderExecutionError::Rejected(format!(
                        "Codex Work failure evidence failed: {error}"
                    ))
                })?;
                WorkTerminalEvidenceV1::failed(digest, observed_at)?
            }
        };
        let completed = self
            .execution
            .terminalize(&self.authority, identity, lease, terminal)?;
        self.publish_activity(attempt_state_key(completed.state()))
            .await;
        Ok(completed)
    }

    pub(crate) async fn cancel(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        request: WorkCancellationRequestV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        if let Some(current) = self.attempt(identity)?
            && current.is_terminal()
        {
            if current.lease() != lease {
                return Err(WorkExecutionError::StaleLease);
            }
            return match current.cancellation() {
                WorkCancellationStateV1::Acknowledged(acknowledgement)
                    if acknowledgement.request() == &request =>
                {
                    Ok(current)
                }
                _ => Err(WorkExecutionError::TerminalConflict),
            };
        }
        let acknowledged_at = request.requested_at();
        let _requested =
            self.execution
                .request_cancellation(&self.authority, identity, lease, request)?;
        self.publish_activity("cancellation_requested").await;
        self.finish(identity, lease, acknowledged_at).await
    }

    pub(crate) async fn recover(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        reason: WorkRestartReasonV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let attempt = self
            .execution
            .require_recovery(&self.authority, identity, lease, reason)?;
        self.provider.request_stop(identity)?;
        self.finish_provider(identity).await?;
        self.publish_activity("recovery_required").await;
        Ok(attempt)
    }

    pub(crate) fn renew_lease(
        &self,
        identity: &WorkAttemptIdentityV1,
        expected: &WorkLeaseFenceV1,
        replacement: WorkLeaseFenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        self.execution
            .renew_lease(&self.authority, identity, expected, replacement)
    }

    pub(crate) fn attempt(
        &self,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<Option<WorkAttemptV1>, WorkExecutionError> {
        WorkAttemptPersistencePort::load(&self.storage, &self.authority, identity)
            .map_err(WorkExecutionError::Persistence)
    }

    pub(crate) async fn terminalize(
        &self,
        identity: &WorkAttemptIdentityV1,
        lease: &WorkLeaseFenceV1,
        terminal: WorkTerminalEvidenceV1,
    ) -> Result<WorkAttemptV1, WorkExecutionError> {
        let was_terminal = self
            .attempt(identity)?
            .is_some_and(|attempt| attempt.is_terminal());
        let completed = self
            .execution
            .terminalize(&self.authority, identity, lease, terminal)?;
        if !was_terminal {
            self.publish_activity(attempt_state_key(completed.state()))
                .await;
        }
        Ok(completed)
    }

    async fn publish_activity(&self, detail: &str) {
        event_lane::publish(
            self.observation_db,
            ActivityFamilyV1::Task,
            &self.project_root,
            Some(self.authority.project_id().as_str()),
            1,
            Some(detail),
        )
        .await;
    }

    async fn finish_provider(
        &self,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<(), WorkExecutionError> {
        let provider = self.provider.clone();
        let identity_for_join = identity.clone();
        let _ = tokio::task::spawn_blocking(move || provider.finish(&identity_for_join))
            .await
            .map_err(|error| {
                WorkProviderExecutionError::Unavailable(format!(
                    "Codex Work execution cleanup failed: {error}"
                ))
            })?
            .map_err(WorkProviderExecutionError::Unavailable)?;
        Ok(())
    }
}

fn map_work_storage_error(error: WorkStorageError) -> WorkProviderExecutionError {
    WorkProviderExecutionError::Unavailable(format!(
        "canonical Work projection is unavailable: {error}"
    ))
}

const fn attempt_state_key(state: WorkAttemptStateV1) -> &'static str {
    match state {
        WorkAttemptStateV1::Leased => "leased",
        WorkAttemptStateV1::Running => "running",
        WorkAttemptStateV1::CancellationRequested => "cancellation_requested",
        WorkAttemptStateV1::CancellationAcknowledged => "cancellation_acknowledged",
        WorkAttemptStateV1::CancellationEscalated => "cancellation_escalated",
        WorkAttemptStateV1::RecoveryRequired => "recovery_required",
        WorkAttemptStateV1::Succeeded => "succeeded",
        WorkAttemptStateV1::Failed => "failed",
        WorkAttemptStateV1::Cancelled => "cancelled",
    }
}

fn artifact_id(attempt_id: &AttemptId) -> Result<WorkArtifactId, WorkProviderExecutionError> {
    WorkArtifactId::new(format!("artifact.work.codex.{}", attempt_id.as_str())).map_err(|error| {
        WorkProviderExecutionError::Rejected(format!(
            "canonical Codex artifact id is invalid: {error}"
        ))
    })
}

fn executable_is_resolvable(executable: &str) -> bool {
    let path = Path::new(executable);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(executable).is_file())
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use tracedecay_application::{
        AcceptProposalCommand, AdmitExecutionCommand, CancellationContext, CapabilityGrantSnapshot,
        CreateWorkCommand, Deadline, DisclosureClass, RequestContext, RequestId, ResolvedScope,
        ReviewProposalCommand, WorkService,
    };
    use tracedecay_domain::{
        ActorId, AttemptId, ManifestDigest, ProjectId, ProjectionGenerationId, ProposalId,
        RepositoryId, RunId, TaskId, WorkCancellationRequestId, WorkFenceEpochV1, WorkLeaseId,
        WorkProjectionCoverageV1, WorkProjectionSequenceV1, WorkVersion, WorktreeId,
    };
    use tracedecay_rusqlite_runtime::work::WorkSqliteStorage;
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;
    use crate::application::event_lane;

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

    fn context(project_id: ProjectId) -> RequestContext {
        let scope = ResolvedScope::new(
            project_id,
            id::<RepositoryId>("repository.work.daemon"),
            id::<WorktreeId>("worktree.work.daemon"),
            None,
        )
        .unwrap();
        let grant = CapabilityGrantSnapshot::new(
            id("grant.work.daemon"),
            1,
            digest('a'),
            id::<ActorId>("actor.work.issuer"),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            BTreeSet::from([CapabilityId::new("capability.work.daemon").unwrap()]),
            BTreeSet::from([UseCaseId::new("use-case.work.daemon").unwrap()]),
            DisclosureClass::Sensitive,
        )
        .unwrap();
        RequestContext::new(
            id("actor.work.daemon"),
            scope,
            grant,
            RequestId::new("request.work.daemon").unwrap(),
            Deadline::new(UtcMicros(9_000)).unwrap(),
            CancellationContext::active("cancel.work.daemon").unwrap(),
        )
        .unwrap()
    }

    fn authority(context: &RequestContext) -> WorkAuthority {
        WorkAuthority::new(
            context.scope().project_id.clone(),
            context.scope().repository_id.clone(),
            context.scope().worktree_id.clone(),
            context.actor().clone(),
            context.grant().digest.clone(),
        )
        .unwrap()
    }

    fn lease(epoch: u64) -> WorkLeaseFenceV1 {
        WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.work.daemon"),
            WorkFenceEpochV1::new(epoch).unwrap(),
        )
        .unwrap()
    }

    fn identity(task_id: &TaskId, suffix: &str) -> WorkAttemptIdentityV1 {
        WorkAttemptIdentityV1::new(
            task_id.clone(),
            id::<RunId>(&format!("run.work.daemon.{suffix}")),
            id::<AttemptId>(&format!("attempt.work.daemon.{suffix}")),
        )
        .unwrap()
    }

    fn install_codex_fixture(path: &Path) {
        fs::write(
            path,
            r#"#!/usr/bin/env python3
import json
import sys

for line in sys.stdin:
    message = json.loads(line)
    request_id = message.get("id")
    method = message.get("method")
    if request_id == 0:
        print(json.dumps({"jsonrpc": "2.0", "id": 0, "result": {}}), flush=True)
    elif request_id == 1:
        print(json.dumps({"jsonrpc": "2.0", "id": 1, "result": {"thread": {"id": "thread.work.fixture", "model": "codex-work-fixture"}}}), flush=True)
    elif request_id == 2 and method == "turn/start":
        print(json.dumps({"method": "item/completed", "params": {"model": "codex-work-fixture", "item": {"content": [{"type": "output_text", "text": "fixture work completed"}]}}}), flush=True)
        print(json.dumps({"method": "turn/completed"}), flush=True)
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn install_stubborn_codex_fixture(path: &Path, descendant_pid_path: &Path) {
        fs::write(
            path,
            format!(
                r#"#!/usr/bin/env python3
import json
import subprocess
import sys
import time

descendant = subprocess.Popen([
    sys.executable,
    "-c",
    "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(30)",
])
with open({pid_path:?}, "w", encoding="utf-8") as handle:
    handle.write(str(descendant.pid))

for line in sys.stdin:
    message = json.loads(line)
    request_id = message.get("id")
    if request_id == 0:
        print(json.dumps({{"jsonrpc": "2.0", "id": 0, "result": {{}}}}), flush=True)
    elif request_id == 1:
        print(json.dumps({{"jsonrpc": "2.0", "id": 1, "result": {{"thread": {{"id": "thread.work.stubborn"}}}}}}), flush=True)
    elif request_id == 2:
        while True:
            time.sleep(1)
"#,
                pid_path = descendant_pid_path.to_string_lossy(),
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn prepare_work(
        storage: &WorkSqliteStorage,
        context: &RequestContext,
    ) -> (TaskId, WorkProjectionSnapshotV1) {
        let service = WorkService::new(storage.clone());
        let task_id = id::<TaskId>("task.work.daemon");
        service
            .create(
                context,
                CreateWorkCommand {
                    task_id: task_id.clone(),
                    title: "Run the daemon Codex fixture".to_owned(),
                    dependencies: BTreeSet::new(),
                    command_id: id("command.work.daemon.create"),
                    occurred_at: UtcMicros(10),
                },
            )
            .unwrap();
        service
            .accept_proposal(
                context,
                AcceptProposalCommand {
                    review: ReviewProposalCommand {
                        task_id: task_id.clone(),
                        proposal_id: id::<ProposalId>("proposal.work.daemon"),
                        proposal_digest: digest('b'),
                        expected_version: WorkVersion::initial(),
                        command_id: id("command.work.daemon.proposal"),
                        occurred_at: UtcMicros(20),
                    },
                },
            )
            .unwrap();
        service
            .admit_execution(
                context,
                AdmitExecutionCommand {
                    task_id: task_id.clone(),
                    expected_version: WorkVersion::new(2).unwrap(),
                    command_id: id("command.work.daemon.admit"),
                    occurred_at: UtcMicros(30),
                },
            )
            .unwrap();
        let projection = service.load(context, &task_id).unwrap();
        let snapshot = WorkProjectionSnapshotV1::new(
            id::<ProjectionGenerationId>("generation.work.daemon"),
            WorkProjectionSequenceV1::new(3),
            vec![projection],
            WorkProjectionCoverageV1::complete(1, 1).unwrap(),
        )
        .unwrap();
        (task_id, snapshot)
    }

    #[tokio::test]
    async fn codex_runtime_covers_fence_terminal_cancel_resume_recovery_and_sse() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_id = id::<ProjectId>("project.work.daemon");
        let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().unwrap(),
            project.path(),
            project_id.clone(),
        )
        .await
        .unwrap();
        let observation_db = host.project_observation_database_for_test().unwrap();
        let storage = observation_db.work_storage().unwrap();
        let context = context(project_id);
        let owner = authority(&context);
        let (task_id, snapshot) = prepare_work(&storage, &context);
        let fixture = project.path().join("codex-work-fixture");
        install_codex_fixture(&fixture);
        let runtime = DaemonWorkRuntimeV1::new(
            owner.clone(),
            storage.clone(),
            CodexAppServerSummaryConfig {
                codex_bin: fixture.to_string_lossy().into_owned(),
                model: Some("codex-work-fixture".to_owned()),
                timeout: Duration::from_secs(5),
            },
            observation_db,
            project.path().to_path_buf(),
        );
        assert!(runtime.is_ready());
        assert_eq!(
            runtime.provider_route().unwrap().provider_id().as_str(),
            CODEX_PROVIDER_ID
        );
        let mut activity = event_lane::subscribe().unwrap();

        let successful_identity = identity(&task_id, "success");
        runtime
            .acquire_lease(&snapshot, successful_identity.clone(), lease(1))
            .await
            .unwrap();
        runtime
            .start(&successful_identity, &lease(1), WorkRecoveryStateV1::Fresh)
            .await
            .unwrap();
        let running = runtime
            .publish_progress(
                &successful_identity,
                &lease(1),
                WorkAttemptProgressV1::new(0, 1).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(running.state(), WorkAttemptStateV1::Running);
        assert_eq!(running.progress().unwrap().completed(), 0);
        let renewed = runtime
            .renew_lease(&successful_identity, &lease(1), lease(2))
            .unwrap();
        assert_eq!(renewed.lease(), &lease(2));
        let stale_terminal = WorkTerminalEvidenceV1::failed(digest('c'), UtcMicros(35)).unwrap();
        assert_eq!(
            runtime
                .terminalize(&successful_identity, &lease(1), stale_terminal)
                .await
                .unwrap_err(),
            WorkExecutionError::StaleLease
        );
        let completed = runtime
            .finish(&successful_identity, &lease(2), UtcMicros(40))
            .await
            .unwrap();
        assert_eq!(completed.state(), WorkAttemptStateV1::Succeeded);
        assert_eq!(completed.progress().unwrap().completed(), 1);
        assert_eq!(completed.artifacts().len(), 1);
        let replayed = runtime
            .terminalize(
                &successful_identity,
                &lease(2),
                completed.terminal().unwrap().clone(),
            )
            .await
            .unwrap();
        assert_eq!(replayed, completed);
        let pulse = tokio::time::timeout(Duration::from_secs(1), activity.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pulse.pulse.family, ActivityFamilyV1::Task);

        let cancelled_identity = identity(&task_id, "cancelled");
        runtime
            .acquire_lease(&snapshot, cancelled_identity.clone(), lease(1))
            .await
            .unwrap();
        runtime
            .start(&cancelled_identity, &lease(1), WorkRecoveryStateV1::Fresh)
            .await
            .unwrap();
        let cancellation = WorkCancellationRequestV1::new(
            id::<WorkCancellationRequestId>("cancel.work.daemon.request"),
            UtcMicros(50),
        )
        .unwrap();
        let cancelled = runtime
            .cancel(&cancelled_identity, &lease(1), cancellation.clone())
            .await
            .unwrap();
        assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
        assert!(matches!(
            cancelled.cancellation(),
            WorkCancellationStateV1::Acknowledged(_)
        ));
        assert_eq!(
            runtime
                .cancel(&cancelled_identity, &lease(1), cancellation)
                .await
                .unwrap(),
            cancelled
        );

        let resumed_identity = identity(&task_id, "resumed");
        runtime
            .acquire_lease(&snapshot, resumed_identity.clone(), lease(1))
            .await
            .unwrap();
        runtime
            .start(
                &resumed_identity,
                &lease(1),
                WorkRecoveryStateV1::Resumed {
                    source_attempt_id: cancelled_identity.attempt_id().clone(),
                    checkpoint: None,
                },
            )
            .await
            .unwrap();
        let resumed = runtime
            .finish(&resumed_identity, &lease(1), UtcMicros(70))
            .await
            .unwrap();
        assert!(matches!(
            resumed.recovery(),
            WorkRecoveryStateV1::Resumed { .. }
        ));

        let restarted_identity = identity(&task_id, "restarted");
        runtime
            .acquire_lease(&snapshot, restarted_identity.clone(), lease(1))
            .await
            .unwrap();
        runtime
            .start(
                &restarted_identity,
                &lease(1),
                WorkRecoveryStateV1::Restarted {
                    source_attempt_id: resumed_identity.attempt_id().clone(),
                    reason: WorkRestartReasonV1::ProcessLost,
                },
            )
            .await
            .unwrap();
        let restarted = runtime
            .finish(&restarted_identity, &lease(1), UtcMicros(80))
            .await
            .unwrap();
        assert!(matches!(
            restarted.recovery(),
            WorkRecoveryStateV1::Restarted { .. }
        ));

        let recovery_identity = identity(&task_id, "recovery");
        runtime
            .acquire_lease(&snapshot, recovery_identity.clone(), lease(1))
            .await
            .unwrap();
        runtime
            .start(&recovery_identity, &lease(1), WorkRecoveryStateV1::Fresh)
            .await
            .unwrap();
        let recovery = runtime
            .recover(
                &recovery_identity,
                &lease(1),
                WorkRestartReasonV1::ProviderUnavailable,
            )
            .await
            .unwrap();
        assert_eq!(recovery.state(), WorkAttemptStateV1::RecoveryRequired);
        assert_eq!(
            runtime.attempt(&recovery_identity).unwrap().unwrap(),
            recovery
        );
        assert!(
            storage
                .execution_attempt_history(&owner, &successful_identity)
                .unwrap()
                .len()
                >= 6
        );
    }

    #[tokio::test]
    async fn codex_cancel_terminates_and_reaps_stubborn_process_tree() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().unwrap();
        let project_id = id::<ProjectId>("project.work.daemon.cancel");
        let host = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().unwrap(),
            project.path(),
            project_id.clone(),
        )
        .await
        .unwrap();
        let observation_db = host.project_observation_database_for_test().unwrap();
        let storage = observation_db.work_storage().unwrap();
        let context = context(project_id);
        let owner = authority(&context);
        let (task_id, snapshot) = prepare_work(&storage, &context);
        let fixture = project.path().join("codex-work-stubborn-fixture");
        let descendant_pid_path = project.path().join("codex-work-descendant.pid");
        install_stubborn_codex_fixture(&fixture, &descendant_pid_path);
        let runtime = DaemonWorkRuntimeV1::new(
            owner,
            storage,
            CodexAppServerSummaryConfig {
                codex_bin: fixture.to_string_lossy().into_owned(),
                model: None,
                timeout: Duration::from_secs(2),
            },
            observation_db,
            project.path().to_path_buf(),
        );
        let attempt_identity = identity(&task_id, "stubborn-cancel");
        runtime
            .acquire_lease(&snapshot, attempt_identity.clone(), lease(1))
            .await
            .unwrap();
        runtime
            .start(&attempt_identity, &lease(1), WorkRecoveryStateV1::Fresh)
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !descendant_pid_path.is_file() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let descendant_pid = fs::read_to_string(&descendant_pid_path)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();

        let cancelled = runtime
            .cancel(
                &attempt_identity,
                &lease(1),
                WorkCancellationRequestV1::new(
                    id::<WorkCancellationRequestId>("cancel.work.daemon.stubborn"),
                    UtcMicros(50),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.state(), WorkAttemptStateV1::Cancelled);
        assert!(matches!(
            cancelled.cancellation(),
            WorkCancellationStateV1::Acknowledged(_)
        ));

        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while unsafe { kill(descendant_pid, 0) } == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_ne!(
            unsafe { kill(descendant_pid, 0) },
            0,
            "Codex Work cancellation must leave no provider descendant alive"
        );
    }
}
