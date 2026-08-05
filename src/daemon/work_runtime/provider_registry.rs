use tracedecay_application::{
    WorkProviderExecutionError, WorkProviderExecutionOutcomeV1, WorkProviderExecutionPort,
    WorkProviderRun,
};
use tracedecay_domain::{WorkAttemptV1, WorkProviderRouteV1};

use super::codex_provider::{NativeWorkProviderV1, NativeWorkRunV1};

/// Explicit production provider composition for canonical Work execution.
///
/// The registry contains only adapters mounted by daemon composition. It never
/// fabricates an executable, route, backend, or fallback from process state.
#[derive(Clone)]
pub(crate) struct WorkProviderRegistry<S> {
    providers: Vec<NativeWorkProviderV1<S>>,
}

impl<S> WorkProviderRegistry<S> {
    pub(crate) fn with_provider(provider: NativeWorkProviderV1<S>) -> Self {
        Self {
            providers: vec![provider],
        }
    }
}

pub(crate) enum RegisteredWorkRun {
    Native(NativeWorkRunV1),
}

impl WorkProviderRun for RegisteredWorkRun {
    fn execute(&self) -> WorkProviderExecutionOutcomeV1 {
        match self {
            Self::Native(run) => run.execute(),
        }
    }

    fn cancel(&self) {
        match self {
            Self::Native(run) => run.cancel(),
        }
    }
}

impl<S> WorkProviderExecutionPort for WorkProviderRegistry<S>
where
    S: tracedecay_application::WorkStoragePort + Clone + Send + Sync + 'static,
{
    type Run = RegisteredWorkRun;

    fn route(&self) -> Result<WorkProviderRouteV1, WorkProviderExecutionError> {
        self.providers
            .first()
            .ok_or_else(|| {
                WorkProviderExecutionError::Unavailable("no Work provider is registered".to_owned())
            })?
            .route()
    }

    fn supports_route(
        &self,
        requested: &WorkProviderRouteV1,
    ) -> Result<bool, WorkProviderExecutionError> {
        for provider in &self.providers {
            if provider.supports_route(requested)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn prepare(&self, attempt: &WorkAttemptV1) -> Result<Self::Run, WorkProviderExecutionError> {
        let actual_route = attempt.actual_route().ok_or_else(|| {
            WorkProviderExecutionError::Rejected(
                "Work provider selection was not persisted before launch".to_owned(),
            )
        })?;
        for provider in &self.providers {
            if provider.supports_route(actual_route)? {
                return provider.prepare(attempt).map(RegisteredWorkRun::Native);
            }
        }
        Err(WorkProviderExecutionError::Unavailable(
            "requested Work provider is not registered".to_owned(),
        ))
    }
}
