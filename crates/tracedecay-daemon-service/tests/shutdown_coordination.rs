use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_daemon_service::ShutdownCoordinatorV1;
use tracedecay_store_runtime::ShutdownStatus;

#[tokio::test]
async fn timed_out_waiter_does_not_cancel_the_owned_shutdown() {
    let coordinator = Arc::new(ShutdownCoordinatorV1::default());
    let attempts = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());

    let first_coordinator = Arc::clone(&coordinator);
    let first_attempts = Arc::clone(&attempts);
    let first_entered = Arc::clone(&entered);
    let first_release = Arc::clone(&release);
    let first = tokio::spawn(async move {
        first_coordinator
            .coordinate_until(
                tokio::time::Instant::now() + std::time::Duration::from_millis(20),
                async move {
                    first_attempts.fetch_add(1, Ordering::AcqRel);
                    first_entered.notify_one();
                    first_release.notified().await;
                    ShutdownStatus::Clean
                },
            )
            .await
    });

    entered.notified().await;
    assert_eq!(first.await.expect("first waiter"), ShutdownStatus::TimedOut);
    assert_eq!(attempts.load(Ordering::Acquire), 1);

    let second_coordinator = Arc::clone(&coordinator);
    let second_attempts = Arc::clone(&attempts);
    let second = tokio::spawn(async move {
        second_coordinator
            .coordinate_until(
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                async move {
                    second_attempts.fetch_add(1, Ordering::AcqRel);
                    ShutdownStatus::Failed("duplicate coordinator".to_owned())
                },
            )
            .await
    });

    release.notify_waiters();
    assert_eq!(second.await.expect("second waiter"), ShutdownStatus::Clean);
    assert_eq!(
        attempts.load(Ordering::Acquire),
        1,
        "a retry must join the retained coordinator instead of starting again"
    );
    assert_eq!(
        coordinator
            .coordinate_until(
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
                async { ShutdownStatus::Failed("terminal rerun".to_owned()) },
            )
            .await,
        ShutdownStatus::Clean,
        "the terminal receipt is stable"
    );
}

#[tokio::test]
async fn cancelled_waiter_does_not_cancel_the_owned_shutdown() {
    let coordinator = Arc::new(ShutdownCoordinatorV1::default());
    let attempts = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());

    let first_coordinator = Arc::clone(&coordinator);
    let first_attempts = Arc::clone(&attempts);
    let first_entered = Arc::clone(&entered);
    let first_release = Arc::clone(&release);
    let first = tokio::spawn(async move {
        first_coordinator
            .coordinate_until(
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                async move {
                    first_attempts.fetch_add(1, Ordering::AcqRel);
                    first_entered.notify_one();
                    first_release.notified().await;
                    ShutdownStatus::Clean
                },
            )
            .await
    });

    entered.notified().await;
    first.abort();
    assert!(first.await.expect_err("cancel first waiter").is_cancelled());

    let retry_coordinator = Arc::clone(&coordinator);
    let retry_attempts = Arc::clone(&attempts);
    let retry = tokio::spawn(async move {
        retry_coordinator
            .coordinate_until(
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                async move {
                    retry_attempts.fetch_add(1, Ordering::AcqRel);
                    ShutdownStatus::Failed("duplicate coordinator".to_owned())
                },
            )
            .await
    });
    release.notify_waiters();

    assert_eq!(retry.await.expect("retry waiter"), ShutdownStatus::Clean);
    assert_eq!(attempts.load(Ordering::Acquire), 1);
}
