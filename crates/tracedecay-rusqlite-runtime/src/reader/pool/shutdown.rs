//! Exclusive ownership is the shutdown boundary, not an observed Arc count.

use std::sync::Arc;

use super::{PoolInner, ReaderPool, ReaderQueryExecutor};

/// Owns the reader workers after every upgradeable pool reference is fenced.
/// The caller drops this outside its attachment lock so worker joins cannot
/// deadlock an attachment callback.
pub(crate) struct ReaderPoolShutdown<E: ReaderQueryExecutor> {
    inner: PoolInner<E>,
}

impl<E: ReaderQueryExecutor> ReaderPoolShutdown<E> {
    pub(crate) fn join(self) {
        drop(self.inner);
    }
}

impl<E: ReaderQueryExecutor> ReaderPool<E> {
    /// Atomically retire the last pool reference before reporting shutdown.
    ///
    /// `is_quiescent` is only a hint: an exact-SQL handle can upgrade its weak
    /// reference after the count check. A plain drop would then leave SQLite
    /// connections alive past publication or release of the file identity pin.
    /// A failed claim returns the same pool, retaining all workers and pins.
    pub(crate) fn try_into_shutdown(self) -> Result<ReaderPoolShutdown<E>, Self> {
        #[cfg(test)]
        if let Some(hook) = self
            .inner
            .before_shutdown_claim
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            hook();
        }
        match Arc::try_unwrap(self.inner) {
            Ok(inner) => Ok(ReaderPoolShutdown { inner }),
            Err(inner) => Err(Self { inner }),
        }
    }

    #[cfg(test)]
    pub(crate) fn run_once_before_shutdown_claim(&self, hook: impl FnOnce() + Send + 'static) {
        *self
            .inner
            .before_shutdown_claim
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(hook));
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_store::AdmissionConfigV1;

    use crate::reader::ReaderPool;
    use crate::reader::tests::{CountExecutor, TestStore};

    #[test]
    fn a_late_weak_upgrade_cannot_outlive_an_acknowledged_reader_shutdown() {
        let store = TestStore::new();
        let pool = ReaderPool::start(
            store.locator(),
            AdmissionConfigV1::default().readers,
            CountExecutor,
        )
        .unwrap();
        let weak = pool.downgrade();
        pool.begin_shutdown_drain();
        assert!(pool.is_quiescent());

        // This is the interleaving between the attachment's advisory count
        // check and retirement; no sleep or scheduler race is needed.
        let late = weak.upgrade().unwrap();
        let pool = match pool.try_into_shutdown() {
            Err(pool) => pool,
            Ok(_) => panic!("a live late reference must prevent shutdown acknowledgment"),
        };
        assert!(!pool.workers_exited());
        drop(late);

        let shutdown = match pool.try_into_shutdown() {
            Ok(shutdown) => shutdown,
            Err(_) => panic!("the last pool reference must be claimable"),
        };
        assert!(weak.upgrade().is_none());
        shutdown.join();
    }
}
