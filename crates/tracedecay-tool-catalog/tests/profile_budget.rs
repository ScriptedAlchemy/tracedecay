mod common;

use std::collections::BTreeSet;

use tracedecay_tool_catalog::{
    BindingId, BindingStatus, BindingSurface, CatalogContributionInputV1, CatalogContributionV1,
    CatalogSnapshotBuilderV1, CatalogValidationError, ContributionId, ProfileBudget,
    ProfileDefinition, ProfileDefinitionInputV1, ProfileKind, ProtocolRevisionRange,
    RoutingFixtureExpectation, RoutingFixtureV1, SurfaceBindingInputV1, SurfaceBindingV1,
    SurfaceOperationName,
};

use common::{
    ample_budget, capability_id, handler_for, profile, profile_id, read_manifest, schema,
    use_case_id,
};

#[test]
fn profile_budgets_reject_overflow_without_a_universal_tool_ceiling() {
    let profile_id = profile_id("profile.host-limited");
    let capability_id = capability_id("capability.source.read");
    let first_binding_id = BindingId::new("binding.source.read.cli").unwrap();
    let second_binding_id = BindingId::new("binding.source.read.alias").unwrap();
    let manifest = read_manifest(
        capability_id.clone(),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        vec![first_binding_id.clone(), second_binding_id.clone()],
        vec![profile_id.clone()],
    );
    let first_binding = SurfaceBindingV1::new(SurfaceBindingInputV1 {
        binding_id: first_binding_id.clone(),
        capability_id: capability_id.clone(),
        surface: BindingSurface::Cli,
        operation: SurfaceOperationName::new("source read").unwrap(),
        protocol_revisions: ProtocolRevisionRange::new(1, 1).unwrap(),
        required_features: Vec::new(),
        status: BindingStatus::Current,
        alias_of: None,
    })
    .unwrap();
    let second_binding = SurfaceBindingV1::new(SurfaceBindingInputV1 {
        binding_id: second_binding_id,
        capability_id: capability_id.clone(),
        surface: BindingSurface::Cli,
        operation: SurfaceOperationName::new("source get").unwrap(),
        protocol_revisions: ProtocolRevisionRange::new(1, 1).unwrap(),
        required_features: Vec::new(),
        status: BindingStatus::Current,
        alias_of: Some(first_binding_id),
    })
    .unwrap();
    let profile = ProfileDefinition::new(ProfileDefinitionInputV1 {
        profile_id: profile_id.clone(),
        kind: ProfileKind::HostLimited,
        capability_ids: vec![capability_id.clone()],
        enabled_surfaces: vec![BindingSurface::Cli],
        requires_cli_mcp_pairing: false,
        budget: ProfileBudget::new(1, 100_000).unwrap(),
        routing_fixtures: vec![
            RoutingFixtureV1::new(
                "read source",
                RoutingFixtureExpectation::Select {
                    capability_id: capability_id.clone(),
                },
            )
            .unwrap(),
            RoutingFixtureV1::new("do nothing", RoutingFixtureExpectation::Reject).unwrap(),
        ],
    })
    .unwrap();
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: vec![first_binding, second_binding],
    })
    .unwrap();
    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(contribution)
        .add_handler(handler_for(&manifest))
        .add_profile(profile);

    assert_eq!(
        builder.build(),
        Err(CatalogValidationError::ProfileBudgetExceeded {
            profile_id,
            budget: "bindings",
            actual: 2,
            maximum: 1,
        })
    );
}

#[test]
fn profile_absence_is_explicit_in_snapshot_discovery() {
    let primary_profile_id = profile_id("profile.default");
    let compact_profile_id = profile_id("profile.compact");
    let capability_id = capability_id("capability.source.outline");
    let manifest = read_manifest(
        capability_id.clone(),
        use_case_id("use-case.source.outline"),
        schema("schema.source.outline.request"),
        schema("schema.source.outline.result"),
        Vec::new(),
        vec![primary_profile_id.clone()],
    );
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let compact_profile = ProfileDefinition::new(ProfileDefinitionInputV1 {
        profile_id: compact_profile_id.clone(),
        kind: ProfileKind::Compact,
        capability_ids: Vec::new(),
        enabled_surfaces: Vec::new(),
        requires_cli_mcp_pairing: false,
        budget: ample_budget(),
        routing_fixtures: Vec::new(),
    })
    .unwrap();
    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(contribution)
        .add_handler(handler_for(&manifest))
        .add_profile(profile(
            primary_profile_id,
            vec![capability_id],
            ample_budget(),
        ))
        .add_profile(compact_profile);
    let snapshot = builder.build().unwrap();

    assert!(
        snapshot
            .visible_capabilities(&compact_profile_id, &BTreeSet::new())
            .is_empty()
    );
}

#[test]
fn paired_profiles_reject_capabilities_without_cli_and_mcp_bindings() {
    let profile_id = profile_id("profile.default");
    let capability_id = capability_id("capability.source.outline");
    let manifest = read_manifest(
        capability_id.clone(),
        use_case_id("use-case.source.outline"),
        schema("schema.source.outline.request"),
        schema("schema.source.outline.result"),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let profile = ProfileDefinition::new(ProfileDefinitionInputV1 {
        profile_id: profile_id.clone(),
        kind: ProfileKind::Default,
        capability_ids: vec![capability_id.clone()],
        enabled_surfaces: vec![BindingSurface::Cli, BindingSurface::Mcp],
        requires_cli_mcp_pairing: true,
        budget: ample_budget(),
        routing_fixtures: vec![
            RoutingFixtureV1::new(
                "outline source",
                RoutingFixtureExpectation::Select {
                    capability_id: capability_id.clone(),
                },
            )
            .unwrap(),
            RoutingFixtureV1::new("do nothing", RoutingFixtureExpectation::Reject).unwrap(),
        ],
    })
    .unwrap();
    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(contribution)
        .add_handler(handler_for(&manifest))
        .add_profile(profile);

    assert_eq!(
        builder.build(),
        Err(CatalogValidationError::PairedProfileMissingBinding {
            profile_id,
            capability_id,
            surface: BindingSurface::Cli,
        })
    );
}
