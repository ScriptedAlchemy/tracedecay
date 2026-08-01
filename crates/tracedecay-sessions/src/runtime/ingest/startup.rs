use std::path::{Path, PathBuf};

use super::authority::SessionIngestAuthority;
use crate::observation::ObservationCancellation;
use crate::runtime::shared::TranscriptIngestStats;
use tracedecay_domain::{BrainId, UserProfileId};

use super::failure::{IngestPassCoverage, IngestPassOutcome, TranscriptCatchUpFailure};
use super::user::{
    ingest_user_global_sources_for_provider_with_roots_and_cancellation,
    registered_project_roots_from,
};

pub struct TranscriptIngestOutcome {
    pub stats: TranscriptIngestStats,
    pub failures: Vec<TranscriptCatchUpFailure>,
    pub coverage: IngestPassCoverage,
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
        }
    }

    pub(super) fn from_pass(outcome: IngestPassOutcome) -> Self {
        Self {
            stats: outcome.stats,
            failures: outcome.failures,
            coverage: outcome.coverage,
        }
    }

    pub fn is_success(&self) -> bool {
        self.coverage.is_complete() && self.failures.is_empty()
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

pub(super) struct StartupUserIngestGuard {
    profile_root: PathBuf,
    pub(super) completed: bool,
}

impl StartupUserIngestGuard {
    pub(super) fn claim(profile_root: PathBuf) -> Option<Self> {
        let ingests = STARTUP_USER_INGESTS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
        let mut ingests = ingests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = ingests.entry(profile_root.clone()).or_default();
        if state.running
            || state
                .last_completed
                .is_some_and(|completed| completed.elapsed() < STARTUP_USER_INGEST_COOLDOWN)
        {
            return None;
        }
        state.running = true;
        Some(Self {
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

async fn ingest_user_global_sources_for_startup_inner<A: SessionIngestAuthority>(
    registered: (&BrainId, &UserProfileId, &A),
    registry_db: &A,
    profile_root: &Path,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestOutcome {
    if cancellation.is_cancelled() {
        return TranscriptIngestOutcome::new(
            TranscriptIngestStats::default(),
            vec![TranscriptCatchUpFailure::pass_cancelled()],
        );
    }
    let Some(mut guard) = StartupUserIngestGuard::claim(profile_root.to_path_buf()) else {
        return TranscriptIngestOutcome::new(TranscriptIngestStats::default(), Vec::new());
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
    let outcome = ingest_user_global_sources_for_provider_with_roots_and_cancellation(
        brain_id,
        profile_id,
        registered,
        profile_root,
        None,
        roots,
        cancellation,
    )
    .await;
    if !outcome.is_success() {
        return outcome;
    }
    guard.completed = true;
    outcome
}
