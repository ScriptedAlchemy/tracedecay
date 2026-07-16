use std::path::{Path, PathBuf};

use crate::global_db::GlobalDb;
use crate::sessions::shared::TranscriptIngestStats;

use super::failure::{IngestPassCoverage, IngestPassOutcome, TranscriptCatchUpFailure};
use super::user::{
    ingest_user_global_sources_for_provider_with_roots, registered_project_roots_from,
};

pub(crate) struct TranscriptIngestOutcome {
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

    pub(crate) fn is_success(&self) -> bool {
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
/// server created during daemon startup. Live hooks still call
/// [`crate::sessions::ingest_user_global_sources`] directly, so the cooldown cannot hide a
/// completed turn.
pub(crate) async fn ingest_user_global_sources_for_startup_with_db(
    db: &GlobalDb,
    registry_db: &GlobalDb,
    profile_root: &Path,
) -> TranscriptIngestOutcome {
    let Some(mut guard) = StartupUserIngestGuard::claim(profile_root.to_path_buf()) else {
        return TranscriptIngestOutcome::new(TranscriptIngestStats::default(), Vec::new());
    };
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
    let outcome =
        ingest_user_global_sources_for_provider_with_roots(db, profile_root, None, roots).await;
    if !outcome.is_success() {
        return outcome;
    }
    guard.completed = true;
    outcome
}
