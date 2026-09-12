//! Process-wide application catalog snapshot and catalog binding resolution.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID;
use tracedecay_contracts::catalog_composition::{
    CatalogCompositionError, build_application_catalog_snapshot,
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

/// Process-immutable application catalog snapshot.
///
/// The catalog is composed entirely from `const` application specs, so nothing
/// about it can change while the process runs. Composition still collects and
/// sorts every contribution, validates the handler/contribution bijection, and
/// derives all four profiles, so rebuilding it per call made a single dispatch
/// pay for the whole pipeline twice: once to resolve the binding and again to
/// re-validate it before execution. The snapshot is built once here and
/// borrowed from then on; the per-call binding identity comparison in
/// [`validate_current_application_binding`] is unchanged and now runs against
/// this cached snapshot.
pub(super) static APPLICATION_SURFACE_CATALOG: LazyLock<
    Result<CatalogSnapshotV1, CatalogCompositionError>,
> = LazyLock::new(build_application_catalog_snapshot);

/// Borrow the process-wide catalog snapshot without recomposing it.
pub fn application_surface_catalog_ref()
-> Result<&'static CatalogSnapshotV1, ApplicationSurfaceAdapterError> {
    match &*APPLICATION_SURFACE_CATALOG {
        Ok(catalog) => Ok(catalog),
        Err(error) => Err(ApplicationSurfaceAdapterError::Catalog(error.clone())),
    }
}

pub fn application_surface_catalog() -> Result<CatalogSnapshotV1, ApplicationSurfaceAdapterError> {
    application_surface_catalog_ref().cloned()
}

pub(super) fn resolve_application_binding(
    resolver: &impl BindingResolver,
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
) -> Option<tracedecay_daemon_protocol::ResolvedBinding> {
    resolve_named_binding(resolver, surface, operation.name_for_surface(surface))
}

pub(super) fn resolve_named_binding(
    resolver: &impl BindingResolver,
    surface: BindingSurface,
    operation: &str,
) -> Option<ResolvedBinding> {
    let profile_id = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).ok()?;
    let operation = SurfaceOperationName::new(operation).ok()?;
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

/// Resolves a public tool name through the application catalog for one host surface.
///
/// Typed application surfaces continue through [`ApplicationSurfaceOperation`];
/// compatibility-owned tools use this boundary before entering their retained
/// execution adapter, so catalog metadata remains the single binding authority.
#[hotpath::measure(label = "application_surface.catalog_binding")]
pub fn resolve_catalog_tool_binding(
    surface: BindingSurface,
    tool_name: &str,
) -> Result<Option<ResolvedBinding>, ApplicationSurfaceAdapterError> {
    let operation = tool_name.strip_prefix("tracedecay_").unwrap_or(tool_name);
    let catalog = application_surface_catalog_ref()?;
    let resolver = CatalogBindingResolver::new(catalog);
    Ok(resolve_named_binding(&resolver, surface, operation))
}

pub(super) fn application_negotiated_features() -> BTreeSet<FeatureId> {
    BTreeSet::new()
}

pub(super) fn validate_current_application_binding(
    operation: ApplicationSurfaceOperation,
    dispatched: &DispatchedInvocation<ApplicationSurfaceRequest>,
) -> Result<(), ApplicationSurfaceAdapterError> {
    let catalog = application_surface_catalog_ref()?;
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
