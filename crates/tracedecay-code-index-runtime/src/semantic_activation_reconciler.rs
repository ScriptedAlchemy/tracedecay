use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use tracedecay_semantic_contracts::SemanticLifecycleVerifiedReadyEventV1;
use tracedecay_usecases::semantic_runtime::{
    ProductionSemanticActivationCoordinatorV1, SemanticActivationCoordinationErrorV1,
};

const REOBSERVATION_UNIT_DEADLINE: Duration = Duration::from_secs(15);
const REOBSERVATION_INITIAL_BACKOFF: Duration = Duration::from_millis(50);
const REOBSERVATION_MAX_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReobservationFailureDispositionV1 {
    Retry,
    Refuse,
}

fn classify_reobservation_failure(
    error: &SemanticActivationCoordinationErrorV1,
) -> ReobservationFailureDispositionV1 {
    match error {
        SemanticActivationCoordinationErrorV1::Rejected
        | SemanticActivationCoordinationErrorV1::RejectedDetail(_) => {
            ReobservationFailureDispositionV1::Refuse
        }
        SemanticActivationCoordinationErrorV1::Unavailable
        | SemanticActivationCoordinationErrorV1::Runtime(_)
        | SemanticActivationCoordinationErrorV1::Conflict => {
            ReobservationFailureDispositionV1::Retry
        }
    }
}

fn should_reconcile_ready_event(
    handled_epoch: Option<u64>,
    event: &SemanticLifecycleVerifiedReadyEventV1,
) -> bool {
    event.artifact_digest.is_some() && handled_epoch.is_none_or(|handled| event.epoch > handled)
}

fn should_reconcile(
    handled_epoch: Option<u64>,
    event: &SemanticLifecycleVerifiedReadyEventV1,
    committed_activation_changed: bool,
) -> bool {
    committed_activation_changed || should_reconcile_ready_event(handled_epoch, event)
}

/// One cancellable recovery owner for one mounted project.
///
/// Verified model-lifecycle events are only wakes. Every attempt rereads the
/// canonical committed configuration tuple, and the existing registrar fences
/// publication by its exact epoch, revision, and transition digest.
pub struct DaemonSemanticActivationReconcilerV1 {
    cancellation: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl DaemonSemanticActivationReconcilerV1 {
    pub fn spawn(
        coordinator: Arc<ProductionSemanticActivationCoordinatorV1>,
        mut lifecycle_events: tokio::sync::watch::Receiver<SemanticLifecycleVerifiedReadyEventV1>,
        committed_activation_wake: Arc<Notify>,
    ) -> Self {
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let worker_committed_activation_wake = Arc::clone(&committed_activation_wake);
        let reconciler_loop = async move {
            let mut handled_epoch = None;
            let mut committed_activation_changed = false;
            loop {
                let event = lifecycle_events.borrow_and_update().clone();
                if should_reconcile(handled_epoch, &event, committed_activation_changed) {
                    if should_reconcile_ready_event(handled_epoch, &event) {
                        handled_epoch = Some(event.epoch);
                    }
                    committed_activation_changed = false;
                    let mut backoff = REOBSERVATION_INITIAL_BACKOFF;
                    loop {
                        // One static label times each bounded reobservation
                        // attempt (the deadline-timeout drop included), beside
                        // the whole-loop lifetime span on the spawned task.
                        let observed = tokio::select! {
                            () = worker_cancellation.cancelled() => return,
                            observed = tokio::time::timeout(
                                REOBSERVATION_UNIT_DEADLINE,
                                hotpath::future!(
                                    coordinator.reobserve_current_activation(),
                                    label = "daemon.semantic.activation_reconciler.reobserve"
                                ),
                            ) => observed,
                        };
                        match observed {
                            Ok(Ok(Some(_) | None)) => {
                                hotpath::gauge!(
                                    "daemon.semantic.activation_reconciler.reobserve.settled_total"
                                )
                                .inc(1_u64);
                                break;
                            }
                            Ok(Err(error)) => match classify_reobservation_failure(&error) {
                                ReobservationFailureDispositionV1::Refuse => {
                                    hotpath::gauge!(
                                        "daemon.semantic.activation_reconciler.reobserve.refused_total"
                                    )
                                    .inc(1_u64);
                                    tracing::warn!(
                                        event = "semantic_activation_reobserve",
                                        outcome = "refused",
                                        error = %error,
                                        "committed semantic activation could not be observed by the runtime"
                                    );
                                    break;
                                }
                                ReobservationFailureDispositionV1::Retry => {
                                    hotpath::gauge!(
                                        "daemon.semantic.activation_reconciler.reobserve.retried_total"
                                    )
                                    .inc(1_u64);
                                    tracing::warn!(
                                        event = "semantic_activation_reobserve",
                                        outcome = "retry",
                                        error = %error,
                                        backoff_ms = backoff.as_millis() as u64,
                                        "committed semantic activation observation failed; retrying"
                                    );
                                }
                            },
                            Err(_) => {
                                hotpath::gauge!(
                                    "daemon.semantic.activation_reconciler.reobserve.retried_total"
                                )
                                .inc(1_u64);
                                tracing::warn!(
                                    event = "semantic_activation_reobserve",
                                    outcome = "timed_out",
                                    deadline_ms = REOBSERVATION_UNIT_DEADLINE.as_millis() as u64,
                                    "committed semantic activation observation exceeded its deadline; retrying"
                                );
                            }
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
                enum WakeV1 {
                    Lifecycle(Result<(), tokio::sync::watch::error::RecvError>),
                    CommittedActivation,
                }
                let changed = tokio::select! {
                    () = worker_cancellation.cancelled() => return,
                    changed = lifecycle_events.changed() => WakeV1::Lifecycle(changed),
                    () = worker_committed_activation_wake.notified() => WakeV1::CommittedActivation,
                };
                match changed {
                    WakeV1::Lifecycle(Ok(())) => {}
                    WakeV1::Lifecycle(Err(_)) => return,
                    WakeV1::CommittedActivation => committed_activation_changed = true,
                }
            }
        };
        let task = tokio::spawn(hotpath::future!(
            reconciler_loop,
            label = "daemon.semantic.activation_reconciler"
        ));
        Self {
            cancellation,
            task: Mutex::new(Some(task)),
        }
    }

    pub async fn cancel_and_join(&self) {
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

    #[tokio::test]
    async fn committed_activation_before_reconciler_subscription_is_retained() {
        let current = SemanticLifecycleVerifiedReadyEventV1 {
            epoch: 7,
            artifact_digest: Some(format!("sha256:{}", "a".repeat(64))),
        };
        assert!(!should_reconcile(Some(7), &current, false));
        assert!(should_reconcile(Some(7), &current, true));

        let registered_configuration_wake = Arc::new(tokio::sync::Notify::new());
        registered_configuration_wake.notify_one();
        let reconciler_wake = Arc::clone(&registered_configuration_wake);

        tokio::time::timeout(Duration::from_millis(100), reconciler_wake.notified())
            .await
            .expect("a pre-install activation must wake reconciliation at the same ready epoch");
    }

    #[test]
    fn coordination_conflict_retries_canonical_reobservation() {
        assert_eq!(
            classify_reobservation_failure(&SemanticActivationCoordinationErrorV1::Conflict),
            ReobservationFailureDispositionV1::Retry
        );
        assert_eq!(
            classify_reobservation_failure(&SemanticActivationCoordinationErrorV1::Rejected),
            ReobservationFailureDispositionV1::Refuse
        );
    }
}
