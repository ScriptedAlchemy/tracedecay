use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use asupersync::Cx;
use asupersync::channel::watch;
use asupersync::runtime::{JoinError, RuntimeHandle, SpawnError, TaskHandle};
use asupersync::sync::Notify;
use asupersync::types::Budget;

#[derive(Debug, Clone, thiserror::Error)]
pub enum RuntimeOperationAdmissionError {
    #[error("runtime operation settlement admission is closed")]
    Closed,
    #[error("runtime operation settlement task identity space is exhausted")]
    IdentityExhausted,
    #[error("runtime operation settlement requires a native blocking pool")]
    BlockingPoolUnavailable,
    #[error("runtime operation settlement spawn failed: {0}")]
    Spawn(#[from] SpawnError),
}

#[derive(Debug, Clone)]
pub struct RuntimeOperationTaskJoinFailure {
    pub task_id: u64,
    pub error: JoinError,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RuntimeOperationShutdownError {
    #[error("runtime operation settlement reaper spawn failed: {0}")]
    ReaperAdmission(SpawnError),
    #[error("runtime operation settlement tasks failed to join: {0:?}")]
    JoinFailures(Vec<RuntimeOperationTaskJoinFailure>),
    #[error("runtime operation settlement shutdown wait was cancelled")]
    WaitCancelled,
    #[error("runtime operation settlement shutdown completion unavailable: {0}")]
    CompletionUnavailable(watch::RecvError),
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
    /// Handles the owner, or its shutdown reaper, still has to join.
    tasks: BTreeMap<u64, TaskHandle<()>>,
}

/// Admitted settlement closures that have not finished running. A counter
/// outside the state mutex lets a closure that is dropped unrun, inside a
/// failed spawn, retire without taking that mutex.
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
        // A panicked closure keeps its handle, so shutdown reports the panic.
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

/// Owns settlement in the daemon's execution context, independently of request waiters.
/// Admission and handle publication share the shutdown fence's mutex boundary.
/// Successful shutdown waits for accepted closures. Drop requests cancellation;
/// running native blocking closures remain nonpreemptible.
pub struct RuntimeOperationTaskOwnerV1 {
    cx: Cx,
    state: Arc<Mutex<RuntimeOperationTaskStateV1>>,
    settlement: Arc<RuntimeOperationSettlementV1>,
}

impl RuntimeOperationTaskOwnerV1 {
    pub fn new(runtime: RuntimeHandle) -> Result<Self, RuntimeOperationAdmissionError> {
        let cx = runtime.try_request_cx_with_budget(Budget::INFINITE)?;
        if cx.blocking_pool_handle().is_none() {
            return Err(RuntimeOperationAdmissionError::BlockingPoolUnavailable);
        }
        Ok(Self {
            cx,
            state: Arc::new(Mutex::new(RuntimeOperationTaskStateV1 {
                lifecycle: RuntimeOperationTaskLifecycleV1::Accepting,
                next_task_id: 0,
                tasks: BTreeMap::new(),
            })),
            settlement: Arc::default(),
        })
    }

    pub fn retain<F, Fut>(&self, settlement: F) -> Result<(), RuntimeOperationAdmissionError>
    where
        F: FnOnce(Cx) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.admit(|finalizer| {
            self.cx.spawn(move |cx| async move {
                // Bind the whole finalizer so the task owns it, not one field.
                let mut finalizer = finalizer;
                settlement(cx).await;
                finalizer.completed = true;
            })
        })
    }

    pub fn retain_blocking<F>(&self, settlement: F) -> Result<(), RuntimeOperationAdmissionError>
    where
        F: FnOnce() + Send + 'static,
    {
        self.admit(|finalizer| {
            self.cx.spawn_blocking(move |_| {
                let mut finalizer = finalizer;
                settlement();
                finalizer.completed = true;
            })
        })
    }

