use std::time::Duration;

use rusqlite::ErrorCode;
use tracedecay_store::{
    RuntimeCancellationStageV1, RuntimeInterruptionV1, RuntimeRequestProbeV1,
    RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, SaturationScopeV1,
    StorageRuntimeContractErrorV1, StorageRuntimeErrorV1, StoreCommitReceiptV1,
    StoreRuntimeBindingV1, UnavailableReasonV1,
};

use super::{
    WriterActorError,
    request::{AcceptedRequest, RequestResult},
};

const RETRY_AFTER_BUSY_MS: u64 = 1;

#[derive(Clone)]
pub(super) enum DriverFailure {
    Busy,
    Error(StorageRuntimeErrorV1),
}

impl DriverFailure {
    pub(super) fn result(&self, request: &RuntimeSubmitRequestV1) -> RequestResult {
        match self {
            Self::Busy => Err(infrastructure(format!(
                "canonical SQLite writer for {:?} encountered a competing write authority",
                request.binding().shard_id
            ))),
            Self::Error(error) => Err(error.clone()),
        }
    }

    pub(super) fn storage_error(self) -> StorageRuntimeErrorV1 {
        match self {
            Self::Busy => infrastructure(
                "canonical SQLite writer encountered a competing write authority during rollback",
            ),
            Self::Error(error) => error,
        }
    }
}

pub(super) fn driver_failure(error: rusqlite::Error, operation: &'static str) -> DriverFailure {
    if matches!(error, rusqlite::Error::SqliteFailure(ref failure, _)
        if matches!(failure.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked))
    {
        DriverFailure::Busy
    } else {
        DriverFailure::Error(infrastructure(operation))
    }
}

pub(super) fn saturation(
    request: &RuntimeSubmitRequestV1,
    scope: SaturationScopeV1,
) -> RuntimeSubmitOutcomeV1 {
    RuntimeSubmitOutcomeV1::Saturated {
        shard_id: Some(request.binding().shard_id.clone()),
        scope,
        retry_after_ms: RETRY_AFTER_BUSY_MS,
    }
}

pub(super) fn missing_authority() -> RuntimeSubmitOutcomeV1 {
    RuntimeSubmitOutcomeV1::Unavailable {
        reason: UnavailableReasonV1::MissingAuthority,
    }
}

pub(super) fn interruption_outcome(
    request: &RuntimeSubmitRequestV1,
    probe: &dyn RuntimeRequestProbeV1,
    stage: RuntimeCancellationStageV1,
) -> Option<RuntimeSubmitOutcomeV1> {
    match probe.interruption()? {
        RuntimeInterruptionV1::Cancelled => Some(RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
            cancellation: request.control().cancellation.clone(),
            stage,
        }),
        RuntimeInterruptionV1::DeadlineExceeded => {
            Some(RuntimeSubmitOutcomeV1::DeadlineExceededBeforeCommit {
                deadline: request.control().deadline.clone(),
            })
        }
    }
}

pub(super) fn binding_outcome(
    binding: &StoreRuntimeBindingV1,
    request: &RuntimeSubmitRequestV1,
) -> Option<RuntimeSubmitOutcomeV1> {
    let requested = request.binding();
    if requested.shard_id != binding.shard_id || requested.incarnation != binding.incarnation {
        return Some(RuntimeSubmitOutcomeV1::Unavailable {
            reason: UnavailableReasonV1::WrongIncarnation,
        });
    }
    (requested.authority_epoch != binding.authority_epoch).then_some(
        RuntimeSubmitOutcomeV1::Fenced {
            expected: requested.authority_epoch,
            actual: binding.authority_epoch,
        },
    )
}

pub(super) fn idempotency_outcome(
    request: &RuntimeSubmitRequestV1,
    receipt: StoreCommitReceiptV1,
) -> RequestResult {
    match receipt
        .idempotency
        .check_replay(&request.envelope().metadata.idempotency)
    {
        Ok(true) => {
            receipt
                .validate_replay_for(&request.envelope().metadata)
                .map_err(invalid_response)?;
            Ok(RuntimeSubmitOutcomeV1::ExactReplay { receipt })
        }
        Err(StorageRuntimeContractErrorV1::IdempotencyConflict) => {
            let outcome = RuntimeSubmitOutcomeV1::IdempotencyConflict {
                existing_receipt: receipt,
            };
            outcome.validate_for(request).map_err(invalid_response)?;
            Ok(outcome)
        }
        _ => Err(infrastructure(
            "idempotency ledger returned a receipt for a different key",
        )),
    }
}

pub(super) fn committed_outcome(
    item: &AcceptedRequest,
    receipt: StoreCommitReceiptV1,
) -> RequestResult {
    let outcome = match item.probe.interruption() {
        Some(RuntimeInterruptionV1::Cancelled) => {
            RuntimeSubmitOutcomeV1::CommittedAfterCancellation {
                receipt,
                cancellation: item.request.control().cancellation.clone(),
            }
        }
        Some(RuntimeInterruptionV1::DeadlineExceeded) | None => {
            RuntimeSubmitOutcomeV1::Committed { receipt }
        }
    };
    outcome
        .validate_for(&item.request)
        .map_err(invalid_response)?;
    Ok(outcome)
}

pub(super) fn validate_probe(
    request: &RuntimeSubmitRequestV1,
    probe: &dyn RuntimeRequestProbeV1,
) -> Result<(), WriterActorError> {
    if probe.cancellation_identity() != &request.control().cancellation {
        return Err(WriterActorError::ProbeBindingMismatch {
            field: "runtime cancellation identity",
        });
    }
    if probe.deadline_identity() != &request.control().deadline {
        return Err(WriterActorError::ProbeBindingMismatch {
            field: "runtime deadline identity",
        });
    }
    Ok(())
}

pub(super) fn invalid_response(error: StorageRuntimeContractErrorV1) -> StorageRuntimeErrorV1 {
    infrastructure(format!(
        "typed writer persistence returned an invalid receipt: {error}"
    ))
}

pub(super) fn infrastructure(operation: impl Into<String>) -> StorageRuntimeErrorV1 {
    StorageRuntimeErrorV1::Infrastructure {
        operation: operation.into(),
    }
}

pub(super) fn is_corrupt(error: &StorageRuntimeErrorV1) -> bool {
    matches!(error, StorageRuntimeErrorV1::Corrupt { .. })
}

pub(super) fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}
