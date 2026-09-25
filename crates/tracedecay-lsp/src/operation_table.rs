use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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
    Saturated,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OperationPoll<M, T> {
    Ready { metadata: M, result: T },
    Pending(M),
    Mismatch(M),
    Dropped(M),
    Missing,
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

    /// Every critical section is a map operation plus a task spawn, so callers
    /// wait for the table instead of reporting a peer's access as busy. A
    /// panicked holder leaves the map itself consistent.
    fn in_flight(&self) -> MutexGuard<'_, BTreeMap<K, PendingOperation<M, T>>> {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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
        let mut in_flight = self.in_flight();
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
        let mut in_flight = self.in_flight();
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
        let pending = self.in_flight().remove(key);
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
    use std::task::{Context, Poll};
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

    #[derive(Default)]
    struct InlineSpawner {
        last_aborted: Mutex<Option<Arc<AtomicBool>>>,
    }

    impl LspRuntimeSpawner for InlineSpawner {
        fn spawn(&self, mut future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
            // These harness futures must complete synchronously; a wake would
            // indicate that the test spawner is not a valid runtime for them.
            let mut context = Context::from_waker(std::task::Waker::noop());
            assert_eq!(Pin::new(&mut future).poll(&mut context), Poll::Ready(()));
            let aborted = Arc::new(AtomicBool::new(false));
            *self.last_aborted.lock().unwrap() = Some(Arc::clone(&aborted));
            Box::new(InlineTask { aborted })
        }
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
        let prepare = || {
            preparations.fetch_add(1, Ordering::AcqRel);
            Ok::<_, ()>(("meta", Box::pin(async { 7 }) as LspRuntimeFuture<i32>))
        };

        assert_eq!(
            table.admit_with("first", &runtime, prepare),
            Ok(OperationAdmission::Started("meta"))
        );
        assert_eq!(
            table.admit_with("first", &runtime, prepare),
            Ok(OperationAdmission::Existing("meta"))
        );
        assert_eq!(
            table.admit_with("second", &runtime, prepare),
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

    #[test]
    fn admission_and_poll_wait_for_table_contention() {
        let table = Arc::new(BoundedOperationTable::new(2));
        assert_eq!(
            table.admit("first", (), &InlineSpawner::default(), || Box::pin(async {
                1
            })),
            OperationAdmission::Started(())
        );

        let guard = table.in_flight.lock().unwrap();
        let (sender, receiver) = sync_channel(2);
        let admit_table = Arc::clone(&table);
        let admit_sender = sender.clone();
        let admission = thread::spawn(move || {
            let outcome = admit_table.admit("second", (), &InlineSpawner::default(), || {
                Box::pin(async { 2 })
            });
            admit_sender
                .send(format!("{outcome:?}"))
                .expect("send admission outcome");
        });
        let poll_table = Arc::clone(&table);
        let poll = thread::spawn(move || {
            sender
                .send(format!("{:?}", poll_table.poll(&"first")))
                .expect("send poll outcome");
        });

        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(25)),
            Err(RecvTimeoutError::Timeout),
            "a peer holding the table must not turn admission or poll into a refusal"
        );
        drop(guard);
        let mut outcomes = [
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("first"),
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("second"),
        ];
        outcomes.sort();
        assert_eq!(
            outcomes,
            [
                "Ready { metadata: (), result: 1 }".to_owned(),
                "Started(())".to_owned()
            ]
        );
        admission.join().expect("admission thread");
        poll.join().expect("poll thread");
        assert_eq!(
            table.poll(&"second"),
            OperationPoll::Ready {
                metadata: (),
                result: 2
            }
        );
    }

    #[test]
    fn concurrent_admissions_all_start_and_complete() {
        const WRITERS: usize = 32;
        let table = Arc::new(BoundedOperationTable::new(WRITERS));
        let barrier = Arc::new(std::sync::Barrier::new(WRITERS));
        let workers: Vec<_> = (0..WRITERS)
            .map(|writer| {
                let table = Arc::clone(&table);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let runtime = InlineSpawner::default();
                    barrier.wait();
                    let admitted = table.admit(writer, writer, &runtime, move || {
                        Box::pin(async move { writer * 10 })
                    });
                    let polled = table.poll(&writer);
                    (admitted, polled)
                })
            })
            .collect();
        for (writer, worker) in workers.into_iter().enumerate() {
            assert_eq!(
                worker.join().expect("writer thread"),
                (
                    OperationAdmission::Started(writer),
                    OperationPoll::Ready {
                        metadata: writer,
                        result: writer * 10
                    }
                )
            );
        }
    }
}
