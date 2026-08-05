use std::path::Path;
use std::sync::Arc;

use tracedecay_store::{StoreRuntimeBindingV1, StoreShardScopeV1, VerifiedStoreLocatorV1};

use super::{Eviction, GraphDbRegistration, RegistryEntry};
use crate::{GraphDbError, GraphFormatVersion};

pub(super) type IdentityRef<'a> = (
    &'a StoreRuntimeBindingV1,
    &'a VerifiedStoreLocatorV1,
    &'a Path,
    GraphFormatVersion,
);

pub(super) fn binding(entry: &RegistryEntry) -> IdentityRef<'_> {
    match entry {
        RegistryEntry::Opening {
            binding,
            verified_locator,
            path,
            expected_format,
        }
        | RegistryEntry::Ready {
            binding,
            verified_locator,
            path,
            expected_format,
            ..
        }
        | RegistryEntry::Closing {
            binding,
            verified_locator,
            path,
            expected_format,
            ..
        }
        | RegistryEntry::Faulted {
            binding,
            verified_locator,
            path,
            expected_format,
            ..
        } => (binding, verified_locator, path, *expected_format),
    }
}

pub(super) fn entry_binding(entry: &RegistryEntry) -> &StoreRuntimeBindingV1 {
    binding(entry).0
}

pub(super) fn require_binding(
    registered: IdentityRef<'_>,
    requested: IdentityRef<'_>,
) -> Result<(), GraphDbError> {
    let (registered_binding, registered_locator, registered_path, registered_format) = registered;
    let (requested_binding, requested_locator, requested_path, requested_format) = requested;
    if registered_binding != requested_binding
        || registered_locator != requested_locator
        || registered_path != requested_path
        || registered_format != requested_format
    {
        Err(GraphDbError::Conflict)
    } else {
        Ok(())
    }
}

pub(super) fn require_closing(
    entry: &RegistryEntry,
    reservation: &Eviction,
) -> Result<(), GraphDbError> {
    let RegistryEntry::Closing {
        binding,
        verified_locator,
        path,
        expected_format,
        owner,
    } = entry
    else {
        return Err(GraphDbError::unavailable(
            "graph close reservation was replaced",
        ));
    };
    if binding != &reservation.binding
        || verified_locator != &reservation.verified_locator
        || path.as_path() != reservation.path
        || expected_format != &reservation.expected_format
        || !Arc::ptr_eq(owner, &reservation.owner)
    {
        return Err(GraphDbError::unavailable(
            "graph close reservation identity changed",
        ));
    }
    Ok(())
}

pub(super) fn validate_registration(
    registration: &GraphDbRegistration,
) -> Result<(), GraphDbError> {
    if !matches!(
        registration.binding.shard_id.scope,
        StoreShardScopeV1::Project { .. }
    ) {
        return Err(GraphDbError::invalid(
            "graph registry requires the canonical project store runtime binding",
        ));
    }
    if registration.verified_locator.shard_id != registration.binding.shard_id
        || registration.verified_locator.incarnation != registration.binding.incarnation
    {
        return Err(GraphDbError::invalid(
            "verified graph locator does not match the runtime binding",
        ));
    }
    Ok(())
}
