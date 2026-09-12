//! Exact-scope code-index read bridges for daemon project owners.

mod ignored_dependency_admission;
pub use ignored_dependency_admission::project_code_index_ignored_dependency_admission_port;
#[cfg(test)]
mod scope_admission_tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_code_index::graph_projection::CodeGraphProjectionStore;
use tracedecay_contracts::{Deadline, ResolvedScope, now_micros};
use tracedecay_graph_query::{CodeGraphReadError, CodeGraphReadRequest, VerifiedCodeGraphRead};

use crate::code_index_scheduler::{
    CodeIndexAutomaticAdmissionV1, CodeIndexSchedulerRegistryV1, LatestCodeTextGenerationV1,
    LatestCompleteCodeIndexV1,
};

/// Whether this route's code index is disabled by contract, so no generation
/// will ever be published for it.
///
/// A disabled linked-worktree route serves but never indexes. Deferred owners
/// must not wait on global publication signals for a generation this scope can
/// never seat, because each wake would contend on the shared project writer.
pub fn code_index_disabled_for_scope(
    schedulers: &CodeIndexSchedulerRegistryV1,
    scope: &ResolvedScope,
) -> bool {
    schedulers.automatic_admission_for_scope(scope)
        == Some(CodeIndexAutomaticAdmissionV1::LinkedWorktreeDisabled)
}

fn refuse_projection_wait(request: &CodeGraphReadRequest<'_>) -> Result<(), CodeGraphReadError> {
    if request.cancellation.is_cancelled()
        || request
            .live_cancellation
            .is_some_and(tracedecay_contracts::CancellationSignal::is_cancelled)
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
        tracedecay_contracts::RequestAdmission::Admitted => Ok(()),
        tracedecay_contracts::RequestAdmission::Cancelled => Err(CodeGraphReadError::Cancelled),
        tracedecay_contracts::RequestAdmission::TimedOut => Err(CodeGraphReadError::TimedOut),
    }
}

pub(crate) async fn sleep_until_deadline(deadline: &Deadline) {
    let now = now_micros();
    if deadline.is_elapsed_at(now) {
        return;
    }
    let remaining = deadline.expires_at.0.saturating_sub(now.0);
    tokio::time::sleep(Duration::from_micros(remaining as u64)).await;
}

struct ProjectCodeGraphProjectionReadPortV1 {
    authority: ProjectCodeGraphServingAuthorityV1,
}

#[derive(Clone)]
struct ProjectCodeGraphServingAuthorityV1 {
    schedulers: CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
}

struct ProjectCodeGraphServingProjectionV1 {
    generation_id: tracedecay_domain::CodeGenerationId,
    statistics: Option<tracedecay_code_index::production::CodeIndexGenerationStatisticsV1>,
    store: Arc<CodeGraphProjectionStore>,
    freshness: tracedecay_graph_query::CodeGraphReadFreshnessV1,
}

impl ProjectCodeGraphServingAuthorityV1 {
    async fn project(&self) -> Result<ProjectCodeGraphServingProjectionV1, CodeGraphReadError> {
        if let Some(latest) = self
            .schedulers
            .latest_complete_ready_decoded_for_root_scope(&self.project_root, &self.scope)
            .await
        {
            return Self::complete_projection(
                latest,
                tracedecay_graph_query::CodeGraphReadFreshnessV1::Current,
            );
        }
        if let Some((text, current)) = self
            .schedulers
            .latest_text_serving_freshness_for_scope(&self.scope)
            .await
            && text.interactive_graph_store().is_ok()
        {
            let freshness = if current {
                tracedecay_graph_query::CodeGraphReadFreshnessV1::Current
            } else {
                tracedecay_graph_query::CodeGraphReadFreshnessV1::LastCompleteStale {
                    sealed_at: text.metadata().manifest().seal.sealed_at,
                    rebuild_in_flight: self
                        .schedulers
                        .rebuild_pass_in_flight_for_root_scope(&self.project_root, &self.scope)
                        .await,
                }
            };
            return Self::text_projection(text, freshness);
        }
        let Some(seated) = self
            .schedulers
            .latest_complete_serving_for_root_scope(&self.project_root, &self.scope)
            .await
        else {
            return Err(CodeGraphReadError::Unavailable {
                detail: "the verified code graph is not ready for the exact project root"
                    .to_owned(),
            });
        };
        let freshness = tracedecay_graph_query::CodeGraphReadFreshnessV1::LastCompleteStale {
            sealed_at: seated.generation().manifest().seal.sealed_at,
            rebuild_in_flight: self
                .schedulers
                .rebuild_pass_in_flight_for_root_scope(&self.project_root, &self.scope)
                .await,
        };
        Self::complete_projection(seated, freshness)
    }

