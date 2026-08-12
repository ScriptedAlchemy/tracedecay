use std::future::Future;
use std::time::Duration;

use tracedecay_application::{
    CancellationStage, RequestAdmission, RetainedSurfaceExecutionContextV1,
    RetainedSurfaceExecutionErrorV1, now_micros,
};

use super::memory_mapping::MemoryCancellationStages;
use super::receipts::effective_memory_deadline;

pub(super) async fn bounded_memory_operation<T, F>(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    stages: MemoryCancellationStages,
    future: F,
) -> Result<(T, bool), RetainedSurfaceExecutionErrorV1>
where
    F: Future<Output = Result<T, RetainedSurfaceExecutionErrorV1>>,
{
    let before_stage = stages.before;
    let active_stage = stages.active;
    let now = now_micros();
    match context.request_context.admission_at(now) {
        RequestAdmission::Admitted if !context.cancellation_signal.is_cancelled() => {}
        RequestAdmission::Admitted | RequestAdmission::Cancelled => {
            return Err(RetainedSurfaceExecutionErrorV1::Cancelled(before_stage));
        }
        RequestAdmission::TimedOut => {
            return Err(RetainedSurfaceExecutionErrorV1::TimedOut(before_stage));
        }
    }
    let remaining = effective_memory_deadline(context)
        .expires_at
        .0
        .saturating_sub(now.0);
    let remaining = u64::try_from(remaining)
        .ok()
        .map(Duration::from_micros)
        .ok_or(RetainedSurfaceExecutionErrorV1::TimedOut(before_stage))?;
    tokio::pin!(future);
    tokio::select! {
        biased;
        outcome = &mut future => classify_memory_settlement(context, active_stage, outcome),
        _ = context.cancellation_signal.cancelled() => {
            if context.cancellation_signal.commit_started() {
                classify_memory_settlement(context, active_stage, future.await)
            } else {
                Err(RetainedSurfaceExecutionErrorV1::Cancelled(active_stage))
            }
        }
        _ = tokio::time::sleep(remaining) => {
            if context.cancellation_signal.commit_started() {
                classify_memory_settlement(context, active_stage, future.await)
            } else {
                Err(RetainedSurfaceExecutionErrorV1::TimedOut(active_stage))
            }
        }
    }
}

fn classify_memory_settlement<T>(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    active_stage: CancellationStage,
    outcome: Result<T, RetainedSurfaceExecutionErrorV1>,
) -> Result<(T, bool), RetainedSurfaceExecutionErrorV1> {
    let commit_started = context.cancellation_signal.commit_started();
    let cancelled = context.cancellation_signal.is_cancelled();
    let timed_out = effective_memory_deadline(context).expires_at <= now_micros();
    match outcome {
        Ok(value) if commit_started => Ok((value, timed_out)),
        Ok(_) if cancelled => Err(RetainedSurfaceExecutionErrorV1::Cancelled(active_stage)),
        Ok(_) if timed_out => Err(RetainedSurfaceExecutionErrorV1::TimedOut(active_stage)),
        Ok(value) => Ok((value, false)),
        Err(_) if cancelled && !commit_started => {
            Err(RetainedSurfaceExecutionErrorV1::Cancelled(active_stage))
        }
        Err(RetainedSurfaceExecutionErrorV1::Cancelled(stage)) if timed_out => {
            Err(RetainedSurfaceExecutionErrorV1::TimedOut(stage))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
pub(super) async fn bounded_memory_stage_for_test(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    stages: MemoryCancellationStages,
) -> Result<((), bool), RetainedSurfaceExecutionErrorV1> {
    bounded_memory_operation(context, stages, async { Ok(()) }).await
}

#[cfg(test)]
#[path = "memory_stage_tests.rs"]
mod tests;
