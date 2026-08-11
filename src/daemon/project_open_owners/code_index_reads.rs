//! Exact-scope code-index read bridges for daemon project owners.

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::ResolvedScope;
use tracedecay_usecases::graph::{CodeGraphReadError, VerifiedCodeGraphRead};

struct ProjectCodeGraphProjectionReadPortV1 {
    schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
}

impl tracedecay_usecases::graph::CodeGraphProjectionReadPort
    for ProjectCodeGraphProjectionReadPortV1
{
    fn open<'a>(
        &'a self,
        request: tracedecay_usecases::graph::CodeGraphReadRequest<'a>,
    ) -> tracedecay_usecases::graph::CodeGraphReadFuture<'a> {
        Box::pin(async move {
            request
                .context
                .validate()
                .map_err(|error| CodeGraphReadError::InvalidRequest {
                    detail: error.to_string(),
                })?;
            if request.context.scope() != &self.scope {
                return Err(CodeGraphReadError::Denied);
            }
            if request.cancellation.is_cancelled() {
                return Err(CodeGraphReadError::Cancelled);
            }
            match request.context.admission_at(request.observed_at) {
                tracedecay_application::RequestAdmission::Admitted => {}
                tracedecay_application::RequestAdmission::Cancelled => {
                    return Err(CodeGraphReadError::Cancelled);
                }
                tracedecay_application::RequestAdmission::TimedOut => {
                    return Err(CodeGraphReadError::TimedOut);
                }
            }
            let latest = self
                .schedulers
                .latest_complete_ready_decoded_for_root_scope(&self.project_root, &self.scope)
                .await
                .ok_or_else(|| CodeGraphReadError::Unavailable {
                    detail: "the verified code graph is not ready for the exact project root"
                        .to_owned(),
                })?;
            let store = latest.interactive_graph_store().map_err(|error| {
                CodeGraphReadError::Unavailable {
                    detail: error.to_string(),
                }
            })?;
            if request.cancellation.is_cancelled() {
                return Err(CodeGraphReadError::Cancelled);
            }
            VerifiedCodeGraphRead::new(self.scope.clone(), store)
        })
    }
}

pub(crate) fn project_code_graph_projection_read_port(
    schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
) -> Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort> {
    Arc::new(ProjectCodeGraphProjectionReadPortV1 {
        schedulers,
        project_root,
        scope,
    })
}

/// Bind runtime generation telemetry to this daemon route's exact project
/// root and resolved scope. A missing or unready sealed generation is an
/// explicit unavailable census; it never falls back to the runtime database.
pub(crate) fn project_code_index_generation_census_reader(
    schedulers: crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
) -> crate::runtime_telemetry::GenerationCensusReader {
    Arc::new(move || {
        let schedulers = schedulers.clone();
        let project_root = project_root.clone();
        let scope = scope.clone();
        Box::pin(async move {
            let Some(latest) = schedulers
                .latest_complete_ready_decoded_for_root_scope(&project_root, &scope)
                .await
            else {
                return crate::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
                    reason: crate::runtime_telemetry::GenerationCensusUnavailableReason::ExactScopeGenerationNotReady,
                };
            };
            match latest.generation().generation_statistics() {
                Ok(statistics) => {
                    crate::runtime_telemetry::GenerationCensusSnapshot::Observed { statistics }
                }
                Err(_) => crate::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
                    reason: crate::runtime_telemetry::GenerationCensusUnavailableReason::SealedGenerationCensusInvalid,
                },
            }
        })
    })
}
