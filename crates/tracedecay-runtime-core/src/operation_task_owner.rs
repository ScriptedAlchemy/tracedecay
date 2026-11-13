use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::runtime::Handle;
use tokio::sync::{Notify, watch};
use tokio::task::{JoinError, JoinHandle};

#[derive(Debug, Clone, thiserror::Error)]
pub enum RuntimeOperationAdmissionError {
    #[error("runtime operation settlement admission is closed")]
    Closed,
    #[error("runtime operation settlement task identity space is exhausted")]
    IdentityExhausted,
}

#[derive(Debug, Clone)]
pub struct RuntimeOperationTaskJoinFailure {
    pub task_id: u64,
    pub error: Arc<JoinError>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RuntimeOperationShutdownError {
    #[error("runtime operation settlement tasks failed to join: {0:?}")]
    JoinFailures(Vec<RuntimeOperationTaskJoinFailure>),
    #[error("runtime operation settlement shutdown completion unavailable: {0}")]
    CompletionUnavailable(watch::error::RecvError),
}

type ShutdownCompletion = Option<Result<(), RuntimeOperationShutdownError>>;

enum RuntimeOperationTaskLifecycleV1 {
    Accepting,
    Fenced,
    Draining {
        completion: watch::Receiver<ShutdownCompletion>,
    },
}

struct RuntimeOperationTaskStateV1 {
    lifecycle: RuntimeOperationTaskLifecycleV1,
    next_task_id: u64,
    tasks: BTreeMap<u64, JoinHandle<()>>,
}

/// Every admitted closure holds its finalizer until it finishes or is dropped.
#[derive(Default)]
struct RuntimeOperationSettlementV1 {
    running: AtomicUsize,
    idle: Notify,
}

struct RuntimeOperationTaskFinalizerV1 {
    state: Weak<Mutex<RuntimeOperationTaskStateV1>>,
    settlement: Arc<RuntimeOperationSettlementV1>,
    task_id: u64,
    completed: bool,
}

impl Drop for RuntimeOperationTaskFinalizerV1 {
    fn drop(&mut self) {
        // Preserve panicked/cancelled handles so shutdown can report failures.
        if self.completed
            && let Some(state) = self.state.upgrade()
        {
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tasks
                .remove(&self.task_id);
        }
        self.settlement.running.fetch_sub(1, Ordering::AcqRel);
        self.settlement.idle.notify_waiters();
    }
}

/// Owns settlement independently of request waiters. Admission and handle
/// publication share the shutdown fence's mutex boundary. A started reaper
/// retains every task even if its caller or this owner stops waiting.
/// Drop before shutdown aborts async tasks; running blocking closures finish.
pub struct RuntimeOperationTaskOwnerV1 {
    runtime: Handle,
    state: Arc<Mutex<RuntimeOperationTaskStateV1>>,
    settlement: Arc<RuntimeOperationSettlementV1>,
}

impl RuntimeOperationTaskOwnerV1 {
    pub fn new(runtime: Handle) -> Self {
        Self {
            runtime,
            state: Arc::new(Mutex::new(RuntimeOperationTaskStateV1 {
                lifecycle: RuntimeOperationTaskLifecycleV1::Accepting,
                next_task_id: 0,
                tasks: BTreeMap::new(),
            })),
            settlement: Arc::default(),
        }
    }

    pub fn retain<F, Fut>(&self, settlement: F) -> Result<(), RuntimeOperationAdmissionError>
    where
        F: FnOnce(Handle) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.admit(|finalizer| {
            let runtime = self.runtime.clone();
            self.runtime.spawn(async move {
                let mut finalizer = finalizer;
                settlement(runtime).await;
                finalizer.completed = true;
            })
        })
    }

    pub fn retain_blocking<F>(&self, settlement: F) -> Result<(), RuntimeOperationAdmissionError>
    where
        F: FnOnce() + Send + 'static,
    {
        self.admit(|finalizer| {
            crate::native_runtime::spawn_blocking_on(&self.runtime, move || {
                let mut finalizer = finalizer;
                settlement();
                finalizer.completed = true;
            })
        })
    }

