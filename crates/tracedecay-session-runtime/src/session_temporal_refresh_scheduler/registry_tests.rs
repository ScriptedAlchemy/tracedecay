use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::FutureExt;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;

use super::history::{
    SessionHistoricalCapacityRelease, SessionHistoricalIngestOutcome, SessionHistoricalIngestPass,
    SessionHistoricalIngestor, SharedSessionHistoricalIngestor,
};
use super::registry::SessionTemporalRefreshSchedulerRegistry;
use crate::StoreOwnerKey;

/// A history pass that ignores cancellation until the test releases it. The
/// pass holds a clone of `held` for as long as the worker owns it, standing in
/// for the lease clones a live worker retains. A `busy` pass is never released
/// and blocks its thread in every poll, so an abort lands mid-poll.
#[derive(Default)]
struct HeldHistory {
    held: Arc<()>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    cancelled: AtomicBool,
    busy: bool,
}

impl HeldHistory {
    fn worker_holds_pass(&self) -> bool {
        Arc::strong_count(&self.held) > 1
    }
}

impl SessionHistoricalIngestor for HeldHistory {
    fn run_pass(&self) -> SessionHistoricalIngestPass<'_> {
        Box::pin(async move {
            let _held = Arc::clone(&self.held);
            self.entered.notify_one();
            if self.busy {
                loop {
                    std::thread::sleep(Duration::from_millis(50));
                    tokio::task::yield_now().await;
                }
            }
            self.release.notified().await;
            SessionHistoricalIngestOutcome::Cancelled
        })
    }

    fn capacity_release(&self) -> SessionHistoricalCapacityRelease {
        SessionHistoricalCapacityRelease::new(Vec::new())
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

fn project_owner(name: &str) -> StoreOwnerKey {
    let root = PathBuf::from("/refresh-registry-tests").join(name);
    StoreOwnerKey {
        profile_root: root.join("profile"),
        global_db_path: root.join("global.db"),
        project_id: Some(name.to_owned()),
        store_root: root.join("store"),
        graph_db_path: root.join("graph.db"),
    }
}

async fn mount_held_project(
    registry: &SessionTemporalRefreshSchedulerRegistry,
    harness: &RegisteredGlobalDbHarness,
    owner: &StoreOwnerKey,
    history: HeldHistory,
) -> Arc<HeldHistory> {
    let history = Arc::new(history);
    registry
        .ensure_project_with_history(
            owner.clone(),
            harness.registered.clone(),
            Arc::clone(&history) as SharedSessionHistoricalIngestor,
        )
        .await;
    history.entered.notified().await;
    history
}

/// Mounts a project whose worker holds its history pass and a profile entry.
async fn mount_drain_fixture(
    harness: &RegisteredGlobalDbHarness,
    name: &str,
) -> (
    SessionTemporalRefreshSchedulerRegistry,
    Arc<HeldHistory>,
    Arc<super::wake::SessionTemporalRefreshWakeState>,
) {
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let owner = project_owner(name);
    let history = mount_held_project(&registry, harness, &owner, HeldHistory::default()).await;
    registry
        .ensure_profile(
            PathBuf::from("/refresh-registry-tests")
                .join(name)
                .join("profile.db"),
            harness.registered.clone(),
        )
        .await;
    let state = registry.project_state(&owner).await.expect("project entry");
    (registry, history, state)
}

/// After a drain was dropped, the stop must already be issued and the next
/// drain must join the same worker, claiming completion only once it let go.
async fn assert_dropped_drain_retained(
    registry: &SessionTemporalRefreshSchedulerRegistry,
    history: &HeldHistory,
    state: &super::wake::SessionTemporalRefreshWakeState,
) {
    assert!(
        history.cancelled.load(Ordering::Acquire),
        "a dropped drain must still stop the history pass it removed"
    );
    assert!(
        state.cancelled.load(Ordering::Acquire),
        "a dropped drain must still cancel the worker it removed"
    );
    assert_eq!(registry.project_worker_count().await, 0);
    assert_eq!(registry.profile_worker_count().await, 0);
    assert!(history.worker_holds_pass());

    let mut drain = std::pin::pin!(registry.shutdown());
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut drain)
            .await
            .is_err(),
        "a repeated drain must join the retired worker, not report an empty registry"
    );
    history.release.notify_one();
    drain.await;
    assert!(
        !history.worker_holds_pass(),
        "drain completion must follow the worker releasing what it holds"
    );
    registry.shutdown().await;
}

/// The daemon drains the registry under one shared shutdown deadline. A drain
/// whose deadline already expired is polled once and dropped, after it removed
/// the entries from the registry but before anything else ran.
#[tokio::test]
async fn a_drain_dropped_after_its_first_poll_keeps_stop_and_join_authority() {
    let harness = RegisteredGlobalDbHarness::open("refresh-drain-first-poll").await;
    let (registry, history, state) = mount_drain_fixture(&harness, "drain-first-poll").await;

    assert!(
        registry.shutdown().now_or_never().is_none(),
        "the drain cannot finish while the worker holds its history pass"
    );

    assert_dropped_drain_retained(&registry, &history, &state).await;
}

/// A drain dropped by its deadline after the stop was signalled, while it
/// waits for the supervisors, still leaves their join to the next drain.
#[tokio::test]
async fn a_drain_dropped_while_joining_keeps_join_authority() {
    let harness = RegisteredGlobalDbHarness::open("refresh-drain-joining").await;
    let (registry, history, state) = mount_drain_fixture(&harness, "drain-joining").await;

    assert!(
        tokio::time::timeout(Duration::from_millis(50), registry.shutdown())
            .await
            .is_err(),
        "the drain cannot finish while the worker holds its history pass"
    );

    assert_dropped_drain_retained(&registry, &history, &state).await;
}

/// A worker that ignores cancellation past the drain grace is aborted. An
/// abort only requests cancellation, and a worker mid-poll on another thread
/// keeps running until that poll returns, so the drain must still wait until
/// the worker has dropped what it holds before it reports completion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drain_past_the_grace_joins_the_aborted_worker() {
    let harness = RegisteredGlobalDbHarness::open("refresh-aborted-worker").await;
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let owner = project_owner("aborted-worker");
    let busy = HeldHistory {
        busy: true,
        ..HeldHistory::default()
    };
    let history = mount_held_project(&registry, &harness, &owner, busy).await;

    registry.shutdown().await;

    assert!(history.cancelled.load(Ordering::Acquire));
    assert!(
        !history.worker_holds_pass(),
        "drain completion must follow the aborted worker releasing what it holds"
    );
}

/// Retiring a project while its worker ignores cancellation, then dropping the
/// retirement, must not let the owner admit a second worker beside the first.
#[tokio::test]
async fn a_readmitted_owner_waits_for_its_dropped_retirement() {
    let harness = RegisteredGlobalDbHarness::open("refresh-readmitted-owner").await;
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let owner = project_owner("readmitted-owner");
    let history = mount_held_project(&registry, &harness, &owner, HeldHistory::default()).await;

    assert!(
        registry.retire_project(&owner).now_or_never().is_none(),
        "retirement cannot finish while the worker holds its history pass"
    );
    assert!(history.cancelled.load(Ordering::Acquire));

    let mut readmit =
        std::pin::pin!(registry.ensure_project(owner.clone(), harness.registered.clone()));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut readmit)
            .await
            .is_err(),
        "the owner must not run a second worker while its retired one still runs"
    );
    history.release.notify_one();
    readmit.await;
    assert!(!history.worker_holds_pass());
    assert_eq!(registry.project_worker_count().await, 1);

    registry.shutdown().await;
}
