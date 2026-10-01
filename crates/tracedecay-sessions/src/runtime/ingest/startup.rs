use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use tokio::sync::watch;

use super::authority::SessionIngestAuthority;
use crate::observation::ObservationCancellation;
use crate::runtime::hosts::codex::CodexDiscoveryHub;
use crate::runtime::jsonl_observation_admission::shared_jsonl_memory_headroom;
use crate::runtime::shared::TranscriptIngestStats;
use tracedecay_domain::{BrainId, UserProfileId};

use super::failure::{IngestPassCoverage, IngestPassOutcome, TranscriptCatchUpFailure};
use super::scheduler::default_ingest_pass_bounds;
use super::user::{
    ingest_user_global_sources_for_provider_with_roots_and_cancellation,
    ingest_user_global_sources_for_provider_with_roots_bounded_and_codex_state,
    registered_project_roots_from,
};

pub struct TranscriptIngestOutcome {
    pub stats: TranscriptIngestStats,
    pub failures: Vec<TranscriptCatchUpFailure>,
    pub coverage: IngestPassCoverage,
    pub scheduling_state_written: bool,
    /// The pass durably advanced source coverage beyond its upserts.
    pub coverage_advanced: bool,
}

impl TranscriptIngestOutcome {
    pub(super) fn new(
        stats: TranscriptIngestStats,
        failures: Vec<TranscriptCatchUpFailure>,
    ) -> Self {
        Self {
            stats,
            failures,
            coverage: IngestPassCoverage::Complete,
            scheduling_state_written: false,
            coverage_advanced: false,
        }
    }

    pub(super) fn from_pass(outcome: IngestPassOutcome) -> Self {
        Self {
            stats: outcome.stats,
            failures: outcome.failures,
            coverage: outcome.coverage,
            scheduling_state_written: outcome.scheduling_state_written,
            coverage_advanced: outcome.coverage_advanced,
        }
    }

    pub fn is_success(&self) -> bool {
        self.coverage.is_complete() && self.failures.is_empty()
    }

    pub fn has_deferred_work(&self) -> bool {
        !self.coverage.is_complete()
    }

    /// The next pass starts from coverage this one committed, so it can run
    /// at once instead of waiting for released capacity.
    pub fn made_progress(&self) -> bool {
        self.stats.sessions_upserted > 0
            || self.stats.messages_upserted > 0
            || self.coverage_advanced
    }
}

const STARTUP_USER_INGEST_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Default)]
struct StartupUserIngestState {
    running: bool,
    last_completed: Option<std::time::Instant>,
}

static STARTUP_USER_INGESTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, StartupUserIngestState>>,
> = std::sync::OnceLock::new();

static STARTUP_USER_INGEST_RELEASED: LazyLock<watch::Sender<u64>> =
    LazyLock::new(|| watch::Sender::new(0));

/// Signals that capacity a backpressured history pass reported waiting on may
/// be available again: the profile sweep another pass held, the Codex
/// discovery scanner, or shared JSONL memory headroom. Receivers are
/// subscribed now, so a release during the pass that follows is not lost.
pub fn subscribe_history_capacity_release(
    codex_discovery: &CodexDiscoveryHub,
) -> Vec<watch::Receiver<u64>> {
    let mut signals = vec![
        STARTUP_USER_INGEST_RELEASED.subscribe(),
        codex_discovery.subscribe_scan_release(),
    ];
    signals.extend(shared_jsonl_memory_headroom());
    signals
}

pub(super) struct StartupUserIngestGuard {
    profile_root: PathBuf,
    pub(super) completed: bool,
}

pub(super) enum StartupUserIngestClaim {
    Acquired(StartupUserIngestGuard),
    Running,
    RecentlyCompleted,
}

impl StartupUserIngestGuard {
    pub(super) fn claim(profile_root: PathBuf) -> StartupUserIngestClaim {
        let ingests = STARTUP_USER_INGESTS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
        let mut ingests = ingests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = ingests.entry(profile_root.clone()).or_default();
        if state.running {
            return StartupUserIngestClaim::Running;
        }
        if state
            .last_completed
            .is_some_and(|completed| completed.elapsed() < STARTUP_USER_INGEST_COOLDOWN)
        {
            return StartupUserIngestClaim::RecentlyCompleted;
        }
        state.running = true;
        StartupUserIngestClaim::Acquired(Self {
            profile_root,
            completed: false,
        })
    }
}

