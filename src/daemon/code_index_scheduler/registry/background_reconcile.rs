use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::super::{
    CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1,
    LatestCompleteCodeIndexV1, cancelled_code_index_reconcile,
};

pub(super) enum BackgroundCodeIndexReconcileV1 {
    Completed {
        outcome: Result<CodeIndexReconcileOutcomeV1, CodeIndexSchedulerErrorV1>,
        latest: Option<LatestCompleteCodeIndexV1>,
    },
    SchedulerBusy,
}

pub(super) fn reconcile(
    scheduler: Arc<Mutex<CodeIndexWorktreeSchedulerV1>>,
    shutting_down: Arc<AtomicBool>,
) -> BackgroundCodeIndexReconcileV1 {
    let mut scheduler = match scheduler.try_lock() {
        Ok(scheduler) => scheduler,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            return BackgroundCodeIndexReconcileV1::SchedulerBusy;
        }
    };
    let outcome = scheduler.reconcile_now();
    let latest = match scheduler.try_latest_complete() {
        Ok(latest) => latest,
        Err(error) => {
            return BackgroundCodeIndexReconcileV1::Completed {
                outcome: Err(error),
                latest: None,
            };
        }
    };
    if shutting_down.load(Ordering::Acquire) {
        if let Some(latest) = latest.as_ref() {
            latest.warm_control.cancel();
        }
        return BackgroundCodeIndexReconcileV1::Completed {
            outcome: Err(cancelled_code_index_reconcile()),
            latest: None,
        };
    }
    BackgroundCodeIndexReconcileV1::Completed { outcome, latest }
}

pub(super) fn spawn_bounded_warm(
    admission: Arc<tokio::sync::Semaphore>,
    latest: LatestCompleteCodeIndexV1,
) {
    tokio::spawn(async move {
        let Ok(_admission) = admission.acquire_owned().await else {
            latest.warm_control.cancel();
            return;
        };
        let _ = tokio::task::spawn_blocking(move || latest.warm_serving_caches()).await;
    });
}
