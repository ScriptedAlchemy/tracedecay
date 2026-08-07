use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::application::observation::ObservationCancellation;
use crate::daemon::profile_identity::LocalProfileIdentityAuthorityV1;
use crate::global_db::RegisteredGlobalDb;

pub(super) type SessionHistoricalIngestPass<'a> =
    Pin<Box<dyn Future<Output = SessionHistoricalIngestOutcome> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SessionHistoricalIngestOutcome {
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

pub(super) trait SessionHistoricalIngestor: Send + Sync {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_>;
    fn cancel(&self);
}

pub(super) type SharedSessionHistoricalIngestor = Arc<dyn SessionHistoricalIngestor>;

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
            let pass = crate::sessions::ingest_project_sources_for_provider_with_cancellation(
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
                Some(home) => crate::sessions::with_transcript_source_home(home, pass).await,
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
            let pass = crate::sessions::ingest_user_global_sources_for_startup_with_db(
                self.profile_identity.brain_id(),
                self.profile_identity.profile_id(),
                &authority,
                &registry_authority,
                self.profile_identity.profile_root(),
                &self.cancellation,
            );
            let outcome = match self.transcript_source_home.clone() {
                Some(home) => crate::sessions::with_transcript_source_home(home, pass).await,
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
    outcome: crate::sessions::TranscriptIngestOutcome,
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