impl Drop for StartupUserIngestGuard {
    fn drop(&mut self) {
        let Some(ingests) = STARTUP_USER_INGESTS.get() else {
            return;
        };
        let mut ingests = ingests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = ingests.entry(self.profile_root.clone()).or_default();
        state.running = false;
        if self.completed {
            state.last_completed = Some(std::time::Instant::now());
        }
        drop(ingests);
        STARTUP_USER_INGEST_RELEASED.send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
}

/// Coalesces the profile-wide user transcript sweep shared by every project
/// server created during daemon startup. Live hooks use the retained
/// registered profile authority, so the cooldown cannot hide a completed turn.
pub async fn ingest_user_global_sources_for_startup_with_db<A: SessionIngestAuthority>(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    registered: &A,
    registry_db: &A,
    profile_root: &Path,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestOutcome {
    ingest_user_global_sources_for_startup_inner(
        (brain_id, profile_id, registered),
        registry_db,
        profile_root,
        cancellation,
        None,
    )
    .await
}

pub async fn ingest_user_global_sources_for_startup_with_db_and_codex_state<
    A: SessionIngestAuthority,
>(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    registered: &A,
    registry_db: &A,
    profile_root: &Path,
    cancellation: &ObservationCancellation,
    codex_state: (&crate::runtime::hosts::codex::CodexDiscoveryHub, &str),
) -> TranscriptIngestOutcome {
    ingest_user_global_sources_for_startup_inner(
        (brain_id, profile_id, registered),
        registry_db,
        profile_root,
        cancellation,
        Some(codex_state),
    )
    .await
}

#[cfg(any(test, feature = "test-helpers"))]
pub async fn ingest_user_global_sources_for_startup_with_db_without_registered_authority<
    A: SessionIngestAuthority,
>(
    _db: &A,
    _registry_db: &A,
    _profile_root: &Path,
) -> TranscriptIngestOutcome {
    TranscriptIngestOutcome::new(
        TranscriptIngestStats::default(),
        vec![TranscriptCatchUpFailure::registered_authority_unavailable(
            "all",
        )],
    )
}

#[hotpath::measure(label = "sessions.ingest.startup", future = true)]
async fn ingest_user_global_sources_for_startup_inner<A: SessionIngestAuthority>(
    registered: (&BrainId, &UserProfileId, &A),
    registry_db: &A,
    profile_root: &Path,
    cancellation: &ObservationCancellation,
    codex_discovery: Option<(&crate::runtime::hosts::codex::CodexDiscoveryHub, &str)>,
) -> TranscriptIngestOutcome {
    if cancellation.is_cancelled() {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::pass_cancelled()],
        );
    }
    let mut guard = match StartupUserIngestGuard::claim(profile_root.to_path_buf()) {
        StartupUserIngestClaim::Acquired(guard) => guard,
        StartupUserIngestClaim::Running => {
            return TranscriptIngestOutcome {
                stats: TranscriptIngestStats::default(),
                failures: Vec::new(),
                coverage: IngestPassCoverage::Partial { deferred_units: 1 },
                scheduling_state_written: false,
                coverage_advanced: false,
            };
        }
        StartupUserIngestClaim::RecentlyCompleted => {
            return TranscriptIngestOutcome::new(TranscriptIngestStats::default(), Vec::new());
        }
    };
    if cancellation.is_cancelled() {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::pass_cancelled()],
        );
    }
    let Some(roots) = registered_project_roots_from(registry_db).await else {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::new(
                "all",
                "project_registry",
                "project_registry_unavailable",
                true,
            )],
        );
    };
    if cancellation.is_cancelled() {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::pass_cancelled()],
        );
    }
    let (brain_id, profile_id, registered) = registered;
    let outcome = match codex_discovery {
        Some((hub, consumer)) => {
            ingest_user_global_sources_for_provider_with_roots_bounded_and_codex_state(
                (brain_id, profile_id, registered),
                profile_root,
                None,
                roots,
                default_ingest_pass_bounds(),
                cancellation,
                (hub, consumer),
            )
            .await
            .into_transcript_outcome()
        }
        None => {
            ingest_user_global_sources_for_provider_with_roots_and_cancellation(
                brain_id,
                profile_id,
                registered,
                profile_root,
                None,
                roots,
                cancellation,
            )
            .await
        }
    };
    if !outcome.is_success() {
        return outcome;
    }
    guard.completed = true;
    outcome
}
