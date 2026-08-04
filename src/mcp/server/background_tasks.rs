use std::future::Future;
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct McpBackgroundTaskOwner {
    state: Mutex<McpBackgroundTaskState>,
}

#[derive(Default)]
struct McpBackgroundTaskState {
    closed: bool,
    tasks: tokio::task::JoinSet<()>,
}

impl McpBackgroundTaskOwner {
    pub(super) fn spawn<Task>(&self, task: Task) -> bool
    where
        Task: Future<Output = ()> + Send + 'static,
    {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return false;
        }
        state.tasks.spawn(task);
        true
    }

    pub(super) async fn shutdown(&self) -> Vec<String> {
        let mut tasks = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            std::mem::take(&mut state.tasks)
        };
        tasks.abort_all();
        let mut failures = Vec::new();
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                failures.push(error.to_string());
            }
        }
        failures
    }
}
