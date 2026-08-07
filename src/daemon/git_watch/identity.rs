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

/// How admission responds to one identity resolution.
///
/// A bounded git timeout (`Unknown`) is uncertainty, not absence: it must
/// retry with backoff instead of leaving the repository unwatched forever.
/// Only a definitive `NotRepository` refuses without retry.
pub(super) enum IdentityDiscoveryDisposition {
    Watch(GitRepositoryIdentity),
    ShutDown,
    NotRepository,
    Retry,
}

pub(super) fn identity_discovery_disposition(
    resolution: WatchIdentityResolution,
) -> IdentityDiscoveryDisposition {
    match resolution {
        WatchIdentityResolution::Ready(identity) => IdentityDiscoveryDisposition::Watch(identity),
        WatchIdentityResolution::Cancelled => IdentityDiscoveryDisposition::ShutDown,
        WatchIdentityResolution::NotRepository => IdentityDiscoveryDisposition::NotRepository,
        WatchIdentityResolution::Unknown => IdentityDiscoveryDisposition::Retry,
    }
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
