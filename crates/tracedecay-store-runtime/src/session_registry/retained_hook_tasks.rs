use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};

use tracedecay_runtime_core::native_runtime::{NativeTask, NativeTaskOutcome};
use tracedecay_sessions::observation::ObservationCancellation;

struct RetainedHookTask {
    key: String,
    generation: u64,
    cancellation: ObservationCancellation,
    join: Arc<NativeTask<()>>,
}

#[derive(Default)]
struct RetainedHookTaskState {
    accepting: bool,
    next_generation: u64,
    tasks: BTreeMap<String, RetainedHookTask>,
    retiring: Vec<RetainedHookTask>,
    join_failures: BTreeMap<String, Vec<String>>,
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
            let Ok(join) = NativeTask::spawn(async move {
                operation(task_cancellation).await;
                finish_retained_hook_task(weak_state, &task_key, generation);
            }) else {
                return false;
            };
            let previous = state.tasks.insert(
                key.clone(),
                RetainedHookTask {
                    key,
                    generation,
                    cancellation,
                    join: Arc::new(join),
                },
            );
            if let Some(previous) = previous {
                previous.cancellation.cancel();
                state.retiring.push(previous);
            }
            state.reap_finished();
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
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "retained hook task state lock is poisoned".to_owned())?;
            if let Some(task) = state.tasks.remove(&key) {
                state.retiring.push(task);
            }
            for task in &state.retiring {
                if task.key == key {
                    task.cancellation.cancel();
                }
            }
        }
        self.join_retiring(Some(&key)).await
    }

    pub(super) async fn shutdown(&self) -> Result<(), String> {
        self.begin_shutdown();
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "retained hook task state lock is poisoned".to_owned())?;
            state.accepting = false;
            let tasks = std::mem::take(&mut state.tasks);
            state.retiring.extend(tasks.into_values());
            for task in &state.retiring {
                task.cancellation.cancel();
            }
        }
        self.join_retiring(None).await
    }

    async fn join_retiring(&self, key: Option<&str>) -> Result<(), String> {
        let joins = {
            let state = self
                .state
                .lock()
                .map_err(|_| "retained hook task state lock is poisoned".to_owned())?;
            state
                .retiring
                .iter()
                .filter(|task| key.is_none_or(|key| task.key == key))
                .map(|task| (task.generation, Arc::clone(&task.join)))
                .collect::<Vec<_>>()
        };
        for (generation, join) in joins {
            let result = join_result(join.wait().await);
            let mut state = self
                .state
                .lock()
                .map_err(|_| "retained hook task state lock is poisoned".to_owned())?;
            if let Some(index) = state
                .retiring
                .iter()
                .position(|task| task.generation == generation)
            {
                state.finish_join(index, result);
            }
        }
        let state = self
            .state
            .lock()
            .map_err(|_| "retained hook task state lock is poisoned".to_owned())?;
        let failures = state
            .join_failures
            .iter()
            .filter(|(task_key, _)| key.is_none_or(|key| task_key.as_str() == key))
            .flat_map(|(_, errors)| errors.iter().cloned())
            .collect::<Vec<_>>();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

/// A retained task that was aborted or dropped by its runtime stopped
/// cleanly; only a panic fails its join.
pub(super) fn join_result(outcome: NativeTaskOutcome<()>) -> Result<(), String> {
    match outcome {
        NativeTaskOutcome::Completed(()) | NativeTaskOutcome::Cancelled => Ok(()),
        NativeTaskOutcome::Panicked(payload) => Err(format!(
            "retained hook task join failed: task panicked: {payload}"
        )),
    }
}

impl RetainedHookTaskState {
    fn finish_join(&mut self, index: usize, result: Result<(), String>) {
        let task = self.retiring.swap_remove(index);
        if let Err(error) = result {
            self.join_failures.entry(task.key).or_default().push(error);
        }
    }

    fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.retiring.len() {
            if let Some(result) = self.retiring[index].join.try_finished().map(join_result) {
                self.finish_join(index, result);
            } else {
                index += 1;
            }
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
            task.join.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use asupersync::runtime::yield_now;
    use std::future::poll_fn;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Poll;
    use tokio::sync::Notify;

    async fn assert_pending<F: Future + Unpin>(future: &mut F, message: &str) {
        poll_fn(|cx| {
            assert!(Pin::new(&mut *future).poll(cx).is_pending(), "{message}");
            Poll::Ready(())
        })
        .await;
    }

    #[asupersync::test]
    async fn shutdown_fences_new_tasks_and_joins_active_task() {
        let tasks = RetainedHookTasks::new();
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
                    yield_now().await;
                }
                cancelled.notify_one();
                release.notified().await;
            }
        }));
        started.notified().await;

        let mut shutdown = Box::pin(tasks.shutdown());
        assert_pending(&mut shutdown, "shutdown must join the active task").await;
        cancelled.notified().await;
        assert_pending(&mut shutdown, "shutdown must join the active task").await;
        assert!(
            !tasks.retain("codex", "session-2", |_| async {}),
            "shutdown must fence later admission"
        );

        drop(shutdown);
        let mut retry = Box::pin(tasks.shutdown());
        // Poll the canonical task-owner drain itself: there is no later
        // blocking lifecycle cleanup that could make a lost-handle retry
        // appear pending. The acknowledged task cannot finish until release.
        assert_pending(
            &mut retry,
            "cancelled shutdown must retain the acknowledged live task for retry",
        )
        .await;
        release.notify_one();
        retry
            .await
            .expect("retried task-owner drain joins the released task");
    }

    #[test]
    fn retained_tasks_run_without_a_caller_runtime() {
        let tasks = RetainedHookTasks::new();
        let (sender, receiver) = std::sync::mpsc::channel();
        assert!(
            tasks.retain("codex", "session-1", move |_| async move {
                let _ = sender.send(std::thread::current().name().map(str::to_owned));
            }),
            "a caller outside any runtime must still admit retained work"
        );
        let thread = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("retained task ran");
        assert!(
            thread
                .as_deref()
                .is_some_and(|name| name.starts_with("tracedecay-native")),
            "retained task must run on the native process runtime, ran on {thread:?}"
        );
    }

    #[asupersync::test]
    async fn targeted_retirement_preserves_failure_without_failing_other_keys() {
        let tasks = RetainedHookTasks::new();
        assert!(tasks.retain("memory-graph", "failed", |_| async {
            panic!("retained task failure");
        }));
        let error = tasks
            .retire("memory-graph", "failed")
            .await
            .expect_err("task panic must fail retirement");
        assert!(error.contains("retained hook task join failed"));
        assert_eq!(
            tasks.retire("memory-graph", "failed").await.unwrap_err(),
            error
        );
        assert!(tasks.retain("memory-graph", "healthy", |_| async {}));
        tasks
            .retire("memory-graph", "healthy")
            .await
            .expect("other key remains healthy");
        assert_eq!(tasks.shutdown().await.unwrap_err(), error);
    }

    #[asupersync::test]
    async fn shutdown_retry_preserves_failed_join() {
        let tasks = RetainedHookTasks::new();
        assert!(tasks.retain("codex", "failed-session", |_| async {
            panic!("retained task failure");
        }));
        let error = tasks
            .shutdown()
            .await
            .expect_err("task panic must fail drain");
        assert!(error.contains("retained hook task join failed"));
        assert_eq!(
            tasks
                .shutdown()
                .await
                .expect_err("retry must retain failure"),
            error,
        );
    }

    #[asupersync::test]
    async fn shutdown_joins_superseded_task() {
        let tasks = RetainedHookTasks::new();
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
                    yield_now().await;
                }
                cancelled.notify_one();
                release.notified().await;
            }
        }));
        first_started.notified().await;
        assert!(tasks.retain("codex", "session-1", |_| async {}));
        first_cancelled.notified().await;

        let mut shutdown = Box::pin(tasks.shutdown());
        assert_pending(
            &mut shutdown,
            "shutdown must retain and join the cancelled predecessor",
        )
        .await;

        first_release.notify_one();
        shutdown
            .await
            .expect("retained hook tasks shut down cleanly");
    }

    #[asupersync::test]
    async fn retiring_one_task_cancels_and_joins_only_that_key() {
        let tasks = RetainedHookTasks::new();
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
                    yield_now().await;
                }
                cancelled.notify_one();
                release.notified().await;
            }
        }));
        assert!(tasks.retain("memory-graph", "project-2", {
            let second_cancelled = Arc::clone(&second_cancelled);
            move |cancellation| async move {
                while !cancellation.is_cancelled() {
                    yield_now().await;
                }
                second_cancelled.store(true, Ordering::Release);
            }
        }));
        first_started.notified().await;

        let mut retire = Box::pin(tasks.retire("memory-graph", "project-1"));
        assert_pending(&mut retire, "retirement must join its task").await;
        first_cancelled.notified().await;
        assert_pending(&mut retire, "retirement must join its task").await;
        assert!(!second_cancelled.load(Ordering::Acquire));

        drop(retire);
        assert!(tasks.retain("memory-graph", "project-3", |_| async {}));
        let mut retry = Box::pin(tasks.retire("memory-graph", "project-1"));
        assert_pending(
            &mut retry,
            "retry must retain the acknowledged task until release",
        )
        .await;
        assert!(!second_cancelled.load(Ordering::Acquire));
        asupersync::time::timeout(
            asupersync::time::wall_now(),
            std::time::Duration::from_secs(2),
            tasks.retire("memory-graph", "project-2"),
        )
        .await
        .expect("independent B retirement must complete while A remains held")
        .expect("independent B retirement succeeds");
        assert!(second_cancelled.load(Ordering::Acquire));
        assert_pending(&mut retry, "A remains held after B retirement completes").await;
        first_release.notify_one();
        retry
            .await
            .expect("retried exact retirement joins released task");
        assert!(second_cancelled.load(Ordering::Acquire));

        tasks.begin_shutdown();
        tasks.shutdown().await.expect("remaining tasks shut down");
        assert!(second_cancelled.load(Ordering::Acquire));
    }
}
