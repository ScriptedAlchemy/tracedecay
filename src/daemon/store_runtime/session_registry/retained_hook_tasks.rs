use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};

use crate::application::observation::ObservationCancellation;

struct RetainedHookTask {
    generation: u64,
    cancellation: ObservationCancellation,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct RetainedHookTaskState {
    accepting: bool,
    next_generation: u64,
    tasks: BTreeMap<String, RetainedHookTask>,
}

/// Daemon-owned terminal-hook work. A new terminal receipt for one provider
/// session cancels its predecessor, and daemon retirement cancels every task.
#[derive(Default)]
pub(super) struct RetainedHookTasks {
    state: Arc<Mutex<RetainedHookTaskState>>,
}

impl RetainedHookTasks {
    pub(super) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RetainedHookTaskState {
                accepting: true,
                ..RetainedHookTaskState::default()
            })),
        }
    }

    pub(super) fn retain<F, Fut>(&self, provider: &str, session_id: &str, operation: F) -> bool
    where
        F: FnOnce(ObservationCancellation) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        let key = format!("{provider}\0{session_id}");
        let previous = {
            let Ok(mut state) = self.state.lock() else {
                return false;
            };
            if !state.accepting {
                return false;
            }
            let Some(generation) = state.next_generation.checked_add(1) else {
                return false;
            };
            state.next_generation = generation;
            let cancellation = ObservationCancellation::default();
            let task_cancellation = cancellation.clone();
            let weak_state = Arc::downgrade(&self.state);
            let task_key = key.clone();
            let task = handle.spawn(async move {
                operation(task_cancellation).await;
                finish_retained_hook_task(weak_state, &task_key, generation);
            });
            state.tasks.insert(
                key,
                RetainedHookTask {
                    generation,
                    cancellation,
                    handle: task,
                },
            )
        };
        if let Some(previous) = previous {
            previous.cancellation.cancel();
        }
        true
    }
}

fn finish_retained_hook_task(
    state: Weak<Mutex<RetainedHookTaskState>>,
    key: &str,
    generation: u64,
) {
    let Some(state) = state.upgrade() else {
        return;
    };
    let Ok(mut state) = state.lock() else {
        return;
    };
    if state
        .tasks
        .get(key)
        .is_some_and(|task| task.generation == generation)
    {
        state.tasks.remove(key);
    }
}

impl Drop for RetainedHookTasks {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.accepting = false;
        for task in std::mem::take(&mut state.tasks).into_values() {
            task.cancellation.cancel();
            task.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn new_terminal_receipt_cancels_the_retained_predecessor() {
        let tasks = RetainedHookTasks::new();
        let cancelled = Arc::new(AtomicBool::new(false));
        let first_cancelled = Arc::clone(&cancelled);
        assert!(
            tasks.retain("codex", "session-1", move |cancellation| async move {
                tokio::task::yield_now().await;
                first_cancelled.store(cancellation.is_cancelled(), Ordering::Release);
            })
        );
        assert!(tasks.retain("codex", "session-1", |_| async {}));
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        assert!(cancelled.load(Ordering::Acquire));
    }
}
