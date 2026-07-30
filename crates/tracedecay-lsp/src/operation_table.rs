use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};

use crate::gateway::{LspRuntimeFuture, LspRuntimeSpawner, LspRuntimeTask};

#[derive(Clone)]
pub(crate) struct BoundedOperationCapacity {
    available: Arc<AtomicUsize>,
}

impl BoundedOperationCapacity {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            available: Arc::new(AtomicUsize::new(limit)),
        }
    }

    pub(crate) fn acquire(&self) -> Option<OperationCapacityPermit> {
        self.available
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |available| {
                available.checked_sub(1)
            })
            .ok()
            .map(|_| OperationCapacityPermit {
                available: Arc::clone(&self.available),
            })
    }
}

pub(crate) struct OperationCapacityPermit {
    available: Arc<AtomicUsize>,
}

impl Drop for OperationCapacityPermit {
    fn drop(&mut self) {
        self.available.fetch_add(1, Ordering::Release);
    }
}

struct PendingOperation<M, T> {
    metadata: M,
    receiver: Receiver<T>,
    task: Box<dyn LspRuntimeTask>,
    _permit: OperationCapacityPermit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OperationAdmission<M> {
    Started(M),
    Existing(M),
    Busy,
    Saturated,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OperationPoll<M, T> {
    Ready { metadata: M, result: T },
    Pending(M),
    Mismatch(M),
    Dropped(M),
    Missing,
    Busy,
}

pub(crate) struct BoundedOperationTable<K, M, T> {
    capacity: BoundedOperationCapacity,
    in_flight: Mutex<BTreeMap<K, PendingOperation<M, T>>>,
}

impl<K, M, T> BoundedOperationTable<K, M, T>
where
    K: Ord,
    M: Clone,
    T: Send + 'static,
{
    pub(crate) fn new(limit: usize) -> Self {
        Self::with_capacity(BoundedOperationCapacity::new(limit))
    }

    pub(crate) fn with_capacity(capacity: BoundedOperationCapacity) -> Self {
        Self {
            capacity,
            in_flight: Mutex::new(BTreeMap::new()),
        }
    }

    pub(crate) fn admit(
        &self,
        key: K,
        metadata: M,
        runtime: &dyn LspRuntimeSpawner,
        operation: impl FnOnce() -> LspRuntimeFuture<T> + Send + 'static,
    ) -> OperationAdmission<M> {
        match self.admit_with(key, runtime, || {
            Ok::<_, Infallible>((metadata, operation()))
        }) {
            Ok(admission) => admission,
            Err(error) => match error {},
        }
    }

    pub(crate) fn admit_with<E>(
        &self,
        key: K,
        runtime: &dyn LspRuntimeSpawner,
        prepare: impl FnOnce() -> Result<(M, LspRuntimeFuture<T>), E>,
    ) -> Result<OperationAdmission<M>, E> {
        let Ok(mut in_flight) = self.in_flight.try_lock() else {
            return Ok(OperationAdmission::Busy);
        };
        if let Some(pending) = in_flight.get(&key) {
            return Ok(OperationAdmission::Existing(pending.metadata.clone()));
        }
        let Some(permit) = self.capacity.acquire() else {
            return Ok(OperationAdmission::Saturated);
        };
        let (metadata, operation) = prepare()?;
        let (sender, receiver) = sync_channel(1);
        let task = runtime.spawn(Box::pin(async move {
            let _ = sender.send(operation.await);
        }));
        in_flight.insert(
            key,
            PendingOperation {
                metadata: metadata.clone(),
                receiver,
                task,
                _permit: permit,
            },
        );
        Ok(OperationAdmission::Started(metadata))
    }

    pub(crate) fn poll_matching(
        &self,
        key: &K,
        matches: impl FnOnce(&M) -> bool,
    ) -> OperationPoll<M, T> {
        let Ok(mut in_flight) = self.in_flight.try_lock() else {
            return OperationPoll::Busy;
        };
        let Some(pending) = in_flight.get_mut(key) else {
            return OperationPoll::Missing;
        };
        let metadata = pending.metadata.clone();
        if !matches(&metadata) {
            return OperationPoll::Mismatch(metadata);
        }
        match pending.receiver.try_recv() {
            Ok(result) => {
                in_flight.remove(key);
                OperationPoll::Ready { metadata, result }
            }
            Err(TryRecvError::Empty) => OperationPoll::Pending(metadata),
            Err(TryRecvError::Disconnected) => {
                in_flight.remove(key);
                OperationPoll::Dropped(metadata)
            }
        }
    }

    pub(crate) fn poll(&self, key: &K) -> OperationPoll<M, T> {
        self.poll_matching(key, |_| true)
    }

    pub(crate) fn cancel(&self, key: &K) -> bool {
        let pending = self
            .in_flight
            .lock()
            .ok()
            .and_then(|mut in_flight| in_flight.remove(key));
        if let Some(pending) = pending {
            pending.task.abort();
            true
        } else {
            false
        }
    }
}

impl<K, M, T> Drop for BoundedOperationTable<K, M, T> {
    fn drop(&mut self) {
        if let Ok(in_flight) = self.in_flight.get_mut() {
            for pending in in_flight.values() {
                pending.task.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::RecvTimeoutError;
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;
    use std::time::Duration;

    use super::*;

    struct InlineTask {
        aborted: Arc<AtomicBool>,
    }

    impl LspRuntimeTask for InlineTask {
        fn abort(&self) {
            self.aborted.store(true, Ordering::Release);
        }
    }

    struct InlineWake;

    impl Wake for InlineWake {
        fn wake(self: Arc<Self>) {}
    }

    #[derive(Default)]
    struct InlineSpawner {
        last_aborted: Mutex<Option<Arc<AtomicBool>>>,
    }

    impl LspRuntimeSpawner for InlineSpawner {
        fn spawn(&self, mut future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
            let waker = Waker::from(Arc::new(InlineWake));
            let mut context = Context::from_waker(&waker);
            assert_eq!(Pin::new(&mut future).poll(&mut context), Poll::Ready(()));
            let aborted = Arc::new(AtomicBool::new(false));
            *self.last_aborted.lock().unwrap() = Some(Arc::clone(&aborted));
            Box::new(InlineTask { aborted })
        }
    }

    #[test]
    fn delivers_once_and_reclaims_capacity() {
        let runtime = InlineSpawner::default();
        let table = BoundedOperationTable::new(1);

        assert_eq!(
            table.admit("first", "meta", &runtime, || Box::pin(async { 7 })),
            OperationAdmission::Started("meta")
        );
        assert_eq!(
            table.admit("second", "other", &runtime, || Box::pin(async { 8 })),
            OperationAdmission::Saturated
        );
        assert_eq!(
            table.poll(&"first"),
            OperationPoll::Ready {
                metadata: "meta",
                result: 7
            }
        );
        assert_eq!(table.poll(&"first"), OperationPoll::Missing);
        assert_eq!(
            table.admit("second", "other", &runtime, || Box::pin(async { 8 })),
            OperationAdmission::Started("other")
        );
    }

    #[test]
    fn preserves_identity_on_duplicate_and_mismatch() {
        let runtime = InlineSpawner::default();
        let table = BoundedOperationTable::new(1);

        assert_eq!(
            table.admit(1, "definition", &runtime, || Box::pin(async { 11 })),
            OperationAdmission::Started("definition")
        );
        assert_eq!(
            table.admit(1, "hover", &runtime, || Box::pin(async { 12 })),
            OperationAdmission::Existing("definition")
        );
        assert_eq!(
            table.poll_matching(&1, |method| *method == "hover"),
            OperationPoll::Mismatch("definition")
        );
        assert_eq!(
            table.poll_matching(&1, |method| *method == "definition"),
            OperationPoll::Ready {
                metadata: "definition",
                result: 11
            }
        );
    }

    #[test]
    fn prepares_work_only_after_identity_and_capacity_admission() {
        let runtime = InlineSpawner::default();
        let table = BoundedOperationTable::new(1);
        let preparations = AtomicUsize::new(0);
        let mut prepare = || {
            preparations.fetch_add(1, Ordering::AcqRel);
            Ok::<_, ()>(("meta", Box::pin(async { 7 }) as LspRuntimeFuture<i32>))
        };

        assert_eq!(
            table.admit_with("first", &runtime, &mut prepare),
            Ok(OperationAdmission::Started("meta"))
        );
        assert_eq!(
            table.admit_with("first", &runtime, &mut prepare),
            Ok(OperationAdmission::Existing("meta"))
        );
        assert_eq!(
            table.admit_with("second", &runtime, &mut prepare),
            Ok(OperationAdmission::Saturated)
        );
        assert_eq!(preparations.load(Ordering::Acquire), 1);
    }

    #[test]
    fn shares_capacity_and_aborts_cancelled_work() {
        let runtime = InlineSpawner::default();
        let capacity = BoundedOperationCapacity::new(1);
        let first = BoundedOperationTable::with_capacity(capacity.clone());
        let second = BoundedOperationTable::with_capacity(capacity);

        assert_eq!(
            first.admit("first", (), &runtime, || Box::pin(async { 1 })),
            OperationAdmission::Started(())
        );
        assert_eq!(
            second.admit("second", (), &runtime, || Box::pin(async { 2 })),
            OperationAdmission::Saturated
        );
        let aborted = runtime.last_aborted.lock().unwrap().clone().unwrap();
        assert!(first.cancel(&"first"));
        assert!(aborted.load(Ordering::Acquire));
        assert_eq!(
            second.admit("second", (), &runtime, || Box::pin(async { 2 })),
            OperationAdmission::Started(())
        );
    }

    #[test]
    fn cancellation_always_aborts_the_local_task() {
        let runtime = InlineSpawner::default();
        let table = BoundedOperationTable::new(1);

        assert_eq!(
            table.admit("first", (), &runtime, || Box::pin(async { 1 })),
            OperationAdmission::Started(())
        );
        let aborted = runtime.last_aborted.lock().unwrap().clone().unwrap();
        assert!(table.cancel(&"first"));
        assert!(aborted.load(Ordering::Acquire));
    }

    #[test]
    fn cancellation_waits_for_table_contention() {
        let runtime = InlineSpawner::default();
        let table = Arc::new(BoundedOperationTable::new(1));
        assert_eq!(
            table.admit("first", (), &runtime, || Box::pin(async { 1 })),
            OperationAdmission::Started(())
        );

        let guard = table.in_flight.lock().unwrap();
        let cancel_table = Arc::clone(&table);
        let (sender, receiver) = sync_channel(1);
        let cancellation = thread::spawn(move || {
            sender
                .send(cancel_table.cancel(&"first"))
                .expect("send cancellation outcome");
        });

        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(25)),
            Err(RecvTimeoutError::Timeout)
        );
        drop(guard);
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("cancellation completes after contention")
        );
        cancellation.join().expect("cancellation thread");
    }
}
