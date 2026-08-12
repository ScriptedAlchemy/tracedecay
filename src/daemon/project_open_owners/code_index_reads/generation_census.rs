//! Runtime generation telemetry bound to one exact daemon project scope.

use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_application::ResolvedScope;

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
