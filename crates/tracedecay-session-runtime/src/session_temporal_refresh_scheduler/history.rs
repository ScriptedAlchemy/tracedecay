use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_application::ProfileIdentityReadPort;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_usecases::observation::ObservationCancellation;

pub type SessionHistoricalIngestPass<'a> =
    Pin<Box<dyn Future<Output = SessionHistoricalIngestOutcome> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionHistoricalIngestOutcome {
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
        made_progress: bool,
    },
    Cancelled,
}

impl SessionHistoricalIngestOutcome {
    #[hotpath::skip]
    pub const fn needs_another_pass(self) -> bool {
        matches!(self, Self::Pending { .. } | Self::Retryable { .. })
    }

    #[hotpath::skip]
    pub const fn made_progress(self) -> bool {
        matches!(
            self,
            Self::Pending {
                made_progress: true
            } | Self::Retryable {
                made_progress: true,
                ..
            } | Self::Blocked {
                made_progress: true,
                ..
            }
        )
    }
}

pub trait SessionHistoricalIngestor: Send + Sync {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_>;
    fn cancel(&self);
}

pub type SharedSessionHistoricalIngestor = Arc<dyn SessionHistoricalIngestor>;

pub struct ProjectSessionHistoricalIngestor {
    database: RegisteredGlobalDbLeaseV1,
    profile_identity: Arc<dyn ProfileIdentityReadPort>,
    project_root: PathBuf,
    project_id: tracedecay_domain::ProjectId,
    transcript_source_home: Option<PathBuf>,
    cancellation: ObservationCancellation,
    codex_discovery: Arc<tracedecay_sessions::runtime::codex::CodexDiscoveryHub>,
    codex_consumer: String,
    codex_registered: AtomicBool,
}

impl ProjectSessionHistoricalIngestor {
    pub fn new(
        database: RegisteredGlobalDbLeaseV1,
        profile_identity: Arc<dyn ProfileIdentityReadPort>,
        project_root: PathBuf,
        project_id: tracedecay_domain::ProjectId,
        transcript_source_home: Option<PathBuf>,
        codex_discovery: Arc<tracedecay_sessions::runtime::codex::CodexDiscoveryHub>,
    ) -> Self {
        let source_home = transcript_source_home
            .as_deref()
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
        let codex_consumer = codex_consumer_key(
            "project",
            profile_identity.brain_id().as_str(),
            profile_identity.profile_id().as_str(),
            project_id.as_str(),
            &source_home,
        );
        codex_discovery.register(&codex_consumer, transcript_source_home.as_deref());
        Self {
            database,
            profile_identity,
            project_root,
            project_id,
            transcript_source_home,
            cancellation: ObservationCancellation::default(),
            codex_discovery,
            codex_consumer,
            codex_registered: AtomicBool::new(true),
        }
    }

    fn deregister_codex_once(&self) {
        if self.codex_registered.swap(false, Ordering::AcqRel) {
            self.codex_discovery.deregister(&self.codex_consumer);
        }
    }
}

impl SessionHistoricalIngestor for ProjectSessionHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            let authority =
                tracedecay_host_admission::session_ingest_authority::GlobalDbSessionIngestAuthority::new(self.database.clone());
            let pass = Box::pin(
                tracedecay_sessions::runtime::ingest_project_sources_for_provider_with_cancellation_and_codex_state(
                    self.profile_identity.brain_id(),
                    self.profile_identity.profile_id(),
                    &authority,
                    &self.project_root,
                    Some(self.project_id.clone()),
                    None,
                    true,
                    &self.cancellation,
                    self.codex_discovery.as_ref(),
                    &self.codex_consumer,
                ),
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
        self.deregister_codex_once();
    }
}

impl Drop for ProjectSessionHistoricalIngestor {
    fn drop(&mut self) {
        self.deregister_codex_once();
    }
}

pub struct ProfileSessionHistoricalIngestor {
    database: RegisteredGlobalDbLeaseV1,
    registry_database: RegisteredGlobalDbLeaseV1,
    profile_identity: Arc<dyn ProfileIdentityReadPort>,
    transcript_source_home: Option<PathBuf>,
    cancellation: ObservationCancellation,
    codex_discovery: Arc<tracedecay_sessions::runtime::codex::CodexDiscoveryHub>,
    codex_consumer: String,
    codex_registered: AtomicBool,
}

