use tracedecay_application::{
    WorkProviderExecutionError, WorkProviderExecutionPort, WorkProviderRun,
    WorkProviderSettlementV1, WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    ProviderId, WorkAttemptV1, WorkAuthority, WorkProjection, WorkProviderRouteId,
    WorkProviderRouteV1,
};

use crate::sessions::codex_app_server::{
    CodexAppServerCancellation, CodexAppServerSummaryConfig,
    run_prompt_with_codex_app_server_cancellable,
};

pub(crate) const CODEX_PROVIDER_ID: &str = "provider.work.codex-app-server";
const CODEX_ROUTE_ID: &str = "route.work.codex-app-server.v1";
const CODEX_THREAD_SOURCE: &str = "tracedecay_work";

/// Builds Codex app-server executions for admitted Work attempts.
///
/// The provider owns no execution: `WorkExecutionQueueV1` owns every worker
/// thread, the in-flight registry, and the concurrency bound.
#[derive(Clone)]
pub(crate) struct CodexAppServerWorkProviderV1<S> {
    storage: S,
    authority: WorkAuthority,
    config: CodexAppServerSummaryConfig,
}

impl<S> CodexAppServerWorkProviderV1<S>
where
    S: WorkStoragePort + Clone,
{
    pub(crate) const fn new(
        storage: S,
        authority: WorkAuthority,
        config: CodexAppServerSummaryConfig,
    ) -> Self {
        Self {
            storage,
            authority,
            config,
        }
    }

    pub(crate) fn mounted_route() -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
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
}

impl<S> WorkProviderExecutionPort for CodexAppServerWorkProviderV1<S>
where
    S: WorkStoragePort + Clone + Send + Sync + 'static,
{
    type Run = CodexAppServerWorkRunV1;

    fn route(&self) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
        Self::mounted_route()
    }

    fn prepare(&self, attempt: &WorkAttemptV1) -> Result<Self::Run, WorkProviderExecutionError> {
        if !self.is_ready() {
            return Err(WorkProviderExecutionError::Unavailable(format!(
                "Codex app-server executable is unavailable: {}",
                self.config.codex_bin
            )));
        }
        let projection = self.projection(attempt)?;
        Ok(CodexAppServerWorkRunV1 {
            prompt: format!(
                "Execute the admitted TraceDecay Work task.\nTask: {}\nTitle: {}\n\
                 Use the current project context and return a concise completion report.",
                projection.task_id().as_str(),
                projection.title()
            ),
            config: self.config.clone(),
            cancellation: CodexAppServerCancellation::default(),
        })
    }
}

/// One prepared Codex app-server execution.
pub(crate) struct CodexAppServerWorkRunV1 {
    prompt: String,
    config: CodexAppServerSummaryConfig,
    cancellation: CodexAppServerCancellation,
}

impl WorkProviderRun for CodexAppServerWorkRunV1 {
    fn execute(&self) -> WorkProviderSettlementV1 {
        let outcome = run_prompt_with_codex_app_server_cancellable(
            &self.prompt,
            &self.config,
            CODEX_THREAD_SOURCE,
            &self.cancellation,
        );
        if self.cancellation.is_cancelled() {
            return WorkProviderSettlementV1::Cancelled;
        }
        match outcome {
            Ok(summary) => WorkProviderSettlementV1::Completed {
                evidence: summary.text,
            },
            Err(error) => WorkProviderSettlementV1::Failed {
                message: error.to_string(),
            },
        }
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }
}

fn map_work_storage_error(error: WorkStorageError) -> WorkProviderExecutionError {
    WorkProviderExecutionError::Unavailable(format!(
        "canonical Work projection is unavailable: {error}"
    ))
}

fn executable_is_resolvable(executable: &str) -> bool {
    let path = std::path::Path::new(executable);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(executable).is_file())
    })
}
