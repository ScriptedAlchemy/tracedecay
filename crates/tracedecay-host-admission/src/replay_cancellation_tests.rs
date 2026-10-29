use super::{HostAdmissionBroker, HostAdmissionRuntime, SpoolBounds};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
async fn matrix_reordered_completion_waits_for_contiguous_frontier() {
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    broker.admit("first", b"durable-event").await.unwrap();
    broker.admit("second", b"durable-event").await.unwrap();

    let replay = broker.begin_replay().await.unwrap();
    let first = replay.lease_next().await.unwrap().unwrap();
    let second = replay.lease_next().await.unwrap().unwrap();
    assert_ne!(first.seq, second.seq);

    assert_eq!(replay.commit(second.seq).await.unwrap(), 0);
    assert_eq!(broker.pending_count().await, 2);
    assert_eq!(replay.commit(first.seq).await.unwrap(), 2);
    assert_eq!(broker.pending_count().await, 0);
    drop(replay);
}

#[tokio::test]
async fn matrix_timeout_cancels_in_flight_replay_and_preserves_durable_frontier() {
    let spool = TempDir::new().unwrap();
    let broker = default_spool_broker(spool.path());
    let admitted = broker.admit("cancel-test", b"durable-event").await.unwrap();
    let lease_observed = Arc::new(AtomicBool::new(false));
    let replay_broker = Arc::clone(&broker);
    let replay_lease_observed = Arc::clone(&lease_observed);
    let cancelled = tokio::time::timeout(Duration::from_millis(100), async move {
        let replay = replay_broker.begin_replay().await.unwrap();
        let leased = replay
            .lease_next()
            .await
            .expect("lease operation must succeed")
            .expect("admitted frame must be leased");
        assert_eq!(leased.seq, admitted.seq);
        replay_lease_observed.store(true, Ordering::SeqCst);
        std::future::pending::<()>().await;
        drop(replay);
    })
    .await;

    assert!(cancelled.is_err(), "in-flight replay must time out");
    assert!(
        lease_observed.load(Ordering::SeqCst),
        "timeout must occur after replay lease"
    );
    assert_eq!(broker.pending_count().await, 1);

    // Dropping the timed-out future also drops the replay guard. The next
    // replay recovers the abandoned lease and can lease the same durable frame.
    let replay = broker.begin_replay().await.unwrap();
    let recovered = replay
        .lease_next()
        .await
        .expect("lease operation must succeed")
        .expect("cancelled lease must remain replayable");
    assert_eq!(broker.pending_count().await, 1);
    replay.defer(recovered.seq).await.unwrap();
    drop(replay);

    drop(broker);

    // Persistence, not only in-memory queue state: reopen retains the frontier.
    let reopened = default_spool_broker(spool.path());
    assert_eq!(reopened.pending_count().await, 1);
}

fn default_spool_broker(spool: &std::path::Path) -> Arc<HostAdmissionBroker> {
    let runtime = HostAdmissionRuntime::open(spool, SpoolBounds::default())
        .unwrap()
        .0;
    Arc::new(HostAdmissionBroker::new(runtime))
}
