mod common;

use tracedecay_tool_catalog::{
    CancellationPoint, CatalogContributionInputV1, CatalogContributionV1, CatalogSnapshotBuilderV1,
    ContributionContractRef, ContributionId, CoverageContractRef, DeadlineBehavior,
    OmissionContractRef, RetrievalFamily, RetrievalPrimitiveManifestInputV1,
    RetrievalPrimitiveManifestV1, RetrieverId, ScoringContractRef, SortContract, SortContractId,
    TemporalMode,
};

use common::{
    ample_budget, capability_id, handler_for, profile, profile_id, read_manifest, schema,
    use_case_id,
};

fn source_retrieval(
    capability_id: tracedecay_tool_catalog::CapabilityId,
    request_schema: tracedecay_tool_catalog::SchemaRef,
    result_schema: tracedecay_tool_catalog::SchemaRef,
) -> RetrievalPrimitiveManifestV1 {
    RetrievalPrimitiveManifestV1::new(RetrievalPrimitiveManifestInputV1 {
        capability_id,
        family: RetrievalFamily::Source,
        retriever_id: RetrieverId::new("retriever.source.lines").unwrap(),
        request_schema,
        evidence_packet_schema: result_schema,
        coverage_contract: CoverageContractRef::new(schema("schema.coverage.v1")),
        omission_contract: OmissionContractRef::new(schema("schema.omission.v1")),
        scoring_contract: ScoringContractRef::new(schema("schema.scoring.v1")),
        contribution_contract: ContributionContractRef::new(schema("schema.contribution.v1")),
        deterministic_order: SortContract::new(
            SortContractId::new("sort.source.path-offset.v1").unwrap(),
            1,
        )
        .unwrap(),
        default_page_size: 10,
        maximum_page_size: 100,
        temporal_modes: vec![TemporalMode::Forensic, TemporalMode::Current],
        cancellation_points: vec![
            CancellationPoint::BeforeRead,
            CancellationPoint::BeforeAdmission,
        ],
        deadline_behavior: DeadlineBehavior::ReturnOperationReceipt,
    })
    .unwrap()
}

#[test]
fn retrieval_primitives_canonicalize_temporal_and_cancellation_metadata() {
    let profile_id = profile_id("profile.default");
    let capability_id = capability_id("capability.source.lines");
    let request_schema = schema("schema.source.lines.request");
    let result_schema = schema("schema.source.lines.result");
    let manifest = read_manifest(
        capability_id.clone(),
        use_case_id("use-case.source.lines"),
        request_schema.clone(),
        result_schema.clone(),
        Vec::new(),
        vec![profile_id.clone()],
    );
    let retrieval = source_retrieval(capability_id.clone(), request_schema, result_schema);

    assert_eq!(
        retrieval.temporal_modes(),
        &[TemporalMode::Current, TemporalMode::Forensic]
    );
    assert_eq!(
        retrieval.cancellation_points(),
        &[
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeRead
        ]
    );

    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![manifest.clone()],
        retrieval_primitives: vec![retrieval],
        bindings: Vec::new(),
    })
    .unwrap();
    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(contribution)
        .add_handler(handler_for(&manifest))
        .add_profile(profile(
            profile_id,
            vec![capability_id.clone()],
            ample_budget(),
        ));
    let snapshot = builder.build().unwrap();
    assert!(snapshot.retrieval_primitive(&capability_id).is_some());
}

#[test]
fn retrieval_primitive_rejects_unbounded_page_metadata() {
    let result = RetrievalPrimitiveManifestV1::new(RetrievalPrimitiveManifestInputV1 {
        capability_id: capability_id("capability.source.invalid"),
        family: RetrievalFamily::Source,
        retriever_id: RetrieverId::new("retriever.source.invalid").unwrap(),
        request_schema: schema("schema.invalid.request"),
        evidence_packet_schema: schema("schema.invalid.result"),
        coverage_contract: CoverageContractRef::new(schema("schema.invalid.coverage")),
        omission_contract: OmissionContractRef::new(schema("schema.invalid.omission")),
        scoring_contract: ScoringContractRef::new(schema("schema.invalid.scoring")),
        contribution_contract: ContributionContractRef::new(schema("schema.invalid.contribution")),
        deterministic_order: SortContract::new(SortContractId::new("sort.invalid.v1").unwrap(), 1)
            .unwrap(),
        default_page_size: 101,
        maximum_page_size: 100,
        temporal_modes: vec![TemporalMode::Current],
        cancellation_points: vec![CancellationPoint::BeforeAdmission],
        deadline_behavior: DeadlineBehavior::ReturnOperationReceipt,
    });

    assert!(result.is_err());
}
