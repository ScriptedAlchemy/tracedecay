use std::collections::BTreeMap;
use std::future::Future;
#[cfg(test)]
use std::future::poll_fn;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};

use tracedecay_sessions::observation::ObservationCancellation;

struct RetainedHookTask {
    key: String,
    generation: u64,
    cancellation: ObservationCancellation,
    join: Arc<RetainedHookTaskJoin>,
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
            let previous = state.tasks.insert(
                key.clone(),
                RetainedHookTask {
                    key,
                    generation,
                    cancellation,
                    join: Arc::new(RetainedHookTaskJoin::new(task)),
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

    #[hotpath::skip]
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

    #[hotpath::skip]
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
            let result = join.wait().await;
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

// One retained join state per existing task. Concurrent drains share its
// terminal result; cancelling a waiter never moves out or detaches the handle.
pub(super) struct RetainedHookTaskJoin {
    state: tokio::sync::Mutex<RetainedHookTaskJoinState>,
    abort: tokio::task::AbortHandle,
}

enum RetainedHookTaskJoinState {
    Running(tokio::task::JoinHandle<()>),
    Finished(Result<(), String>),
}

impl RetainedHookTaskJoin {
    pub(super) fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self {
            abort: handle.abort_handle(),
            state: tokio::sync::Mutex::new(RetainedHookTaskJoinState::Running(handle)),
        }
    }

    pub(super) async fn wait(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        match &mut *state {
            RetainedHookTaskJoinState::Finished(result) => result.clone(),
            RetainedHookTaskJoinState::Running(handle) => {
                let result = Self::outcome(handle.await);
                *state = RetainedHookTaskJoinState::Finished(result.clone());
                result
            }
        }
    }

    pub(super) fn abort(&self) {
        self.abort.abort();
    }

    fn try_finished(&self) -> Option<Result<(), String>> {
        if !self.abort.is_finished() {
            return None;
        }
        let mut state = self.state.try_lock().ok()?;
        if let RetainedHookTaskJoinState::Running(handle) = &mut *state {
            let mut cx = Context::from_waker(Waker::noop());
            let Poll::Ready(result) = Pin::new(handle).poll(&mut cx) else {
                return None;
            };
            *state = RetainedHookTaskJoinState::Finished(Self::outcome(result));
        }
        match &*state {
            RetainedHookTaskJoinState::Finished(result) => Some(result.clone()),
            RetainedHookTaskJoinState::Running(_) => None,
        }
    }

    fn outcome(result: Result<(), tokio::task::JoinError>) -> Result<(), String> {
        match result {
            Ok(()) => Ok(()),
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(format!("retained hook task join failed: {error}")),
        }
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
            if let Some(result) = self.retiring[index].join.try_finished() {
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

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

        shutdown.abort();
        assert!(
            shutdown
                .await
                .expect_err("first drain is cancelled")
                .is_cancelled()
        );
        let mut retry = Box::pin(tasks.shutdown());
        // Poll the canonical task-owner drain itself: there is no later
        // blocking lifecycle cleanup that could make a lost-handle retry
        // appear pending. The acknowledged task cannot finish until release.
        poll_fn(|cx| {
            assert!(
                retry.as_mut().poll(cx).is_pending(),
                "cancelled shutdown must retain the acknowledged live task for retry"
            );
            Poll::Ready(())
        })
        .await;
        release.notify_one();
        retry
            .await
            .expect("retried task-owner drain joins the released task");
    }

    #[tokio::test]
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

    #[tokio::test]
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

        retire.abort();
        assert!(
            retire
                .await
                .expect_err("first retirement cancelled")
                .is_cancelled()
        );
        assert!(tasks.retain("memory-graph", "project-3", |_| async {}));
        let mut retry = Box::pin(tasks.retire("memory-graph", "project-1"));
        poll_fn(|cx| {
            assert!(
                retry.as_mut().poll(cx).is_pending(),
                "retry must retain the acknowledged task until release"
            );
            Poll::Ready(())
        })
        .await;
        assert!(!second_cancelled.load(Ordering::Acquire));
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tasks.retire("memory-graph", "project-2"),
        )
        .await
        .expect("independent B retirement must complete while A remains held")
        .expect("independent B retirement succeeds");
        assert!(second_cancelled.load(Ordering::Acquire));
        poll_fn(|cx| {
            assert!(
                retry.as_mut().poll(cx).is_pending(),
                "A remains held after B retirement completes"
            );
            Poll::Ready(())
        })
        .await;
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
