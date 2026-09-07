use std::path::PathBuf;
use std::time::Instant;

use tracedecay_runtime_core::cancellation::MonotonicDeadline;
use tracedecay_runtime_core::git_discovery::{
    GitDiscoveryUnknown, GitRepositoryIdentity, GitRepositoryIdentityOutcome,
    discover_repository_identity,
};

use super::GIT_OBSERVATION_BUDGET;

pub enum WatchIdentityResolution {
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
pub enum IdentityDiscoveryDisposition {
    Watch(GitRepositoryIdentity),
    ShutDown,
    NotRepository,
    Retry,
}

pub fn identity_discovery_disposition(
    resolution: WatchIdentityResolution,
) -> IdentityDiscoveryDisposition {
    match resolution {
        WatchIdentityResolution::Ready(identity) => IdentityDiscoveryDisposition::Watch(identity),
        WatchIdentityResolution::Cancelled => IdentityDiscoveryDisposition::ShutDown,
        WatchIdentityResolution::NotRepository => IdentityDiscoveryDisposition::NotRepository,
        WatchIdentityResolution::Unknown => IdentityDiscoveryDisposition::Retry,
    }
}

#[hotpath::measure(label = "daemon.git.watch.identity", future = true)]
pub async fn resolve_watch_identity(
    project_root: PathBuf,
    cancellation: tracedecay_session_memory::context::CancellationToken,
) -> WatchIdentityResolution {
    let resolution = discover_watch_identity(project_root, cancellation).await;
    record_identity_resolution(&resolution);
    resolution
}

/// Bounded typed discovery-outcome counters. `Unknown` (a bounded git
/// timeout) drives the admission backoff retry owner, so its rate versus
/// `resolved` separates discovery churn from healthy admission cost.
fn record_identity_resolution(resolution: &WatchIdentityResolution) {
    match resolution {
        WatchIdentityResolution::Ready(_) => {
            hotpath::gauge!("daemon.git.watch.identity.resolved_total").inc(1_u64);
        }
        WatchIdentityResolution::Cancelled => {
            hotpath::gauge!("daemon.git.watch.identity.cancelled_total").inc(1_u64);
        }
        WatchIdentityResolution::NotRepository => {
            hotpath::gauge!("daemon.git.watch.identity.not_repository_total").inc(1_u64);
        }
        WatchIdentityResolution::Unknown => {
            hotpath::gauge!("daemon.git.watch.identity.unknown_total").inc(1_u64);
        }
    }
}

async fn discover_watch_identity(
    project_root: PathBuf,
    cancellation: tracedecay_session_memory::context::CancellationToken,
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
