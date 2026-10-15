use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::FutureExt;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness;
use tracedecay_runtime_core::native_runtime;

use super::super::history::{
    SessionHistoricalCapacityRelease, SessionHistoricalIngestOutcome, SessionHistoricalIngestPass,
    SessionHistoricalIngestor, SharedSessionHistoricalIngestor,
};
use super::super::wake::SessionTemporalRefreshWakeState;
use super::SessionTemporalRefreshSchedulerRegistry;
use crate::StoreOwnerKey;

/// A history pass that ignores cancellation until the test releases it. The
/// pass holds a clone of `held` for as long as the worker owns it, standing in
/// for the lease clones a live worker retains. A `busy` pass is never released
/// and blocks its thread in every poll, so an abort lands mid-poll. A
/// `panic_once` history panics its worker in the first pass.
#[derive(Default)]
struct HeldHistory {
    held: Arc<()>,
    entered: tokio::sync::Notify,
    passes: AtomicUsize,
    release: tokio::sync::Notify,
    cancelled: AtomicBool,
    busy: bool,
    panic_once: AtomicBool,
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
            self.passes.fetch_add(1, Ordering::AcqRel);
            self.entered.notify_one();
            assert!(
                !self.panic_once.swap(false, Ordering::AcqRel),
                "held history panics its worker once"
            );
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
    Arc<SessionTemporalRefreshWakeState>,
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
    state: &SessionTemporalRefreshWakeState,
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
///
/// Runs without Tokio, so a supervisor or worker that still reached Tokio
/// would panic instead of draining.
#[test]
fn a_drain_past_the_grace_joins_the_aborted_worker() {
    native_runtime::block_on(async {
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
    })
    .expect("native runtime");
}

/// A worker that panics is replaced after the retry backoff, and the
/// replacement runs the history pass again.
#[test]
fn a_panicked_worker_is_restarted() {
    native_runtime::block_on(async {
        let harness = RegisteredGlobalDbHarness::open("refresh-panicked-worker").await;
        let registry = SessionTemporalRefreshSchedulerRegistry::default();
        let owner = project_owner("panicked-worker");
        let panics = HeldHistory {
            panic_once: AtomicBool::new(true),
            ..HeldHistory::default()
        };
        let history = mount_held_project(&registry, &harness, &owner, panics).await;

        native_runtime::timeout(Duration::from_secs(10), async {
            while history.passes.load(Ordering::Acquire) < 2 {
                history.entered.notified().await;
            }
        })
        .await
        .expect("the supervisor must restart a panicked worker");
        assert!(history.worker_holds_pass());

        history.release.notify_one();
        registry.shutdown().await;
        assert!(!history.worker_holds_pass());
    })
    .expect("native runtime");
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

/// After a dropped retirement, a repeated `retire_project` finds nothing in
/// the map; it must still join the worker the dropped attempt returned to
/// `retired`, since callers treat its return as permission to release the
/// owner's stores.
#[tokio::test]
async fn a_repeated_retirement_joins_the_retired_worker() {
    let harness = RegisteredGlobalDbHarness::open("refresh-repeated-retirement").await;
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let owner = project_owner("repeated-retirement");
    let history = mount_held_project(&registry, &harness, &owner, HeldHistory::default()).await;

    assert!(
        registry.retire_project(&owner).now_or_never().is_none(),
        "retirement cannot finish while the worker holds its history pass"
    );
    assert!(history.cancelled.load(Ordering::Acquire));

    let mut retire = std::pin::pin!(registry.retire_project(&owner));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut retire)
            .await
            .is_err(),
        "a repeated retirement must join the retired worker, not report an empty registry"
    );
    history.release.notify_one();
    retire.await;
    assert!(
        !history.worker_holds_pass(),
        "retirement completion must follow the worker releasing what it holds"
    );

    registry.shutdown().await;
}