    fn complete_projection(
        latest: LatestCompleteCodeIndexV1,
        freshness: tracedecay_graph_query::CodeGraphReadFreshnessV1,
    ) -> Result<ProjectCodeGraphServingProjectionV1, CodeGraphReadError> {
        let store =
            latest
                .interactive_graph_store()
                .map_err(|error| CodeGraphReadError::Unavailable {
                    detail: error.to_string(),
                })?;
        Ok(ProjectCodeGraphServingProjectionV1 {
            generation_id: latest.generation().manifest().generation_id.clone(),
            statistics: latest.generation().generation_statistics().ok(),
            store,
            freshness,
        })
    }

    fn text_projection(
        latest: LatestCodeTextGenerationV1,
        freshness: tracedecay_graph_query::CodeGraphReadFreshnessV1,
    ) -> Result<ProjectCodeGraphServingProjectionV1, CodeGraphReadError> {
        let store =
            latest
                .interactive_graph_store()
                .map_err(|error| CodeGraphReadError::Unavailable {
                    detail: error.to_string(),
                })?;
        Ok(ProjectCodeGraphServingProjectionV1 {
            generation_id: latest.metadata().manifest().generation_id.clone(),
            statistics: latest.metadata().generation_statistics().cloned(),
            store,
            freshness,
        })
    }
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
                .identifies_same_checkout(&self.authority.scope)
            {
                return Err(CodeGraphReadError::Denied);
            }
            refuse_projection_wait(&request)?;
            let wait = self.authority.project();
            let projection = match (request.deadline.as_ref(), request.live_cancellation) {
                (None, None) => wait.await,
                (deadline, live_cancellation) => {
                    tokio::select! {
                        biased;
                        () = async {
                            if let Some(signal) = live_cancellation {
                                signal.cancelled().await;
                            } else {
                                std::future::pending::<()>().await;
                            }
                        } => return Err(CodeGraphReadError::Cancelled),
                        () = async {
                            if let Some(deadline) = deadline {
                                sleep_until_deadline(deadline).await;
                            } else {
                                std::future::pending::<()>().await;
                            }
                        } => return Err(CodeGraphReadError::TimedOut),
                        projection = wait => projection,
                    }
                }
            }?;
            refuse_projection_wait(&request)?;
            VerifiedCodeGraphRead::new(
                self.authority.scope.clone(),
                projection.store,
                projection.freshness,
            )
        })
    }
}

pub fn project_code_graph_projection_read_port(
    schedulers: CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
) -> Arc<dyn tracedecay_graph_query::CodeGraphProjectionReadPort> {
    Arc::new(ProjectCodeGraphProjectionReadPortV1 {
        authority: ProjectCodeGraphServingAuthorityV1 {
            schedulers,
            project_root,
            scope,
        },
    })
}

/// Bind runtime generation telemetry to this daemon route's exact project
/// root and resolved scope through the same serving projection that graph
/// queries open. A missing graph seat is an explicit unavailable census; it
/// never falls back to the runtime database.
pub fn project_code_index_generation_census_reader(
    schedulers: CodeIndexSchedulerRegistryV1,
    project_root: PathBuf,
    scope: ResolvedScope,
) -> tracedecay_runtime_core::runtime_telemetry::GenerationCensusReader {
    let authority = ProjectCodeGraphServingAuthorityV1 {
        schedulers,
        project_root,
        scope,
    };
    Arc::new(move || {
        let authority = authority.clone();
        Box::pin(async move {
            let Ok(projection) = authority.project().await else {
                return tracedecay_runtime_core::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
                    reason: tracedecay_runtime_core::runtime_telemetry::GenerationCensusUnavailableReason::ExactScopeGenerationNotReady,
                };
            };
            match projection.statistics {
                Some(statistics) => {
                    let freshness = match projection.freshness {
                        tracedecay_graph_query::CodeGraphReadFreshnessV1::Current => {
                            tracedecay_runtime_core::runtime_telemetry::GenerationCensusServingFreshness::Current
                        }
                        tracedecay_graph_query::CodeGraphReadFreshnessV1::LastCompleteStale {
                            sealed_at,
                            rebuild_in_flight,
                        } => {
                            tracedecay_runtime_core::runtime_telemetry::GenerationCensusServingFreshness::LastCompleteStale {
                                sealed_at_micros: sealed_at.0,
                                rebuild_in_flight,
                            }
                        }
                    };
                    tracedecay_runtime_core::runtime_telemetry::GenerationCensusSnapshot::Observed {
                        generation_id: projection.generation_id.as_str().to_owned(),
                        freshness,
                        statistics:
                            tracedecay_runtime_core::runtime_telemetry::GenerationCensusStatistics {
                                source_total_bytes: statistics.source_total_bytes,
                                symbol_count: statistics.symbol_count,
                                edge_count: statistics.edge_count,
                            },
                    }
                }
                None => tracedecay_runtime_core::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
                    reason: tracedecay_runtime_core::runtime_telemetry::GenerationCensusUnavailableReason::SealedGenerationCensusInvalid,
                },
            }
        })
    })
}
