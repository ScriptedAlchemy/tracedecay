#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::time::Instant;

    use super::{ShutdownOwner, join_shutdown_owners};

    #[tokio::test(start_paused = true)]
    async fn cancellation_reaches_every_owner_before_any_join_is_polled() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let first_join_polled = Arc::new(AtomicBool::new(false));
        let mut owners = Vec::new();
        for name in ["first", "second", "third"] {
            let cancel_count = Arc::clone(&cancelled);
            let observed_cancel_count = Arc::clone(&cancelled);
            let join_polled = Arc::clone(&first_join_polled);
            owners.push(ShutdownOwner::new(
                name,
                move || {
                    cancel_count.fetch_add(1, Ordering::AcqRel);
                },
                async move {
                    join_polled.store(true, Ordering::Release);
                    assert_eq!(
                        observed_cancel_count.load(Ordering::Acquire),
                        3,
                        "every owner must receive cancellation before the first join is polled"
                    );
                },
            ));
        }

        let receipt =
            join_shutdown_owners(Instant::now() + Duration::from_secs(1), owners).await;

        assert!(first_join_polled.load(Ordering::Acquire));
        assert!(receipt.owners.iter().all(|owner| owner.finished));
    }

    #[tokio::test(start_paused = true)]
    async fn every_owner_uses_the_same_deadline_and_unfinished_receipts_are_exact() {
        let owner_deadlines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut owners = Vec::new();
        for (name, finishes) in [("quick", true), ("blocked-a", false), ("blocked-b", false)] {
            let observed = Arc::clone(&owner_deadlines);
            owners.push(ShutdownOwner::with_deadline(
                name,
                || {},
                move |owner_deadline| async move {
                    observed
                        .lock()
                        .expect("deadline observations")
                        .push(owner_deadline);
                    if finishes {
                        return;
                    }
                    std::future::pending::<()>().await;
                },
            ));
        }

        let shutdown = join_shutdown_owners(deadline, owners);
        tokio::pin!(shutdown);
        tokio::time::advance(Duration::from_secs(2)).await;
        let receipt = shutdown.await;

        assert_eq!(
            owner_deadlines
                .lock()
                .expect("deadline observations")
                .as_slice(),
            &[deadline, deadline, deadline]
        );
        assert_eq!(receipt.unfinished(), &["blocked-a", "blocked-b"]);
        assert_eq!(receipt.deadline, deadline);
    }
}
