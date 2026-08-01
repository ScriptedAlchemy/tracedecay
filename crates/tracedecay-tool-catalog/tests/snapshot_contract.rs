mod common;

use tracedecay_tool_catalog::{
    ApplicationHandlerDescriptorV1, CatalogContributionInputV1, CatalogContributionV1,
    CatalogSnapshotBuilderV1, CatalogValidationError, ContributionId, ProfileDefinition,
    ProfileDefinitionInputV1, ProfileKind, RoutingFixtureExpectation, RoutingFixtureV1,
};

use common::{
    ample_budget, capability_id, handler_for, profile, profile_id, read_manifest, schema,
    use_case_id,
};

#[test]
fn snapshots_have_insertion_order_independent_canonical_digests() {
    let profile_id = profile_id("profile.default");
    let first_capability_id = capability_id("capability.source.outline");
    let second_capability_id = capability_id("capability.symbol.search");
    let first_manifest = read_manifest(
        first_capability_id.clone(),
        use_case_id("use-case.source.outline"),
        schema("schema.source.outline.request"),
        schema("schema.source.outline.result"),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let second_manifest = read_manifest(
        second_capability_id.clone(),
        use_case_id("use-case.symbol.search"),
        schema("schema.symbol.search.request"),
        schema("schema.symbol.search.result"),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let source_contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![first_manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let symbol_contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.symbol").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![second_manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let profile = profile(
        profile_id,
        vec![first_capability_id, second_capability_id],
        ample_budget(),
    );

    let mut first_builder = CatalogSnapshotBuilderV1::new();
    first_builder
        .add_contribution(source_contribution.clone())
        .add_contribution(symbol_contribution.clone())
        .add_handler(handler_for(&first_manifest))
        .add_handler(handler_for(&second_manifest))
        .add_profile(profile.clone());
    let first_snapshot = first_builder.build().unwrap();

    let mut second_builder = CatalogSnapshotBuilderV1::new();
    second_builder
        .add_contribution(symbol_contribution)
        .add_contribution(source_contribution)
        .add_handler(handler_for(&second_manifest))
        .add_handler(handler_for(&first_manifest))
        .add_profile(profile);
    let second_snapshot = second_builder.build().unwrap();

    assert_eq!(first_snapshot.digest(), second_snapshot.digest());
}

#[test]
fn snapshots_canonicalize_routing_fixture_order_before_digesting() {
    let profile_id = profile_id("profile.default");
    let capability_id = capability_id("capability.source.read");
    let manifest = read_manifest(
        capability_id.clone(),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
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
    let fixtures = vec![
        RoutingFixtureV1::new("do nothing", RoutingFixtureExpectation::Reject).unwrap(),
        RoutingFixtureV1::new(
            "read source",
            RoutingFixtureExpectation::Select {
                capability_id: capability_id.clone(),
            },
        )
        .unwrap(),
    ];
    let make_profile = |routing_fixtures| {
        ProfileDefinition::new(ProfileDefinitionInputV1 {
            profile_id: profile_id.clone(),
            kind: ProfileKind::Default,
            capability_ids: vec![capability_id.clone()],
            enabled_surfaces: Vec::new(),
            requires_cli_mcp_pairing: false,
            budget: ample_budget(),
            routing_fixtures,
        })
        .unwrap()
    };

    let mut first_builder = CatalogSnapshotBuilderV1::new();
    first_builder
        .add_contribution(contribution.clone())
        .add_handler(handler_for(&manifest))
        .add_profile(make_profile(fixtures.clone()));

    let mut reversed_fixtures = fixtures;
    reversed_fixtures.reverse();
    let mut second_builder = CatalogSnapshotBuilderV1::new();
    second_builder
        .add_contribution(contribution)
        .add_handler(handler_for(&manifest))
        .add_profile(make_profile(reversed_fixtures));

    assert_eq!(
        first_builder.build().unwrap().digest(),
        second_builder.build().unwrap().digest()
    );
}

#[test]
fn snapshot_rejects_duplicate_capability_ids() {
    let profile_id = profile_id("profile.default");
    let capability_id = capability_id("capability.source.read");
    let manifest = read_manifest(
        capability_id.clone(),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let first = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.first").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let second = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.second").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(first)
        .add_contribution(second)
        .add_handler(handler_for(&manifest))
        .add_profile(profile(
            profile_id,
            vec![capability_id.clone()],
            ample_budget(),
        ));

    assert_eq!(
        builder.build(),
        Err(CatalogValidationError::DuplicateCapabilityId(capability_id))
    );
}

#[test]
fn snapshot_rejects_duplicate_contribution_ids_before_folding_records() {
    let profile_id = profile_id("profile.default");
    let capability_id = capability_id("capability.source.read");
    let manifest = read_manifest(
        capability_id.clone(),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let contribution_id = ContributionId::new("contribution.source").unwrap();
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: contribution_id.clone(),
        depends_on: Vec::new(),
        capabilities: vec![manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(contribution.clone())
        .add_contribution(contribution)
        .add_handler(handler_for(&manifest))
        .add_profile(profile(profile_id, vec![capability_id], ample_budget()));

    assert_eq!(
        builder.build(),
        Err(CatalogValidationError::DuplicateContributionId(
            contribution_id
        ))
    );
}

#[test]
fn snapshot_deduplicates_shared_schema_identity() {
    let profile_id = profile_id("profile.default");
    let first_capability_id = capability_id("capability.source.first");
    let second_capability_id = capability_id("capability.source.second");
    let first_manifest = read_manifest(
        first_capability_id.clone(),
        use_case_id("use-case.source.first"),
        schema("schema.source.shared.request"),
        schema("schema.source.first.result"),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let second_manifest = read_manifest(
        second_capability_id.clone(),
        use_case_id("use-case.source.second"),
        schema("schema.source.shared.request"),
        schema("schema.source.second.result"),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![first_manifest.clone(), second_manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(contribution)
        .add_handler(handler_for(&first_manifest))
        .add_handler(handler_for(&second_manifest))
        .add_profile(profile(
            profile_id,
            vec![first_capability_id, second_capability_id],
            ample_budget(),
        ));

    let snapshot = builder.build().unwrap();
    assert!(
        snapshot
            .schema(
                &tracedecay_tool_catalog::SchemaId::new("schema.source.shared.request").unwrap(),
                1
            )
            .is_some()
    );
}

#[test]
fn snapshot_rejects_handler_schema_drift() {
    let profile_id = profile_id("profile.default");
    let capability_id = capability_id("capability.source.body");
    let manifest = read_manifest(
        capability_id.clone(),
        use_case_id("use-case.source.body"),
        schema("schema.source.body.request"),
        schema("schema.source.body.result"),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source-body").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let stale_handler = ApplicationHandlerDescriptorV1::new(
        manifest.capability_id().clone(),
        manifest.use_case_id().clone(),
        manifest.request_schema().clone(),
        schema("schema.source.body.stale-result"),
    );
    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(contribution)
        .add_handler(stale_handler)
        .add_profile(profile(
            profile_id,
            vec![capability_id.clone()],
            ample_budget(),
        ));

    assert_eq!(
        builder.build(),
        Err(CatalogValidationError::HandlerSchemaMismatch { capability_id })
    );
}

#[test]
fn snapshot_rejects_handler_capability_drift() {
    let profile_id = profile_id("profile.default");
    let manifest_capability_id = capability_id("capability.source.body");
    let manifest = read_manifest(
        manifest_capability_id.clone(),
        use_case_id("use-case.source.body"),
        schema("schema.source.body.request"),
        schema("schema.source.body.result"),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source-body").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let handler_capability_id = capability_id("capability.source.lines");
    let stale_handler = ApplicationHandlerDescriptorV1::new(
        handler_capability_id.clone(),
        manifest.use_case_id().clone(),
        manifest.request_schema().clone(),
        manifest.result_schema().clone(),
    );
    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(contribution)
        .add_handler(stale_handler)
        .add_profile(profile(
            profile_id,
            vec![manifest_capability_id.clone()],
            ample_budget(),
        ));

    assert_eq!(
        builder.build(),
        Err(CatalogValidationError::HandlerCapabilityMismatch {
            capability_id: manifest_capability_id,
            handler_capability_id,
        })
    );
}
