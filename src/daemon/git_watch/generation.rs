use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::time::Instant;

pub(super) const SYNC_RETRY_INITIAL: Duration = Duration::from_secs(1);
pub(super) const SYNC_RETRY_MAX: Duration = Duration::from_mins(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SnapshotGeneration {
    pub(super) root: PathBuf,
    pub(super) branch: Option<String>,
    head: String,
}

impl SnapshotGeneration {
    pub(super) fn unavailable(root: &Path, kind: GenerationErrorKind) -> Self {
        Self {
            root: root.to_path_buf(),
            branch: None,
            head: format!("unavailable:{}", kind.as_str()),
        }
    }

    #[cfg(test)]
    pub(super) fn test(
        root: impl Into<PathBuf>,
        branch: impl Into<String>,
        head: impl Into<String>,
    ) -> Self {
        Self {
            root: root.into(),
            branch: Some(branch.into()),
            head: head.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GenerationErrorKind {
    Repository,
    Head,
    Commit,
}

impl GenerationErrorKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "repository_unavailable",
            Self::Head => "head_unavailable",
            Self::Commit => "commit_unavailable",
        }
    }
}

#[derive(Debug)]
pub(super) struct GenerationError {
    pub(super) kind: GenerationErrorKind,
    root: PathBuf,
    detail: String,
}

impl GenerationError {
    fn new(kind: GenerationErrorKind, root: &Path, detail: impl Into<String>) -> GenerationError {
        Self {
            kind,
            root: root.to_path_buf(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for GenerationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} for {}: {}",
            self.kind.as_str(),
            self.root.display(),
            self.detail
        )
    }
}

pub(super) fn snapshot_generation(root: &Path) -> Result<SnapshotGeneration, GenerationError> {
    let canonical = root.canonicalize().map_err(|error| {
        GenerationError::new(GenerationErrorKind::Repository, root, error.to_string())
    })?;
    let repository = gix::discover(&canonical).map_err(|error| {
        GenerationError::new(
            GenerationErrorKind::Repository,
            &canonical,
            error.to_string(),
        )
    })?;
    let head = repository.rev_parse_single("HEAD").map_err(|error| {
        GenerationError::new(GenerationErrorKind::Head, &canonical, error.to_string())
    })?;
    let object = head.object().map_err(|error| {
        GenerationError::new(GenerationErrorKind::Commit, &canonical, error.to_string())
    })?;
    let commit = object.peel_to_commit().map_err(|error| {
        GenerationError::new(GenerationErrorKind::Commit, &canonical, error.to_string())
    })?;

    Ok(SnapshotGeneration {
        root: canonical.clone(),
        branch: crate::branch::current_branch(&canonical),
        head: commit.id.to_string(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GenerationDecision {
    Attempt,
    Unchanged,
    InFlight,
    Backoff { remaining: Duration },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReservationError {
    Unchanged,
    InFlight,
    Backoff { remaining: Duration },
}

#[derive(Clone, Debug)]
struct FailedGeneration {
    generation: SnapshotGeneration,
    retry_at: Instant,
    delay: Duration,
}

#[derive(Debug)]
struct InFlightGeneration {
    active: Arc<AtomicBool>,
}

/// Releases an in-flight generation claim if its watcher task is cancelled.
#[must_use]
#[derive(Debug)]
pub(super) struct GenerationReservation {
    active: Arc<AtomicBool>,
}

impl Drop for GenerationReservation {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

#[derive(Debug, Default)]
pub(super) struct GenerationGate {
    successful: Option<SnapshotGeneration>,
    failed: Option<FailedGeneration>,
    in_flight: Option<InFlightGeneration>,
}

impl GenerationGate {
    pub(super) fn decision(
        &mut self,
        generation: &SnapshotGeneration,
        now: Instant,
    ) -> GenerationDecision {
        self.clear_cancelled_reservation();
        if self.successful.as_ref() == Some(generation) {
            return GenerationDecision::Unchanged;
        }
        if self.in_flight.is_some() {
            return GenerationDecision::InFlight;
        }
        let Some(failed) = self
            .failed
            .as_ref()
            .filter(|failed| failed.generation == *generation)
        else {
            return GenerationDecision::Attempt;
        };
        if now >= failed.retry_at {
            GenerationDecision::Attempt
        } else {
            GenerationDecision::Backoff {
                remaining: failed.retry_at.saturating_duration_since(now),
            }
        }
    }

    pub(super) fn reserve(
        &mut self,
        generation: &SnapshotGeneration,
        now: Instant,
    ) -> Result<GenerationReservation, ReservationError> {
        match self.decision(generation, now) {
            GenerationDecision::Attempt => {
                let active = Arc::new(AtomicBool::new(true));
                self.in_flight = Some(InFlightGeneration {
                    active: Arc::clone(&active),
                });
                Ok(GenerationReservation { active })
            }
            GenerationDecision::Unchanged => Err(ReservationError::Unchanged),
            GenerationDecision::InFlight => Err(ReservationError::InFlight),
            GenerationDecision::Backoff { remaining } => {
                Err(ReservationError::Backoff { remaining })
            }
        }
    }

    /// Records a successful sync only when the repository generation observed
    /// after the operation still matches the generation that claimed the lane.
    pub(super) fn record_success_if_current(
        &mut self,
        generation: SnapshotGeneration,
        observed: &SnapshotGeneration,
    ) -> bool {
        if generation != *observed {
            self.finish_reservation();
            return false;
        }
        self.record_success(generation);
        true
    }

    /// Releases a claim whose repository moved before work could begin.
    pub(super) fn release_if_stale(
        &mut self,
        generation: &SnapshotGeneration,
        observed: &SnapshotGeneration,
    ) -> bool {
        if generation == observed {
            return false;
        }
        self.finish_reservation();
        true
    }

    pub(super) fn record_success(&mut self, generation: SnapshotGeneration) {
        self.successful = Some(generation);
        self.failed = None;
        self.finish_reservation();
    }

    pub(super) fn record_failure(
        &mut self,
        generation: SnapshotGeneration,
        now: Instant,
    ) -> Duration {
        let delay = self
            .failed
            .as_ref()
            .filter(|failed| failed.generation == generation)
            .map_or(SYNC_RETRY_INITIAL, |failed| {
                (failed.delay * 2).min(SYNC_RETRY_MAX)
            });
        self.failed = Some(FailedGeneration {
            generation,
            retry_at: now + delay,
            delay,
        });
        self.finish_reservation();
        delay
    }

    fn clear_cancelled_reservation(&mut self) {
        if self
            .in_flight
            .as_ref()
            .is_some_and(|in_flight| !in_flight.active.load(Ordering::Acquire))
        {
            self.in_flight = None;
        }
    }

    fn finish_reservation(&mut self) {
        if let Some(in_flight) = self.in_flight.take() {
            in_flight.active.store(false, Ordering::Release);
        }
    }
}
