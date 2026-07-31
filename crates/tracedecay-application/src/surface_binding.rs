//! Shared construction of the surface bindings every catalog contribution
//! declares.
//!
//! Each contribution module used to spell out the same `SurfaceBindingInputV1`
//! literal inside its own per-surface loop, so the wire spelling of a surface
//! and the default binding shape were both restated a dozen times.

use tracedecay_tool_catalog::{
    BindingId, BindingStatus, BindingSurface, CapabilityId, ProtocolRevisionRange,
    SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName,
};

use crate::error::ApplicationContractError;

/// The single wire spelling of a binding surface, as it appears inside every
/// `binding.{surface}.{operation}.v1` identifier this crate mints.
pub(crate) const fn surface_name(surface: BindingSurface) -> &'static str {
    match surface {
        BindingSurface::Cli => "cli",
        BindingSurface::Mcp => "mcp",
        BindingSurface::Http => "http",
        BindingSurface::Lsp => "lsp",
        BindingSurface::Dashboard => "dashboard",
    }
}

/// Bind one operation across `surfaces` with the default binding shape:
/// protocol revision 1, no required features, `Current` status, and no alias.
///
/// Returns the bindings alongside their ids in surface order, because callers
/// accumulate the bindings into the contribution while handing the ids to the
/// capability manifest. Operations that need a non-default status or feature
/// gate build their bindings directly instead.
pub(crate) fn current_bindings(
    capability_id: &CapabilityId,
    operation: &str,
    surfaces: impl IntoIterator<Item = BindingSurface>,
) -> Result<(Vec<SurfaceBindingV1>, Vec<BindingId>), ApplicationContractError> {
    current_bindings_with_slug(capability_id, operation, operation, surfaces)
}

/// [`current_bindings`] for the operations whose binding-id slug differs from
/// their wire operation name.
pub(crate) fn current_bindings_with_slug(
    capability_id: &CapabilityId,
    operation: &str,
    slug: &str,
    surfaces: impl IntoIterator<Item = BindingSurface>,
) -> Result<(Vec<SurfaceBindingV1>, Vec<BindingId>), ApplicationContractError> {
    let surfaces = surfaces.into_iter();
    let expected = surfaces.size_hint().0;
    let mut bindings = Vec::with_capacity(expected);
    let mut binding_ids = Vec::with_capacity(expected);
    for surface in surfaces {
        let binding_id = BindingId::new(format!("binding.{}.{slug}.v1", surface_name(surface)))?;
        bindings.push(SurfaceBindingV1::new(SurfaceBindingInputV1 {
            binding_id: binding_id.clone(),
            capability_id: capability_id.clone(),
            surface,
            operation: SurfaceOperationName::new(operation)?,
            protocol_revisions: ProtocolRevisionRange::new(1, 1)?,
            required_features: Vec::new(),
            status: BindingStatus::Current,
            alias_of: None,
        })?);
        binding_ids.push(binding_id);
    }
    Ok((bindings, binding_ids))
}
