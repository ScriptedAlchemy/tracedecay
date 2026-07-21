use std::collections::BTreeSet;

use tracedecay::catalog_composition::{
    CatalogCompositionError, build_application_catalog_snapshot, validate_application_catalog,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationHandlerDescriptor, ApplicationHandlerDescriptors,
    ApplicationOperation, ResultContractRef, application_catalog_contributions,
    application_handler_descriptors, retrieval::catalog::symbol_search_contribution,
};
use tracedecay_tool_catalog::{
    AvailabilityContract, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, ProfileId, SchemaId, SchemaRef,
    UnavailabilityReason, UseCaseId,
};

#[test]
fn root_snapshot_validates_every_application_contribution_against_declared_descriptors() {
    let contributions = application_catalog_contributions().unwrap();
    let handlers = application_handler_descriptors().unwrap();
    let snapshot = build_application_catalog_snapshot().unwrap();

    let contributed_capabilities = contributions
        .iter()
        .flat_map(|contribution| contribution.capabilities())
        .count();
    assert_eq!(contributed_capabilities, handlers.iter().count());
    assert_eq!(snapshot.capabilities().count(), contributed_capabilities);

    for contribution in &contributions {
        assert!(contribution.bindings().is_empty());
        for capability in contribution.capabilities() {
            let handler = handlers
                .get(capability.use_case_id())
                .expect("every declared capability has one validation-only descriptor");
            assert_eq!(handler.operation().use_case_id(), capability.use_case_id());
            assert_eq!(handler.request_schema(), capability.request_schema());
            assert_eq!(handler.result_schema(), capability.result_schema());
            assert!(matches!(
                capability.availability(),
                AvailabilityContract::Unavailable {
                    reason: UnavailabilityReason::NotImplemented,
                }
            ));
        }
    }

    let symbol_search = CapabilityId::new("capability.retrieval.symbol-search").unwrap();
    assert!(snapshot.capability(&symbol_search).is_some());

    let default_profile = ProfileId::new("profile.default").unwrap();
    assert!(snapshot.profile(&default_profile).is_some());
    assert_eq!(
        snapshot
            .visible_capabilities(&default_profile, &BTreeSet::new())
            .into_iter()
            .map(|capability| capability.capability_id())
            .collect::<Vec<_>>(),
        Vec::<&CapabilityId>::new()
    );
}

#[test]
fn unwired_available_capability_is_rejected_before_snapshot_composition() {
    let mut contributions = application_catalog_contributions().unwrap();
    let symbol_contribution = contributions
        .iter()
        .find(|contribution| {
            contribution
                .capabilities()
                .iter()
                .any(|capability| capability.capability_id() == &symbol_search_capability())
        })
        .cloned()
        .expect("symbol contribution is declared");
    let index = contributions
        .iter()
        .position(|contribution| contribution == &symbol_contribution)
        .expect("symbol contribution is present");
    contributions[index] =
        contribution_with_availability(&symbol_contribution, AvailabilityContract::Available);

    assert_eq!(
        validate_application_catalog(&contributions, &application_handler_descriptors().unwrap()),
        inconsistent("unwired application capability availability")
    );
}

#[test]
fn mismatched_descriptor_schema_is_rejected() {
    let contribution = symbol_search_contribution().unwrap();
    let cases = [
        (
            descriptor_with_contract(
                "capability.retrieval.symbol-search",
                "use-case.retrieval.symbol-search",
                schema("schema.test.drifted-request", 384),
                symbol_result_schema(),
            ),
            "application capability schema mapping",
        ),
        (
            descriptor_with_contract(
                "capability.retrieval.symbol-search",
                "use-case.retrieval.symbol-search",
                symbol_request_schema(),
                schema("schema.test.drifted-result", 1_024),
            ),
            "application capability schema mapping",
        ),
    ];

    for (descriptor, field) in cases {
        let handlers = ApplicationHandlerDescriptors::new([descriptor]).unwrap();
        assert_eq!(
            validate_application_catalog(std::slice::from_ref(&contribution), &handlers),
            inconsistent(field),
            "descriptor mismatch for {field} must be rejected"
        );
    }
}

