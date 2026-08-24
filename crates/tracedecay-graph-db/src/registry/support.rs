use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tracedecay_store::{
    RetainedGraphStoreOwnerAttachmentV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
};

use super::identity::{binding, require_binding};
use super::path::inspect_graph_database_file;
use super::{
    GraphDbRegistration, GraphDbRegistryStatus, RegisteredGraphOpenCancellation, RegistryEntry,
    RegistryState,
};
use crate::error::rollback_failure;
use crate::location::PersistentGraphStoreState;
use crate::{
    GraphCancellation, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner,
    GraphDbRuntimeState, GraphDurability, GraphFormatVersion,
};

struct ProspectiveGraphFormatCancellation;

impl GraphCancellation for ProspectiveGraphFormatCancellation {
    fn is_cancelled(&self) -> bool {
        false
    }
}

pub(super) fn reject_path_alias(
    state: &RegistryState,
    requested_binding: &StoreRuntimeBindingV1,
    requested_locator: &VerifiedStoreLocatorV1,
    path: &Path,
    expected_format: GraphFormatVersion,
) -> Result<(), GraphDbError> {
    for entry in state.entries.values() {
        let (registered_binding, registered_locator, registered_path, registered_format) =
            binding(entry);
        if registered_binding.shard_id == requested_binding.shard_id {
            require_binding(
                (
                    registered_binding,
                    registered_locator,
                    registered_path,
                    registered_format,
                ),
                (requested_binding, requested_locator, path, expected_format),
            )?;
        } else if registered_path == path {
            return Err(GraphDbError::Conflict);
        }
    }
    Ok(())
}

pub(super) fn open_registered_graph(
    path: &Path,
    expected_format: GraphFormatVersion,
    registration: &GraphDbRegistration,
    authority_attachment: Box<dyn RetainedGraphStoreOwnerAttachmentV1>,
) -> Result<GraphDbOwner, GraphDbError> {
    check_request(
        registration.lifecycle_cancellation.as_ref(),
        registration.deadline,
    )?;
    check_request(registration.cancellation.as_ref(), registration.deadline)?;
    let persistent_store_state = inspect_graph_database_file(path)?;
    let cancellation: Arc<dyn GraphCancellation> = match persistent_store_state {
        PersistentGraphStoreState::Prospective => Arc::new(ProspectiveGraphFormatCancellation),
        PersistentGraphStoreState::Existing => Arc::new(RegisteredGraphOpenCancellation {
            request: Arc::clone(&registration.cancellation),
            lifecycle: Arc::clone(&registration.lifecycle_cancellation),
        }),
    };
    let owner = GraphDbOwner::open_registered(
        GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(path.to_path_buf()),
            expected_format,
            durability: GraphDurability::WalSync,
            cancellation,
        },
        persistent_store_state,
        authority_attachment,
    )?;
    if persistent_store_state == PersistentGraphStoreState::Prospective
        && let Err(error) = check_request(
            registration.lifecycle_cancellation.as_ref(),
            registration.deadline,
        )
        .and_then(|()| check_request(registration.cancellation.as_ref(), registration.deadline))
    {
        return match owner.close() {
            Ok(()) => Err(error),
            Err(close_error) => Err(rollback_failure(
                "cancelled graph format initialization",
                error,
                close_error,
            )),
        };
    }
    Ok(owner)
}

fn check_cancelled(cancellation: &dyn GraphCancellation) -> Result<(), GraphDbError> {
    if cancellation.is_cancelled() {
        Err(GraphDbError::Cancelled)
    } else {
        Ok(())
    }
}

pub(super) fn check_deadline(deadline: Instant) -> Result<(), GraphDbError> {
    if Instant::now() >= deadline {
        Err(GraphDbError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

pub(super) fn check_request(
    cancellation: &dyn GraphCancellation,
    deadline: Instant,
) -> Result<(), GraphDbError> {
    check_cancelled(cancellation)?;
    check_deadline(deadline)
}

pub(super) fn check_registration_request(
    registration: &GraphDbRegistration,
) -> Result<(), GraphDbError> {
    check_cancelled(registration.lifecycle_cancellation.as_ref())?;
    check_request(registration.cancellation.as_ref(), registration.deadline)
}

pub(super) fn retains_fault(error: &GraphDbError) -> bool {
    matches!(
        error,
        GraphDbError::ResetRequired { .. }
            | GraphDbError::Corrupt { .. }
            | GraphDbError::DurabilityUncertain { .. }
    )
}

pub(super) fn status(entry: &RegistryEntry) -> GraphDbRegistryStatus {
    match entry {
        RegistryEntry::Opening { .. } => GraphDbRegistryStatus::Opening,
        RegistryEntry::Closing { .. } | RegistryEntry::Retiring { .. } => {
            GraphDbRegistryStatus::Closing
        }
        RegistryEntry::Ready { owner, .. } => match owner.runtime_state() {
            GraphDbRuntimeState::Ready => GraphDbRegistryStatus::Ready,
            GraphDbRuntimeState::Closed => GraphDbRegistryStatus::Closed,
            GraphDbRuntimeState::DurabilityUncertain => GraphDbRegistryStatus::DurabilityUncertain,
        },
        RegistryEntry::Faulted { error, .. } => match error {
            GraphDbError::ResetRequired { .. } => GraphDbRegistryStatus::ResetRequired,
            GraphDbError::Corrupt { .. } => GraphDbRegistryStatus::Corrupt,
            GraphDbError::DurabilityUncertain { .. } => GraphDbRegistryStatus::DurabilityUncertain,
            GraphDbError::Cancelled
            | GraphDbError::InvalidRequest { .. }
            | GraphDbError::Conflict
            | GraphDbError::BudgetExhausted { .. }
            | GraphDbError::DeadlineExceeded
            | GraphDbError::ProjectionMismatch { .. }
            | GraphDbError::GenerationMismatch { .. }
            | GraphDbError::Unavailable { .. }
            | GraphDbError::Closed => GraphDbRegistryStatus::Closed,
        },
    }
}
