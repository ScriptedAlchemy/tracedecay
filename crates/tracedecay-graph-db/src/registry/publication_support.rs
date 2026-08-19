use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use tracedecay_store::runtime::{
    GraphGenerationIdV1, GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1,
    GraphPublicationIdempotencyKeyV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationReplayCursorV1, GraphPublicationReplayLookupV1, GraphPublicationReplayRecordV1,
    GraphPublicationStoreErrorV1, GraphVerifiedHeadV1, RuntimeInterruptionV1,
};
use tracedecay_store::{StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use super::path::canonical_graph_database_file;
use super::{GraphDbRegistration, GraphDbRegistry, check_registration_request};
use crate::lease::{GenerationLocator, VerifiedGenerationLease, VerifiedGraphSnapshot};
use crate::{
    GraphDb, GraphDbError, GraphDbLeaseV1, GraphGenerationDependency, GraphGenerationId,
    GraphGenerationManifest, GraphIdempotencyKey, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity,
};

/// Registry-validated graph operation capability.
///
/// It retains the one ordinary graph lease issued for the operation and its
/// exact mounted Store binding. Lease-derived operations never reconstruct a
/// `GraphDbRegistration`; the registry proves the lease token belongs to its
/// current owner before constructing this capability.
pub(super) struct RegisteredGraphDbOperationV1 {
    database: GraphDbLeaseV1,
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
    request: Option<GraphDbRegistration>,
}

impl RegisteredGraphDbOperationV1 {
    pub(super) fn database(&self) -> &GraphDbLeaseV1 {
        &self.database
    }

    pub(super) fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    pub(super) fn check(
        &self,
        registry: &GraphDbRegistry,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> Result<(), GraphDbError> {
        if let Some(request) = &self.request {
            check_registration_request(request)?;
        }
        let current = registry.registered_operation_with_lease(&self.database)?;
        if current.binding != self.binding
            || current.verified_locator != self.verified_locator
            || current.canonical_path != self.canonical_path
        {
            return Err(GraphDbError::Conflict);
        }
        check_context(context)
    }

    pub(super) fn require_publication_binding(
        &self,
        key: &GraphPublicationKeyV1,
    ) -> Result<(), GraphDbError> {
        self.require_projection_binding(&key.projection)
    }

    pub(super) fn require_projection_binding(
        &self,
        projection: &GraphProjectionIdentityV1,
    ) -> Result<(), GraphDbError> {
        if projection.shard_id != self.binding.shard_id {
            return Err(GraphDbError::Conflict);
        }
        Ok(())
    }
}

impl GraphDbRegistry {
    pub(super) fn registered_operation(
        &self,
        registration: GraphDbRegistration,
    ) -> Result<RegisteredGraphDbOperationV1, GraphDbError> {
        check_registration_request(&registration)?;
        let binding = registration.binding().clone();
        let verified_locator = registration.verified_locator().clone();
        // `check` compares this against the registry entry, which stores the
        // file's canonical name. Capture the same canonical form here so a
        // lease that spells the identical file through a symlinked ancestor
        // (macOS `/var` -> `/private/var`) is not refused as a conflict.
        let canonical_path = canonical_graph_database_file(registration.canonical_path())?;
        let database = self.registered_database(&registration)?;
        Ok(RegisteredGraphDbOperationV1 {
            database,
            binding,
            verified_locator,
            canonical_path,
            request: Some(registration),
        })
    }

    pub(super) fn registered_operation_with_lease(
        &self,
        database: &GraphDbLeaseV1,
    ) -> Result<RegisteredGraphDbOperationV1, GraphDbError> {
        let lease_owner_id = database.owner_id();
        let mut state = self.state_lock()?;
        for entry in state.entries.values_mut() {
            match entry {
                super::RegistryEntry::Ready {
                    binding,
                    verified_locator,
                    path,
                    expected_format: _expected_format,
                    owner,
                    last_used,
                } if owner.owns_lease(database) => {
                    *last_used = std::time::Instant::now();
                    return Ok(RegisteredGraphDbOperationV1 {
                        database: database.clone(),
                        binding: binding.clone(),
                        verified_locator: verified_locator.clone(),
                        canonical_path: path.clone(),
                        request: None,
                    });
                }
                super::RegistryEntry::Closing { owner_id, .. }
                | super::RegistryEntry::Retiring { owner_id, .. }
                    if *owner_id == lease_owner_id =>
                {
                    return Err(GraphDbError::Conflict);
                }
                super::RegistryEntry::Faulted {
                    owner: Some(owner),
                    error,
                    ..
                } if owner.owns_lease(database) => return Err(error.clone()),
                _ => {}
            }
        }
        Err(GraphDbError::unavailable(
            "graph lease is not retained by this registry's mounted owner",
        ))
    }

    pub(super) fn registered_database(
        &self,
        registration: &GraphDbRegistration,
    ) -> Result<GraphDbLeaseV1, GraphDbError> {
        let canonical_path = canonical_graph_database_file(registration.canonical_path())?;
        let mut state = self.state_lock()?;
        let entry = state
            .entries
            .get_mut(&registration.binding().shard_id)
            .ok_or_else(|| GraphDbError::unavailable("graph runtime is not registered"))?;
        match entry {
            super::RegistryEntry::Ready {
                binding,
                verified_locator,
                path,
                expected_format,
                owner,
                last_used,
                ..
            } => {
                super::require_binding(
                    (binding, verified_locator, path, *expected_format),
                    (
                        registration.binding(),
                        registration.verified_locator(),
                        &canonical_path,
                        crate::GraphFormatVersion::current(),
                    ),
                )?;
                *last_used = std::time::Instant::now();
                owner.issue_registered_lease(registration)
            }
            super::RegistryEntry::Faulted {
                binding,
                verified_locator,
                path,
                expected_format,
                error,
                ..
            } => {
                super::require_binding(
                    (binding, verified_locator, path, *expected_format),
                    (
                        registration.binding(),
                        registration.verified_locator(),
                        &canonical_path,
                        crate::GraphFormatVersion::current(),
                    ),
                )?;
                Err(error.clone())
            }
            _ => Err(GraphDbError::unavailable(
                "graph runtime is not ready for verified reads",
            )),
        }
    }

    pub fn verified_snapshot(
        &self,
        registration: GraphDbRegistration,
        projection: &GraphProjectionIdentity,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        check_registration_request(&registration)?;
        let database = self.registered_database(&registration)?;
        let state = database.inner.verified_generations.read().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        let head = state.head(projection).ok_or_else(|| {
            GraphDbError::unavailable(
                "graph projection is not recovered into an installed verified head",
            )
        })?;
        drop(state);
        let mut closure = BTreeMap::new();
        collect_closure(&head, &mut closure)?;
        Ok(VerifiedGraphSnapshot::new(database, head, closure))
    }
}

pub(super) fn dependency_key_for_binding(
    binding: &StoreRuntimeBindingV1,
    dependency: &GraphGenerationDependency,
) -> Result<GraphPublicationKeyV1, GraphDbError> {
    Ok(GraphPublicationKeyV1::new(
        GraphProjectionIdentityV1 {
            shard_id: binding.shard_id.clone(),
            namespace: GraphNamespaceV1::new(dependency.projection.namespace.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(dependency.projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        },
        GraphGenerationIdV1::new(dependency.generation.as_str())
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        GraphPublicationIdempotencyKeyV1::new(dependency.idempotency_key.as_str())
            .map_err(|error| GraphDbError::invalid(error.to_string()))?,
    ))
}

pub(super) fn locator_from_key(
    key: &GraphPublicationKeyV1,
) -> Result<GenerationLocator, GraphDbError> {
    Ok(GenerationLocator::new(
        GraphProjectionIdentity::new(
            GraphNamespace::new(key.projection.namespace.as_str())?,
            GraphProjectionId::new(key.projection.projection.as_str())?,
        ),
        GraphGenerationId::new(key.generation.as_str())?,
    ))
}

pub(super) fn locator_from_dependency(
    registration: &GraphDbRegistration,
    dependency: &tracedecay_store::runtime::GraphDependencyGenerationIdentityV1,
) -> Result<GenerationLocator, GraphDbError> {
    if dependency.projection.shard_id != registration.binding().shard_id {
        return Err(GraphDbError::Conflict);
    }
    Ok(GenerationLocator::new(
        GraphProjectionIdentity::new(
            GraphNamespace::new(dependency.projection.namespace.as_str())?,
            GraphProjectionId::new(dependency.projection.projection.as_str())?,
        ),
        GraphGenerationId::new(dependency.generation.as_str())?,
    ))
}

pub(super) fn retain_lease_closure(
    lease: &Arc<VerifiedGenerationLease>,
    retained: &mut BTreeSet<GenerationLocator>,
) {
    if !retained.insert(lease.locator.clone()) {
        return;
    }
    for dependency in lease.dependencies.values() {
        retain_lease_closure(dependency, retained);
    }
}

pub(super) fn clear_retiring_fence(
    database: &GraphDb,
    locator: &GenerationLocator,
) -> Result<(), GraphDbError> {
    database
        .inner
        .verified_generations
        .write()
        .map_err(|_| GraphDbError::unavailable("verified graph generation state lock is poisoned"))?
        .retiring
        .remove(locator);
    Ok(())
}

pub(super) fn require_head_replay(
    head: &GraphVerifiedHeadV1,
    replay: &GraphPublicationReplayRecordV1,
) -> Result<(), GraphDbError> {
    let reconstructed = GraphVerifiedHeadV1::from_replay(
        replay,
        replay.publication.expected_recovered_digest.clone(),
    )
    .map_err(|error| GraphDbError::Corrupt {
        message: format!("verified graph replay evidence is invalid: {error}"),
    })?;
    if &reconstructed != head {
        return Err(GraphDbError::Corrupt {
            message: "verified graph head does not match its immutable replay".to_owned(),
        });
    }
    Ok(())
}

pub(super) fn require_active_replay_evidence(
    replay: GraphPublicationReplayLookupV1,
    missing_message: &str,
) -> Result<GraphPublicationReplayRecordV1, GraphDbError> {
    match replay {
        GraphPublicationReplayLookupV1::Active(replay) => Ok(replay),
        GraphPublicationReplayLookupV1::Retired(_) | GraphPublicationReplayLookupV1::Missing => {
            Err(GraphDbError::Corrupt {
                message: missing_message.to_owned(),
            })
        }
    }
}

pub(super) fn collect_closure(
    lease: &Arc<VerifiedGenerationLease>,
    closure: &mut BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>,
) -> Result<(), GraphDbError> {
    if let Some(existing) = closure.get(&lease.locator.projection) {
        if existing.locator != lease.locator {
            return Err(GraphDbError::Corrupt {
                message: "verified snapshot pins conflicting generations of one projection"
                    .to_owned(),
            });
        }
        return Ok(());
    }
    closure.insert(lease.locator.projection.clone(), Arc::clone(lease));
    for dependency in lease.dependencies.values() {
        collect_closure(dependency, closure)?;
    }
    Ok(())
}

pub(super) fn validate_exact_dependency_closure(
    manifest: &GraphGenerationManifest,
    loaded: &BTreeMap<GraphProjectionIdentity, Arc<VerifiedGenerationLease>>,
) -> Result<(), GraphDbError> {
    let declared = manifest
        .dependencies
        .iter()
        .map(|dependency| {
            (
                dependency.projection.clone(),
                (
                    dependency.generation.clone(),
                    dependency.idempotency_key.clone(),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut observed = BTreeMap::new();
    for lease in loaded.values() {
        let idempotency_key = GraphIdempotencyKey::new(lease.head.key.idempotency_key.as_str())?;
        let evidence = (lease.locator.generation.clone(), idempotency_key);
        if let Some(existing) = observed.insert(lease.locator.projection.clone(), evidence.clone())
            && existing != evidence
        {
            return Err(GraphDbError::Conflict);
        }
    }
    if observed != declared {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

pub(super) fn require_publication_binding(
    registration: &GraphDbRegistration,
    key: &GraphPublicationKeyV1,
) -> Result<(), GraphDbError> {
    require_projection_binding(registration, &key.projection)
}

pub(super) fn require_projection_binding(
    registration: &GraphDbRegistration,
    projection: &GraphProjectionIdentityV1,
) -> Result<(), GraphDbError> {
    if projection.shard_id != registration.binding().shard_id {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

pub(super) fn validate_replay_cursor(
    projection: &GraphProjectionIdentityV1,
    previous: Option<&GraphPublicationReplayCursorV1>,
    continuation: &GraphPublicationReplayCursorV1,
    subject: &str,
) -> Result<(), GraphDbError> {
    if continuation.projection != *projection {
        return Err(GraphDbError::Corrupt {
            message: format!("{subject} cursor escaped its projection"),
        });
    }
    if previous.is_some_and(|previous| continuation.sequence <= previous.sequence) {
        return Err(GraphDbError::Corrupt {
            message: format!("{subject} cursor did not advance"),
        });
    }
    Ok(())
}

pub(super) fn check_all(
    registration: &GraphDbRegistration,
    context: &GraphPublicationOperationContextV1<'_>,
) -> Result<(), GraphDbError> {
    check_registration_request(registration)?;
    check_context(context)
}

pub(super) fn check_context(
    context: &GraphPublicationOperationContextV1<'_>,
) -> Result<(), GraphDbError> {
    if let Some(error) = interruption_error(context) {
        return Err(error);
    }
    Ok(())
}

fn interruption_error(context: &GraphPublicationOperationContextV1<'_>) -> Option<GraphDbError> {
    context
        .interruption()
        .map(|interruption| match interruption {
            RuntimeInterruptionV1::Cancelled => GraphDbError::Cancelled,
            RuntimeInterruptionV1::DeadlineExceeded => GraphDbError::DeadlineExceeded,
        })
}

pub(super) fn map_publication_error(error: GraphPublicationStoreErrorV1) -> GraphDbError {
    match error {
        GraphPublicationStoreErrorV1::InvalidRequest(error) => {
            GraphDbError::invalid(error.to_string())
        }
        GraphPublicationStoreErrorV1::Interrupted(RuntimeInterruptionV1::Cancelled) => {
            GraphDbError::Cancelled
        }
        GraphPublicationStoreErrorV1::Interrupted(RuntimeInterruptionV1::DeadlineExceeded) => {
            GraphDbError::DeadlineExceeded
        }
        GraphPublicationStoreErrorV1::Infrastructure => {
            GraphDbError::unavailable("relational graph publication authority is unavailable")
        }
        GraphPublicationStoreErrorV1::Corrupt(message) => GraphDbError::Corrupt { message },
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_store::runtime::{
        BrainId, GraphNamespaceV1, GraphProjectionIdV1, GraphPublicationReplayCursorV1,
        GraphPublicationSequenceV1, ProjectId, StoreShardIdV1, UserProfileId,
    };

    use super::{GraphProjectionIdentityV1, validate_replay_cursor};

    fn projection(project: &str) -> GraphProjectionIdentityV1 {
        GraphProjectionIdentityV1 {
            shard_id: StoreShardIdV1::project(
                BrainId::new("brain.cursor").unwrap(),
                UserProfileId::new("profile.cursor").unwrap(),
                ProjectId::new(project).unwrap(),
            ),
            namespace: GraphNamespaceV1::new("code").unwrap(),
            projection: GraphProjectionIdV1::new("generation").unwrap(),
        }
    }

    #[test]
    fn replay_cursor_rejects_foreign_projection() {
        let expected = projection("project.expected");
        let continuation = GraphPublicationReplayCursorV1::new(
            projection("project.foreign"),
            GraphPublicationSequenceV1::new(1).unwrap(),
        )
        .unwrap();

        assert!(validate_replay_cursor(&expected, None, &continuation, "cleanup").is_err());
    }

    #[test]
    fn replay_cursor_rejects_nonadvancing_sequence() {
        let expected = projection("project.expected");
        let previous = GraphPublicationReplayCursorV1::new(
            expected.clone(),
            GraphPublicationSequenceV1::new(2).unwrap(),
        )
        .unwrap();
        let continuation = previous.clone();

        assert!(
            validate_replay_cursor(&expected, Some(&previous), &continuation, "cleanup").is_err()
        );
    }
}
