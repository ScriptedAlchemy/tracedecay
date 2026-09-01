//! Live `tools/list` filtering against the root application-surface catalog.
//!
//! Catalog assembly lives in `tracedecay-mcp`. These wrappers stay in the
//! composition root because they name `application_surface` and attach
//! dispatch metadata from the daemon-coupled binding table.

use std::collections::BTreeSet;

use tracedecay_mcp::{
    ToolDefinition, ToolRegistryMode, apply_context_warming_budget,
    get_maximal_tool_definitions_with_budget, retain_host_available_tool_definitions,
};
use tracedecay_tool_catalog::{CapabilityId, FeatureId, ProfileId, ScopeDimension};

use super::dispatch::McpDispatchMetadataError;

/// Build the live MCP discovery result from the application catalog rather
/// than publishing the static compatibility registry as an unfiltered
/// superset.
#[hotpath::measure]
pub fn get_catalog_filtered_tool_definitions_with_budget(
    node_count: u64,
    budget: u8,
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
) -> Result<Vec<ToolDefinition>, McpDispatchMetadataError> {
    let catalog = crate::application_surface::application_surface_catalog_ref()?;
    let visible_operations = catalog
        .visible_bindings(
            profile_id,
            tracedecay_tool_catalog::BindingSurface::Mcp,
            1,
            &BTreeSet::<FeatureId>::new(),
            authorized_capabilities,
            available_scope,
        )
        .into_iter()
        .map(|(binding, _)| format!("tracedecay_{}", binding.operation().as_str()))
        .collect::<BTreeSet<_>>();
    let catalog_operations = catalog
        .capabilities()
        .flat_map(tracedecay_tool_catalog::CapabilityManifestV1::binding_ids)
        .filter_map(|binding_id| catalog.binding(binding_id))
        .filter(|binding| binding.surface() == tracedecay_tool_catalog::BindingSurface::Mcp)
        .map(|binding| format!("tracedecay_{}", binding.operation().as_str()))
        .collect::<BTreeSet<_>>();
    let mut definitions = get_maximal_tool_definitions_with_budget(node_count, budget)?;
    if registry_mode == ToolRegistryMode::HostAvailable {
        retain_host_available_tool_definitions(&mut definitions);
    }
    let mut definitions = definitions
        .into_iter()
        .filter(|definition| {
            !catalog_operations.contains(&definition.name)
                || visible_operations.contains(&definition.name)
        })
        .collect::<Vec<_>>();
    super::dispatch::attach_dispatch_metadata(&mut definitions)?;
    Ok(definitions)
}

#[hotpath::measure]
pub fn get_catalog_filtered_tool_definitions_with_warming_budget(
    budget: u8,
    profile_id: &ProfileId,
    authorized_capabilities: &BTreeSet<CapabilityId>,
    available_scope: &BTreeSet<ScopeDimension>,
    registry_mode: ToolRegistryMode,
) -> Result<Vec<ToolDefinition>, McpDispatchMetadataError> {
    let mut definitions = get_catalog_filtered_tool_definitions_with_budget(
        0,
        budget,
        profile_id,
        authorized_capabilities,
        available_scope,
        registry_mode,
    )?;
    apply_context_warming_budget(&mut definitions, budget);
    Ok(definitions)
}

#[hotpath::measure]
pub fn default_catalog_discovery_authority()
-> Result<BTreeSet<CapabilityId>, crate::application_surface::ApplicationSurfaceAdapterError> {
    Ok(
        crate::application_surface::application_surface_catalog_ref()?
            .capabilities()
            .map(|capability| capability.capability_id().clone())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_mcp::{explore_call_budget, project_catalog_discovery_scope};

    #[test]
    fn catalog_filtered_discovery_uses_the_deterministic_maximal_registry() {
        let profile_id = ProfileId::new(tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID)
            .expect("default profile");
        let definitions = get_catalog_filtered_tool_definitions_with_budget(
            0,
            explore_call_budget(0),
            &profile_id,
            &default_catalog_discovery_authority().expect("default discovery authority"),
            &project_catalog_discovery_scope(),
            ToolRegistryMode::DeterministicMaximal,
        )
        .expect("catalog-filtered definitions");

        let source_edit = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_ast_grep_rewrite")
            .expect("available source-edit handler is advertised");
        let source_edit_dispatch = &source_edit.meta.as_ref().unwrap()["tracedecay/dispatch"];
        assert_eq!(source_edit_dispatch["effect"], "source_edit");
        assert_eq!(source_edit_dispatch["availability"]["state"], "available");
        assert_eq!(source_edit_dispatch["idempotency"], "key_required");

        let fingerprints = definitions
            .iter()
            .map(|definition| {
                let dispatch = &definition.meta.as_ref().unwrap()["tracedecay/dispatch"];
                assert_eq!(dispatch["version"], 1);
                assert_eq!(
                    definition.annotations.as_ref().unwrap()["readOnlyHint"],
                    dispatch["read_only"]
                );
                assert!(dispatch["deadline"]["maximum_millis"].as_u64().unwrap() > 0);
                dispatch["fingerprint"].as_str().unwrap()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            fingerprints.len(),
            1,
            "one catalog snapshot must fingerprint every advertised contract"
        );

        let dashboard = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_dashboard")
            .unwrap();
        let dispatch = &dashboard.meta.as_ref().unwrap()["tracedecay/dispatch"];
        assert_eq!(dispatch["effect"], "administrative");
        assert_eq!(dispatch["availability"]["state"], "available");
        assert_eq!(dispatch["idempotency"], "idempotent");
        assert_eq!(dispatch["inverse"]["mode"], "same_tool");

        let doctor = definitions
            .iter()
            .find(|definition| definition.name == "tracedecay_lcm_doctor")
            .unwrap();
        let dispatch = &doctor.meta.as_ref().unwrap()["tracedecay/dispatch"];
        assert_eq!(dispatch["effect"], "read");
        assert_eq!(dispatch["availability"]["state"], "available");
        assert!(dispatch.get("receipt").is_none());
        assert!(dispatch.get("reconciliation").is_none());

        for retired in [
            "tracedecay_lcm_preflight",
            "tracedecay_lcm_compress",
            "tracedecay_lcm_session_boundary",
        ] {
            assert!(
                definitions
                    .iter()
                    .all(|definition| definition.name != retired),
                "{retired} must remain daemon-internal"
            );
        }
    }

    #[test]
    fn catalog_filter_preserves_non_catalog_tools_and_filters_catalog_bindings() {
        let profile =
            ProfileId::new(tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID).unwrap();
        let definitions = get_catalog_filtered_tool_definitions_with_budget(
            10_000,
            4,
            &profile,
            &BTreeSet::new(),
            &project_catalog_discovery_scope(),
            ToolRegistryMode::HostAvailable,
        )
        .unwrap();

        assert!(
            definitions
                .iter()
                .any(|definition| definition.name == "tracedecay_search"),
            "legacy production tools remain discoverable until cataloged"
        );
        assert!(
            definitions.iter().all(|definition| {
                definition.name != "tracedecay_context"
                    && definition.name != "tracedecay_git_preview"
            }),
            "catalog-bound tools require explicit capability authority"
        );
    }
}