    fn admit(
        &self,
        spawn: impl FnOnce(RuntimeOperationTaskFinalizerV1) -> JoinHandle<()>,
    ) -> Result<(), RuntimeOperationAdmissionError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !matches!(state.lifecycle, RuntimeOperationTaskLifecycleV1::Accepting) {
            return Err(RuntimeOperationAdmissionError::Closed);
        }
        let task_id = state
            .next_task_id
            .checked_add(1)
            .ok_or(RuntimeOperationAdmissionError::IdentityExhausted)?;
        self.settlement.running.fetch_add(1, Ordering::AcqRel);
        let task = spawn(RuntimeOperationTaskFinalizerV1 {
            state: Arc::downgrade(&self.state),
            settlement: Arc::clone(&self.settlement),
            task_id,
            completed: false,
        });
        state.next_task_id = task_id;
        state.tasks.insert(task_id, task);
        Ok(())
    }

    /// Waits for admitted settlement without fencing later admission.
    pub async fn settled(&self) {
        loop {
            // Construct before checking: notify_waiters retains its generation
            // even when notification happens before the first poll.
            let idle = self.settlement.idle.notified();
            if self.settlement.running.load(Ordering::Acquire) == 0 {
                return;
            }
            idle.await;
        }
    }

    pub fn begin_shutdown(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(state.lifecycle, RuntimeOperationTaskLifecycleV1::Accepting) {
            state.lifecycle = RuntimeOperationTaskLifecycleV1::Fenced;
        }
    }

    pub async fn shutdown(&self) -> Result<(), RuntimeOperationShutdownError> {
        let mut completion = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let RuntimeOperationTaskLifecycleV1::Draining { completion, .. } = &state.lifecycle {
                completion.clone()
            } else {
                state.lifecycle = RuntimeOperationTaskLifecycleV1::Fenced;
                let (publish_completion, completion) = watch::channel(None);
                let tasks = std::mem::take(&mut state.tasks);
                if tasks.is_empty() {
                    // Completed owners need no runtime to drain. Cleanup can
                    // arrive after their admission runtime has stopped; an
                    // empty reaper spawned there would be cancelled without
                    // publishing the already-settled result.
                    publish_completion.send_replace(Some(Ok(())));
                } else {
                    self.runtime.spawn(async move {
                        let mut failures = Vec::new();
                        for (task_id, task) in tasks {
                            if let Err(error) = task.await {
                                failures.push(RuntimeOperationTaskJoinFailure {
                                    task_id,
                                    error: Arc::new(error),
                                });
                            }
                        }
                        let result = if failures.is_empty() {
                            Ok(())
                        } else {
                            Err(RuntimeOperationShutdownError::JoinFailures(failures))
                        };
                        publish_completion.send_replace(Some(result));
                    });
                }
                state.lifecycle = RuntimeOperationTaskLifecycleV1::Draining {
                    completion: completion.clone(),
                };
                completion
            }
        };
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result;
            }
            if let Err(error) = completion.changed().await {
                return completion.borrow().clone().unwrap_or(Err(
                    RuntimeOperationShutdownError::CompletionUnavailable(error),
                ));
            }
        }
    }
}

