use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_application::{
    WorkProviderExecutionError, WorkProviderExecutionPort, WorkProviderRun,
    WorkProviderSettlementV1, WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    ManifestDigest, ProviderId, UtcMicros, WorkAttemptV1, WorkAuthority, WorkProjection,
    WorkProviderBackendV1, WorkProviderRouteId, WorkProviderRouteV1,
};

use crate::config::work_executable_binding::{
    WorkExecutableBindingError, WorkExecutableBindingResolver,
};
use crate::sessions::codex_app_server::{
    CodexAppServerCancellation, CodexAppServerSummaryConfig, run_work_with_codex_app_server,
};

use super::native_cli::{
    NativeCliCancellation, NativeCliKind, NativeCliLaunchPlan, NativeCliWorkRun,
};

const CODEX_PROVIDER_ID: &str = "provider.work.codex-app-server";
const CODEX_ROUTE_ID: &str = "route.work.codex-app-server.v1";
const CLAUDE_PROVIDER_ID: &str = "provider.work.claude-code-cli";
const CLAUDE_ROUTE_ID: &str = "route.work.claude-code-cli.v1";
const CODEX_CLI_PROVIDER_ID: &str = "provider.work.codex-cli";
const CODEX_CLI_ROUTE_ID: &str = "route.work.codex-cli.v1";
const CODEX_THREAD_SOURCE: &str = "tracedecay_work";

#[derive(Clone)]
pub(crate) struct NativeWorkProviderConfigV1 {
    codex_app_server: CodexAppServerSummaryConfig,
    executable_bindings: Arc<dyn WorkExecutableBindingResolver + Send + Sync>,
    configuration_digest: ManifestDigest,
    project_root: PathBuf,
}

impl NativeWorkProviderConfigV1 {
    pub(crate) fn from_registered(
        codex_app_server: CodexAppServerSummaryConfig,
        executable_bindings: Arc<dyn WorkExecutableBindingResolver + Send + Sync>,
        configuration_digest: ManifestDigest,
        project_root: PathBuf,
    ) -> Self {
        Self {
            codex_app_server,
            executable_bindings,
            configuration_digest,
            project_root,
        }
    }
}

/// Builds native provider executions for admitted Work attempts.
#[derive(Clone)]
pub(crate) struct NativeWorkProviderV1<S> {
    storage: S,
    authority: WorkAuthority,
    config: NativeWorkProviderConfigV1,
}

impl<S> NativeWorkProviderV1<S>
where
    S: WorkStoragePort + Clone,
{
    pub(crate) const fn new(
        storage: S,
        authority: WorkAuthority,
        config: NativeWorkProviderConfigV1,
    ) -> Self {
        Self {
            storage,
            authority,
            config,
        }
    }

    pub(crate) fn codex_app_server_route() -> Result<WorkProviderRouteV1, WorkProviderExecutionError>
    {
        route(CODEX_PROVIDER_ID, CODEX_ROUTE_ID)
    }

    pub(crate) fn claude_code_route() -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
        route(CLAUDE_PROVIDER_ID, CLAUDE_ROUTE_ID)
    }

    pub(crate) fn codex_cli_route() -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
        route(CODEX_CLI_PROVIDER_ID, CODEX_CLI_ROUTE_ID)
    }

    fn validate_execution(
        &self,
        attempt: &WorkAttemptV1,
    ) -> Result<(), WorkProviderExecutionError> {
        let execution = attempt.execution();
        if execution.project_id() != self.authority.project_id()
            || execution.repository_id() != self.authority.repository_id()
            || execution.worktree_id() != self.authority.worktree_id()
            || execution.configuration_digest() != &self.config.configuration_digest
            || Path::new(execution.worktree_root()) != self.config.project_root
        {
            return Err(WorkProviderExecutionError::Rejected(
                "Work execution envelope does not match the registered authority".to_owned(),
            ));
        }
        Ok(())
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

    fn prompt(&self, projection: &WorkProjection, attempt: &WorkAttemptV1) -> String {
        format!(
            "Execute the admitted TraceDecay Work operation {}.\nTask: {}\nTitle: {}\n\
             Work only in the admitted current directory and return a concise completion report.",
            attempt.execution().operation().as_str(),
            projection.task_id().as_str(),
            projection.title()
        )
    }
}