#[test]
fn capability_without_descriptor_is_rejected() {
    assert_eq!(
        validate_application_catalog(
            &[symbol_search_contribution().unwrap()],
            &ApplicationHandlerDescriptors::default(),
        ),
        inconsistent("application capability handler mapping")
    );
}

#[test]
fn orphan_handler_descriptor_is_rejected() {
    let mut descriptors: Vec<_> = application_handler_descriptors()
        .unwrap()
        .iter()
        .cloned()
        .collect();
    descriptors.push(descriptor_with_contract(
        "capability.application.orphan",
        "use-case.application.orphan",
        symbol_request_schema(),
        symbol_result_schema(),
    ));
    let handlers = ApplicationHandlerDescriptors::new(descriptors).unwrap();

    assert_eq!(
        validate_application_catalog(&application_catalog_contributions().unwrap(), &handlers),
        inconsistent("application handler use case")
    );
}

#[test]
fn root_composition_is_deterministic() {
    let first = build_application_catalog_snapshot().unwrap();
    let second = build_application_catalog_snapshot().unwrap();

    assert_eq!(first, second);
    assert_eq!(first.digest(), second.digest());
}

fn symbol_search_capability() -> CapabilityId {
    CapabilityId::new("capability.retrieval.symbol-search").unwrap()
}

fn descriptor_with_contract(
    capability_id: &str,
    use_case_id: &str,
    request_schema: SchemaRef,
    result_schema: SchemaRef,
) -> ApplicationHandlerDescriptor {
    ApplicationHandlerDescriptor::new(
        ApplicationOperation::new(
            CapabilityId::new(capability_id).unwrap(),
            UseCaseId::new(use_case_id).unwrap(),
            ResultContractRef::from_schema(&result_schema),
            true,
        ),
        request_schema,
        result_schema,
    )
    .unwrap()
}

fn symbol_request_schema() -> SchemaRef {
    schema("schema.application.symbol-search.request", 384)
}

fn symbol_result_schema() -> SchemaRef {
    schema("schema.application.symbol-search.result", 1_024)
}

fn schema(id: &str, maximum_bytes: u32) -> SchemaRef {
    SchemaRef::new(SchemaId::new(id).unwrap(), 1, maximum_bytes).unwrap()
}

fn inconsistent(field: &'static str) -> Result<(), CatalogCompositionError> {
    Err(CatalogCompositionError::Application(
        ApplicationContractError::Inconsistent { field },
    ))
}

fn contribution_with_availability(
    contribution: &CatalogContributionV1,
    availability: AvailabilityContract,
) -> CatalogContributionV1 {
    let original = contribution
        .capabilities()
        .first()
        .expect("test contribution has one capability");
    let capability = CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: original.capability_id().clone(),
        use_case_id: original.use_case_id().clone(),
        routing: original.routing().clone(),
        request_schema: original.request_schema().clone(),
        result_schema: original.result_schema().clone(),
        effect: original.effect(),
        scope: original.scope().clone(),
        authority: original.authority(),
        denied_disclosure: original.denied_disclosure(),
        privacy: original.privacy(),
        lifecycle: original.lifecycle(),
        streaming: original.streaming().clone(),
        cancellation: original.cancellation().clone(),
        deadline: original.deadline().clone(),
        pagination: original.pagination().cloned(),
        idempotency: original.idempotency(),
        authority_revalidation: original.authority_revalidation().clone(),
        reconciliation: original.reconciliation(),
        receipt: original.receipt(),
        terminal_states: original.terminal_states().clone(),
        availability,
        binding_ids: original.binding_ids().to_vec(),
        profile_eligibility: original.profile_eligibility().to_vec(),
        required_features: original.required_features().to_vec(),
    })
    .unwrap();
    CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: contribution.contribution_id().clone(),
        depends_on: contribution.depends_on().to_vec(),
        capabilities: vec![capability],
        retrieval_primitives: contribution.retrieval_primitives().to_vec(),
        bindings: contribution.bindings().to_vec(),
    })
    .unwrap()
}
