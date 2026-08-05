use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Semaphore;
use tracedecay_application::{CancellationContext, Deadline};
use tracedecay_runtime_core::errors::Result;

use crate::tracedecay::TraceDecay;

const SOURCE_SEARCH_CEILING: Duration = Duration::from_secs(10);
static SOURCE_SEARCH_CAPACITY: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(2)));

pub(crate) enum BoundedSourceSearch<T, E> {
    Completed(std::result::Result<T, E>),
    Cancelled,
    TimedOut,
    WorkerFailed,
}

struct CancelSourceSearchOnDrop(Arc<AtomicBool>);

impl Drop for CancelSourceSearchOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(crate) async fn run_bounded_source_search<T, E, F>(
    deadline: &Deadline,
    cancellation: &CancellationContext,
    worker: F,
) -> BoundedSourceSearch<T, E>
where
    T: Send + 'static,
    E: Send + 'static,
    F: FnOnce(Arc<AtomicBool>) -> std::result::Result<T, E> + Send + 'static,
{
    if cancellation.is_cancelled() {
        return BoundedSourceSearch::Cancelled;
    }
    let Some(budget) = remaining_budget(deadline) else {
        return BoundedSourceSearch::TimedOut;
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = CancelSourceSearchOnDrop(Arc::clone(&cancelled));
    let capacity = Arc::clone(&SOURCE_SEARCH_CAPACITY);
    let outcome = tokio::time::timeout(budget, async move {
        let Ok(permit) = capacity.acquire_owned().await else {
            return BoundedSourceSearch::WorkerFailed;
        };
        match tokio::task::spawn_blocking(move || {
            let _permit = permit;
            worker(cancelled)
        })
        .await
        {
            Ok(result) => BoundedSourceSearch::Completed(result),
            Err(_) => BoundedSourceSearch::WorkerFailed,
        }
    })
    .await;
    drop(cancel_on_drop);
    outcome.unwrap_or(BoundedSourceSearch::TimedOut)
}

fn remaining_budget(deadline: &Deadline) -> Option<Duration> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_micros()
        .min(i64::MAX as u128) as i64;
    let remaining = deadline.expires_at.0.checked_sub(now)?;
    if remaining <= 0 {
        return None;
    }
    Some(Duration::from_micros(remaining as u64).min(SOURCE_SEARCH_CEILING))
}

#[cfg(test)]
mod bounded_source_search_tests {
    use super::*;
    use tracedecay_domain::UtcMicros;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn elapsed_deadline_cancels_the_off_thread_worker() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;
        let deadline = Deadline::new(UtcMicros(now + 20_000)).unwrap();
        let cancellation = CancellationContext::active("cancel.grep-deadline-test").unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();

        let outcome = run_bounded_source_search(&deadline, &cancellation, move |cancelled| {
            started_tx.send(()).unwrap();
            while !cancelled.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            cancelled_tx.send(()).unwrap();
            Ok::<_, String>(())
        })
        .await;

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(outcome, BoundedSourceSearch::TimedOut));
        cancelled_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RankedAffectedTest {
    pub(crate) path: String,
    pub(crate) distance: usize,
}

pub(crate) struct AffectedTestTraversal {
    pub(crate) test_distances: HashMap<String, usize>,
}

pub(crate) fn rank_affected_tests(
    test_distances: &HashMap<String, usize>,
) -> Vec<RankedAffectedTest> {
    let mut ranked = test_distances
        .iter()
        .map(|(path, distance)| RankedAffectedTest {
            path: path.clone(),
            distance: *distance,
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.distance
            .cmp(&right.distance)
            .then_with(|| left.path.cmp(&right.path))
    });
    ranked
}

pub(crate) const fn affected_test_proximity(distance: usize) -> &'static str {
    match distance {
        0 => "changed",
        1 => "direct",
        2 => "near",
        _ => "transitive",
    }
}

pub(crate) async fn collect_affected_test_files(
    graph: &TraceDecay,
    files: &[String],
    max_depth: usize,
    custom_glob: Option<&glob::Pattern>,
    files_with_inline_tests: &HashSet<String>,
) -> Result<AffectedTestTraversal> {
    let is_test = |path: &str| {
        custom_glob.map_or_else(
            || tracedecay_code_index::is_test_file(path) || files_with_inline_tests.contains(path),
            |pattern| pattern.matches(path),
        )
    };
    let mut test_distances = HashMap::new();
    let mut visited = HashSet::new();
    let mut frontier = Vec::new();
    for file in files {
        if is_test(file) {
            test_distances.insert(file.clone(), 0);
        }
        if visited.insert(file.clone()) {
            frontier.push(file.clone());
        }
    }
    frontier.sort();
    for depth in 0..max_depth {
        if frontier.is_empty() {
            break;
        }
        let mut dependents = Vec::new();
        for file in &frontier {
            dependents.extend(graph.get_file_dependents(file).await?);
        }
        dependents.sort();
        dependents.dedup();
        let mut next = Vec::new();
        for dependent in dependents {
            if !visited.insert(dependent.clone()) {
                continue;
            }
            if is_test(&dependent) {
                test_distances.insert(dependent, depth + 1);
            } else {
                next.push(dependent);
            }
        }
        frontier = next;
    }
    Ok(AffectedTestTraversal { test_distances })
}
