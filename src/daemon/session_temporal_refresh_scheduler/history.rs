use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::application::observation::ObservationCancellation;
use crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1;
use crate::global_db::RegisteredGlobalDb;

pub(in crate::daemon) type SessionHistoricalIngestPass<'a> =
    Pin<Box<dyn Future<Output = SessionHistoricalIngestOutcome> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::daemon) enum SessionHistoricalIngestOutcome {
    Complete,
    Pending {
        made_progress: bool,
    },
    Retryable {
        reason_code: &'static str,
        made_progress: bool,
    },
    Blocked {
        reason_code: &'static str,
    },
    Cancelled,
}

impl SessionHistoricalIngestOutcome {
    pub(super) const fn needs_another_pass(self) -> bool {
        matches!(self, Self::Pending { .. } | Self::Retryable { .. })
    }

    pub(super) const fn made_progress(self) -> bool {
        matches!(
            self,
            Self::Pending {
                made_progress: true
            } | Self::Retryable {
                made_progress: true,
                ..
            }
        )
    }
}

pub(in crate::daemon) trait SessionHistoricalIngestor: Send + Sync {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_>;
    fn cancel(&self);
}

pub(in crate::daemon) type SharedSessionHistoricalIngestor = Arc<dyn SessionHistoricalIngestor>;

pub(in crate::daemon) struct ProjectSessionHistoricalIngestor {
    database: Arc<RegisteredGlobalDb>,
    profile_identity: LocalProfileIdentityAuthorityV1,
    project_root: PathBuf,
    project_id: tracedecay_domain::ProjectId,
    transcript_source_home: Option<PathBuf>,
    cancellation: ObservationCancellation,
}

impl ProjectSessionHistoricalIngestor {
    pub(in crate::daemon) fn new(
        database: Arc<RegisteredGlobalDb>,
        profile_identity: LocalProfileIdentityAuthorityV1,
        project_root: PathBuf,
        project_id: tracedecay_domain::ProjectId,
        transcript_source_home: Option<PathBuf>,
    ) -> Self {
        Self {
            database,
            profile_identity,
            project_root,
            project_id,
            transcript_source_home,
            cancellation: ObservationCancellation::default(),
        }
    }
}

impl SessionHistoricalIngestor for ProjectSessionHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            let authority =
                crate::store::GlobalDbSessionIngestAuthority::new(Arc::clone(&self.database));
            let pass =
                tracedecay_sessions::runtime::ingest_project_sources_for_provider_with_cancellation(
                    self.profile_identity.brain_id(),
                    self.profile_identity.profile_id(),
                    &authority,
                    &self.project_root,
                    Some(self.project_id.clone()),
                    None,
                    true,
                    &self.cancellation,
                );
            let outcome = match self.transcript_source_home.clone() {
                Some(home) => {
                    tracedecay_sessions::runtime::with_transcript_source_home(home, pass).await
                }
                None => pass.await,
            };
            classify_transcript_ingest_outcome(outcome, &self.cancellation)
        })
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }
}

pub(in crate::daemon) struct ProfileSessionHistoricalIngestor {
    database: Arc<RegisteredGlobalDb>,
    registry_database: Arc<RegisteredGlobalDb>,
    profile_identity: LocalProfileIdentityAuthorityV1,
    transcript_source_home: Option<PathBuf>,
    cancellation: ObservationCancellation,
}

impl ProfileSessionHistoricalIngestor {
    pub(in crate::daemon) fn new(
        database: Arc<RegisteredGlobalDb>,
        registry_database: Arc<RegisteredGlobalDb>,
        profile_identity: LocalProfileIdentityAuthorityV1,
        transcript_source_home: Option<PathBuf>,
    ) -> Self {
        Self {
            database,
            registry_database,
            profile_identity,
            transcript_source_home,
            cancellation: ObservationCancellation::default(),
        }
    }
}

