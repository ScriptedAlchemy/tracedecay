use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay_application::{
    WorkProviderExecutionError, WorkProviderExecutionPort, WorkProviderRun,
    WorkProviderSettlementV1, WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    ManifestDigest, ProviderId, UtcMicros, WorkAttemptV1, WorkAuthority, WorkProjection,
    WorkProviderBackendV1, WorkProviderRouteId, WorkProviderRouteV1,
};

use crate::sessions::codex_app_server::{
    CodexAppServerCancellation, CodexAppServerSummaryConfig, run_work_with_codex_app_server,
};

pub(crate) const CODEX_PROVIDER_ID: &str = "provider.work.codex-app-server";
const CODEX_ROUTE_ID: &str = "route.work.codex-app-server.v1";
const CODEX_THREAD_SOURCE: &str = "tracedecay_work";

#[derive(Clone, Debug)]
pub(crate) struct NativeWorkProviderConfigV1 {
    codex_app_server: CodexAppServerSummaryConfig,
    configuration_digest: ManifestDigest,
    project_root: PathBuf,
}

impl NativeWorkProviderConfigV1 {
    pub(crate) fn from_registered(
        codex_app_server: CodexAppServerSummaryConfig,
        configuration_digest: ManifestDigest,
        project_root: PathBuf,
    ) -> Self {
        Self {
            codex_app_server,
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

    #[cfg(all(test, unix))]
    pub(crate) fn is_ready(&self) -> bool {
        executable_is_resolvable(&self.config.codex_app_server.codex_bin)
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
        Ok(requested == &Self::codex_app_server_route()?)
    }

    fn prepare(&self, attempt: &WorkAttemptV1) -> Result<Self::Run, WorkProviderExecutionError> {
        self.validate_execution(attempt)?;
        let projection = self.projection(attempt)?;
        let execution = attempt.execution();
        let prompt = self.prompt(&projection, attempt);
        let timeout =
            remaining_timeout(execution.deadline(), self.config.codex_app_server.timeout)?;
        if execution.backend() != WorkProviderBackendV1::CodexAppServer {
            return Err(WorkProviderExecutionError::Unavailable(
                "requested Work provider backend is not registered".to_owned(),
            ));
        }
        require_exact_route(execution.route(), &Self::codex_app_server_route()?)?;
        require_executable(&self.config.codex_app_server.codex_bin, "Codex app-server")?;
        let mut config = self.config.codex_app_server.clone();
        config.model = Some(execution.model().to_owned());
        Ok(NativeWorkRunV1(CodexAppServerWorkRunV1 {
            prompt,
            config,
            cwd: self.config.project_root.clone(),
            timeout,
            cancellation: CodexAppServerCancellation::default(),
        }))
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

pub(crate) struct NativeWorkRunV1(CodexAppServerWorkRunV1);

impl WorkProviderRun for NativeWorkRunV1 {
    fn execute(&self) -> WorkProviderSettlementV1 {
        self.0.execute()
    }

    fn cancel(&self) {
        self.0.cancel();
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

fn require_executable(executable: &str, provider: &str) -> Result<(), WorkProviderExecutionError> {
    if !executable_is_resolvable(executable) {
        return Err(WorkProviderExecutionError::Unavailable(format!(
            "{provider} executable is unavailable"
        )));
    }
    Ok(())
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

fn executable_is_resolvable(executable: &str) -> bool {
    let path = Path::new(executable);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(executable).is_file())
    })
}