/// A rekey dropped while it joins the retired worker leaves that worker, its
/// route, and its queued requests in `retired`. Until a retry runs, the route
/// reports no live target; the retry joins the worker and lands both on the
/// new owner instead of spawning it beside a live worker without them.
#[tokio::test]
async fn a_retried_rekey_recovers_the_dropped_attempts_route_and_requests() {
    let harness = RegisteredGlobalDbHarness::open("refresh-dropped-rekey").await;
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let old_owner = project_owner("dropped-rekey-old");
    let new_owner = project_owner("dropped-rekey-new");
    let history = Arc::new(HeldHistory::default());
    let old_wake = registry
        .ensure_project_with_history(
            old_owner.clone(),
            harness.registered.clone(),
            Arc::clone(&history) as SharedSessionHistoricalIngestor,
        )
        .await;
    history.entered.notified().await;
    let old_state = registry
        .project_state(&old_owner)
        .await
        .expect("project entry");
    old_state.requeue_request(tracedecay_store::SessionRefreshBeginOrJoinRequestV1::new(
        tracedecay_domain::SessionId::new("session.dropped-rekey").unwrap(),
        tracedecay_store::SessionRefreshFrontierV1::new(0, 0).unwrap(),
    ));
    assert_eq!(old_state.queued_request_count(), 1);

    assert!(
        registry
            .rekey_project(&old_owner, new_owner.clone(), harness.registered.clone())
            .now_or_never()
            .is_none(),
        "the rekey cannot finish while the retired worker holds its history pass"
    );
    assert!(
        !old_wake.wake(),
        "no worker owns the route of a dropped rekey, so a wake must not be accepted"
    );

    let mut rekey = std::pin::pin!(registry.rekey_project(
        &old_owner,
        new_owner.clone(),
        harness.registered.clone(),
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut rekey)
            .await
            .is_err(),
        "the retry must join the retired worker its first attempt left behind"
    );
    history.release.notify_one();
    rekey.await;

    assert!(registry.project_state(&old_owner).await.is_none());
    let new_state = registry
        .project_state(&new_owner)
        .await
        .expect("new owner entry");
    assert_eq!(
        new_state.queued_request_count(),
        1,
        "the retry must land the retired owner's queued request on the new owner"
    );
    assert!(
        old_wake.wake(),
        "the old route must reach the new owner once the rekey completes"
    );
    assert!(new_state.take_dirty());

    registry.shutdown().await;
}

/// A rekey dropped after its join — the retirement consumed the finished
/// worker's entry, so neither `retired` nor the map has anything for a
/// retry to join — keeps the old route and the queued requests in the
/// parked handoff. The route reports no live target until a retry lands
/// them on the new owner.
#[tokio::test]
async fn a_dropped_rekey_keeps_its_route_and_requests_after_the_worker_finished() {
    let harness = RegisteredGlobalDbHarness::open("refresh-finished-rekey").await;
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let old_owner = project_owner("finished-rekey-old");
    let new_owner = project_owner("finished-rekey-new");
    let history = Arc::new(HeldHistory::default());
    let old_wake = registry
        .ensure_project_with_history(
            old_owner.clone(),
            harness.registered.clone(),
            Arc::clone(&history) as SharedSessionHistoricalIngestor,
        )
        .await;
    history.entered.notified().await;
    let old_state = registry
        .project_state(&old_owner)
        .await
        .expect("project entry");
    old_state.requeue_request(tracedecay_store::SessionRefreshBeginOrJoinRequestV1::new(
        tracedecay_domain::SessionId::new("session.finished-rekey").unwrap(),
        tracedecay_store::SessionRefreshFrontierV1::new(0, 0).unwrap(),
    ));
    assert_eq!(old_state.queued_request_count(), 1);

    {
        let mut rekey = std::pin::pin!(registry.rekey_project(
            &old_owner,
            new_owner.clone(),
            harness.registered.clone(),
        ));
        assert!(
            rekey.as_mut().now_or_never().is_none(),
            "the rekey cannot finish while the retired worker holds its history pass"
        );
        // The rekey released the map lock before its join; holding it now
        // parks the attempt at the lock it reacquires after joining, so the
        // drop below lands past the finished worker's join.
        let hold = registry.project.lock().await;
        history.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while history.worker_holds_pass() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the cancelled supervisor finishes before the rekey is dropped");
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rekey.as_mut())
                .await
                .is_err(),
            "the rekey must wait on the held map lock once its join finished"
        );
        drop(hold);
    }
    assert!(
        !old_wake.wake(),
        "a rekey dropped after its join must leave the route without a live target"
    );

    registry
        .rekey_project(&old_owner, new_owner.clone(), harness.registered.clone())
        .await;
    let resumed = registry
        .project_state(&new_owner)
        .await
        .expect("new owner entry");
    assert_eq!(
        resumed.queued_request_count(),
        1,
        "a rekey dropped after its join must retain the requests it staged"
    );
    assert!(
        old_wake.wake(),
        "the old route must reach the new owner once the retry completes"
    );

    registry.shutdown().await;
}

