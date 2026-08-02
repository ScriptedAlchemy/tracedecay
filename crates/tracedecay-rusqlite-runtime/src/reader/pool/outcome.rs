//! What an acquisition reports, and the probe checks every read passes first.
//!
//! These are the pool's outward-facing result types: nothing here touches pool
//! capacity state, so the pool internals and the vocabulary callers match on
//! stay separable.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use tracedecay_store::{
    RuntimeInterruptionV1, RuntimeReadRequestV1, RuntimeRequestProbeV1, SaturationScopeV1,
    StorageRuntimeContractErrorV1, UnavailableReasonV1,
};

use super::super::{ReaderStartError, ReaderWorkerError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderPoolState {
    Ready,
    Draining,
}

/// Point-in-time occupancy of one shard's reader pool.
///
/// Serializable because live saturation is only diagnosable from outside the
/// process: `available + leased + limbo` per lane says where the workers went,
/// and `waiting_*` says whether anyone is being turned away.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderPoolSnapshot {
    pub state: ReaderPoolState,
    pub general_workers: u16,
    pub available_general: u16,
    pub health_workers: u16,
    pub available_health: u16,
    pub leased_general: u16,
    pub leased_health: u16,
    /// Workers whose lease ended but whose snapshot rollback has not been
    /// confirmed. They belong to neither `available_*` nor `leased_*`.
    pub limbo_general: u16,
    pub limbo_health: u16,
    /// Acquisitions blocked waiting for capacity in each lane.
    pub waiting_general: u16,
    pub waiting_health: u16,
}

#[derive(Debug)]
pub enum ReaderAcquireError {
    InvalidRequest(StorageRuntimeContractErrorV1),
    ProbeBindingMismatch { field: &'static str },
    BindingMismatch,
    Interrupted { reason: UnavailableReasonV1 },
    Saturated { scope: SaturationScopeV1 },
    WorkerStart(ReaderStartError),
    Worker(ReaderWorkerError),
}

impl fmt::Display for ReaderAcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(error) => write!(f, "invalid reader request: {error}"),
            Self::ProbeBindingMismatch { field } => {
                write!(f, "reader probe does not match {field}")
            }
            Self::BindingMismatch => f.write_str("reader request does not bind to this pool"),
            Self::Interrupted { reason } => write!(f, "reader acquisition interrupted: {reason:?}"),
            Self::Saturated { scope } => write!(f, "reader acquisition saturated: {scope:?}"),
            Self::WorkerStart(error) => write!(f, "reader burst worker failed to start: {error}"),
            Self::Worker(error) => write!(f, "reader worker failed: {error}"),
        }
    }
}

impl Error for ReaderAcquireError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRequest(error) => Some(error),
            Self::WorkerStart(error) => Some(error),
            Self::Worker(error) => Some(error),
            _ => None,
        }
    }
}

pub(super) fn map_worker_error(error: ReaderWorkerError) -> ReaderAcquireError {
    match error {
        ReaderWorkerError::Interrupted { reason } => ReaderAcquireError::Interrupted { reason },
        error => ReaderAcquireError::Worker(error),
    }
}

pub(super) fn validate_probe(
    request: &RuntimeReadRequestV1,
    probe: &dyn RuntimeRequestProbeV1,
) -> Result<(), ReaderAcquireError> {
    if probe.cancellation_identity() != &request.control().cancellation {
        return Err(ReaderAcquireError::ProbeBindingMismatch {
            field: "cancellation identity",
        });
    }
    if probe.deadline_identity() != &request.control().deadline {
        return Err(ReaderAcquireError::ProbeBindingMismatch {
            field: "deadline identity",
        });
    }
    Ok(())
}

pub(super) fn interruption(probe: &dyn RuntimeRequestProbeV1) -> Option<UnavailableReasonV1> {
    match probe.interruption() {
        Some(RuntimeInterruptionV1::Cancelled) => Some(UnavailableReasonV1::Cancelled),
        Some(RuntimeInterruptionV1::DeadlineExceeded) => {
            Some(UnavailableReasonV1::DeadlineExceeded)
        }
        None => None,
    }
}
