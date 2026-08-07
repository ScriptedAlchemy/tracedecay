//! Retained lifecycle for advisory and Context Scout post-open setup.
//!
//! The deferred gateway is published before provider/model setup begins, so
//! hook admission distinguishes a live warming owner from a terminally
//! unavailable one. Setup runs once under a bounded budget; project-runtime
//! retirement cancels and joins it, and a staged runtime publication either
//! commits or rolls back — never leaking a partial advisory runtime.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::now_micros;

use super::service::invocation::{
    AdvisoryRuntimeReadinessV1, AdvisoryRuntimeUnavailableReasonV1, DeferredHookOrchestratorV1,
    HookOrchestrationPortV1,
};
use crate::errors::{Result, TraceDecayError};

const POST_OPEN_ADVISORY_SETUP_BUDGET: Duration = Duration::from_secs(15);
const POST_OPEN_ADVISORY_RETIREMENT_GRACE: Duration = Duration::from_secs(15);
const POST_OPEN_ADVISORY_DEADLINE_GRACE: Duration = Duration::from_millis(250);

/// A fully constructed advisory runtime whose publication has been staged but
/// not committed. Committing is deferred to the deferred-gateway owner so a
/// setup that lost to cancellation or the deadline rolls its publication back
/// instead of exposing it.
pub(super) struct PreparedAdvisoryRuntimeV1 {
    runtime: Arc<dyn HookOrchestrationPortV1>,
    commit: Option<Box<dyn FnOnce() + Send>>,
}

impl PreparedAdvisoryRuntimeV1 {
    pub(super) fn new(
        runtime: Arc<dyn HookOrchestrationPortV1>,
        commit: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            runtime,
            commit: Some(Box::new(commit)),
        }
    }

    fn commit(mut self) {
        if let Some(commit) = self.commit.take() {
            commit();
        }
    }
}

