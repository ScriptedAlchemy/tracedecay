//! Exact-scope code-index read bridges for daemon project owners.

mod ignored_dependency_admission;
pub(crate) use ignored_dependency_admission::project_code_index_ignored_dependency_admission_port;
#[cfg(test)]
mod ignored_dependency_admission_tests;
#[cfg(test)]
mod scope_admission_tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::{Deadline, ResolvedScope, now_micros};
use tracedecay_graph_query::{CodeGraphReadError, CodeGraphReadRequest, VerifiedCodeGraphRead};

fn refuse_projection_wait(request: &CodeGraphReadRequest<'_>) -> Result<(), CodeGraphReadError> {
    if request.cancellation.is_cancelled()
        || request
            .live_cancellation
            .is_some_and(|signal| signal.is_cancelled())
    {
        return Err(CodeGraphReadError::Cancelled);
    }
    let observed_at = now_micros();
    if request
        .deadline
        .as_ref()
        .is_some_and(|deadline| deadline.is_elapsed_at(observed_at))
    {
        return Err(CodeGraphReadError::TimedOut);
    }
    match request.context.admission_at(observed_at) {
        tracedecay_application::RequestAdmission::Admitted => Ok(()),
        tracedecay_application::RequestAdmission::Cancelled => Err(CodeGraphReadError::Cancelled),
        tracedecay_application::RequestAdmission::TimedOut => Err(CodeGraphReadError::TimedOut),
    }
}

async fn sleep_until_deadline(deadline: &Deadline) {
    let now = now_micros();
    if deadline.is_elapsed_at(now) {
        return;
    }
    let remaining = deadline.expires_at.0.saturating_sub(now.0);
    tokio::time::sleep(Duration::from_micros(remaining as u64)).await;
}

struct ProjectCodeGraphProjectionReadPortV1 {
    schedulers: tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
}

impl tracedecay_graph_query::CodeGraphProjectionReadPort for ProjectCodeGraphProjectionReadPortV1 {
    fn open<'a>(
        &'a self,
        request: tracedecay_graph_query::CodeGraphReadRequest<'a>,
    ) -> tracedecay_graph_query::CodeGraphReadFuture<'a> {
        Box::pin(async move {
            request
                .context
                .validate()
                .map_err(|error| CodeGraphReadError::InvalidRequest {
                    detail: error.to_string(),
                })?;
            // Checkout identity, not label equality: the retained route scope
            // pins the branch label that was live at project open, while a
            // request scope is resolved against live HEAD. Full-struct
            // equality (reference + scope digest) denied the route's own
            // checkout after every ordinary `git switch`. A genuinely
            // different project, repository, or worktree stays denied.
            if !request
                .context
                .scope()
                .identifies_same_checkout(&self.scope)
            {
                return Err(CodeGraphReadError::Denied);
            }
            refuse_projection_wait(&request)?;
            let wait = self
                .schedulers
                .latest_complete_ready_decoded_for_root_scope(&self.project_root, &self.scope);
            let latest = match (request.deadline.as_ref(), request.live_cancellation) {
                (None, None) => wait.await,
                (deadline, live_cancellation) => {
                    tokio::select! {
                        biased;
                        _ = async {
                            if let Some(signal) = live_cancellation {
                                signal.cancelled().await;
                            } else {
                                std::future::pending::<()>().await;
                            }
                        } => return Err(CodeGraphReadError::Cancelled),
                        _ = async {
                            if let Some(deadline) = deadline {
                                sleep_until_deadline(deadline).await;
                            } else {
                                std::future::pending::<()>().await;
                            }
                        } => return Err(CodeGraphReadError::TimedOut),
                        latest = wait => latest,
                    }
                }
            };
            // Serve the last complete generation while the scheduler
            // rebuilds: when the ready gate abstains (a background pass owns
            // the scheduler, or a new tip commit disproved the currency
            // witness), the seat still holds the last complete generation.
            // Refusing it withdrew exact-scope retrieval for entire
            // regeneration windows; instead it serves with typed staleness,
            // exactly as the search lane's `served_stale` arm does. Only a
            // route with no seated complete generation at all stays a typed
            // unavailable refusal.
            let (latest, freshness) = match latest {
                Some(latest) => (
                    latest,
                    tracedecay_graph_query::CodeGraphReadFreshnessV1::Current,
                ),
                None => match self
                    .schedulers
                    .latest_complete_serving_for_root_scope(&self.project_root, &self.scope)
                    .await
                {
                    Some(seated) => {
                        // Capture the wedge-distinguishing evidence at open
                        // time: a seat sealed days ago with no reconcile pass
                        // or pending wake is a stalled route, not a routine
                        // rebuild window, and the caveat must say which.
                        let rebuild_in_flight = self
                            .schedulers
                            .rebuild_pass_in_flight_for_root_scope(&self.project_root, &self.scope)
                            .await;
                        let sealed_at = seated.generation().manifest().seal.sealed_at;
                        (
                            seated,
                            tracedecay_graph_query::CodeGraphReadFreshnessV1::LastCompleteStale {
                                sealed_at,
                                rebuild_in_flight,
                            },
                        )
                    }
                    None => {
                        return Err(CodeGraphReadError::Unavailable {
                            detail:
                                "the verified code graph is not ready for the exact project root"
                                    .to_owned(),
                        });
                    }
                },
            };
            refuse_projection_wait(&request)?;
            let store = latest.interactive_graph_store().map_err(|error| {
                CodeGraphReadError::Unavailable {
                    detail: error.to_string(),
                }
            })?;
            refuse_projection_wait(&request)?;
            VerifiedCodeGraphRead::new(self.scope.clone(), store, freshness)
        })
    }
}

pub(crate) fn project_code_graph_projection_read_port(
    schedulers: tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
) -> Arc<dyn tracedecay_graph_query::CodeGraphProjectionReadPort> {
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
    schedulers: tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
) -> tracedecay_session_memory::runtime_telemetry::GenerationCensusReader {
    Arc::new(move || {
        let schedulers = schedulers.clone();
        let project_root = project_root.clone();
        let scope = scope.clone();
        Box::pin(async move {
            let Some(latest) = schedulers
                .latest_complete_ready_decoded_for_root_scope(&project_root, &scope)
                .await
            else {
                return tracedecay_session_memory::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
                    reason: tracedecay_session_memory::runtime_telemetry::GenerationCensusUnavailableReason::ExactScopeGenerationNotReady,
                };
            };
            match latest.generation().generation_statistics() {
                Ok(statistics) => {
                    tracedecay_session_memory::runtime_telemetry::GenerationCensusSnapshot::Observed {
                        statistics:
                            tracedecay_session_memory::runtime_telemetry::GenerationCensusStatistics {
                                source_total_bytes: statistics.source_total_bytes,
                                symbol_count: statistics.symbol_count,
                                edge_count: statistics.edge_count,
                            },
                    }
                }
                Err(_) => tracedecay_session_memory::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
                    reason: tracedecay_session_memory::runtime_telemetry::GenerationCensusUnavailableReason::SealedGenerationCensusInvalid,
                },
            }
        })
    })
}
