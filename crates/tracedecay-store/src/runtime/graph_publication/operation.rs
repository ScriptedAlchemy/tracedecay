use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;

use super::super::{
    RuntimeInterruptionV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
    StorageRuntimeContractErrorV1,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GraphPublicationStoreErrorV1 {
    #[error("invalid graph publication request: {0}")]
    InvalidRequest(#[from] StorageRuntimeContractErrorV1),
    #[error("graph publication interrupted: {0:?}")]
    Interrupted(RuntimeInterruptionV1),
    #[error("graph publication persistence is unavailable")]
    Infrastructure,
    #[error("graph publication persistence is corrupt: {0}")]
    Corrupt(String),
}

pub type GraphPublicationStoreResultV1<T> = Result<T, GraphPublicationStoreErrorV1>;

/// Existing caller-owned cancellation and monotonic deadline authority.
pub struct GraphPublicationOperationContextV1<'a> {
    probe: &'a dyn RuntimeRequestProbeV1,
    commit_started: AtomicBool,
}

impl<'a> GraphPublicationOperationContextV1<'a> {
    pub fn new(
        control: &RuntimeRequestControlV1,
        probe: &'a dyn RuntimeRequestProbeV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        control.validate()?;
        if probe.cancellation_identity() != &control.cancellation {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph publication cancellation probe identity",
            });
        }
        if probe.deadline_identity() != &control.deadline {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph publication deadline probe identity",
            });
        }
        Ok(Self {
            probe,
            commit_started: AtomicBool::new(false),
        })
    }

    pub fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        self.probe.interruption()
    }

    pub fn try_begin_verified_commit(&self) -> bool {
        self.try_begin_commit()
    }

    pub fn try_begin_semantic_vector_stage_commit(&self) -> bool {
        self.try_begin_commit()
    }

    pub fn try_begin_replay_retirement_commit(&self) -> bool {
        self.try_begin_commit()
    }

    pub fn try_begin_retired_cleanup_finalize_commit(&self) -> bool {
        self.try_begin_commit()
    }

    fn try_begin_commit(&self) -> bool {
        self.commit_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && self.probe.try_begin_commit()
    }
}
