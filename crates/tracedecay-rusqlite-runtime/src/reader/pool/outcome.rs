//! What an acquisition reports, and the probe checks every read passes first.
//!
//! These are the pool's outward-facing result types: nothing here touches pool
//! capacity state, so the pool internals and the vocabulary callers match on
//! stay separable.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use tracedecay_contracts::storage::TableGrowthUnavailableReasonV1;
use tracedecay_store::{
    RuntimeInterruptionV1, RuntimeReadRequestV1, RuntimeRequestProbeV1, SaturationScopeV1,
    StorageRuntimeContractErrorV1, StorageRuntimeErrorV1, UnavailableReasonV1,
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
/// and `waiting_*` says whether anyone is being turned away. The cumulative
/// snapshot count distinguishes snapshot-heavy workloads from ordinary
/// one-query reads without retaining request content or identity.
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
    /// Successfully admitted exact-SQL snapshots since this pool attached.
    ///
    /// Saturates at `u64::MAX`; telemetry must never wrap a long-running
    /// process back to a smaller apparent workload.
    #[serde(default)]
    pub snapshot_admissions: u64,
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

impl ReaderAcquireError {
    /// Why a table-growth measurement that failed with this error is
    /// unavailable, as a coarse class that never carries the raw message.
    #[must_use]
    pub fn table_growth_unavailable_reason(&self) -> TableGrowthUnavailableReasonV1 {
        match self {
            Self::Interrupted { reason }
            | Self::Worker(ReaderWorkerError::Interrupted { reason }) => match reason {
                UnavailableReasonV1::DeadlineExceeded => {
                    TableGrowthUnavailableReasonV1::DeadlineExceeded
                }
                UnavailableReasonV1::Cancelled => TableGrowthUnavailableReasonV1::Cancelled,
                _ => TableGrowthUnavailableReasonV1::ReaderUnavailable,
            },
            Self::Saturated { .. }
            | Self::WorkerStart(_)
            | Self::Worker(ReaderWorkerError::WorkerClosed) => {
                TableGrowthUnavailableReasonV1::ReaderUnavailable
            }
            Self::Worker(ReaderWorkerError::Storage(StorageRuntimeErrorV1::Corrupt { .. })) => {
                TableGrowthUnavailableReasonV1::StorageCorrupt
            }
            Self::Worker(ReaderWorkerError::Storage(StorageRuntimeErrorV1::Infrastructure {
                ..
            })) => TableGrowthUnavailableReasonV1::StorageInfrastructure,
            Self::InvalidRequest(_)
            | Self::ProbeBindingMismatch { .. }
            | Self::BindingMismatch
            | Self::Worker(
                ReaderWorkerError::SnapshotAlreadyActive
                | ReaderWorkerError::SnapshotNotActive
                | ReaderWorkerError::Storage(_),
            ) => TableGrowthUnavailableReasonV1::StorageOther,
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

#[cfg(test)]
mod table_growth_reason_tests {
    use tracedecay_contracts::storage::TableGrowthUnavailableReasonV1 as Reason;
    use tracedecay_store::{
        CorruptionClassV1, SaturationScopeV1, StorageRuntimeErrorV1, UnavailableReasonV1,
    };

    use super::{ReaderAcquireError, ReaderWorkerError};

    #[test]
    fn every_reader_failure_names_its_table_growth_reason() {
        let cases = [
            (
                ReaderAcquireError::Interrupted {
                    reason: UnavailableReasonV1::DeadlineExceeded,
                },
                Reason::DeadlineExceeded,
            ),
            (
                ReaderAcquireError::Worker(ReaderWorkerError::Interrupted {
                    reason: UnavailableReasonV1::Cancelled,
                }),
                Reason::Cancelled,
            ),
            (
                ReaderAcquireError::Interrupted {
                    reason: UnavailableReasonV1::Draining,
                },
                Reason::ReaderUnavailable,
            ),
            (
                ReaderAcquireError::Saturated {
                    scope: SaturationScopeV1::ReaderPool,
                },
                Reason::ReaderUnavailable,
            ),
            (
                ReaderAcquireError::Worker(ReaderWorkerError::WorkerClosed),
                Reason::ReaderUnavailable,
            ),
            (
                ReaderAcquireError::Worker(ReaderWorkerError::Storage(
                    StorageRuntimeErrorV1::Corrupt {
                        class: CorruptionClassV1::Authoritative,
                    },
                )),
                Reason::StorageCorrupt,
            ),
            (
                ReaderAcquireError::Worker(ReaderWorkerError::Storage(
                    StorageRuntimeErrorV1::Infrastructure {
                        operation: "dbstat".to_owned(),
                    },
                )),
                Reason::StorageInfrastructure,
            ),
            (ReaderAcquireError::BindingMismatch, Reason::StorageOther),
            (
                ReaderAcquireError::Worker(ReaderWorkerError::SnapshotNotActive),
                Reason::StorageOther,
            ),
        ];
        for (error, reason) in cases {
            assert_eq!(error.table_growth_unavailable_reason(), reason, "{error}");
        }
    }
}
