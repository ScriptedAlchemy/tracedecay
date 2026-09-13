use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::watch;

struct RuntimeOperationTaskStateV1 {
    accepting: bool,
    next_task_id: u64,
    tasks: BTreeMap<u64, tokio::task::JoinHandle<()>>,
    shutdown_completion: Option<watch::Receiver<Option<Result<(), String>>>>,
}

struct RuntimeOperationTaskFinalizerV1 {
    state: Weak<Mutex<RuntimeOperationTaskStateV1>>,
    task_id: u64,
    completed: bool,
}

impl Drop for RuntimeOperationTaskFinalizerV1 {
    fn drop(&mut self) {
        if !self.completed {
            return;
        }
        let Some(state) = self.state.upgrade() else {
            return;
        };
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tasks
            .remove(&self.task_id);
    }
}

/// Lifecycle owner for runtime operation settlement tasks.
///
/// Admission and task publication share one synchronous mutex boundary, so
/// `begin_shutdown` fences every later operation before shutdown atomically
/// takes and joins all previously retained settlement tasks.
pub struct RuntimeOperationTaskOwnerV1 {
    state: Arc<Mutex<RuntimeOperationTaskStateV1>>,
}

impl RuntimeOperationTaskOwnerV1 {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RuntimeOperationTaskStateV1 {
                accepting: true,
                next_task_id: 0,
                tasks: BTreeMap::new(),
                shutdown_completion: None,
            })),
        }
    }

    /// Synchronously admits and spawns one settlement future.
    ///
    /// `false` means admission was fenced, no Tokio runtime was available, or
    /// the monotonic task identity space was exhausted. In every case the
    /// supplied future is dropped without being polled.
    pub fn retain<Fut>(&self, settlement: Fut) -> bool
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.accepting {
            return false;
        }
        let Some(task_id) = state.next_task_id.checked_add(1) else {
            return false;
        };
        state.next_task_id = task_id;
        let finalizer_state = Arc::downgrade(&self.state);
        let task = runtime.spawn(async move {
            let mut finalizer = RuntimeOperationTaskFinalizerV1 {
                state: finalizer_state,
                task_id,
                completed: false,
            };
            settlement.await;
            finalizer.completed = true;
        });
        state.tasks.insert(task_id, task);
        true
    }

    pub fn begin_shutdown(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accepting = false;
    }

    /// Fences admission and joins every settlement task retained before the
    /// fence. Operation results are delivered separately to live callers, so
    /// only an actual settlement-task join failure is reported here.
    pub async fn shutdown(&self) -> Result<(), String> {
        let mut completion = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.accepting = false;
            if let Some(completion) = state.shutdown_completion.clone() {
                completion
            } else {
                let tasks = std::mem::take(&mut state.tasks);
                let (publish_completion, completion) = watch::channel(None);
                state.shutdown_completion = Some(completion.clone());
                drop(tokio::spawn(async move {
                    let mut failures = Vec::new();
                    for (task_id, task) in tasks {
                        if let Err(error) = task.await {
                            failures.push(format!(
                                "runtime operation settlement task {task_id} join failed: {error}"
                            ));
                        }
                    }
                    let result = if failures.is_empty() {
                        Ok(())
                    } else {
                        Err(failures.join("; "))
                    };
                    publish_completion.send_replace(Some(result));
                }));
                completion
            }
        };

        loop {
            if let Some(result) = completion.borrow().clone() {
                return result;
            }
            if completion.changed().await.is_err() {
                if let Some(result) = completion.borrow().clone() {
                    return result;
                }
                return Err(
                    "runtime operation settlement reaper ended without publishing shutdown completion"
                        .to_owned(),
                );
            }
        }
    }
}

impl Default for RuntimeOperationTaskOwnerV1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RuntimeOperationTaskOwnerV1 {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.accepting = false;
        // A started shutdown has already transferred its handles to the
        // detached reaper. Only tasks still owned by this map may be aborted.
        for (_, task) in std::mem::take(&mut state.tasks) {
            task.abort();
        }
    }
}