impl ProfileSessionHistoricalIngestor {
    pub fn new(
        database: RegisteredGlobalDbLeaseV1,
        registry_database: RegisteredGlobalDbLeaseV1,
        profile_identity: Arc<dyn ProfileIdentityReadPort>,
        transcript_source_home: Option<PathBuf>,
        codex_discovery: Arc<tracedecay_sessions::runtime::codex::CodexDiscoveryHub>,
    ) -> Self {
        let source_home = transcript_source_home
            .as_deref()
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
        let codex_consumer = codex_consumer_key(
            "profile",
            profile_identity.brain_id().as_str(),
            profile_identity.profile_id().as_str(),
            "",
            &source_home,
        );
        codex_discovery.register(&codex_consumer, transcript_source_home.as_deref());
        Self {
            database,
            registry_database,
            profile_identity,
            transcript_source_home,
            cancellation: ObservationCancellation::default(),
            codex_discovery,
            codex_consumer,
            codex_registered: AtomicBool::new(true),
        }
    }

    fn deregister_codex_once(&self) {
        if self.codex_registered.swap(false, Ordering::AcqRel) {
            self.codex_discovery.deregister(&self.codex_consumer);
        }
    }
}

fn codex_consumer_key(
    kind: &str,
    brain_id: &str,
    profile_id: &str,
    project_id: &str,
    source_home: &str,
) -> String {
    let fields = [kind, brain_id, profile_id, project_id, source_home];
    let mut key = String::from("codex-consumer");
    for field in fields {
        key.push('|');
        key.push_str(&field.len().to_string());
        key.push(':');
        key.push_str(field);
    }
    key
}

impl SessionHistoricalIngestor for ProfileSessionHistoricalIngestor {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            let authority =
                tracedecay_host_admission::session_ingest_authority::GlobalDbSessionIngestAuthority::new(self.database.clone());
            let registry_authority =
                tracedecay_host_admission::session_ingest_authority::GlobalDbSessionIngestAuthority::new(self.registry_database.clone());
            let pass = Box::pin(
                tracedecay_sessions::runtime::ingest_user_global_sources_for_startup_with_db_and_codex_state(
                    self.profile_identity.brain_id(),
                    self.profile_identity.profile_id(),
                    &authority,
                    &registry_authority,
                    self.profile_identity.profile_root(),
                    &self.cancellation,
                    (self.codex_discovery.as_ref(), &self.codex_consumer),
                ),
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
        self.deregister_codex_once();
    }
}

impl Drop for ProfileSessionHistoricalIngestor {
    fn drop(&mut self) {
        self.deregister_codex_once();
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
            made_progress,
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
    use tracedecay_sessions::runtime::{
        IngestPassCoverage, TranscriptCatchUpFailure, TranscriptIngestOutcome,
    };
    use tracedecay_usecases::observation::ObservationCancellation;

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
                made_progress: false,
            }
        );
        assert!(!outcome.needs_another_pass());
    }

    #[test]
    fn permanent_cursor_failure_preserves_healthy_provider_progress() {
        let outcome = classify_transcript_ingest_outcome(
            TranscriptIngestOutcome {
                stats: tracedecay_sessions::TranscriptIngestStats {
                    sessions_upserted: 2,
                    messages_upserted: 4,
                },
                failures: vec![TranscriptCatchUpFailure {
                    provider: "cursor",
                    source: "observation",
                    reason_code: "observation_cursor_advance_collision",
                    retryable: false,
                    source_locator: None,
                }],
                coverage: IngestPassCoverage::Complete,
                scheduling_state_written: false,
            },
            &ObservationCancellation::default(),
        );

        assert_eq!(
            outcome,
            SessionHistoricalIngestOutcome::Blocked {
                reason_code: "observation_cursor_advance_collision",
                made_progress: true,
            }
        );
        assert!(
            outcome.made_progress(),
            "the healthy Claude and Codex rows must still reach temporal projection"
        );
    }
}
