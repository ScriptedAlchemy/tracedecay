use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{DurabilityClassV1, StoreIncarnationV1};

/// Validation failures for pure runtime contract DTOs.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum StorageRuntimeContractErrorV1 {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} must be non-zero")]
    Zero { field: &'static str },
    #[error("{field} is not canonical")]
    NonCanonical { field: &'static str },
    #[error("{field} length {actual} exceeds the maximum of {max}")]
    TooLong {
        field: &'static str,
        actual: usize,
        max: usize,
    },
    #[error("{field} value {actual} exceeds the maximum of {max}")]
    LimitExceeded {
        field: &'static str,
        actual: u64,
        max: u64,
    },
    #[error("{field} must be at least {min}, got {actual}")]
    BelowMinimum {
        field: &'static str,
        actual: u64,
        min: u64,
    },
    #[error("{field} range is invalid: minimum {min}, maximum {max}")]
    InvalidRange {
        field: &'static str,
        min: u64,
        max: u64,
    },
    #[error("{field} does not match its canonical shard identity")]
    ShardMismatch { field: &'static str },
    #[error("watermark vector must contain at least one shard")]
    EmptyWatermarkVector,
    #[error("operation {operation} is incompatible with shard family {shard_family}")]
    OperationScopeMismatch {
        operation: &'static str,
        shard_family: &'static str,
    },
    #[error("operation {operation} cannot mutate an immutable shard")]
    ImmutableShard { operation: &'static str },
    #[error("operation {operation} requires {required:?} durability, not {actual:?} durability")]
    DurabilityMismatch {
        operation: &'static str,
        required: DurabilityClassV1,
        actual: DurabilityClassV1,
    },
    #[error("idempotency key was replayed with a different command digest")]
    IdempotencyConflict,
    #[error("repository payload for {payload} failed its owning store contract")]
    InvalidRepositoryPayload { payload: &'static str },
    #[error("{field} does not bind to the request or effect identity")]
    ReceiptBindingMismatch { field: &'static str },
    #[error("{field} is not a valid lease interval")]
    InvalidLeaseInterval { field: &'static str },
    #[error("maintenance transition from {from} to {to} is not allowed")]
    InvalidMaintenanceTransition {
        from: &'static str,
        to: &'static str,
    },
    #[error("reader health leases require the reserved health lane")]
    ReaderHealthLaneRequired,
    #[error("runtime batch is incompatible at {field}")]
    BatchIncompatible { field: &'static str },
    #[error("invalid effect state transition from {from} to {to}")]
    InvalidEffectTransition {
        from: &'static str,
        to: &'static str,
    },
    #[error("an acknowledged outbox entry requires a typed acknowledgment receipt")]
    AcknowledgementReceiptRequired,
    #[error("authority epoch mismatch for {side} effect sink")]
    EffectEpochMismatch { side: &'static str },
    #[error("store incarnation mismatch for {side} effect sink")]
    EffectIncarnationMismatch { side: &'static str },
    #[error("{field} has an unexpected store incarnation")]
    IncarnationMismatch {
        field: &'static str,
        expected: StoreIncarnationV1,
        actual: StoreIncarnationV1,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SaturationScopeV1 {
    ShardOperations,
    ShardBytes,
    GlobalBytes,
    ReaderPool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReasonV1 {
    Closed,
    Opening,
    Draining,
    Maintenance,
    Faulted,
    SnapshotExpired,
    SnapshotNotRetained,
    WatermarkNotReached,
    WrongIncarnation,
    WrongAuthorityEpoch,
    MissingAuthority,
    UnsupportedOperation,
    Cancelled,
    DeadlineExceeded,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CorruptionClassV1 {
    Authoritative,
    RebuildableProjection,
    IntegrityUnknown,
}

/// Storage-runtime cancellation observation stage.
///
/// This is not the application `CancellationStage`: queue, commit, consistency,
/// and reader waits are runtime-owned points, while cancellation token identity
/// and caller deadlines remain application-owned and are not duplicated here.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCancellationStageV1 {
    BeforeAdmission,
    Queued,
    BeforeCommit,
    WaitingForConsistency,
    WaitingForReader,
}

/// Stable, driver-neutral failures exposed by runtime ports.
///
/// Expected admission, interruption, fencing, consistency, and unsupported
/// decisions belong in submit outcomes or read coverage. This error channel is
/// reserved for infrastructure failure and detected corruption.
#[derive(Clone, Debug, Error, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StorageRuntimeErrorV1 {
    #[error("storage infrastructure failed during {operation}")]
    Infrastructure { operation: String },
    #[error("storage corruption detected: {class:?}")]
    Corrupt { class: CorruptionClassV1 },
}

pub type StorageRuntimeResultV1<T> = Result<T, StorageRuntimeErrorV1>;
