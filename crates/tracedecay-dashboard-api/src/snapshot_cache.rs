use std::future::Future;
use std::sync::Arc;

use tracedecay_store::ProjectMemoryStoreRevisionV1;

use crate::graph_structure_api::CachedStrataV1;
use crate::memory_analysis::SimilarityComputation;
use crate::memory_service::{ProjectionCacheRevision, ProjectionComputation};

/// Single-revision cache for one generation-bound derived snapshot.
///
/// The slot retains one revision and provides single-flight computation. It
/// is owned by the `DashboardState` whose stores it derives from (see
/// [`DerivedSnapshotCaches`]), so it is released with that state instead of
/// outliving it in process-global memory. A failed or cancelled computation
/// leaves the slot exactly as it was: nothing is cached and the next reader
/// recomputes.
pub(crate) struct DerivedSnapshotCache<R, V> {
    state: tokio::sync::Mutex<Option<(R, Arc<V>)>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DerivedSnapshotCacheState {
    Hit,
    Miss,
}

impl DerivedSnapshotCacheState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
        }
    }
}

impl<R, V> DerivedSnapshotCache<R, V>
where
    R: PartialEq,
{
    pub(crate) fn new() -> Self {
        Self {
            state: tokio::sync::Mutex::new(None),
        }
    }

    pub(crate) async fn get_or_compute<E, F, Fut>(
        &self,
        revision: R,
        compute: F,
    ) -> Result<(Arc<V>, DerivedSnapshotCacheState), E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(R, Arc<V>), E>>,
    {
        let mut state = self.state.lock().await;
        if let Some((cached_revision, cached)) = state.as_ref()
            && cached_revision == &revision
        {
            return Ok((Arc::clone(cached), DerivedSnapshotCacheState::Hit));
        }

        let (observed_revision, computed) = compute().await?;
        *state = Some((observed_revision, Arc::clone(&computed)));
        Ok((computed, DerivedSnapshotCacheState::Miss))
    }
}

/// Every derived snapshot a dashboard state retains for its own stores: the
/// PCA projection and similarity pairs of its project-memory store and the
/// dependency strata of its code graph. One instance is created with each
/// `DashboardState` and shared by its clones, so retiring the state (daemon
/// shutdown, or a selected project being rebuilt after its registry context
/// changes) retires the derived data with it.
pub(crate) struct DerivedSnapshotCaches {
    pub(crate) projection: DerivedSnapshotCache<ProjectionCacheRevision, ProjectionComputation>,
    pub(crate) similarity:
        DerivedSnapshotCache<ProjectMemoryStoreRevisionV1, SimilarityComputation>,
    pub(crate) strata: DerivedSnapshotCache<String, CachedStrataV1>,
}