impl<S> WorkProviderExecutionPort for NativeWorkProviderV1<S>
where
    S: WorkStoragePort + Clone + Send + Sync + 'static,
{
    type Run = NativeWorkRunV1;

    fn route(&self) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
        Self::codex_app_server_route()
    }

    fn supports_route(
        &self,
        requested: &WorkProviderRouteV1,
    ) -> Result<bool, WorkProviderExecutionError> {
        Ok(requested == &Self::codex_app_server_route()?
            || requested == &Self::claude_code_route()?
            || requested == &Self::codex_cli_route()?)
    }

    fn prepare(&self, attempt: &WorkAttemptV1) -> Result<Self::Run, WorkProviderExecutionError> {
        self.validate_execution(attempt)?;
        let projection = self.projection(attempt)?;
        let execution = attempt.execution();
        let prompt = self.prompt(&projection, attempt);
        let timeout =
            remaining_timeout(execution.deadline(), self.config.codex_app_server.timeout)?;
        let snapshot = execution.execution_snapshot();
        let expected_route = match execution.backend() {
            WorkProviderBackendV1::ClaudeCodeCli => Self::claude_code_route()?,
            WorkProviderBackendV1::CodexAppServer => Self::codex_app_server_route()?,
            WorkProviderBackendV1::CodexCli => Self::codex_cli_route()?,
        };
        require_exact_route(execution.route(), &expected_route)?;
        let resolved = self
            .config
            .executable_bindings
            .resolve(
                snapshot.executable(),
                execution.backend(),
                snapshot.protocol(),
            )
            .map_err(map_executable_binding_error)?;
        if resolved.configuration_revision_id() != snapshot.configuration_revision_id()
            || resolved.configuration_snapshot_id() != snapshot.configuration_snapshot_id()
            || resolved.executable() != snapshot.executable()
            || resolved.backend() != execution.backend()
            || resolved.protocol() != snapshot.protocol()
            || resolved.verified_byte_length() == 0
        {
            return Err(WorkProviderExecutionError::Rejected(
                "Work executable binding does not match the admitted execution snapshot".to_owned(),
            ));
        }
        match execution.backend() {
            WorkProviderBackendV1::CodexAppServer => {
                let mut config = self.config.codex_app_server.clone();
                config.codex_bin = resolved.canonical_path().to_string_lossy().into_owned();
                config.model = Some(execution.model().to_owned());
                Ok(NativeWorkRunV1::CodexAppServer(CodexAppServerWorkRunV1 {
                    prompt,
                    config,
                    cwd: self.config.project_root.clone(),
                    timeout,
                    cancellation: CodexAppServerCancellation::default(),
                }))
            }
            WorkProviderBackendV1::ClaudeCodeCli | WorkProviderBackendV1::CodexCli => {
                if !snapshot.environment_allowlist().is_empty()
                    || !snapshot.credential_references().is_empty()
                    || snapshot.egress() != tracedecay_domain::WorkEgressPolicy::Deny
                    || (execution.backend() == WorkProviderBackendV1::ClaudeCodeCli
                        && snapshot.approval() != tracedecay_domain::WorkApprovalPolicy::Never)
                {
                    return Err(WorkProviderExecutionError::Unavailable(
                        "Work provider policy requires an authority this adapter does not mount"
                            .to_owned(),
                    ));
                }
                let kind = match execution.backend() {
                    WorkProviderBackendV1::ClaudeCodeCli => NativeCliKind::ClaudeCode,
                    WorkProviderBackendV1::CodexCli => NativeCliKind::Codex,
                    WorkProviderBackendV1::CodexAppServer => {
                        return Err(WorkProviderExecutionError::Rejected(
                            "Work provider backend changed during preparation".to_owned(),
                        ));
                    }
                };
                Ok(NativeWorkRunV1::Cli(NativeCliWorkRun {
                    plan: NativeCliLaunchPlan {
                        executable: resolved.canonical_path().to_path_buf(),
                        kind,
                        model: execution.model().to_owned(),
                        prompt,
                        cwd: self.config.project_root.clone(),
                        timeout,
                        budget: execution.budget(),
                        approval: snapshot.approval(),
                        filesystem: snapshot.filesystem(),
                        environment: std::collections::BTreeMap::new(),
                    },
                    cancellation: NativeCliCancellation::default(),
                }))
            }
        }
    }
}

