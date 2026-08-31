//! Bounded off-thread source-scan worker used by content and structural search.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use tokio::sync::Semaphore;
use tracedecay_domain::errors::{Result, TraceDecayError};

const SEARCH_SCAN_CEILING: Duration = Duration::from_secs(10);
static SEARCH_SCAN_SEMAPHORE: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(2)));

pub(crate) struct CancelSearchOnDrop(Arc<AtomicBool>);

impl CancelSearchOnDrop {
    #[cfg(test)]
    pub(crate) fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self(cancelled)
    }
}

impl Drop for CancelSearchOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub async fn run_bounded_search<T, E, F>(
    tool_name: &'static str,
    query: String,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
    worker: F,
) -> Result<T>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    F: FnOnce(
            Arc<AtomicBool>,
            Option<tracedecay_application::CancellationSignal>,
        ) -> std::result::Result<T, E>
        + Send
        + 'static,
{
    let budget = search_budget(tool_name, deadline.as_ref())?;
    run_bounded_search_with_capacity(
        Arc::clone(&SEARCH_SCAN_SEMAPHORE),
        budget,
        tool_name,
        query,
        cancellation,
        worker,
    )
    .await
}

#[hotpath::measure(future = true, label = "mcp.search.bounded")]
async fn run_bounded_search_with_capacity<T, E, F>(
    capacity: Arc<Semaphore>,
    budget: Duration,
    tool_name: &'static str,
    query: String,
    cancellation: Option<tracedecay_application::CancellationSignal>,
    worker: F,
) -> Result<T>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    F: FnOnce(
            Arc<AtomicBool>,
            Option<tracedecay_application::CancellationSignal>,
        ) -> std::result::Result<T, E>
        + Send
        + 'static,
{
    if cancellation
        .as_ref()
        .is_some_and(tracedecay_application::CancellationSignal::is_cancelled)
    {
        return Err(search_cancelled_error(tool_name));
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = CancelSearchOnDrop(Arc::clone(&cancelled));
    let worker_cancellation = cancellation.clone();
    let outcome = tokio::time::timeout(budget, async move {
        let permit = capacity
            .acquire_owned()
            .await
            .map_err(|error| TraceDecayError::Search {
                message: format!("{tool_name} concurrency gate closed: {error}"),
                query: query.clone(),
            })?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            worker(cancelled, worker_cancellation)
        })
        .await
        .map_err(|error| TraceDecayError::Search {
            message: format!("{tool_name} worker failed: {error}"),
            query,
        })?
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })
    })
    .await;
    drop(cancel_on_drop);

    match outcome {
        Ok(result) => {
            if cancellation
                .as_ref()
                .is_some_and(tracedecay_application::CancellationSignal::is_cancelled)
            {
                Err(search_cancelled_error(tool_name))
            } else {
                result
            }
        }
        Err(_) => Err(TraceDecayError::project_route(
            "source_search_deadline_exceeded",
            true,
            format!(
                "{tool_name} exceeded its {}s source-scan deadline; narrow the request with path_glob",
                budget.as_secs_f64()
            ),
        )),
    }
}

fn search_budget(
    tool_name: &str,
    deadline: Option<&tracedecay_application::Deadline>,
) -> Result<Duration> {
    match deadline {
        Some(deadline) => tracedecay_daemon_protocol::deadline_remaining(deadline)
            .map(|remaining| remaining.min(SEARCH_SCAN_CEILING))
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                TraceDecayError::project_route(
                    "source_search_deadline_exceeded",
                    true,
                    format!("{tool_name} request deadline elapsed before source scanning"),
                )
            }),
        None => Ok(SEARCH_SCAN_CEILING),
    }
}

fn search_cancelled_error(tool_name: &str) -> TraceDecayError {
    TraceDecayError::project_route(
        "source_search_cancelled",
        true,
        format!("{tool_name} was cancelled during source scanning"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timeout_includes_waiting_for_the_worker_permit() {
        let capacity = Arc::new(Semaphore::new(1));
        let held = Arc::clone(&capacity).acquire_owned().await.unwrap();
        let worker_started = Arc::new(AtomicBool::new(false));
        let worker_observer = Arc::clone(&worker_started);

        let error = run_bounded_search_with_capacity(
            capacity,
            Duration::from_millis(20),
            "test_search",
            "needle".to_owned(),
            None,
            move |_, _| -> std::result::Result<(), String> {
                worker_observer.store(true, Ordering::Release);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        drop(held);

        assert!(!worker_started.load(Ordering::Acquire));
        assert_eq!(
            error.project_route_context().map(|problem| problem.0),
            Some("source_search_deadline_exceeded")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_the_waiter_cancels_the_blocking_worker() {
        let capacity = Arc::new(Semaphore::new(1));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let task = tokio::spawn(run_bounded_search_with_capacity(
            capacity,
            Duration::from_secs(5),
            "test_search",
            "needle".to_owned(),
            None,
            move |cancelled, _| -> std::result::Result<(), String> {
                started_tx.send(()).unwrap();
                while !cancelled.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                cancelled_tx.send(()).unwrap();
                Ok(())
            },
        ));

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        task.abort();
        cancelled_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}
