use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use tokio::sync::{Semaphore, oneshot};

use super::*;
use crate::observation::ObservationCancellation;

fn control(cancellation: ObservationCancellation, timeout: Duration) -> BoundedGitControl {
    BoundedGitControl::new(cancellation, timeout)
}

#[tokio::test]
async fn cancellation_while_queued_never_starts_the_closure() {
    let permits = Arc::new(Semaphore::new(1));
    let held = Arc::clone(&permits).acquire_owned().await.unwrap();
    let cancellation = ObservationCancellation::default();
    let queued_control = control(cancellation.clone(), Duration::from_secs(10));
    let queued_permits = Arc::clone(&permits);
    let started = Arc::new(AtomicBool::new(false));
    let task_started = Arc::clone(&started);
    let queued = tokio::spawn(async move {
        run_with_semaphore(queued_permits, &queued_control, move || {
            task_started.store(true, Ordering::SeqCst);
            Ok::<_, BoundedBackfillInterruption>(())
        })
        .await
    });
    tokio::task::yield_now().await;
    cancellation.cancel();

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), queued)
            .await
            .unwrap()
            .unwrap(),
        Err(BoundedBackfillInterruption::Cancelled)
    );
    assert!(!started.load(Ordering::SeqCst));
    drop(held);
    assert_eq!(permits.available_permits(), 1);
}

#[tokio::test]
async fn completed_join_cannot_win_over_cancellation() {
    let permits = Arc::new(Semaphore::new(1));
    let cancellation = ObservationCancellation::default();
    let task_cancellation = cancellation.clone();
    let run_control = control(cancellation, Duration::from_secs(10));

    assert_eq!(
        run_with_semaphore(permits, &run_control, move || {
            task_cancellation.cancel();
            Ok::<_, BoundedBackfillInterruption>(())
        })
        .await,
        Err(BoundedBackfillInterruption::Cancelled)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detached_task_holds_capacity_until_its_closure_finishes() {
    let permits = Arc::new(Semaphore::new(1));
    let cancellation = ObservationCancellation::default();
    let (first_started_tx, first_started_rx) = oneshot::channel();
    let (release_first_tx, release_first_rx) = mpsc::channel();
    let first_control = control(cancellation.clone(), Duration::from_secs(10));
    let first_permits = Arc::clone(&permits);
    let first = tokio::spawn(async move {
        run_with_semaphore(first_permits, &first_control, move || {
            first_started_tx.send(()).unwrap();
            release_first_rx.recv().unwrap();
            Ok::<_, BoundedBackfillInterruption>(1)
        })
        .await
    });
    first_started_rx.await.unwrap();

    cancellation.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .unwrap()
            .unwrap(),
        Err(BoundedBackfillInterruption::Cancelled)
    );

    let (second_started_tx, mut second_started_rx) = oneshot::channel();
    let second_control = control(ObservationCancellation::default(), Duration::from_secs(10));
    let second_permits = Arc::clone(&permits);
    let second = tokio::spawn(async move {
        run_with_semaphore(second_permits, &second_control, move || {
            second_started_tx.send(()).unwrap();
            Ok::<_, BoundedBackfillInterruption>(2)
        })
        .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second_started_rx)
            .await
            .is_err()
    );

    release_first_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), &mut second_started_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), second)
            .await
            .unwrap()
            .unwrap(),
        Ok(2)
    );
    assert_eq!(permits.available_permits(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deadline_detaches_without_leaking_the_permit() {
    let permits = Arc::new(Semaphore::new(1));
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let deadline_control = control(
        ObservationCancellation::default(),
        Duration::from_millis(25),
    );
    let run_permits = Arc::clone(&permits);
    let run = tokio::spawn(async move {
        run_with_semaphore(run_permits, &deadline_control, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok::<_, BoundedBackfillInterruption>(())
        })
        .await
    });
    started_rx.await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), run)
            .await
            .unwrap()
            .unwrap(),
        Err(BoundedBackfillInterruption::CommandTimedOut)
    );
    assert_eq!(permits.available_permits(), 0);

    release_tx.send(()).unwrap();
    let permit = tokio::time::timeout(Duration::from_secs(1), Arc::clone(&permits).acquire_owned())
        .await
        .unwrap()
        .unwrap();
    drop(permit);
    assert_eq!(permits.available_permits(), 1);
}
