use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;

/// Process-local cache for generation-bound derived snapshots.
///
/// The map lock protects only scope-slot lookup. Each scope has its own state
/// lock, which retains one revision and provides single-flight computation
/// without serializing unrelated stores.
pub(crate) struct DerivedSnapshotCache<S, R, V> {
    scopes: tokio::sync::Mutex<HashMap<S, Arc<Slot<R, V>>>>,
}

struct Slot<R, V> {
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

impl<S, R, V> DerivedSnapshotCache<S, R, V>
where
    S: Eq + Hash,
    R: PartialEq,
{
    pub(crate) fn new() -> Self {
        Self {
            scopes: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn get_or_compute<E, F, Fut>(
        &self,
        scope: S,
        revision: R,
        compute: F,
    ) -> Result<(Arc<V>, DerivedSnapshotCacheState), E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(R, Arc<V>), E>>,
    {
        let slot = {
            let mut scopes = self.scopes.lock().await;
            Arc::clone(scopes.entry(scope).or_insert_with(|| {
                Arc::new(Slot {
                    state: tokio::sync::Mutex::new(None),
                })
            }))
        };

        let mut state = slot.state.lock().await;
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

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};

    use super::{DerivedSnapshotCache, DerivedSnapshotCacheState};

    fn poll_once<T>(future: impl Future<Output = T>) -> Poll<T> {
        let mut future = pin!(future);
        poll_pinned_once(future.as_mut())
    }

    fn poll_pinned_once<T>(future: std::pin::Pin<&mut impl Future<Output = T>>) -> Poll<T> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn different_cold_scopes_poll_their_computations_concurrently() {
        let cache = DerivedSnapshotCache::<&'static str, u64, usize>::new();
        let first_polls = AtomicUsize::new(0);
        let second_polls = AtomicUsize::new(0);

        let first = cache.get_or_compute("project-a", 1, || {
            std::future::poll_fn(|_| {
                first_polls.fetch_add(1, Ordering::SeqCst);
                Poll::<Result<(u64, Arc<usize>), ()>>::Pending
            })
        });
        let second = cache.get_or_compute("project-b", 1, || async {
            second_polls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, ()>((1, Arc::new(2)))
        });

        assert!(poll_once(first).is_pending());
        assert!(matches!(poll_once(second), Poll::Ready(Ok((value, _))) if *value == 2));
        assert_eq!(first_polls.load(Ordering::SeqCst), 1);
        assert_eq!(second_polls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn warm_hit_skips_row_loader_while_unrelated_cold_key_is_pending() {
        let cache = DerivedSnapshotCache::<&'static str, u64, usize>::new();
        assert!(matches!(
            poll_once(cache.get_or_compute("project-warm", 1, || async {
                Ok::<_, ()>((1, Arc::new(7)))
            })),
            Poll::Ready(Ok((value, DerivedSnapshotCacheState::Miss))) if *value == 7
        ));
        let cold_polls = AtomicUsize::new(0);
        let vector_rows_read = AtomicUsize::new(0);

        let cold = cache.get_or_compute("project-cold", 1, || {
            std::future::poll_fn(|_| {
                cold_polls.fetch_add(1, Ordering::SeqCst);
                Poll::<Result<(u64, Arc<usize>), ()>>::Pending
            })
        });
        assert!(poll_once(cold).is_pending());

        let warm = cache.get_or_compute("project-warm", 1, || async {
            vector_rows_read.fetch_add(2_000, Ordering::SeqCst);
            Ok::<_, ()>((1, Arc::new(99)))
        });
        assert!(matches!(
            poll_once(warm),
            Poll::Ready(Ok((value, DerivedSnapshotCacheState::Hit))) if *value == 7
        ));
        assert_eq!(cold_polls.load(Ordering::SeqCst), 1);
        assert_eq!(vector_rows_read.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn concurrent_cold_requests_for_one_scope_compute_once() {
        let cache = DerivedSnapshotCache::<&'static str, u64, usize>::new();
        let first_ready = AtomicBool::new(false);
        let first_computes = AtomicUsize::new(0);
        let second_computes = AtomicUsize::new(0);

        let first = cache.get_or_compute("project", 1, || {
            first_computes.fetch_add(1, Ordering::SeqCst);
            std::future::poll_fn(|_| {
                if first_ready.load(Ordering::SeqCst) {
                    Poll::Ready(Ok::<_, ()>((1, Arc::new(7))))
                } else {
                    Poll::Pending
                }
            })
        });
        let second = cache.get_or_compute("project", 1, || async {
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
    async fn revision_change_replaces_the_scope_value() {
        let cache = DerivedSnapshotCache::<&'static str, u64, usize>::new();

        let (_, first_state) = cache
            .get_or_compute("project", 1, || async { Ok::<_, ()>((1, Arc::new(7))) })
            .await
            .unwrap();
        let (second, second_state) = cache
            .get_or_compute("project", 2, || async { Ok::<_, ()>((2, Arc::new(8))) })
            .await
            .unwrap();
        let (hit, hit_state) = cache
            .get_or_compute("project", 2, || async { Ok::<_, ()>((2, Arc::new(99))) })
            .await
            .unwrap();

        assert_eq!(first_state, DerivedSnapshotCacheState::Miss);
        assert_eq!(second_state, DerivedSnapshotCacheState::Miss);
        assert_eq!(hit_state, DerivedSnapshotCacheState::Hit);
        assert_eq!(*second, 8);
        assert_eq!(*hit, 8);
        assert_eq!(cache.scopes.lock().await.len(), 1);
    }
}
