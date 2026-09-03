//! Canonical retained adapter for automatic fact curation.

use std::sync::Arc;

use tracedecay_application::retained_surfaces::{
    FactStoreCurateRequestV1, RetainedAutomationExecutionPortV1, RetainedSurfaceExecutionContextV1,
    RetainedSurfaceExecutionFutureV1,
};
use tracedecay_daemon_service::DaemonInvocationService;

use crate::tracedecay::TraceDecay;

pub(super) struct DirectRetainedAutomationPortV1 {
    cg: Arc<tokio::sync::RwLock<Arc<TraceDecay>>>,
    invocation_service: DaemonInvocationService,
}

impl DirectRetainedAutomationPortV1 {
    #[hotpath::skip]
    pub(super) const fn new(
        cg: Arc<tokio::sync::RwLock<Arc<TraceDecay>>>,
        invocation_service: DaemonInvocationService,
    ) -> Self {
        Self {
            cg,
            invocation_service,
        }
    }
}

impl RetainedAutomationExecutionPortV1 for DirectRetainedAutomationPortV1 {
    fn execute_fact_store_curate<'a>(
        &'a self,
        context: RetainedSurfaceExecutionContextV1<'a>,
        request: &'a FactStoreCurateRequestV1,
    ) -> RetainedSurfaceExecutionFutureV1<'a> {
        Box::pin(async move {
            let cg = self.cg.read().await.clone();
            hotpath::future!(
                crate::daemon::dashboard_automation::execute_retained_memory_curator(
                    cg.as_ref(),
                    &self.invocation_service,
                    &context,
                    request
                ),
                label = "daemon.retained.automation.curate"
            )
            .await
        })
    }
}
