use std::sync::Arc;
use std::time::Duration;

use tracedecay_private_fs::framed_log::sync_latency;

use super::*;

const CONCURRENT_ADMISSIONS: usize = 8;

fn open_broker(dir: &std::path::Path) -> Arc<HostAdmissionBroker> {
    let (runtime, _) = HostAdmissionRuntime::open(dir, SpoolBounds::default()).unwrap();
    Arc::new(HostAdmissionBroker::new(runtime))
}

// The held runtime guard is the in-flight batch the admissions queue behind.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admissions_queued_behind_a_batch_share_the_next_durable_batch() {
    let spool = tempfile::tempdir().unwrap();
    let broker = open_broker(spool.path());
    broker.admit("warm", b"first").await.unwrap();

    // Hold the spool the way an in-flight batch does, so every admission
    // below queues before the next runtime turn.
    let in_flight = broker.runtime.lock().unwrap();
    let admissions = (0..CONCURRENT_ADMISSIONS)
        .map(|index| {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move {
                broker
                    .admit(
                        &format!("source-{index}"),
                        format!("event-{index}").as_bytes(),
                    )
                    .await
            })
        })
        .collect::<Vec<_>>();
    while broker.appends.lock().unwrap().len() < CONCURRENT_ADMISSIONS {
        tokio::task::yield_now().await;
    }
    let barriers = sync_latency::inject(spool.path(), Duration::ZERO);
    drop(in_flight);

    let mut seqs = Vec::new();
    for admission in admissions {
        seqs.push(admission.await.unwrap().unwrap().seq);
    }
    seqs.sort_unstable();
    assert_eq!(seqs, (2..=9).collect::<Vec<u64>>());
    // One intent publish (file, plus directory on Unix) and one frame sync
    // for all eight; one-at-a-time appends paid three metadata publishes each.
    assert_eq!(barriers.syncs(), 2 + u64::from(cfg!(unix)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_acknowledged_concurrent_admission_survives_a_crash() {
    let spool = tempfile::tempdir().unwrap();
    let broker = open_broker(spool.path());
    let _slow_disk = sync_latency::inject(spool.path(), Duration::from_millis(5));
    let admissions = (0..CONCURRENT_ADMISSIONS)
        .map(|index| {
            let broker = Arc::clone(&broker);
            tokio::spawn(async move {
                let payload = format!("event-{index}");
                let admitted = broker
                    .admit(&format!("source-{index}"), payload.as_bytes())
                    .await;
                (admitted.map(|admitted| admitted.seq), payload)
            })
        })
        .collect::<Vec<_>>();
    let mut acknowledged = Vec::new();
    for admission in admissions {
        let (seq, payload) = admission.await.unwrap();
        acknowledged.push((seq.unwrap(), payload.into_bytes()));
    }
    acknowledged.sort_unstable();
    // The daemon dies with nothing published after the acknowledgements.
    drop(broker);

    let (mut runtime, report) =
        HostAdmissionRuntime::open(spool.path(), SpoolBounds::default()).unwrap();
    assert_eq!(report.next_seq, 9);
    let mut recovered = Vec::new();
    while let Some(record) = runtime.try_lease_next().unwrap() {
        recovered.push((record.seq, record.payload));
    }
    recovered.sort_unstable();
    assert_eq!(recovered, acknowledged);
}

async fn commit_leased(broker: &HostAdmissionBroker) -> u64 {
    let replay = broker.begin_replay().await.unwrap();
    let leased = replay.lease_next().await.unwrap().unwrap();
    assert_eq!(replay.commit(leased.seq).await.unwrap(), 1);
    leased.seq
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_commit_waits_on_no_barrier_and_the_next_batch_publishes_its_watermark() {
    let spool = tempfile::tempdir().unwrap();
    let broker = open_broker(spool.path());
    let first = broker.admit("source", b"first").await.unwrap();
    let barriers = sync_latency::inject(spool.path(), Duration::ZERO);
    assert_eq!(commit_leased(&broker).await, first.seq);
    assert_eq!(barriers.syncs(), 0);
    drop(barriers);
    // The daemon dies before any later publish: the commit replays once more.
    drop(broker);

    // Reopening publishes once to clear the first batch's reconciled append
    // intent; count only the barriers after it.
    let broker = open_broker(spool.path());
    assert_eq!(broker.pending_replay_count().await.unwrap(), 1);
    let barriers = sync_latency::inject(spool.path(), Duration::ZERO);
    assert_eq!(commit_leased(&broker).await, first.seq);
    assert_eq!(barriers.syncs(), 0);
    let second = broker.admit("source", b"second").await.unwrap();
    // The second batch's intent publish and frame sync carry the watermark.
    assert_eq!(barriers.syncs(), 2 + u64::from(cfg!(unix)));
    drop(broker);

    let (mut runtime, report) =
        HostAdmissionRuntime::open(spool.path(), SpoolBounds::default()).unwrap();
    assert_eq!(report.committed_through, first.seq);
    let leased = runtime.try_lease_next().unwrap().unwrap();
    assert_eq!(
        (leased.seq, leased.payload),
        (second.seq, b"second".to_vec())
    );
    assert_eq!(runtime.try_lease_next().unwrap(), None);
}
