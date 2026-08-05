use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{BoundedBackfillInterruption, BoundedGitControl};

const MAX_NATIVE_HISTORY_BLOCKING_TASKS: usize = 4;
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(10);

static NATIVE_HISTORY_BLOCKING_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

pub(super) async fn run<T, F>(
    control: &BoundedGitControl,
    task: F,
) -> Result<T, BoundedBackfillInterruption>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, BoundedBackfillInterruption> + Send + 'static,
{
    let permits = NATIVE_HISTORY_BLOCKING_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_NATIVE_HISTORY_BLOCKING_TASKS)))
        .clone();
    run_with_semaphore(permits, control, task).await
}

async fn run_with_semaphore<T, F>(
    permits: Arc<Semaphore>,
    control: &BoundedGitControl,
    task: F,
) -> Result<T, BoundedBackfillInterruption>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, BoundedBackfillInterruption> + Send + 'static,
{
    let permit = acquire_permit(permits, control).await?;
    control.check()?;
    let join = tokio::task::spawn_blocking(move || {
        // A cancelled caller drops the join handle but cannot cancel blocking
        // work, so capacity stays charged until the detached closure exits.
        let _permit = permit;
        task()
    });
    tokio::pin!(join);
    loop {
        control.check()?;
        tokio::select! {
            biased;
            result = &mut join => {
                control.check()?;
                return result
                    .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
            }
            () = tokio::time::sleep(CONTROL_POLL_INTERVAL) => {}
        }
    }
}

async fn acquire_permit(
    permits: Arc<Semaphore>,
    control: &BoundedGitControl,
) -> Result<OwnedSemaphorePermit, BoundedBackfillInterruption> {
    let acquire = permits.acquire_owned();
    tokio::pin!(acquire);
    loop {
        control.check()?;
        tokio::select! {
            biased;
            permit = &mut acquire => {
                return permit.map_err(|_| BoundedBackfillInterruption::SourceUnavailable);
            }
            () = tokio::time::sleep(CONTROL_POLL_INTERVAL) => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
