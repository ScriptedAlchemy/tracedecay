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
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner,
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
            return Err(GraphDbError::conflict("support.reject_path_alias"));
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
        "registry.open.lifecycle",
    )?;
    check_request(
        registration.cancellation.as_ref(),
        registration.deadline,
        "registry.open",
    )?;
    let persistent_store_state = inspect_graph_database_file(path)?;
    let (database, final_store_state) =
        open_registered_database(path, expected_format, registration, persistent_store_state)?;
    let owner = GraphDbOwner::register_database(database, authority_attachment)?;
    if final_store_state == PersistentGraphStoreState::Prospective
        && let Err(error) = check_request(
            registration.lifecycle_cancellation.as_ref(),
            registration.deadline,
            "registry.open.format_init.lifecycle",
        )
        .and_then(|()| {
            check_request(
                registration.cancellation.as_ref(),
                registration.deadline,
                "registry.open.format_init",
            )
        })
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

pub(super) fn open_registered_graph_lazy(
    path: &Path,
    expected_format: GraphFormatVersion,
    registration: &GraphDbRegistration,
    authority_attachment: Box<dyn RetainedGraphStoreOwnerAttachmentV1>,
) -> Result<GraphDbOwner, GraphDbError> {
    check_request(
        registration.lifecycle_cancellation.as_ref(),
        registration.deadline,
        "registry.open_lazy.lifecycle",
    )?;
    check_request(
        registration.cancellation.as_ref(),
        registration.deadline,
        "registry.open_lazy",
    )?;
    let persistent_store_state = inspect_graph_database_file(path)?;
    let database = GraphDb::open_lazy_with_store_state(
        registered_open_options(path, expected_format, registration, persistent_store_state),
        persistent_store_state,
    )?;
    GraphDbOwner::register_database(database, authority_attachment)
}

/// Opens the registry-owned database, running the deterministic-corruption
/// quarantine protocol when a preexisting container reports the typed
/// corruption verdict, then reopening the vacated path as a fresh store that
/// the canonical replay authorities re-project into.
fn open_registered_database(
    path: &Path,
    expected_format: GraphFormatVersion,
    registration: &GraphDbRegistration,
    persistent_store_state: PersistentGraphStoreState,
) -> Result<(Arc<GraphDb>, PersistentGraphStoreState), GraphDbError> {
    let open = |state: PersistentGraphStoreState| {
        GraphDb::open_with_store_state(
            registered_open_options(path, expected_format, registration, state),
            Some(state),
        )
    };
    match open(persistent_store_state) {
        Ok(database) => Ok((database, persistent_store_state)),
        Err(GraphDbError::Corrupt { message })
            if persistent_store_state == PersistentGraphStoreState::Existing =>
        {
            let recovery = crate::store_quarantine::recover_deterministically_corrupt_container(
                path,
                &message,
                &|| open(PersistentGraphStoreState::Existing),
            )?;
            match recovery {
                crate::store_quarantine::CorruptStoreRecovery::Reopened(database) => {
                    Ok((database, PersistentGraphStoreState::Existing))
                }
                crate::store_quarantine::CorruptStoreRecovery::Quarantined {
                    quarantine_directory,
                } => {
                    let fresh_state = inspect_graph_database_file(path)?;
                    let database = open(fresh_state)?;
                    tracing::info!(
                        event = "store_rebuilt_after_quarantine",
                        container = %path.display(),
                        quarantine = %quarantine_directory.display(),
                        "fresh graph store opened after corruption quarantine; canonical \
                         replay authorities re-project its generations"
                    );
                    Ok((database, fresh_state))
                }
            }
        }
        Err(error) => Err(error),
    }
}

fn registered_open_options(
    path: &Path,
    expected_format: GraphFormatVersion,
    registration: &GraphDbRegistration,
    persistent_store_state: PersistentGraphStoreState,
) -> GraphDbOpenOptions {
    let cancellation: Arc<dyn GraphCancellation> = match persistent_store_state {
        PersistentGraphStoreState::Prospective => Arc::new(ProspectiveGraphFormatCancellation),
        PersistentGraphStoreState::Existing => Arc::new(RegisteredGraphOpenCancellation {
            request: Arc::clone(&registration.cancellation),
            lifecycle: Arc::clone(&registration.lifecycle_cancellation),
        }),
    };
    GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path.to_path_buf()),
        expected_format,
        durability: GraphDurability::WalSync,
        cancellation,
    }
}

fn check_cancelled(cancellation: &dyn GraphCancellation) -> Result<(), GraphDbError> {
    if cancellation.is_cancelled() {
        Err(GraphDbError::Cancelled)
    } else {
        Ok(())
    }
}

/// `op` names the registered operation whose deadline is being enforced.
/// `DeadlineExceeded` is a unit error, so without this label a failure deep
/// in a projection or publication cannot be attributed to the registration
/// that armed the clock.
pub(super) fn check_deadline(deadline: Instant, op: &'static str) -> Result<(), GraphDbError> {
    if Instant::now() >= deadline {
        tracing::warn!(
            event = "graph_db_deadline_exceeded",
            op,
            "graph operation exceeded its registered deadline"
        );
        Err(GraphDbError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

pub(super) fn check_request(
    cancellation: &dyn GraphCancellation,
    deadline: Instant,
    op: &'static str,
) -> Result<(), GraphDbError> {
    check_cancelled(cancellation)?;
    check_deadline(deadline, op)
}

pub(super) fn check_registration_request(
    registration: &GraphDbRegistration,
    op: &'static str,
) -> Result<(), GraphDbError> {
    check_cancelled(registration.lifecycle_cancellation.as_ref())?;
    check_request(
        registration.cancellation.as_ref(),
        registration.deadline,
        op,
    )
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
            | GraphDbError::Conflict { .. }
            | GraphDbError::BudgetExhausted { .. }
            | GraphDbError::DeadlineExceeded
            | GraphDbError::ProjectionMismatch { .. }
            | GraphDbError::GenerationMismatch { .. }
            | GraphDbError::Unavailable { .. }
            | GraphDbError::SealedStoreImmutable { .. }
            | GraphDbError::Closed => GraphDbRegistryStatus::Closed,
        },
    }
}
