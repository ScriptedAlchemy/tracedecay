use crate::common;

use std::collections::BTreeSet;

use schemars::JsonSchema;
use tracedecay_tool_catalog::{
    ApplicationHandlerDescriptorV1, BindingId, BindingStatus, BindingSurface,
    CatalogContributionInputV1, CatalogContributionV1, CatalogSnapshotBuilderV1,
    CatalogValidationError, ContributionId, ExecutableSchemaAuthority, ProfileDefinition,
    ProfileDefinitionInputV1, ProfileKind, ProtocolRevisionRange, SurfaceBindingInputV1,
    SurfaceBindingV1, SurfaceOperationName,
};

use common::{
    ample_budget, capability_id, handler_for, profile, profile_id, read_manifest, schema,
    use_case_id,
};

#[derive(JsonSchema)]
#[allow(dead_code)]
struct ReadRequest {
    path: String,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct ReadResult {
    contents: String,
}

/// The catalog digest is authorization-relevant identity: source-edit and git
/// effect proofs carry it and are refused on mismatch. This pin fails whenever
/// the canonical digest bytes of a fixed catalog drift, so an identity change
/// has to be a deliberate decision rather than a side effect of refactoring
/// the builder.
#[test]
fn snapshot_digest_bytes_are_pinned_for_a_fixed_catalog() {
    let profile_id = profile_id("profile.default");
    let read_capability_id = capability_id("capability.source.read");
    let search_capability_id = capability_id("capability.symbol.search");
    let read_binding_id = BindingId::new("binding.source.read.cli").unwrap();
    let read_capability = read_manifest(
        read_capability_id.clone(),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        vec![read_binding_id.clone()],
        vec![profile_id.clone()],
    );
    let search_capability = read_manifest(
        search_capability_id.clone(),
        use_case_id("use-case.symbol.search"),
        schema("schema.symbol.search.request"),
        schema("schema.symbol.search.result"),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let read_binding = SurfaceBindingV1::new(SurfaceBindingInputV1 {
        binding_id: read_binding_id.clone(),
        capability_id: read_capability_id.clone(),
        surface: BindingSurface::Cli,
        operation: SurfaceOperationName::new("source read").unwrap(),
        protocol_revisions: ProtocolRevisionRange::new(1, 2).unwrap(),
        required_features: Vec::new(),
        status: BindingStatus::Current,
        alias_of: None,
    })
    .unwrap();
    let source_contribution_id = ContributionId::new("contribution.source").unwrap();
    let source_contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: source_contribution_id.clone(),
        depends_on: Vec::new(),
        capabilities: vec![read_capability.clone()],
        retrieval_primitives: Vec::new(),
        bindings: vec![read_binding],
    })
    .unwrap()
    .with_executable_schemas(vec![
        ExecutableSchemaAuthority::for_types_at_paths::<ReadRequest, ReadResult>(
            &read_capability,
            "snapshot_contract::ReadRequest",
            "snapshot_contract::ReadResult",
        )
        .unwrap(),
    ])
    .unwrap();
    let symbol_contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.symbol").unwrap(),
        depends_on: vec![source_contribution_id],
        capabilities: vec![search_capability.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();
    let profile = ProfileDefinition::new(ProfileDefinitionInputV1 {
        profile_id: profile_id.clone(),
        kind: ProfileKind::Default,
        capability_ids: vec![search_capability_id.clone(), read_capability_id.clone()],
        enabled_surfaces: vec![BindingSurface::Cli],
        requires_cli_mcp_pairing: false,
        budget: ample_budget(),
    })
    .unwrap();

    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(symbol_contribution)
        .add_contribution(source_contribution)
        .add_handler(handler_for(&search_capability))
        .add_handler(handler_for(&read_capability))
        .add_profile(profile);
    let snapshot = builder.build().unwrap();

    assert_eq!(
        snapshot.digest().to_string(),
        "sha256:1588d3f6cfa939feac2e00e953c0cd17bef681bb986515dcfefae9a53d41319a"
    );
    assert_eq!(
        snapshot
            .capabilities()
            .map(|capability| capability.capability_id().clone())
            .collect::<Vec<_>>(),
        vec![read_capability_id.clone(), search_capability_id.clone()]
    );
    assert_eq!(
        snapshot
            .binding(&read_binding_id)
            .map(SurfaceBindingV1::capability_id),
        Some(&read_capability_id)
    );
    assert!(snapshot.executable_schema(&read_capability_id).is_some());
    assert!(snapshot.executable_schema(&search_capability_id).is_none());
    assert_eq!(
        snapshot
            .resolve_binding(
                &profile_id,
                BindingSurface::Cli,
                &SurfaceOperationName::new("source read").unwrap(),
                1,
                &BTreeSet::new(),
            )
            .map(|capability| capability.capability_id()),
        Some(&read_capability_id)
    );
}

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
