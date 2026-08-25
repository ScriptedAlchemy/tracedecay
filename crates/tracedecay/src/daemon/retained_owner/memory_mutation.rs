//! Receipt-preserving settlement for canonical retained-memory mutations.

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_application::RetainedSurfaceExecutionErrorV1;
use tracedecay_domain::ManifestDigest;
use tracedecay_usecases::memory::MemoryMutationError;

use super::memory_mapping;
use super::receipts::PreparedRetainedEffect;

pub(super) enum MemoryMutationSettlement<T> {
    Validated(T),
    InvalidAuthority(T),
}

pub(super) fn memory_mutation_settlement<T: Debug>(
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

pub(super) fn validate_memory_mutation<T: Debug>(
    settlement: Result<T, MemoryMutationError<T>>,
    prepared: &PreparedRetainedEffect,
    committed_state: impl for<'a> FnOnce(&'a T) -> Option<&'a ManifestDigest>,
) -> Result<T, RetainedSurfaceExecutionErrorV1> {
    match memory_mutation_settlement(settlement)? {
        MemoryMutationSettlement::Validated(outcome) => Ok(outcome),
        MemoryMutationSettlement::InvalidAuthority(outcome) => {
            let committed_state =
                committed_state(&outcome).ok_or(RetainedSurfaceExecutionErrorV1::Unavailable)?;
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
pub(super) fn fresh_one_shot_commit_gate(
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
