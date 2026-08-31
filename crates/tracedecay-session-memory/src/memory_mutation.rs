//! Receipt-preserving settlement for canonical retained-memory mutations.

use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracedecay_application::{
    PreparedRetainedEffect, RequestAdmission, RetainedSurfaceExecutionContextV1,
    RetainedSurfaceExecutionErrorV1, effective_memory_deadline, now_micros,
};
use tracedecay_domain::ManifestDigest;
use tracedecay_store::FactWriteControl;

use crate::memory::MemoryMutationError;
use crate::memory_mapping;

pub enum MemoryMutationSettlement<T> {
    Validated(T),
    InvalidAuthority(T),
}

pub fn memory_mutation_settlement<T: Debug>(
    settlement: Result<T, MemoryMutationError<T>>,
) -> Result<MemoryMutationSettlement<T>, RetainedSurfaceExecutionErrorV1> {
    match settlement {
        Ok(outcome) => Ok(MemoryMutationSettlement::Validated(outcome)),
        Err(MemoryMutationError::Application(error)) => {
            Err(memory_mapping::map_memory_error(error))
        }
        Err(MemoryMutationError::InvalidAuthorityResult {
            authority_result, ..
        }) => Ok(MemoryMutationSettlement::InvalidAuthority(authority_result)),
    }
}

pub fn validate_memory_mutation<T: Debug>(
    settlement: Result<T, MemoryMutationError<T>>,
    prepared: &PreparedRetainedEffect,
    committed_state: impl for<'a> FnOnce(&'a T) -> Option<&'a ManifestDigest>,
) -> Result<T, RetainedSurfaceExecutionErrorV1> {
    match memory_mutation_settlement(settlement)? {
        MemoryMutationSettlement::Validated(outcome) => Ok(outcome),
        MemoryMutationSettlement::InvalidAuthority(outcome) => {
            let committed_state = committed_state(&outcome).ok_or_else(|| {
                RetainedSurfaceExecutionErrorV1::unavailable(
                    "the invalid authority result carried no committed state",
                )
            })?;
            Err(prepared.partial_error_with_digest(
                committed_state,
                "application.retained.memory-authority-result-invalid",
                "The canonical fact committed, but the authority result failed validation.",
            ))
        }
    }
}

/// Owns admission for exactly one commit attempt while retaining the caller's
/// live interruption boundary until that attempt starts.
pub fn fresh_one_shot_commit_gate(
    interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Arc<dyn Fn() -> bool + Send + Sync> {
    let admitted = Arc::new(AtomicBool::new(false));
    Arc::new(move || {
        !interrupted()
            && admitted
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    })
}

fn effective_expiry(
    context: &RetainedSurfaceExecutionContextV1<'_>,
) -> tracedecay_domain::UtcMicros {
    effective_memory_deadline(context).expires_at
}

pub fn fact_write_control(context: &RetainedSurfaceExecutionContextV1<'_>) -> FactWriteControl {
    let interrupted_signal = context.cancellation_signal.clone();
    let commit_signal = context.cancellation_signal.clone();
    let expires_at = effective_expiry(context);
    let commit_expires_at = expires_at;
    FactWriteControl::new(
        Arc::new(move || interrupted_signal.is_cancelled() || expires_at <= now_micros()),
        fresh_one_shot_commit_gate(Arc::new(move || {
            commit_signal.is_cancelled()
                || commit_expires_at <= now_micros()
                || !commit_signal.try_begin_commit()
        })),
    )
}

#[hotpath::measure(label = "daemon.retained.memory.bounded_operation", future = true)]
pub async fn bounded_memory_operation<T, F>(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    future: F,
) -> Result<(T, bool), RetainedSurfaceExecutionErrorV1>
where
    F: Future<Output = Result<T, RetainedSurfaceExecutionErrorV1>>,
{
    let now = now_micros();
    match context.request_context.admission_at(now) {
        RequestAdmission::Admitted if !context.cancellation_signal.is_cancelled() => {}
        RequestAdmission::Admitted | RequestAdmission::Cancelled => {
            return Err(RetainedSurfaceExecutionErrorV1::Cancelled(
                tracedecay_application::CancellationStage::BeforeEffect,
            ));
        }
        RequestAdmission::TimedOut => {
            return Err(RetainedSurfaceExecutionErrorV1::TimedOut(
                tracedecay_application::CancellationStage::BeforeEffect,
            ));
        }
    }
    let remaining = effective_expiry(context).0.saturating_sub(now.0);
    let remaining = u64::try_from(remaining)
        .ok()
        .map(Duration::from_micros)
        .ok_or(RetainedSurfaceExecutionErrorV1::TimedOut(
            tracedecay_application::CancellationStage::BeforeEffect,
        ))?;
    tokio::pin!(future);
    tokio::select! {
        biased;
        outcome = &mut future => classify_memory_settlement(context, outcome),
        () = context.cancellation_signal.cancelled() => {
            if context.cancellation_signal.commit_started() {
                classify_memory_settlement(context, future.await)
            } else {
                Err(RetainedSurfaceExecutionErrorV1::Cancelled(tracedecay_application::CancellationStage::BeforeEffect))
            }
        }
        () = tokio::time::sleep(remaining) => {
            if context.cancellation_signal.commit_started() {
                classify_memory_settlement(context, future.await)
            } else {
                Err(RetainedSurfaceExecutionErrorV1::TimedOut(tracedecay_application::CancellationStage::BeforeEffect))
            }
        }
    }
}

fn classify_memory_settlement<T>(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    outcome: Result<T, RetainedSurfaceExecutionErrorV1>,
) -> Result<(T, bool), RetainedSurfaceExecutionErrorV1> {
    let commit_started = context.cancellation_signal.commit_started();
    let cancelled = context.cancellation_signal.is_cancelled();
    let timed_out = effective_expiry(context) <= now_micros();
    match outcome {
        Ok(value) if commit_started => Ok((value, timed_out)),
        Ok(_) if cancelled => Err(RetainedSurfaceExecutionErrorV1::Cancelled(
            tracedecay_application::CancellationStage::BeforeEffect,
        )),
        Ok(_) if timed_out => Err(RetainedSurfaceExecutionErrorV1::TimedOut(
            tracedecay_application::CancellationStage::BeforeEffect,
        )),
        Ok(value) => Ok((value, false)),
        Err(_) if cancelled && !commit_started => Err(RetainedSurfaceExecutionErrorV1::Cancelled(
            tracedecay_application::CancellationStage::BeforeEffect,
        )),
        Err(RetainedSurfaceExecutionErrorV1::Cancelled(_)) if timed_out => {
            Err(RetainedSurfaceExecutionErrorV1::TimedOut(
                tracedecay_application::CancellationStage::BeforeEffect,
            ))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::fresh_one_shot_commit_gate;

    #[test]
    fn commit_gate_admits_exactly_one_fresh_commit() {
        let gate = fresh_one_shot_commit_gate(Arc::new(|| false));

        assert!(gate());
        assert!(!gate());
    }

    #[test]
    fn commit_gate_rejects_an_interrupted_commit_without_consuming_admission() {
        let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = fresh_one_shot_commit_gate({
            let interrupted = Arc::clone(&interrupted);
            Arc::new(move || interrupted.load(std::sync::atomic::Ordering::Acquire))
        });

        assert!(!gate());
        interrupted.store(false, std::sync::atomic::Ordering::Release);
        assert!(gate());
        assert!(!gate());
    }
}