/// Starts at most one bounded setup for a retained deferred gateway.
///
/// The gateway itself is already published, so callers see `warming` while
/// this future runs. Project-runtime retirement cancels and joins setup until
/// any staged runtime publication either commits or rolls back.
pub(super) async fn schedule_bounded_post_open_advisory_setup<F, Fut>(
    deferred: Arc<DeferredHookOrchestratorV1>,
    setup: F,
) -> bool
where
    F: FnOnce(crate::application::context::CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = Result<PreparedAdvisoryRuntimeV1>> + Send + 'static,
{
    schedule_bounded_post_open_advisory_setup_with_budget(
        deferred,
        setup,
        POST_OPEN_ADVISORY_SETUP_BUDGET,
    )
    .await
}

async fn schedule_bounded_post_open_advisory_setup_with_budget<F, Fut>(
    deferred: Arc<DeferredHookOrchestratorV1>,
    setup: F,
    budget: Duration,
) -> bool
where
    F: FnOnce(crate::application::context::CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = Result<PreparedAdvisoryRuntimeV1>> + Send + 'static,
{
    if !deferred.claim_setup() {
        return false;
    }
    let (start, started) = tokio::sync::oneshot::channel();
    let retained = Arc::clone(&deferred);
    let task = tokio::spawn(async move {
        let _ = started.await;
        let cancellation = deferred.cancellation();
        let setup_started_at = match deferred.readiness() {
            AdvisoryRuntimeReadinessV1::Warming { started_at }
            | AdvisoryRuntimeReadinessV1::Ready { started_at, .. }
            | AdvisoryRuntimeReadinessV1::Unavailable { started_at, .. } => started_at,
        };
        enum SetupOutcome {
            Ready(PreparedAdvisoryRuntimeV1),
            Cancelled,
            DeadlineExceeded,
            Failed(TraceDecayError),
        }
        let work_cancellation = crate::application::context::CancellationToken::new();
        let mut setup_task = tokio::spawn(setup(work_cancellation.clone()));
        let outcome = tokio::select! {
            biased;
            result = &mut setup_task => match result {
                Ok(Ok(runtime)) => SetupOutcome::Ready(runtime),
                Ok(Err(error)) => SetupOutcome::Failed(error),
                Err(error) => SetupOutcome::Failed(TraceDecayError::Config {
                    message: format!("advisory runtime setup task failed: {error}"),
                }),
            },
            () = cancellation.cancelled() => {
                work_cancellation.cancel();
                join_cancelled_setup(&mut setup_task, POST_OPEN_ADVISORY_RETIREMENT_GRACE).await;
                SetupOutcome::Cancelled
            },
            () = tokio::time::sleep(budget) => {
                work_cancellation.cancel();
                join_cancelled_setup(&mut setup_task, POST_OPEN_ADVISORY_DEADLINE_GRACE).await;
                SetupOutcome::DeadlineExceeded
            }
        };
        let finished_at = now_micros();
        match outcome {
            SetupOutcome::Ready(prepared) => {
                let runtime = Arc::clone(&prepared.runtime);
                if deferred.mark_ready(runtime, finished_at) {
                    prepared.commit();
                    tracing::info!(
                        event = "advisory_runtime_setup",
                        state = "ready",
                        started_at_micros = setup_started_at.0,
                        finished_at_micros = finished_at.0,
                    );
                } else {
                    tracing::info!(
                        event = "advisory_runtime_setup",
                        state = "unavailable",
                        reason = "cancelled",
                        started_at_micros = setup_started_at.0,
                        finished_at_micros = finished_at.0,
                    );
                }
            }
            SetupOutcome::Cancelled => {
                deferred
                    .mark_unavailable(AdvisoryRuntimeUnavailableReasonV1::Cancelled, finished_at);
                tracing::info!(
                    event = "advisory_runtime_setup",
                    state = "unavailable",
                    reason = "cancelled",
                    started_at_micros = setup_started_at.0,
                    finished_at_micros = finished_at.0,
                );
            }
            SetupOutcome::DeadlineExceeded => {
                deferred.mark_unavailable(
                    AdvisoryRuntimeUnavailableReasonV1::DeadlineExceeded,
                    finished_at,
                );
                tracing::warn!(
                    event = "advisory_runtime_setup",
                    state = "unavailable",
                    reason = "deadline_exceeded",
                    started_at_micros = setup_started_at.0,
                    finished_at_micros = finished_at.0,
                );
            }
            SetupOutcome::Failed(error) => {
                deferred.mark_unavailable(
                    AdvisoryRuntimeUnavailableReasonV1::RegistrationFailed,
                    finished_at,
                );
                tracing::warn!(
                    event = "advisory_runtime_setup",
                    state = "unavailable",
                    reason = "registration_failed",
                    started_at_micros = setup_started_at.0,
                    finished_at_micros = finished_at.0,
                    error = %error,
                );
            }
        }
        deferred.setup_task_finished().await;
    });
    if let Err(task) = retained.retain_setup_task(task).await {
        task.abort();
        let _ = task.await;
        retained.mark_unavailable(AdvisoryRuntimeUnavailableReasonV1::Cancelled, now_micros());
        return false;
    }
    let _ = start.send(());
    true
}

async fn join_cancelled_setup(
    task: &mut tokio::task::JoinHandle<Result<PreparedAdvisoryRuntimeV1>>,
    grace: Duration,
) {
    match tokio::time::timeout(grace, &mut *task).await {
        Ok(_) => {}
        Err(_) => {
            task.abort();
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::super::service::invocation::{
        HookOrchestrationAdmissionV1, HookOrchestrationRequestV1,
    };
    use super::*;

    #[tokio::test]
    async fn post_open_advisory_setup_has_a_truthful_terminal_deadline() {
        let deferred = DeferredHookOrchestratorV1::new(now_micros());
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                Arc::clone(&deferred),
                |_| std::future::pending(),
                Duration::from_millis(1),
            )
            .await
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while matches!(
                deferred.readiness(),
                AdvisoryRuntimeReadinessV1::Warming { .. }
            ) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bounded setup terminal");
        assert!(matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Unavailable {
                reason: AdvisoryRuntimeUnavailableReasonV1::DeadlineExceeded,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn deadline_rolls_back_a_late_success_before_reporting_terminal() {
        struct Ready;
        impl HookOrchestrationPortV1 for Ready {
            fn admit(
                &self,
                _request: HookOrchestrationRequestV1,
            ) -> HookOrchestrationAdmissionV1 {
                HookOrchestrationAdmissionV1::Unavailable
            }
        }
        struct PublicationGuard {
            committed: bool,
            rolled_back: Arc<AtomicBool>,
        }
        impl PublicationGuard {
            fn commit(mut self) {
                self.committed = true;
            }
        }
        impl Drop for PublicationGuard {
            fn drop(&mut self) {
                if !self.committed {
                    self.rolled_back.store(true, Ordering::Release);
                }
            }
        }

        let deferred = DeferredHookOrchestratorV1::new(now_micros());
        let rolled_back = Arc::new(AtomicBool::new(false));
        let observed_rollback = Arc::clone(&rolled_back);
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                Arc::clone(&deferred),
                move |cancellation| async move {
                    cancellation.cancelled().await;
                    let guard = PublicationGuard {
                        committed: false,
                        rolled_back: observed_rollback,
                    };
                    Ok(PreparedAdvisoryRuntimeV1::new(Arc::new(Ready), move || {
                        guard.commit();
                    }))
                },
                Duration::from_millis(1),
            )
            .await
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            while matches!(
                deferred.readiness(),
                AdvisoryRuntimeReadinessV1::Warming { .. }
            ) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("late success reached a terminal");
        deferred.cancel_and_join().await;
        assert!(rolled_back.load(Ordering::Acquire));
        assert!(matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Unavailable {
                reason: AdvisoryRuntimeUnavailableReasonV1::DeadlineExceeded,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn retained_owner_cancellation_drops_post_open_setup() {
        struct PendingSetupGuard(Arc<AtomicBool>);
        impl Drop for PendingSetupGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let deferred = DeferredHookOrchestratorV1::new(now_micros());
        let setup_dropped = Arc::new(AtomicBool::new(false));
        let observed_setup_drop = Arc::clone(&setup_dropped);
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                Arc::clone(&deferred),
                move |cancellation| async move {
                    let _guard = PendingSetupGuard(observed_setup_drop);
                    cancellation.cancelled().await;
                    Err(TraceDecayError::Config {
                        message: "cancelled test setup".to_owned(),
                    })
                },
                Duration::from_secs(10),
            )
            .await
        );
        tokio::task::yield_now().await;
        deferred.cancel_and_join().await;
        assert!(setup_dropped.load(Ordering::Acquire));
        assert!(matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Unavailable {
                reason: AdvisoryRuntimeUnavailableReasonV1::Cancelled,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn failed_setup_can_be_retried_without_republishing_the_gateway() {
        struct Ready;
        impl HookOrchestrationPortV1 for Ready {
            fn admit(
                &self,
                _request: HookOrchestrationRequestV1,
            ) -> HookOrchestrationAdmissionV1 {
                HookOrchestrationAdmissionV1::Enqueued
            }
        }

        let deferred = DeferredHookOrchestratorV1::new(now_micros());
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                Arc::clone(&deferred),
                |_| async {
                    Err(TraceDecayError::Config {
                        message: "injected setup failure".to_owned(),
                    })
                },
                Duration::from_secs(1),
            )
            .await
        );
        tokio::task::yield_now().await;
        while matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Warming { .. }
        ) {
            tokio::task::yield_now().await;
        }
        assert!(
            schedule_bounded_post_open_advisory_setup_with_budget(
                Arc::clone(&deferred),
                |_| async { Ok(PreparedAdvisoryRuntimeV1::new(Arc::new(Ready), || {})) },
                Duration::from_secs(1),
            )
            .await
        );
        while !matches!(
            deferred.readiness(),
            AdvisoryRuntimeReadinessV1::Ready { .. }
        ) {
            tokio::task::yield_now().await;
        }
    }
}