/// A dropped A-to-B parks A's route. A C-to-A readmission then installs C's
/// route as A's live wake, so the next A-to-D rekey must land both routes —
/// the parked one and the live one — on D, or callers holding either handle
/// are stranded on a cancelled state.
#[tokio::test]
async fn a_rekey_after_a_readmission_lands_every_route_and_request() {
    let harness = RegisteredGlobalDbHarness::open("refresh-readmitted-rekey").await;
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let a_owner = project_owner("readmitted-rekey-old");
    let b_owner = project_owner("readmitted-rekey-first");
    let c_owner = project_owner("readmitted-rekey-carrier");
    let d_owner = project_owner("readmitted-rekey-final");
    let e_owner = project_owner("readmitted-rekey-last");

    let a_history = Arc::new(HeldHistory::default());
    let a_wake = registry
        .ensure_project_with_history(
            a_owner.clone(),
            harness.registered.clone(),
            Arc::clone(&a_history) as SharedSessionHistoricalIngestor,
        )
        .await;
    a_history.entered.notified().await;
    let a_state = registry
        .project_state(&a_owner)
        .await
        .expect("project entry");
    a_state.requeue_request(tracedecay_store::SessionRefreshBeginOrJoinRequestV1::new(
        tracedecay_domain::SessionId::new("session.readmitted-a").unwrap(),
        tracedecay_store::SessionRefreshFrontierV1::new(0, 0).unwrap(),
    ));

    {
        let mut rekey = std::pin::pin!(registry.rekey_project(
            &a_owner,
            b_owner.clone(),
            harness.registered.clone(),
        ));
        assert!(
            rekey.as_mut().now_or_never().is_none(),
            "the rekey cannot finish while the retired worker holds its history pass"
        );
        // Same placement as the finished-join drop: hold the map lock so the
        // attempt drops after its join consumed the entry, parking the
        // handoff with nothing left in `retired` or the map.
        let hold = registry.project.lock().await;
        a_history.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while a_history.worker_holds_pass() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the cancelled supervisor finishes before the rekey is dropped");
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rekey.as_mut())
                .await
                .is_err(),
            "the rekey must wait on the held map lock once its join finished"
        );
        drop(hold);
    }
    assert!(
        !a_wake.wake(),
        "the parked handoff's route must report no live target"
    );

    let c_history = Arc::new(HeldHistory::default());
    let c_wake = registry
        .ensure_project_with_history(
            c_owner.clone(),
            harness.registered.clone(),
            Arc::clone(&c_history) as SharedSessionHistoricalIngestor,
        )
        .await;
    c_history.entered.notified().await;
    c_history.release.notify_one();
    registry
        .rekey_project(&c_owner, a_owner.clone(), harness.registered.clone())
        .await;
    assert!(
        c_wake.wake(),
        "the readmission rekey installs its route as the owner's live wake"
    );
    assert!(
        !a_wake.wake(),
        "the parked handoff stays owed to its own destination"
    );
    let readmitted = registry
        .project_state(&a_owner)
        .await
        .expect("readmitted project entry");
    readmitted.requeue_request(tracedecay_store::SessionRefreshBeginOrJoinRequestV1::new(
        tracedecay_domain::SessionId::new("session.readmitted-live").unwrap(),
        tracedecay_store::SessionRefreshFrontierV1::new(0, 0).unwrap(),
    ));

    registry
        .rekey_project(&a_owner, d_owner.clone(), harness.registered.clone())
        .await;

    assert!(registry.project_state(&a_owner).await.is_none());
    let landed = registry
        .project_state(&d_owner)
        .await
        .expect("final owner entry");
    assert_eq!(
        landed.queued_request_count(),
        2,
        "the parked request and the readmitted entry's request must both reach the final owner"
    );
    assert!(
        a_wake.wake(),
        "the parked route must reach the new owner once the rekey completes"
    );
    assert!(
        c_wake.wake(),
        "the live route the readmission installed must reach the new owner too"
    );

    // The destination retains the complete route set it absorbed: a later
    // D-to-E rekey must still land both handles, or older callers die at
    // the next move.
    registry
        .rekey_project(&d_owner, e_owner.clone(), harness.registered.clone())
        .await;
    assert!(registry.project_state(&d_owner).await.is_none());
    registry
        .project_state(&e_owner)
        .await
        .expect("final destination entry");
    assert!(
        a_wake.wake(),
        "the parked route must survive every successive rekey"
    );
    assert!(
        c_wake.wake(),
        "the readmission route must survive every successive rekey"
    );

    registry.shutdown().await;
}

