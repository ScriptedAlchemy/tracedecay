use std::future::Future;

use tracedecay_domain::errors::{Result, TraceDecayError};

/// Spawn HTTP so its sync router build cannot starve owner polls.
/// Box both arms so the caller's phase future stays one leaf wide.
pub(super) fn join_independent_full_owner_mounts<Owners, Http, OwnersMount, HttpMount>(
    owners_mount: OwnersMount,
    http_mount: HttpMount,
) -> impl Future<Output = Result<(Owners, Http)>>
where
    OwnersMount: Future<Output = Result<Owners>>,
    HttpMount: Future<Output = Result<Http>> + Send + 'static,
    Http: Send + 'static,
{
    let owners_mount = Box::pin(tracing::Instrument::instrument(
        owners_mount,
        tracing::trace_span!("daemon.project.open.production_owners"),
    ));
    let http_mount = Box::pin(tracing::Instrument::instrument(
        http_mount,
        tracing::trace_span!("daemon.project.open.http_application"),
    ));
    tracing::Instrument::instrument(
        async move {
            let mut http_tasks = tokio::task::JoinSet::new();
            http_tasks.spawn(http_mount);
            tokio::try_join!(owners_mount, async {
                http_tasks
                    .join_next()
                    .await
                    .ok_or_else(|| TraceDecayError::Config {
                        message: "http application mount task missing".to_owned(),
                    })?
                    .map_err(|error| TraceDecayError::Config {
                        message: format!("http application mount task failed: {error}"),
                    })?
            })
        },
        tracing::trace_span!("daemon.project.compose.join_full_mounts"),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{Barrier, oneshot};
    use tracedecay_domain::errors::TraceDecayError;

    use super::join_independent_full_owner_mounts;

    #[tokio::test]
    async fn independent_full_mounts_preserve_identity_in_either_completion_order() {
        for owners_first in [true, false] {
            let entered = Arc::new(Barrier::new(3));
            let (release_owners, owners_released) = oneshot::channel();
            let (release_http, http_released) = oneshot::channel();
            let owners_entered = Arc::clone(&entered);
            let http_entered = Arc::clone(&entered);
            let joined = tokio::spawn(join_independent_full_owner_mounts(
                async move {
                    owners_entered.wait().await;
                    owners_released.await.expect("release owner mount");
                    Ok("owners-ready")
                },
                async move {
                    http_entered.wait().await;
                    http_released.await.expect("release http mount");
                    Ok("http-ready")
                },
            ));

            tokio::time::timeout(Duration::from_secs(2), entered.wait())
                .await
                .expect("both independent mounts must start before either completes");
            if owners_first {
                release_owners.send(()).expect("finish owners first");
                tokio::task::yield_now().await;
                assert!(!joined.is_finished());
                release_http.send(()).expect("finish http second");
            } else {
                release_http.send(()).expect("finish http first");
                tokio::task::yield_now().await;
                assert!(!joined.is_finished());
                release_owners.send(()).expect("finish owners second");
            }
            let (owners, http) = joined
                .await
                .expect("join full-mount task")
                .expect("admit both full mounts");
            assert_eq!(owners, "owners-ready");
            assert_eq!(http, "http-ready");
        }
    }

    struct DropReceipt(Option<oneshot::Sender<()>>);

    impl Drop for DropReceipt {
        fn drop(&mut self) {
            if let Some(receipt) = self.0.take() {
                let _ = receipt.send(());
            }
        }
    }

    #[tokio::test]
    async fn failed_full_mount_cancels_its_peer_without_partial_admission() {
        let entered = Arc::new(Barrier::new(3));
        let (dropped, drop_receipt) = oneshot::channel();
        let owners_entered = Arc::clone(&entered);
        let http_entered = Arc::clone(&entered);
        let joined = tokio::spawn(join_independent_full_owner_mounts(
            async move {
                let _drop_receipt = DropReceipt(Some(dropped));
                owners_entered.wait().await;
                std::future::pending::<tracedecay_domain::errors::Result<&'static str>>().await
            },
            async move {
                http_entered.wait().await;
                Err::<&'static str, _>(TraceDecayError::Config {
                    message: "http application mount failed".to_owned(),
                })
            },
        ));

        tokio::time::timeout(Duration::from_secs(2), entered.wait())
            .await
            .expect("both independent mounts must start before either fails");
        let error = joined
            .await
            .expect("join failed full-mount task")
            .expect_err("one failed mount cannot expose a partial pair");
        assert!(matches!(
            error,
            TraceDecayError::Config { message }
                if message == "http application mount failed"
        ));
        tokio::time::timeout(Duration::from_secs(2), drop_receipt)
            .await
            .expect("peer mount must be cancelled within the lifecycle tripwire")
            .expect("peer cancellation receipt");
    }

    #[tokio::test]
    async fn failed_owner_mount_cancels_spawned_http_mount() {
        let (entered, http_entered) = oneshot::channel();
        let (dropped, drop_receipt) = oneshot::channel();
        let error = join_independent_full_owner_mounts(
            async move {
                http_entered.await.expect("HTTP mount started");
                Err::<(), _>(TraceDecayError::Config {
                    message: "owner mount failed".to_owned(),
                })
            },
            async move {
                let _drop_receipt = DropReceipt(Some(dropped));
                entered.send(()).expect("signal HTTP mount started");
                std::future::pending::<tracedecay_domain::errors::Result<()>>().await
            },
        )
        .await
        .expect_err("owner failure must fail the pair");
        assert!(
            matches!(error, TraceDecayError::Config { message } if message == "owner mount failed")
        );
        tokio::time::timeout(Duration::from_secs(2), drop_receipt)
            .await
            .expect("owner failure must cancel the spawned HTTP mount")
            .expect("HTTP cancellation receipt");
    }

    #[tokio::test]
    async fn cancelling_full_mount_cancels_spawned_http_mount() {
        let (entered, http_entered) = oneshot::channel();
        let (dropped, drop_receipt) = oneshot::channel();
        let joined = tokio::spawn(join_independent_full_owner_mounts(
            std::future::pending::<tracedecay_domain::errors::Result<()>>(),
            async move {
                let _drop_receipt = DropReceipt(Some(dropped));
                entered.send(()).expect("signal HTTP mount started");
                std::future::pending::<tracedecay_domain::errors::Result<()>>().await
            },
        ));
        tokio::time::timeout(Duration::from_secs(2), http_entered)
            .await
            .expect("HTTP mount must start")
            .expect("HTTP start receipt");
        joined.abort();
        assert!(
            joined
                .await
                .expect_err("full mount cancelled")
                .is_cancelled()
        );
        tokio::time::timeout(Duration::from_secs(2), drop_receipt)
            .await
            .expect("full mount cancellation must cancel the spawned HTTP mount")
            .expect("HTTP cancellation receipt");
    }
}
