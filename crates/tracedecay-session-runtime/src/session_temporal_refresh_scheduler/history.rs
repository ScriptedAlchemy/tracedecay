use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use asupersync::channel::watch;
use futures_util::future::select_all;
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_contracts::ProfileIdentityReadPort;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_sessions::host_ports::session_review::SessionReviewPort;
use tracedecay_sessions::observation::ObservationCancellation;

pub type SessionHistoricalIngestPass<'a> =
    Pin<Box<dyn Future<Output = SessionHistoricalIngestOutcome> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionHistoricalIngestOutcome {
    Complete,
    Pending {
        made_progress: bool,
    },
    Retryable {
        reason_code: String,
        made_progress: bool,
    },
    Blocked {
        reason_code: String,
        made_progress: bool,
    },
    Cancelled,
}

impl SessionHistoricalIngestOutcome {
    pub const fn needs_another_pass(&self) -> bool {
        matches!(self, Self::Pending { .. } | Self::Retryable { .. })
    }

    pub const fn made_progress(&self) -> bool {
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

/// The capacity-release signals a pass that committed nothing waits on before
/// it runs again, in place of a retry timer.
pub struct SessionHistoricalCapacityRelease {
    signals: Vec<watch::Receiver<u64>>,
}

impl SessionHistoricalCapacityRelease {
    /// Each signal advances when capacity the pass may have been refused is
    /// released. Values already published are treated as seen.
    pub fn new(signals: Vec<watch::Receiver<u64>>) -> Self {
        Self { signals }
    }

    /// Resolves once any signal advances. A closed signal never releases, and
    /// neither does any signal while the native runtime is unavailable.
    pub async fn released(&mut self) {
        if self.signals.is_empty() {
            return std::future::pending().await;
        }
        let Some(cx) = tracedecay_runtime_core::native_runtime::cx() else {
            return std::future::pending().await;
        };
        let cx = &cx;
        let changes = self.signals.iter_mut().map(|signal| {
            Box::pin(async move {
                if signal.changed(cx).await.is_err() {
                    std::future::pending::<()>().await;
                }
            })
        });
        select_all(changes).await;
    }
}

pub trait SessionHistoricalIngestor: Send + Sync {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_>;
    /// Subscribed immediately before each pass.
    fn capacity_release(&self) -> SessionHistoricalCapacityRelease;
    fn cancel(&self);
}

pub type SharedSessionHistoricalIngestor = Arc<dyn SessionHistoricalIngestor>;

pub struct ProjectSessionHistoricalIngestor {
    database: RegisteredGlobalDbLeaseV1,
    profile_identity: Arc<dyn ProfileIdentityReadPort>,
    project_root: PathBuf,
    project_id: tracedecay_domain::ProjectId,
    transcript_source_profile: ProfileRoot,
    cancellation: ObservationCancellation,
    codex_discovery: Arc<tracedecay_sessions::runtime::hosts::codex::CodexDiscoveryHub>,
    background_cpu: Arc<ProcessBackgroundCpuV1>,
    codex_consumer: String,
    codex_registered: AtomicBool,
}

impl ProjectSessionHistoricalIngestor {
    pub fn new(
        database: RegisteredGlobalDbLeaseV1,
        profile_identity: Arc<dyn ProfileIdentityReadPort>,
        project_root: PathBuf,
        project_id: tracedecay_domain::ProjectId,
        transcript_source_profile: ProfileRoot,
        codex_discovery: Arc<tracedecay_sessions::runtime::hosts::codex::CodexDiscoveryHub>,
        background_cpu: Arc<ProcessBackgroundCpuV1>,
    ) -> Self {
        let source_home = transcript_source_profile
            .home()
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
        let codex_consumer = codex_consumer_key(
            "project",
            profile_identity.brain_id().as_str(),
            profile_identity.profile_id().as_str(),
            project_id.as_str(),
            &source_home,
        );
        codex_discovery.register(&codex_consumer, transcript_source_profile.home());
        Self {
            database,
            profile_identity,
            project_root,
            project_id,
            transcript_source_profile,
            cancellation: ObservationCancellation::default(),
            codex_discovery,
            background_cpu,
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
                tracedecay_host_admission::session_ingest_authority::GlobalDbSessionIngestAuthority::new(self.database.clone())
                    .with_background_cpu(Arc::clone(&self.background_cpu));
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
            let outcome = tracedecay_sessions::runtime::with_transcript_source_profile(
                self.transcript_source_profile.clone(),
                pass,
            )
            .await;
            classify_transcript_ingest_outcome(outcome, &self.cancellation)
        })
    }

    fn capacity_release(&self) -> SessionHistoricalCapacityRelease {
        SessionHistoricalCapacityRelease::new(
            tracedecay_sessions::runtime::subscribe_history_capacity_release(&self.codex_discovery),
        )
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
    transcript_source_profile: ProfileRoot,
    cancellation: ObservationCancellation,
    codex_discovery: Arc<tracedecay_sessions::runtime::hosts::codex::CodexDiscoveryHub>,
    background_cpu: Arc<ProcessBackgroundCpuV1>,
    session_review: SessionReviewPort,
    codex_consumer: String,
    codex_registered: AtomicBool,
}

impl ProfileSessionHistoricalIngestor {
    pub fn new(
        database: RegisteredGlobalDbLeaseV1,
        registry_database: RegisteredGlobalDbLeaseV1,
        profile_identity: Arc<dyn ProfileIdentityReadPort>,
        transcript_source_profile: ProfileRoot,
        codex_discovery: Arc<tracedecay_sessions::runtime::hosts::codex::CodexDiscoveryHub>,
        background_cpu: Arc<ProcessBackgroundCpuV1>,
        session_review: SessionReviewPort,
    ) -> Self {
        let source_home = transcript_source_profile
            .home()
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
        let codex_consumer = codex_consumer_key(
            "profile",
            profile_identity.brain_id().as_str(),
            profile_identity.profile_id().as_str(),
            "",
            &source_home,
        );
        codex_discovery.register(&codex_consumer, transcript_source_profile.home());
        Self {
            database,
            registry_database,
            profile_identity,
            transcript_source_profile,
            cancellation: ObservationCancellation::default(),
            codex_discovery,
            background_cpu,
            session_review,
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
                tracedecay_host_admission::session_ingest_authority::GlobalDbSessionIngestAuthority::new(self.database.clone())
                    .with_background_cpu(Arc::clone(&self.background_cpu))
                    .with_session_review(self.session_review);
            let registry_authority =
                tracedecay_host_admission::session_ingest_authority::GlobalDbSessionIngestAuthority::new(self.registry_database.clone())
                    .with_background_cpu(Arc::clone(&self.background_cpu));
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
            let outcome = tracedecay_sessions::runtime::with_transcript_source_profile(
                self.transcript_source_profile.clone(),
                pass,
            )
            .await;
            classify_transcript_ingest_outcome(outcome, &self.cancellation)
        })
    }

    fn capacity_release(&self) -> SessionHistoricalCapacityRelease {
        SessionHistoricalCapacityRelease::new(
            tracedecay_sessions::runtime::subscribe_history_capacity_release(&self.codex_discovery),
        )
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
    let blocked = pass_reason_code(&outcome.failures, false);
    if !blocked.is_empty() {
        return SessionHistoricalIngestOutcome::Blocked {
            reason_code: blocked,
            made_progress,
        };
    }
    let retryable = pass_reason_code(&outcome.failures, true);
    if !retryable.is_empty() {
        return SessionHistoricalIngestOutcome::Retryable {
            reason_code: retryable,
            made_progress,
        };
    }
    if outcome.has_deferred_work() {
        return SessionHistoricalIngestOutcome::Pending { made_progress };
    }
    SessionHistoricalIngestOutcome::Complete
}

/// Names the pass blocker(s) per provider. A single-provider pass keeps the
/// raw reason code so existing status strings stay stable. Multi-provider
/// passes use `provider:reason` so rotation order cannot hide the real stall.
fn pass_reason_code(
    failures: &[tracedecay_sessions::runtime::TranscriptCatchUpFailure],
    retryable: bool,
) -> String {
    let provider_count = failures
        .iter()
        .map(|failure| failure.provider)
        .collect::<BTreeSet<_>>()
        .len();
    let mut blockers = BTreeMap::new();
    for failure in failures {
        if failure.retryable != retryable {
            continue;
        }
        blockers
            .entry(failure.provider)
            .or_insert(failure.reason_code);
    }
    if blockers.is_empty() {
        return String::new();
    }
    if provider_count <= 1 {
        let mut reasons = blockers.into_values();
        return match reasons.next() {
            Some(reason) => reason.to_owned(),
            None => String::new(),
        };
    }
    blockers
        .into_iter()
        .map(|(provider, reason)| format!("{provider}:{reason}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::{SessionHistoricalIngestOutcome, classify_transcript_ingest_outcome};
    use tracedecay_sessions::observation::ObservationCancellation;
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
            coverage_advanced: false,
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
                reason_code: "authority_write_failed".to_owned(),
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
                reason_code: "invalid_observation_contract".to_owned(),
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
                coverage_advanced: false,
            },
            &ObservationCancellation::default(),
        );

        assert_eq!(
            outcome,
            SessionHistoricalIngestOutcome::Blocked {
                reason_code: "observation_cursor_advance_collision".to_owned(),
                made_progress: true,
            }
        );
        assert!(
            outcome.made_progress(),
            "the healthy Claude and Codex rows must still reach temporal projection"
        );
    }

    fn ingest_outcome_with_failures(
        failures: Vec<TranscriptCatchUpFailure>,
    ) -> TranscriptIngestOutcome {
        TranscriptIngestOutcome {
            stats: tracedecay_sessions::runtime::shared::TranscriptIngestStats::default(),
            failures,
            coverage: IngestPassCoverage::Complete,
            scheduling_state_written: false,
            coverage_advanced: false,
        }
    }

    /// Provider rotation folds retryable failures in discovery order. Status
    /// must name each provider's blocker, not whichever failure happened to
    /// land first (the reporting host showed `historical_retry:cursor_conflict`
    /// while Claude was blocked on projection storage).
    #[test]
    fn multi_provider_retryable_pass_names_each_provider_blocker() {
        let outcome = classify_transcript_ingest_outcome(
            ingest_outcome_with_failures(vec![
                TranscriptCatchUpFailure {
                    provider: "opencode",
                    source: "observation",
                    reason_code: "cursor_conflict",
                    retryable: true,
                    source_locator: None,
                },
                TranscriptCatchUpFailure {
                    provider: "claude",
                    source: "observation",
                    reason_code: "projection_storage_retry_scheduled",
                    retryable: true,
                    source_locator: None,
                },
            ]),
            &ObservationCancellation::default(),
        );

        let SessionHistoricalIngestOutcome::Retryable {
            ref reason_code,
            made_progress,
        } = outcome
        else {
            panic!("multi-provider retryable failures must stay retryable, got {outcome:?}");
        };
        assert!(!made_progress);
        assert!(
            reason_code.contains("claude")
                && reason_code.contains("projection_storage_retry_scheduled"),
            "status must name Claude's blocker, got {reason_code}"
        );
        assert!(
            reason_code.contains("opencode") && reason_code.contains("cursor_conflict"),
            "status must name OpenCode's blocker, got {reason_code}"
        );
        assert_ne!(
            reason_code, "cursor_conflict",
            "rotation order must not hide later providers behind the first retryable failure"
        );
        assert!(outcome.needs_another_pass());
    }

    /// A non-retryable provider failure is the pass blocker even when an
    /// earlier retryable cursor conflict is folded first.
    #[test]
    fn multi_provider_pass_names_the_non_retryable_provider_blocker() {
        let outcome = classify_transcript_ingest_outcome(
            ingest_outcome_with_failures(vec![
                TranscriptCatchUpFailure {
                    provider: "opencode",
                    source: "observation",
                    reason_code: "cursor_conflict",
                    retryable: true,
                    source_locator: None,
                },
                TranscriptCatchUpFailure {
                    provider: "claude",
                    source: "observation",
                    reason_code: "projection_store_corrupt",
                    retryable: false,
                    source_locator: None,
                },
            ]),
            &ObservationCancellation::default(),
        );

        let SessionHistoricalIngestOutcome::Blocked {
            ref reason_code,
            made_progress,
        } = outcome
        else {
            panic!("a non-retryable provider failure must block the pass, got {outcome:?}");
        };
        assert!(!made_progress);
        assert!(
            reason_code.contains("projection_store_corrupt"),
            "status must name the corrupt projection store, got {reason_code}"
        );
        assert!(
            reason_code.contains("claude"),
            "status must name the provider that is blocked, got {reason_code}"
        );
        assert!(
            !reason_code.contains("cursor_conflict"),
            "a retryable sibling must not become the blocked label, got {reason_code}"
        );
        assert!(!outcome.needs_another_pass());
    }
}