impl SessionHistoricalIngestor for ProfileSessionHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            let authority =
                crate::store::GlobalDbSessionIngestAuthority::new(Arc::clone(&self.database));
            let registry_authority = crate::store::GlobalDbSessionIngestAuthority::new(Arc::clone(
                &self.registry_database,
            ));
            let pass = tracedecay_sessions::runtime::ingest_user_global_sources_for_startup_with_db(
                self.profile_identity.brain_id(),
                self.profile_identity.profile_id(),
                &authority,
                &registry_authority,
                self.profile_identity.profile_root(),
                &self.cancellation,
            );
            let outcome = match self.transcript_source_home.clone() {
                Some(home) => {
                    tracedecay_sessions::runtime::with_transcript_source_home(home, pass).await
                }
                None => pass.await,
            };
            classify_transcript_ingest_outcome(outcome, &self.cancellation)
        })
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }
}

fn classify_transcript_ingest_outcome(
    outcome: tracedecay_sessions::runtime::TranscriptIngestOutcome,
    cancellation: &ObservationCancellation,
) -> SessionHistoricalIngestOutcome {
    if cancellation.is_cancelled() {
        return SessionHistoricalIngestOutcome::Cancelled;
    }
    let made_progress = outcome.made_progress();
    if let Some(failure) = outcome.failures.iter().find(|failure| !failure.retryable) {
        return SessionHistoricalIngestOutcome::Blocked {
            reason_code: failure.reason_code,
        };
    }
    if let Some(failure) = outcome.failures.first() {
        return SessionHistoricalIngestOutcome::Retryable {
            reason_code: failure.reason_code,
            made_progress,
        };
    }
    if outcome.has_deferred_work() {
        return SessionHistoricalIngestOutcome::Pending { made_progress };
    }
    SessionHistoricalIngestOutcome::Complete
}

#[cfg(test)]
mod tests {
    use super::{SessionHistoricalIngestOutcome, classify_transcript_ingest_outcome};
    use crate::application::observation::ObservationCancellation;
    use tracedecay_sessions::runtime::{
        IngestPassCoverage, TranscriptCatchUpFailure, TranscriptIngestOutcome,
    };

    fn ingest_outcome_with_failure(
        reason_code: &'static str,
        retryable: bool,
    ) -> TranscriptIngestOutcome {
        TranscriptIngestOutcome {
            stats: tracedecay_sessions::runtime::shared::TranscriptIngestStats::default(),
            failures: vec![TranscriptCatchUpFailure {
                provider: "codex",
                source: "observation",
                reason_code,
                retryable,
                source_locator: None,
            }],
            coverage: IngestPassCoverage::Complete,
            scheduling_state_written: false,
        }
    }

    /// A still-mounting write authority during the open window reports a
    /// retryable admission failure; the catch-up must schedule another pass
    /// (`retrying_history_is_typed_stale` proves the worker re-passes on
    /// Retryable) instead of marking the projection historically blocked.
    #[test]
    fn retryable_admission_failures_schedule_another_catch_up_pass() {
        let outcome = classify_transcript_ingest_outcome(
            ingest_outcome_with_failure("authority_write_failed", true),
            &ObservationCancellation::default(),
        );

        assert_eq!(
            outcome,
            SessionHistoricalIngestOutcome::Retryable {
                reason_code: "authority_write_failed",
                made_progress: false,
            }
        );
        assert!(outcome.needs_another_pass());
    }

    #[test]
    fn permanent_failures_still_block_the_catch_up() {
        let outcome = classify_transcript_ingest_outcome(
            ingest_outcome_with_failure("invalid_observation_contract", false),
            &ObservationCancellation::default(),
        );

        assert_eq!(
            outcome,
            SessionHistoricalIngestOutcome::Blocked {
                reason_code: "invalid_observation_contract",
            }
        );
        assert!(!outcome.needs_another_pass());
    }
}