impl Drop for RuntimeOperationTaskOwnerV1 {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.lifecycle {
            RuntimeOperationTaskLifecycleV1::Draining { .. } => {}
            RuntimeOperationTaskLifecycleV1::Accepting
            | RuntimeOperationTaskLifecycleV1::Fenced => {
                state.lifecycle = RuntimeOperationTaskLifecycleV1::Fenced;
                state.tasks.values().for_each(JoinHandle::abort);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::oneshot;

    #[test]
    fn settled_owner_shuts_down_after_its_admission_runtime_stops() {
        let admission_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let owner = RuntimeOperationTaskOwnerV1::new(admission_runtime.handle().clone());
        admission_runtime.block_on(async {
            owner.retain(|_| async {}).unwrap();
            owner.settled().await;
        });
        drop(admission_runtime);

        let cleanup_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cleanup_runtime.block_on(async {
            owner.shutdown().await.unwrap();
            owner.shutdown().await.unwrap();
            assert!(matches!(
                owner.retain(|_| async {}),
                Err(RuntimeOperationAdmissionError::Closed)
            ));
        });
    }

    #[tokio::test]
    async fn accepted_settlement_survives_request_and_shutdown_waiter_cancellation() {
        let owner = Arc::new(RuntimeOperationTaskOwnerV1::new(Handle::current()));
        let settled = Arc::new(AtomicBool::new(false));
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, entered) = oneshot::channel();
        let (accepted, admission) = oneshot::channel();
        let request_owner = Arc::clone(&owner);
        let completed = Arc::clone(&settled);
        let requester = tokio::spawn(async move {
            request_owner
                .retain_blocking(move || {
                    started.send(()).unwrap();
                    blocked.recv().unwrap();
                    completed.store(true, Ordering::Release);
                })
                .unwrap();
            accepted.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        admission.await.unwrap();
        entered.await.unwrap();
        requester.abort();
        assert!(requester.await.unwrap_err().is_cancelled());
        let waiter_owner = Arc::clone(&owner);
        let (waiting, began) = oneshot::channel();
        let waiter = tokio::spawn(async move {
            let shutdown = waiter_owner.shutdown();
            tokio::pin!(shutdown);
            std::future::poll_fn(|context| {
                assert!(shutdown.as_mut().poll(context).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            waiting.send(()).unwrap();
            shutdown.await
        });
        began.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert!(!settled.load(Ordering::Acquire));
        release.send(()).unwrap();
        owner.shutdown().await.unwrap();
        assert!(settled.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn shutdown_preserves_typed_panic_failure() {
        let owner = RuntimeOperationTaskOwnerV1::new(Handle::current());
        owner
            .retain(|_| async { panic!("settlement failed") })
            .unwrap();
        let RuntimeOperationShutdownError::JoinFailures(failures) =
            owner.shutdown().await.unwrap_err()
        else {
            panic!("shutdown must report the task panic")
        };
        assert_eq!(failures.len(), 1);
        assert!(failures[0].error.is_panic());
    }

    #[tokio::test]
    async fn fenced_admission_never_polls_rejected_work() {
        let owner = RuntimeOperationTaskOwnerV1::new(Handle::current());
        owner.begin_shutdown();
        let polled = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&polled);
        assert!(matches!(
            owner.retain(move |_| async move { observed.store(true, Ordering::Release) }),
            Err(RuntimeOperationAdmissionError::Closed)
        ));
        owner.shutdown().await.unwrap();
        assert!(!polled.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn retained_handle_admits_settlement_from_plain_threads() {
        let owner = Arc::new(RuntimeOperationTaskOwnerV1::new(Handle::current()));
        let thread_owner = Arc::clone(&owner);
        let (answer, receipt) = oneshot::channel();
        std::thread::spawn(move || {
            assert!(Handle::try_current().is_err());
            thread_owner
                .retain(move |_| async move { answer.send(17).unwrap() })
                .unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(receipt.await.unwrap(), 17);
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn async_factory_receives_runtime_handle() {
        let owner = RuntimeOperationTaskOwnerV1::new(Handle::current());
        let (answer, receipt) = oneshot::channel();
        owner
            .retain(move |runtime| async move {
                let value = runtime.spawn(async { 19 }).await.unwrap();
                answer.send(value).unwrap();
            })
            .unwrap();
        assert_eq!(receipt.await.unwrap(), 19);
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn dropping_owner_after_shutdown_began_keeps_accepted_settlement() {
        let owner = RuntimeOperationTaskOwnerV1::new(Handle::current());
        let (release, blocked) = oneshot::channel();
        let (answer, receipt) = oneshot::channel();
        owner
            .retain(move |_| async move {
                blocked.await.unwrap();
                answer.send(23).unwrap();
            })
            .unwrap();
        {
            let shutdown = owner.shutdown();
            tokio::pin!(shutdown);
            std::future::poll_fn(|context| {
                assert!(shutdown.as_mut().poll(context).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        }
        drop(owner);
        release.send(()).unwrap();
        assert_eq!(receipt.await.unwrap(), 23);
    }

    #[tokio::test]
    async fn settled_waits_for_running_settlement_and_keeps_admission_open() {
        let owner = RuntimeOperationTaskOwnerV1::new(Handle::current());
        let (release, blocked) = oneshot::channel();
        owner
            .retain(move |_| async move { blocked.await.unwrap() })
            .unwrap();
        {
            let settled = owner.settled();
            tokio::pin!(settled);
            std::future::poll_fn(|context| {
                assert!(settled.as_mut().poll(context).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            release.send(()).unwrap();
            settled.await;
        }
        owner.retain(|_| async {}).unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn dropping_owner_before_shutdown_aborts_owned_tasks() {
        let owner = RuntimeOperationTaskOwnerV1::new(Handle::current());
        let (dropped, receipt) = oneshot::channel::<()>();
        owner
            .retain(move |_| async move {
                let _dropped = dropped;
                std::future::pending::<()>().await;
            })
            .unwrap();
        drop(owner);
        assert!(receipt.await.is_err());
    }
}