/// A readmission stuck joining a retired worker holds the lifecycle lock.
/// Shutdown must still cancel every other supervisor instead of expiring on
/// the shared deadline with those workers never signalled.
#[tokio::test]
async fn a_stuck_readmission_still_lets_shutdown_cancel_every_worker() {
    let harness = RegisteredGlobalDbHarness::open("refresh-stuck-readmission").await;
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let owner_a = project_owner("stuck-readmission-a");
    let owner_b = project_owner("stuck-readmission-b");
    let history_a = mount_held_project(&registry, &harness, &owner_a, HeldHistory::default()).await;
    let history_b = mount_held_project(&registry, &harness, &owner_b, HeldHistory::default()).await;
    let state_b = registry
        .project_state(&owner_b)
        .await
        .expect("project entry");

    assert!(
        registry.retire_project(&owner_a).now_or_never().is_none(),
        "retirement cannot finish while the worker holds its history pass"
    );
    let mut readmit =
        std::pin::pin!(registry.ensure_project(owner_a.clone(), harness.registered.clone()));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut readmit)
            .await
            .is_err(),
        "the owner must not run a second worker while its retired one still runs"
    );

    let mut drain = std::pin::pin!(registry.shutdown());
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut drain)
            .await
            .is_err(),
        "the drain cannot finish while the readmission holds the lifecycle lock"
    );
    assert!(
        state_b.cancelled.load(Ordering::Acquire),
        "shutdown must cancel unrelated workers without waiting on a stuck lifecycle join"
    );
    assert!(
        history_b.cancelled.load(Ordering::Acquire),
        "shutdown must cancel unrelated history passes without waiting on a stuck lifecycle join"
    );

    history_a.release.notify_one();
    readmit.await;
    history_b.release.notify_one();
    drain.await;
    assert!(!history_b.worker_holds_pass());
}