    fn admit(
        &self,
        spawn: impl FnOnce(RuntimeOperationTaskFinalizerV1) -> Result<TaskHandle<()>, SpawnError>,
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
        })?;
        state.next_task_id = task_id;
        state.tasks.insert(task_id, task);
        Ok(())
    }

    /// Waits until every admitted settlement has finished running, without
    /// fencing admission.
    pub async fn settled(&self) {
        self.settlement
            .idle
            .wait_until(|| self.settlement.running.load(Ordering::Acquire) == 0)
            .await;
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

    /// Keeps the reaper and its completion receiver owned after a waiter is dropped.
    pub async fn shutdown(&self, waiter_cx: &Cx) -> Result<(), RuntimeOperationShutdownError> {
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
                // The handles stay in owner state until the reaper runs, so a
                // refused reaper leaves them for a later shutdown to join.
                let reaper_state = Arc::clone(&self.state);
                self.cx
                    .spawn(move |_| async move {
                        let mut tasks = std::mem::take(
                            &mut reaper_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .tasks,
                        );
                        let mut failures = Vec::new();
                        while let Some((task_id, mut task)) = tasks.pop_first() {
                            // `poll_join` never aborts the joined task when the
                            // reaper itself is cancelled.
                            if let Err(error) =
                                std::future::poll_fn(|context| task.poll_join(context)).await
                            {
                                failures.push(RuntimeOperationTaskJoinFailure { task_id, error });
                            }
                        }
                        let result = if failures.is_empty() {
                            Ok(())
                        } else {
                            Err(RuntimeOperationShutdownError::JoinFailures(failures))
                        };
                        publish_completion.send(Some(result))
                    })
                    .map_err(RuntimeOperationShutdownError::ReaperAdmission)?;
                state.lifecycle = RuntimeOperationTaskLifecycleV1::Draining {
                    completion: completion.clone(),
                };
                completion
            }
        };

        loop {
            if let Some(result) = completion.borrow_and_clone() {
                return result;
            }
            if let Err(error) = completion.changed(waiter_cx).await {
                if let Some(result) = completion.borrow_and_clone() {
                    return result;
                }
                return Err(match error {
                    watch::RecvError::Cancelled => RuntimeOperationShutdownError::WaitCancelled,
                    failure => RuntimeOperationShutdownError::CompletionUnavailable(failure),
                });
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
            // A started reaper joins every retained handle, owner or not.
            RuntimeOperationTaskLifecycleV1::Draining { .. } => {}
            RuntimeOperationTaskLifecycleV1::Accepting
            | RuntimeOperationTaskLifecycleV1::Fenced => {
                state.lifecycle = RuntimeOperationTaskLifecycleV1::Fenced;
                state.tasks.values().for_each(TaskHandle::abort);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asupersync::channel::oneshot;
    use asupersync::runtime::RuntimeBuilder;
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Poll;

    #[test]
    fn accepted_settlement_survives_request_region_and_shutdown_waiter_cancellation() {
        let runtime = RuntimeBuilder::current_thread()
            .blocking_threads(1, 1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let cx = Cx::current().unwrap();
            let request_region = cx
                .open_child_region(asupersync::cx::child_region::ChildRegionSpec::inherit())
                .await
                .unwrap();
            let settled = Arc::new(AtomicBool::new(false));
            let (release, blocked) = std::sync::mpsc::channel();
            let (started, mut entered) = oneshot::channel();
            let (accepted, mut admission) = oneshot::channel();
            // Cancellation is cooperative, so the request parks on a receive
            // that observes it rather than on a future that never wakes.
            let (_parked_sender, mut parked) = oneshot::channel::<()>();
            let observed = Arc::clone(&settled);
            let mut requester = request_region
                .cx()
                .spawn(move |request_cx| async move {
                    let execution_runtime = asupersync::runtime::Runtime::current_handle().unwrap();
                    let owner =
                        Arc::new(RuntimeOperationTaskOwnerV1::new(execution_runtime).unwrap());
                    owner
                        .retain_blocking(move || {
                            started.send_blocking(()).unwrap();
                            blocked.recv().unwrap();
                            observed.store(true, Ordering::Release);
                        })
                        .unwrap();
                    assert!(
                        accepted
                            .send(&request_cx, (owner, request_cx.clone()))
                            .is_ok()
                    );
                    parked.recv(&request_cx).await
                })
                .unwrap();
            let (owner, cancelled_request_cx) = admission.recv(&cx).await.unwrap();
            entered.recv(&cx).await.unwrap();
            request_region
                .cancel(asupersync::types::CancelReason::user("request cancelled"))
                .unwrap();
            assert!(matches!(
                requester.join(&cx).await,
                Ok(Err(oneshot::RecvError::Cancelled)) | Err(JoinError::Cancelled(_))
            ));
            request_region.close().await.unwrap();
            assert!(matches!(
                owner.shutdown(&cancelled_request_cx).await,
                Err(RuntimeOperationShutdownError::WaitCancelled)
            ));
            {
                let mut shutdown = std::pin::pin!(owner.shutdown(&cx));
                std::future::poll_fn(|context| {
                    assert!(shutdown.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;
            }
            assert!(!settled.load(Ordering::Acquire));
            assert!(matches!(
                owner.retain_blocking(|| {}),
                Err(RuntimeOperationAdmissionError::Closed)
            ));
            release.send(()).unwrap();
            owner.shutdown(&cx).await.unwrap();
            assert!(settled.load(Ordering::Acquire));
            owner.shutdown(&cx).await.unwrap();
        });
    }

    #[test]
    fn shutdown_preserves_typed_panic_failure() {
        let runtime = RuntimeBuilder::current_thread()
            .blocking_threads(1, 1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let cx = Cx::current().unwrap();
            let owner = RuntimeOperationTaskOwnerV1::new(runtime.handle()).unwrap();
            owner
                .retain_blocking(|| panic!("settlement panic"))
                .unwrap();
            let RuntimeOperationShutdownError::JoinFailures(failures) =
                owner.shutdown(&cx).await.unwrap_err()
            else {
                panic!("shutdown must report the native settlement join failure");
            };
            assert_eq!(failures.len(), 1);
            assert_eq!(failures[0].task_id, 1);
            assert!(matches!(failures[0].error, JoinError::Panicked(_)));
            assert!(matches!(
                owner.shutdown(&cx).await,
                Err(RuntimeOperationShutdownError::JoinFailures(_))
            ));
        });
    }

    #[test]
    fn fenced_admission_never_polls_rejected_work() {
        let runtime = RuntimeBuilder::current_thread()
            .blocking_threads(1, 1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let cx = Cx::current().unwrap();
            let owner = RuntimeOperationTaskOwnerV1::new(runtime.handle()).unwrap();
            let polled = Arc::new(AtomicBool::new(false));
            let observed = Arc::clone(&polled);
            owner.begin_shutdown();
            assert!(matches!(
                owner.retain_blocking(move || observed.store(true, Ordering::Release)),
                Err(RuntimeOperationAdmissionError::Closed)
            ));
            owner.shutdown(&cx).await.unwrap();
            assert!(!polled.load(Ordering::Acquire));
        });
    }

    #[test]
    fn retained_context_admits_settlement_from_plain_threads() {
        let runtime = RuntimeBuilder::current_thread()
            .blocking_threads(1, 1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let cx = Cx::current().unwrap();
            let owner = Arc::new(RuntimeOperationTaskOwnerV1::new(runtime.handle()).unwrap());
            let settled = Arc::new(AtomicBool::new(false));
            let admitted_owner = Arc::clone(&owner);
            let observed = Arc::clone(&settled);
            std::thread::spawn(move || {
                assert!(Cx::current().is_none());
                admitted_owner.retain_blocking(move || observed.store(true, Ordering::Release))
            })
            .join()
            .unwrap()
            .unwrap();
            owner.shutdown(&cx).await.unwrap();
            assert!(settled.load(Ordering::Acquire));
        });
    }

    #[test]
    fn async_factory_receives_runtime_wired_context() {
        let runtime = RuntimeBuilder::current_thread()
            .blocking_threads(1, 1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let cx = Cx::current().unwrap();
            let owner = RuntimeOperationTaskOwnerV1::new(runtime.handle()).unwrap();
            let (completed, mut result) = oneshot::channel();
            owner
                .retain(move |child| async move {
                    let mut nested = child.spawn(|_| async { 73_u32 }).unwrap();
                    let value = nested.join(&child).await.unwrap();
                    completed.send(&child, value).unwrap();
                })
                .unwrap();
            assert_eq!(result.recv(&cx).await.unwrap(), 73);
            owner.shutdown(&cx).await.unwrap();
        });
    }

    #[test]
    fn dropping_owner_after_shutdown_began_keeps_accepted_settlement() {
        let runtime = RuntimeBuilder::current_thread()
            .blocking_threads(1, 1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let cx = Cx::current().unwrap();
            let owner = RuntimeOperationTaskOwnerV1::new(runtime.handle()).unwrap();
            let (release, mut gate) = oneshot::channel::<()>();
            let (settled, mut done) = oneshot::channel();
            owner
                .retain(move |child| async move {
                    gate.recv(&child).await.unwrap();
                    assert!(settled.send(&child, 7_u32).is_ok());
                })
                .unwrap();
            {
                let mut shutdown = std::pin::pin!(owner.shutdown(&cx));
                std::future::poll_fn(|context| {
                    assert!(shutdown.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;
            }
            drop(owner);
            assert!(release.send(&cx, ()).is_ok());
            assert_eq!(
                done.recv(&cx).await.unwrap(),
                7,
                "settlement accepted before shutdown must finish after its owner drops"
            );
        });
    }

    #[test]
    fn settled_waits_for_running_settlement_and_keeps_admission_open() {
        let runtime = RuntimeBuilder::current_thread()
            .blocking_threads(1, 1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let cx = Cx::current().unwrap();
            let owner = RuntimeOperationTaskOwnerV1::new(runtime.handle()).unwrap();
            let settled = Arc::new(AtomicBool::new(false));
            let (release, blocked) = std::sync::mpsc::channel::<()>();
            let (started, mut entered) = oneshot::channel();
            let observed = Arc::clone(&settled);
            owner
                .retain_blocking(move || {
                    started.send_blocking(()).unwrap();
                    blocked.recv().unwrap();
                    observed.store(true, Ordering::Release);
                })
                .unwrap();
            entered.recv(&cx).await.unwrap();
            {
                let mut waiting = std::pin::pin!(owner.settled());
                std::future::poll_fn(|context| {
                    assert!(waiting.as_mut().poll(context).is_pending());
                    Poll::Ready(())
                })
                .await;
            }
            release.send(()).unwrap();
            owner.settled().await;
            assert!(settled.load(Ordering::Acquire));
            owner
                .retain_blocking(|| {})
                .expect("settling never fences admission");
            owner.shutdown(&cx).await.unwrap();
        });
    }

    #[test]
    fn construction_rejects_native_runtime_without_blocking_pool() {
        let runtime = RuntimeBuilder::current_thread()
            .blocking_threads(0, 0)
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(matches!(
                RuntimeOperationTaskOwnerV1::new(runtime.handle()),
                Err(RuntimeOperationAdmissionError::BlockingPoolUnavailable)
            ));
        });
    }
}
