use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};

use tracedecay_sessions::observation::ObservationCancellation;

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
    retiring: Vec<RetainedHookTask>,
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
        {
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
            state.retiring.retain(|task| !task.handle.is_finished());
            let previous = state.tasks.insert(
                key,
                RetainedHookTask {
                    generation,
                    cancellation,
                    handle: task,
                },
            );
            if let Some(previous) = previous {
                previous.cancellation.cancel();
                if !previous.handle.is_finished() {
                    state.retiring.push(previous);
                }
            }
        }
        true
    }

    pub(super) fn begin_shutdown(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.accepting = false;
        for task in state.tasks.values().chain(&state.retiring) {
            task.cancellation.cancel();
        }
    }

    pub(super) async fn retire(&self, provider: &str, session_id: &str) -> Result<(), String> {
        let key = format!("{provider}\0{session_id}");
        let task = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "retained hook task state lock is poisoned".to_owned())?;
            state.tasks.remove(&key)
        };
        let Some(task) = task else {
            return Ok(());
        };
        task.cancellation.cancel();
        match task.handle.await {
            Ok(()) => Ok(()),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(format!("retained hook task join failed: {error}")),
        }
    }

    pub(super) async fn shutdown(&self) -> Result<(), String> {
        let tasks = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "retained hook task state lock is poisoned".to_owned())?;
            state.accepting = false;
            let mut tasks = std::mem::take(&mut state.retiring);
            tasks.extend(std::mem::take(&mut state.tasks).into_values());
            for task in &tasks {
                task.cancellation.cancel();
            }
            tasks
        };
        let mut failures = Vec::new();
        for task in tasks {
            if let Err(error) = task.handle.await
                && !error.is_cancelled()
            {
                failures.push(format!("retained hook task join failed: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
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
        let mut tasks = std::mem::take(&mut state.retiring);
        tasks.extend(std::mem::take(&mut state.tasks).into_values());
        for task in tasks {
            task.cancellation.cancel();
            task.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

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

    #[tokio::test]
    async fn shutdown_fences_new_tasks_and_joins_active_task() {
        let tasks = Arc::new(RetainedHookTasks::new());
        let started = Arc::new(Notify::new());
        let cancelled = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        assert!(tasks.retain("codex", "session-1", {
            let started = Arc::clone(&started);
            let cancelled = Arc::clone(&cancelled);
            let release = Arc::clone(&release);
            move |cancellation| async move {
                started.notify_one();
                while !cancellation.is_cancelled() {
                    tokio::task::yield_now().await;
                }
                cancelled.notify_one();
                release.notified().await;
            }
        }));
        started.notified().await;

        let shutdown = tokio::spawn({
            let tasks = Arc::clone(&tasks);
            async move { tasks.shutdown().await }
        });
        cancelled.notified().await;
        assert!(
            !shutdown.is_finished(),
            "shutdown must join the active task"
        );
        assert!(
            !tasks.retain("codex", "session-2", |_| async {}),
            "shutdown must fence later admission"
        );

        release.notify_one();
        shutdown
            .await
            .expect("shutdown task remains joinable")
            .expect("retained hook tasks shut down cleanly");
    }

    #[tokio::test]
    async fn shutdown_joins_superseded_task() {
        let tasks = Arc::new(RetainedHookTasks::new());
        let first_started = Arc::new(Notify::new());
        let first_cancelled = Arc::new(Notify::new());
        let first_release = Arc::new(Notify::new());
        assert!(tasks.retain("codex", "session-1", {
            let started = Arc::clone(&first_started);
            let cancelled = Arc::clone(&first_cancelled);
            let release = Arc::clone(&first_release);
            move |cancellation| async move {
                started.notify_one();
                while !cancellation.is_cancelled() {
                    tokio::task::yield_now().await;
                }
                cancelled.notify_one();
                release.notified().await;
            }
        }));
        first_started.notified().await;
        assert!(tasks.retain("codex", "session-1", |_| async {}));
        first_cancelled.notified().await;

        let shutdown = tokio::spawn({
            let tasks = Arc::clone(&tasks);
            async move { tasks.shutdown().await }
        });
        tokio::task::yield_now().await;
        assert!(
            !shutdown.is_finished(),
            "shutdown must retain and join the cancelled predecessor"
        );

        first_release.notify_one();
        shutdown
            .await
            .expect("shutdown task remains joinable")
            .expect("retained hook tasks shut down cleanly");
    }

    #[tokio::test]
    async fn retiring_one_task_cancels_and_joins_only_that_key() {
        let tasks = Arc::new(RetainedHookTasks::new());
        let first_started = Arc::new(Notify::new());
        let first_cancelled = Arc::new(Notify::new());
        let first_release = Arc::new(Notify::new());
        let second_cancelled = Arc::new(AtomicBool::new(false));
        assert!(tasks.retain("memory-graph", "project-1", {
            let started = Arc::clone(&first_started);
            let cancelled = Arc::clone(&first_cancelled);
            let release = Arc::clone(&first_release);
            move |cancellation| async move {
                started.notify_one();
                while !cancellation.is_cancelled() {
                    tokio::task::yield_now().await;
                }
                cancelled.notify_one();
                release.notified().await;
            }
        }));
        assert!(tasks.retain("memory-graph", "project-2", {
            let second_cancelled = Arc::clone(&second_cancelled);
            move |cancellation| async move {
                while !cancellation.is_cancelled() {
                    tokio::task::yield_now().await;
                }
                second_cancelled.store(true, Ordering::Release);
            }
        }));
        first_started.notified().await;

        let retire = tokio::spawn({
            let tasks = Arc::clone(&tasks);
            async move { tasks.retire("memory-graph", "project-1").await }
        });
        first_cancelled.notified().await;
        assert!(!retire.is_finished(), "retirement must join its task");
        assert!(!second_cancelled.load(Ordering::Acquire));

        first_release.notify_one();
        retire
            .await
            .expect("retirement task remains joinable")
            .expect("one retained task retires cleanly");
        assert!(tasks.retain("memory-graph", "project-3", |_| async {}));
        assert!(!second_cancelled.load(Ordering::Acquire));

        tasks.begin_shutdown();
        tasks.shutdown().await.expect("remaining tasks shut down");
        assert!(second_cancelled.load(Ordering::Acquire));
    }
}