fn route(provider: &str, route: &str) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
    let provider_id = ProviderId::new(provider).map_err(|error| {
        WorkProviderExecutionError::Rejected(format!(
            "canonical Work provider id is invalid: {error}"
        ))
    })?;
    let route_id = WorkProviderRouteId::new(route).map_err(|error| {
        WorkProviderExecutionError::Rejected(format!("canonical Work route id is invalid: {error}"))
    })?;
    WorkProviderRouteV1::new(provider_id, route_id).map_err(|error| {
        WorkProviderExecutionError::Rejected(format!("canonical Work route is invalid: {error}"))
    })
}

pub(crate) enum NativeWorkRunV1 {
    CodexAppServer(CodexAppServerWorkRunV1),
    Cli(NativeCliWorkRun),
}

impl WorkProviderRun for NativeWorkRunV1 {
    fn execute(&self) -> WorkProviderSettlementV1 {
        match self {
            Self::CodexAppServer(run) => run.execute(),
            Self::Cli(run) => run.execute(),
        }
    }

    fn cancel(&self) {
        match self {
            Self::CodexAppServer(run) => run.cancel(),
            Self::Cli(run) => run.cancel(),
        }
    }
}

pub(crate) struct CodexAppServerWorkRunV1 {
    prompt: String,
    config: CodexAppServerSummaryConfig,
    cwd: PathBuf,
    timeout: Duration,
    cancellation: CodexAppServerCancellation,
}

impl WorkProviderRun for CodexAppServerWorkRunV1 {
    fn execute(&self) -> WorkProviderSettlementV1 {
        let outcome = run_work_with_codex_app_server(
            &self.prompt,
            &self.config,
            CODEX_THREAD_SOURCE,
            &self.cancellation,
            &self.cwd,
            self.timeout,
        );
        if self.cancellation.is_cancelled() {
            return WorkProviderSettlementV1::Cancelled;
        }
        match outcome {
            Ok(summary) => WorkProviderSettlementV1::Completed {
                evidence: summary.text,
            },
            Err(_) => WorkProviderSettlementV1::Failed {
                message: "Codex app-server failed before a valid terminal event".to_owned(),
            },
        }
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }
}

fn require_exact_route(
    actual: &WorkProviderRouteV1,
    expected: &WorkProviderRouteV1,
) -> Result<(), WorkProviderExecutionError> {
    if actual != expected {
        return Err(WorkProviderExecutionError::Rejected(
            "provider backend and route do not match".to_owned(),
        ));
    }
    Ok(())
}

fn map_executable_binding_error(error: WorkExecutableBindingError) -> WorkProviderExecutionError {
    match error {
        WorkExecutableBindingError::Stale { .. }
        | WorkExecutableBindingError::DigestMismatch { .. } => {
            WorkProviderExecutionError::Rejected(error.to_string())
        }
        WorkExecutableBindingError::Absent { .. }
        | WorkExecutableBindingError::Unsupported { .. }
        | WorkExecutableBindingError::Unavailable { .. } => {
            WorkProviderExecutionError::Unavailable(error.to_string())
        }
    }
}

fn remaining_timeout(
    deadline: UtcMicros,
    configured_ceiling: Duration,
) -> Result<Duration, WorkProviderExecutionError> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        WorkProviderExecutionError::Unavailable("runtime clock is unavailable".to_owned())
    })?;
    let now_micros = i64::try_from(now.as_micros()).unwrap_or(i64::MAX);
    let remaining_micros = deadline.0.saturating_sub(now_micros);
    if remaining_micros <= 0 {
        return Ok(Duration::ZERO);
    }
    Ok(
        Duration::from_micros(u64::try_from(remaining_micros).unwrap_or(u64::MAX))
            .min(configured_ceiling),
    )
}

fn map_work_storage_error(error: WorkStorageError) -> WorkProviderExecutionError {
    WorkProviderExecutionError::Unavailable(format!(
        "canonical Work projection is unavailable: {error}"
    ))
}