/// A rekey of a mapped worker lands its still-queued requests on the new
/// owner in the same attempt, not only the requests a dropped earlier
/// attempt left behind.
#[tokio::test]
async fn a_mapped_rekey_lands_the_workers_queued_requests() {
    let harness = RegisteredGlobalDbHarness::open("refresh-mapped-rekey").await;
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let old_owner = project_owner("mapped-rekey-old");
    let new_owner = project_owner("mapped-rekey-new");
    let history = mount_held_project(&registry, &harness, &old_owner, HeldHistory::default()).await;
    let old_wake = registry
        .ensure_project(old_owner.clone(), harness.registered.clone())
        .await;
    let old_state = registry
        .project_state(&old_owner)
        .await
        .expect("project entry");
    old_state.requeue_request(tracedecay_store::SessionRefreshBeginOrJoinRequestV1::new(
        tracedecay_domain::SessionId::new("session.mapped-rekey").unwrap(),
        tracedecay_store::SessionRefreshFrontierV1::new(0, 0).unwrap(),
    ));
    assert_eq!(old_state.queued_request_count(), 1);

    let mut rekey = std::pin::pin!(registry.rekey_project(
        &old_owner,
        new_owner.clone(),
        harness.registered.clone(),
    ));
    assert!(
        rekey.as_mut().now_or_never().is_none(),
        "the rekey cannot finish while the mapped worker holds its history pass"
    );
    history.release.notify_one();
    rekey.await;

    assert!(registry.project_state(&old_owner).await.is_none());
    let new_state = registry
        .project_state(&new_owner)
        .await
        .expect("new owner entry");
    assert_eq!(
        new_state.queued_request_count(),
        1,
        "the new owner must inherit the mapped worker's queued request"
    );
    assert!(
        old_wake.wake(),
        "the old route must reach the new owner once the rekey completes"
    );
    assert!(new_state.take_dirty());

    registry.shutdown().await;
}

/// A retirement dropped while it waits on the project map lock must not
/// detach the retired worker: the next attempt still owns the join.
#[tokio::test]
async fn a_retirement_dropped_on_the_map_lock_keeps_the_join() {
    let harness = RegisteredGlobalDbHarness::open("refresh-map-lock-retirement").await;
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let owner = project_owner("map-lock-retirement");
    let history = mount_held_project(&registry, &harness, &owner, HeldHistory::default()).await;
    assert!(
        registry.retire_project(&owner).now_or_never().is_none(),
        "retirement cannot finish while the worker holds its history pass"
    );

    let hold = registry.project.lock().await;
    {
        let mut retire = std::pin::pin!(registry.retire_project(&owner));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut retire)
                .await
                .is_err(),
            "the retirement must wait on the held map lock"
        );
    }
    drop(hold);

    let mut retry = std::pin::pin!(registry.retire_project(&owner));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut retry)
            .await
            .is_err(),
        "a retirement dropped on the map lock must leave the retired worker joinable"
    );
    history.release.notify_one();
    retry.await;
    assert!(!history.worker_holds_pass());

    registry.shutdown().await;
}

/// A rekey dropped while it waits on the project map lock must not detach
/// the retired worker either: the retry still owns the join.
#[tokio::test]
async fn a_rekey_dropped_on_the_map_lock_keeps_the_join() {
    let harness = RegisteredGlobalDbHarness::open("refresh-map-lock-rekey").await;
    let registry = SessionTemporalRefreshSchedulerRegistry::default();
    let old_owner = project_owner("map-lock-rekey-old");
    let new_owner = project_owner("map-lock-rekey-new");
    let history = mount_held_project(&registry, &harness, &old_owner, HeldHistory::default()).await;
    assert!(
        registry
            .rekey_project(&old_owner, new_owner.clone(), harness.registered.clone())
            .now_or_never()
            .is_none(),
        "the rekey cannot finish while the retired worker holds its history pass"
    );

    let hold = registry.project.lock().await;
    {
        let mut rekey = std::pin::pin!(registry.rekey_project(
            &old_owner,
            new_owner.clone(),
            harness.registered.clone(),
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut rekey)
                .await
                .is_err(),
            "the rekey must wait on the held map lock"
        );
    }
    drop(hold);

    let mut retry = std::pin::pin!(registry.rekey_project(
        &old_owner,
        new_owner.clone(),
        harness.registered.clone(),
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut retry)
            .await
            .is_err(),
        "a rekey dropped on the map lock must leave the retired worker joinable"
    );
    history.release.notify_one();
    retry.await;

    assert!(registry.project_state(&new_owner).await.is_some());

    registry.shutdown().await;
}
