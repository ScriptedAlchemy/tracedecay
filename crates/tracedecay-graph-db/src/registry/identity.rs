use std::path::Path;
use std::sync::Arc;

use tracedecay_store::{
    StoreRuntimeBindingV1, StoreShardScopeV1, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
};

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
            ..
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
        | RegistryEntry::Retiring {
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
        authority_lease,
        binding,
        verified_locator,
        path,
        expected_format,
        owner_id,
        owner_attachment_id,
        reservation_id,
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
        || owner_id != &reservation.owner_id()
        || owner_attachment_id != &reservation.owner_attachment_id()
        || reservation_id != &reservation.reservation_id()
        || !Arc::ptr_eq(authority_lease, &reservation.authority_lease)
    {
        return Err(GraphDbError::unavailable(
            "graph close reservation identity changed",
        ));
    }
    Ok(())
}

pub(super) fn require_retiring(
    entry: &RegistryEntry,
    reservation: &Eviction,
) -> Result<(), GraphDbError> {
    let RegistryEntry::Retiring {
        authority_lease,
        binding,
        verified_locator,
        path,
        expected_format,
        owner_id,
        owner_attachment_id,
        reservation_id,
    } = entry
    else {
        return Err(GraphDbError::unavailable(
            "graph retirement reservation was replaced",
        ));
    };
    if binding != &reservation.binding
        || verified_locator != &reservation.verified_locator
        || path.as_path() != reservation.path
        || expected_format != &reservation.expected_format
        || owner_id != &reservation.owner_id()
        || owner_attachment_id
            != &reservation.owner_attachment_id().ok_or_else(|| {
                GraphDbError::unavailable("graph retirement reservation lacks an owner attachment")
            })?
        || reservation_id != &reservation.reservation_id()
        || !Arc::ptr_eq(authority_lease, &reservation.authority_lease)
    {
        return Err(GraphDbError::unavailable(
            "graph retirement reservation identity changed",
        ));
    }
    Ok(())
}

pub(super) fn validate_registration(
    registration: &GraphDbRegistration,
) -> Result<(), GraphDbError> {
    let binding = registration.binding();
    let verified_locator = registration.verified_locator();
    let canonical_path = registration.canonical_path();
    if !matches!(
        binding.shard_id.scope,
        StoreShardScopeV1::Project { .. }
            | StoreShardScopeV1::ProjectSessions { .. }
            | StoreShardScopeV1::ProfileMemory
            | StoreShardScopeV1::ProfileSessions
            | StoreShardScopeV1::Code { .. }
    ) {
        return Err(GraphDbError::invalid(
            "graph registry requires a canonical project, memory, session, or code runtime binding",
        ));
    }
    if verified_locator.shard_id != binding.shard_id
        || verified_locator.incarnation != binding.incarnation
    {
        return Err(GraphDbError::invalid(
            "verified graph locator does not match the runtime binding",
        ));
    }
    let digest = canonical_store_locator_digest(canonical_path)
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    if digest != verified_locator.locator_digest {
        return Err(GraphDbError::invalid(
            "verified graph locator digest does not bind the canonical graph path",
        ));
    }
    Ok(())
}
