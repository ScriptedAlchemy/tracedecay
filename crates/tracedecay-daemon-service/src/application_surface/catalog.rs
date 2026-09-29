//! Process-wide application catalog snapshot and catalog binding resolution.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID;
use tracedecay_contracts::catalog_composition::{
    CatalogCompositionError, application_catalog_snapshot, build_application_binding_snapshot,
};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceRequest, BindingResolution, BindingResolver,
    CatalogBindingResolver, DispatchedInvocation, ResolvedBinding,
};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingSurface, CatalogSnapshotV1, FeatureId, ProfileId,
    SurfaceOperationName,
};

use super::APPLICATION_PROTOCOL_REVISION;

/// Dispatch snapshot. Bindings and capability contracts, no JSON Schema bodies.
///
/// The process-wide full snapshot ([`application_catalog_snapshot`]) stays
/// the authority for SDK projection, MCP discovery, configuration schema
/// checks, and context-scout digest. Resolving one CLI call must not generate
/// those bodies.
static APPLICATION_BINDING_CATALOG: LazyLock<Result<CatalogSnapshotV1, CatalogCompositionError>> =
    LazyLock::new(build_application_binding_snapshot);

/// Borrow the process-wide catalog snapshot without recomposing it.
pub fn application_surface_catalog_ref()
-> Result<&'static CatalogSnapshotV1, ApplicationSurfaceAdapterError> {
    application_catalog_snapshot()
        .map(|snapshot| &**snapshot)
        .map_err(ApplicationSurfaceAdapterError::Catalog)
}

/// Borrow the schema-body-free snapshot used to resolve and execute a call.
pub fn application_surface_binding_catalog_ref()
-> Result<&'static CatalogSnapshotV1, ApplicationSurfaceAdapterError> {
    catalog_ref(&APPLICATION_BINDING_CATALOG)
}

fn catalog_ref(
    catalog: &'static Result<CatalogSnapshotV1, CatalogCompositionError>,
) -> Result<&'static CatalogSnapshotV1, ApplicationSurfaceAdapterError> {
    match catalog {
        Ok(catalog) => Ok(catalog),
        Err(error) => Err(ApplicationSurfaceAdapterError::Catalog(error.clone())),
    }
}

/// Deadline ceiling recorded on the operation's capability manifest.
///
/// This is the same ceiling the MCP dispatch catalog copies from an executable
/// binding. Reading it here does not build that catalog or any schema body.
pub fn application_operation_deadline_ceiling(
    operation: ApplicationSurfaceOperation,
) -> Result<std::time::Duration, ApplicationSurfaceAdapterError> {
    let catalog = application_surface_binding_catalog_ref()?;
    let names = [
        operation.name_for_surface(BindingSurface::Cli),
        operation.name_for_surface(BindingSurface::Http),
        operation.as_str(),
        operation.mcp_operation_name(),
    ];
    let capability = catalog
        .capabilities()
        .find(|capability| {
            capability.binding_ids().iter().any(|binding_id| {
                catalog
                    .binding(binding_id)
                    .is_some_and(|binding| names.contains(&binding.operation().as_str()))
            })
        })
        .ok_or_else(|| {
            ApplicationSurfaceAdapterError::invalid_request(format!(
                "application operation {} has no catalog deadline",
                operation.as_str()
            ))
        })?;
    Ok(std::time::Duration::from_millis(
        capability.deadline().maximum_millis(),
    ))
}

pub fn application_surface_catalog() -> Result<CatalogSnapshotV1, ApplicationSurfaceAdapterError> {
    application_surface_catalog_ref().cloned()
}

pub(super) fn resolve_application_binding(
    resolver: &impl BindingResolver,
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
) -> Option<ResolvedBinding> {
    let profile_id = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).ok()?;
    let operation = SurfaceOperationName::new(operation.name_for_surface(surface)).ok()?;
    resolver.resolve_binding(
        surface,
        &BindingResolution {
            profile_id,
            operation,
            protocol_revision: APPLICATION_PROTOCOL_REVISION,
            negotiated_features: application_negotiated_features(),
        },
    )
}

pub(super) fn application_negotiated_features() -> BTreeSet<FeatureId> {
    BTreeSet::new()
}

pub(super) fn validate_current_application_binding(
    operation: ApplicationSurfaceOperation,
    dispatched: &DispatchedInvocation<ApplicationSurfaceRequest>,
) -> Result<(), ApplicationSurfaceAdapterError> {
    let catalog = application_surface_binding_catalog_ref()?;
    let resolver = CatalogBindingResolver::new(catalog);
    let current = resolve_application_binding(&resolver, dispatched.surface, operation)
        .ok_or(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)?;
    if current.binding_id != dispatched.invocation.binding_id
        || current.request_schema != dispatched.invocation.request_schema
        || current.result_schema != dispatched.invocation.result_schema
        || !dispatched.invocation.invocation.request.matches(operation)
    {
        return Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized);
    }
    Ok(())
}
