use std::future::Future;

use tracedecay_domain::errors::{Result, TraceDecayError};

/// Spawn HTTP so its sync router build cannot starve owner polls.
#[tracing::instrument(
    name = "daemon.project.compose.join_full_mounts",
    level = "trace",
    skip_all
)]
pub(super) async fn join_independent_full_owner_mounts<Owners, Http, OwnersMount, HttpMount>(
    owners_mount: OwnersMount,
    http_mount: HttpMount,
) -> Result<(Owners, Http)>
where
    OwnersMount: Future<Output = Result<Owners>>,
    HttpMount: Future<Output = Result<Http>> + Send + 'static,
    Http: Send + 'static,
{
    let http_task = tokio::spawn(tracing::Instrument::instrument(
        http_mount,
        tracing::trace_span!("daemon.project.open.http_application"),
    ));
    tokio::try_join!(
        tracing::Instrument::instrument(
            owners_mount,
            tracing::trace_span!("daemon.project.open.production_owners")
        ),
        async {
            http_task.await.map_err(|error| TraceDecayError::Config {
                message: format!("http application mount task failed: {error}"),
            })?
        }
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
}
