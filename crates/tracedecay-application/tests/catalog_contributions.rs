use tracedecay_application::{
    application_catalog_contributions, application_handler_descriptors,
    git::git_index_catalog_contribution, retrieval::catalog::symbol_search_contribution,
};
use tracedecay_tool_catalog::{AvailabilityContract, UnavailabilityReason};

#[test]
fn inert_symbol_search_contribution_has_one_matching_handler_descriptor() {
    let contribution = symbol_search_contribution().unwrap();
    let descriptors = application_handler_descriptors().unwrap();
    let capability = contribution
        .capabilities()
        .first()
        .expect("symbol search contribution has one capability");
    let handler = descriptors
        .get(capability.use_case_id())
        .expect("declared application use case has a validation-only descriptor");

    assert_eq!(handler.operation().use_case_id(), capability.use_case_id());
    assert_eq!(handler.request_schema(), capability.request_schema());
    assert_eq!(handler.result_schema(), capability.result_schema());
    assert!(contribution.bindings().is_empty());
}

#[test]
fn application_contribution_set_contains_only_declared_unwired_use_cases() {
    let contributions = application_catalog_contributions().unwrap();
    let handlers = application_handler_descriptors().unwrap();

    assert_eq!(contributions.len(), 2);
    assert_eq!(
        contributions
            .iter()
            .flat_map(|contribution| contribution.capabilities())
            .count(),
        handlers.iter().count()
    );
    for capability in contributions
        .iter()
        .flat_map(|contribution| contribution.capabilities())
    {
        assert!(matches!(
            capability.availability(),
            AvailabilityContract::Unavailable {
                reason: UnavailabilityReason::NotImplemented,
            }
        ));
        assert!(!capability.availability().is_callable());
        assert!(handlers.get(capability.use_case_id()).is_some());
    }
    assert!(
        git_index_catalog_contribution()
            .unwrap()
            .bindings()
            .is_empty()
    );
}
