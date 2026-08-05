use std::path::PathBuf;
use std::time::Instant;

use tracedecay_runtime_core::cancellation::MonotonicDeadline;
use tracedecay_runtime_core::git_discovery::{
    GitDiscoveryUnknown, GitRepositoryIdentity, GitRepositoryIdentityOutcome,
    discover_repository_identity,
};

use super::GIT_OBSERVATION_BUDGET;

pub(super) enum WatchIdentityResolution {
    Ready(GitRepositoryIdentity),
    Cancelled,
    NotRepository,
    Unknown,
}

pub(super) async fn resolve_watch_identity(
    project_root: PathBuf,
    cancellation: crate::application::context::CancellationToken,
) -> WatchIdentityResolution {
    let Some(deadline) = Instant::now().checked_add(GIT_OBSERVATION_BUDGET) else {
        return WatchIdentityResolution::Unknown;
    };
    let discovery = discover_repository_identity(
        &project_root,
        MonotonicDeadline::at(deadline),
        &cancellation,
    );
    let outcome = tokio::select! {
        biased;
        () = cancellation.cancelled() => return WatchIdentityResolution::Cancelled,
        outcome = tokio::time::timeout(GIT_OBSERVATION_BUDGET, discovery) => match outcome {
            Ok(outcome) => outcome,
            Err(_) => return WatchIdentityResolution::Unknown,
        }
    };
    match outcome {
        GitRepositoryIdentityOutcome::Resolved(identity) => {
            WatchIdentityResolution::Ready(identity)
        }
        GitRepositoryIdentityOutcome::NotRepository => WatchIdentityResolution::NotRepository,
        GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::Cancelled)
            if cancellation.is_cancelled() =>
        {
            WatchIdentityResolution::Cancelled
        }
        GitRepositoryIdentityOutcome::Unknown(_) => WatchIdentityResolution::Unknown,
    }
}