impl DerivedSnapshotCaches {
    pub(crate) fn new() -> Self {
        Self {
            projection: DerivedSnapshotCache::new(),
            similarity: DerivedSnapshotCache::new(),
            strata: DerivedSnapshotCache::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};

    use super::{DerivedSnapshotCache, DerivedSnapshotCacheState};

    fn poll_pinned_once<T>(future: std::pin::Pin<&mut impl Future<Output = T>>) -> Poll<T> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn warm_hit_skips_row_loader_while_another_store_is_pending() {
        let warm_store = DerivedSnapshotCache::<u64, usize>::new();
        let cold_store = DerivedSnapshotCache::<u64, usize>::new();
        let mut warm_fill =
            pin!(warm_store.get_or_compute(1, || async { Ok::<_, ()>((1, Arc::new(7))) }));
        assert!(matches!(
            poll_pinned_once(warm_fill.as_mut()),
            Poll::Ready(Ok((value, DerivedSnapshotCacheState::Miss))) if *value == 7
        ));
        let cold_polls = AtomicUsize::new(0);
        let vector_rows_read = AtomicUsize::new(0);

        let mut cold = pin!(cold_store.get_or_compute(1, || {
            std::future::poll_fn(|_| {
                cold_polls.fetch_add(1, Ordering::SeqCst);
                Poll::<Result<(u64, Arc<usize>), ()>>::Pending
            })
        }));
        assert!(poll_pinned_once(cold.as_mut()).is_pending());

        let mut warm = pin!(warm_store.get_or_compute(1, || async {
            vector_rows_read.fetch_add(2_000, Ordering::SeqCst);
            Ok::<_, ()>((1, Arc::new(99)))
        }));
        assert!(matches!(
            poll_pinned_once(warm.as_mut()),
            Poll::Ready(Ok((value, DerivedSnapshotCacheState::Hit))) if *value == 7
        ));

        assert!(poll_pinned_once(cold.as_mut()).is_pending());
        assert_eq!(cold_polls.load(Ordering::SeqCst), 2);
        assert_eq!(vector_rows_read.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn concurrent_cold_requests_for_one_store_compute_once() {
        let cache = DerivedSnapshotCache::<u64, usize>::new();
        let first_ready = AtomicBool::new(false);
        let first_computes = AtomicUsize::new(0);
        let second_computes = AtomicUsize::new(0);

        let first = cache.get_or_compute(1, || {
            first_computes.fetch_add(1, Ordering::SeqCst);
            std::future::poll_fn(|_| {
                if first_ready.load(Ordering::SeqCst) {
                    Poll::Ready(Ok::<_, ()>((1, Arc::new(7))))
                } else {
                    Poll::Pending
                }
            })
        });
        let second = cache.get_or_compute(1, || async {
            second_computes.fetch_add(1, Ordering::SeqCst);
            Ok::<_, ()>((1, Arc::new(99)))
        });
        let mut first = pin!(first);
        let mut second = pin!(second);

        assert!(poll_pinned_once(first.as_mut()).is_pending());
        assert!(poll_pinned_once(second.as_mut()).is_pending());
        first_ready.store(true, Ordering::SeqCst);
        assert!(matches!(
            poll_pinned_once(first.as_mut()),
            Poll::Ready(Ok((value, DerivedSnapshotCacheState::Miss))) if *value == 7
        ));
        assert!(matches!(
            poll_pinned_once(second.as_mut()),
            Poll::Ready(Ok((value, DerivedSnapshotCacheState::Hit))) if *value == 7
        ));
        assert_eq!(first_computes.load(Ordering::SeqCst), 1);
        assert_eq!(second_computes.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn failed_computation_is_not_cached_and_is_recomputed() {
        let cache = DerivedSnapshotCache::<u64, usize>::new();
        let computes = AtomicUsize::new(0);

        let failed = cache
            .get_or_compute(1, || async {
                computes.fetch_add(1, Ordering::SeqCst);
                Err::<(u64, Arc<usize>), &str>("row loader failed")
            })
            .await;
        assert_eq!(failed.err(), Some("row loader failed"));
        assert!(
            cache.state.lock().await.is_none(),
            "failure must leave no slot"
        );

        let (value, state) = cache
            .get_or_compute(1, || async {
                computes.fetch_add(1, Ordering::SeqCst);
                Ok::<_, &str>((1, Arc::new(7)))
            })
            .await
            .unwrap();
        assert_eq!(state, DerivedSnapshotCacheState::Miss);
        assert_eq!(*value, 7);
        assert_eq!(computes.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failed_recompute_keeps_the_previous_revision_untouched() {
        let cache = DerivedSnapshotCache::<u64, usize>::new();
        cache
            .get_or_compute(1, || async { Ok::<_, &str>((1, Arc::new(7))) })
            .await
            .unwrap();

        let failed = cache
            .get_or_compute(2, || async {
                Err::<(u64, Arc<usize>), &str>("store closed")
            })
            .await;
        assert_eq!(failed.err(), Some("store closed"));

        // The stale revision is still served for its own revision and the
        // new revision is recomputed, never reported as a success.
        let (stale, stale_state) = cache
            .get_or_compute(1, || async { Ok::<_, &str>((1, Arc::new(99))) })
            .await
            .unwrap();
        assert_eq!((*stale, stale_state), (7, DerivedSnapshotCacheState::Hit));
        let (fresh, fresh_state) = cache
            .get_or_compute(2, || async { Ok::<_, &str>((2, Arc::new(8))) })
            .await
            .unwrap();
        assert_eq!((*fresh, fresh_state), (8, DerivedSnapshotCacheState::Miss));
    }
}
