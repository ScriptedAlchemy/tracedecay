use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::semantic_code::SemanticLifecycleVerifiedReadyEventV1;
use tracedecay_usecases::semantic_runtime::{
    ProductionSemanticActivationCoordinatorV1, SemanticActivationCoordinationErrorV1,
};

const REOBSERVATION_UNIT_DEADLINE: Duration = Duration::from_secs(15);
const REOBSERVATION_INITIAL_BACKOFF: Duration = Duration::from_millis(50);
const REOBSERVATION_MAX_BACKOFF: Duration = Duration::from_secs(5);

fn should_reconcile_ready_event(
    handled_epoch: Option<u64>,
    event: &SemanticLifecycleVerifiedReadyEventV1,
) -> bool {
    event.artifact_digest.is_some() && handled_epoch.is_none_or(|handled| event.epoch > handled)
}

/// One cancellable recovery owner for one mounted project.
///
/// Verified model-lifecycle events are only wakes. Every attempt rereads the
/// canonical committed configuration tuple, and the existing registrar fences
/// publication by its exact epoch, revision, and transition digest.
pub(crate) struct DaemonSemanticActivationReconcilerV1 {
    cancellation: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl DaemonSemanticActivationReconcilerV1 {
    pub(crate) fn spawn(
        coordinator: Arc<ProductionSemanticActivationCoordinatorV1>,
        mut lifecycle_events: tokio::sync::watch::Receiver<
            crate::semantic_code::SemanticLifecycleVerifiedReadyEventV1,
        >,
    ) -> Self {
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            let mut handled_epoch = None;
            loop {
                let event = lifecycle_events.borrow_and_update().clone();
                if should_reconcile_ready_event(handled_epoch, &event) {
                    handled_epoch = Some(event.epoch);
                    let mut backoff = REOBSERVATION_INITIAL_BACKOFF;
                    loop {
                        let observed = tokio::select! {
                            () = worker_cancellation.cancelled() => return,
                            observed = tokio::time::timeout(
                                REOBSERVATION_UNIT_DEADLINE,
                                coordinator.reobserve_current_activation(),
                            ) => observed,
                        };
                        match observed {
                            Ok(Ok(Some(_) | None)) => break,
                            Ok(Err(
                                SemanticActivationCoordinationErrorV1::Rejected
                                | SemanticActivationCoordinationErrorV1::RejectedDetail(_)
                                | SemanticActivationCoordinationErrorV1::Conflict,
                            )) => break,
                            Ok(Err(
                                SemanticActivationCoordinationErrorV1::Unavailable
                                | SemanticActivationCoordinationErrorV1::Runtime(_),
                            ))
                            | Err(_) => {}
                        }
                        tokio::select! {
                            () = worker_cancellation.cancelled() => return,
                            () = tokio::time::sleep(backoff) => {}
                        }
                        backoff = backoff.saturating_mul(2).min(REOBSERVATION_MAX_BACKOFF);
                        let latest = lifecycle_events.borrow_and_update().clone();
                        if latest.epoch > handled_epoch.unwrap_or_default() {
                            handled_epoch = Some(latest.epoch);
                            backoff = REOBSERVATION_INITIAL_BACKOFF;
                        }
                    }
                }
                let changed = tokio::select! {
                    () = worker_cancellation.cancelled() => return,
                    changed = lifecycle_events.changed() => changed,
                };
                if changed.is_err() {
                    return;
                }
            }
        });
        Self {
            cancellation,
            task: Mutex::new(Some(task)),
        }
    }

    pub(crate) async fn cancel_and_join(&self) {
        self.cancellation.cancel();
        let task = self
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

impl Drop for DaemonSemanticActivationReconcilerV1 {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self
            .task
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_verified_ready_event_is_not_lost_before_subscription_wait() {
        let current = SemanticLifecycleVerifiedReadyEventV1 {
            epoch: 7,
            artifact_digest: Some(format!("sha256:{}", "a".repeat(64))),
        };

        assert!(should_reconcile_ready_event(None, &current));
        assert!(!should_reconcile_ready_event(Some(7), &current));
        assert!(should_reconcile_ready_event(
            Some(7),
            &SemanticLifecycleVerifiedReadyEventV1 {
                epoch: 8,
                artifact_digest: current.artifact_digest,
            }
        ));
    }
}
