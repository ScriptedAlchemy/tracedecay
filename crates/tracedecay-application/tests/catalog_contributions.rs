use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
    PROXIMITY_CAPABILITY_ID_V1,
};
use tracedecay_application::{
    application_catalog_contributions, application_handler_descriptors,
    callable_code_catalog_contribution, feedback_surface_catalog_contribution,
    feedback_surface_handler_descriptors, git::git_index_catalog_contribution,
    retrieval::catalog::symbol_search_contribution,
};
use tracedecay_tool_catalog::BindingSurface;

#[test]
fn direct_symbol_search_contribution_has_one_matching_handler_descriptor() {
    let contribution = symbol_search_contribution().unwrap();
    let descriptors = application_handler_descriptors().unwrap();
    let capability = contribution
        .capabilities()
        .first()
        .expect("symbol search contribution has one capability");
    let handler = descriptors
        .get(capability.use_case_id())
        .expect("declared application use case has a validation-only descriptor");

    assert_eq!(
        handler.operation().capability_id(),
        capability.capability_id()
    );
    assert_eq!(handler.operation().use_case_id(), capability.use_case_id());
    assert_eq!(handler.request_schema(), capability.request_schema());
    assert_eq!(handler.result_schema(), capability.result_schema());
    assert!(capability.availability().is_callable());
    assert_eq!(capability.binding_ids().len(), 4);
    assert_eq!(contribution.bindings().len(), 4);
    for surface in [
        BindingSurface::Cli,
        BindingSurface::Mcp,
        BindingSurface::Http,
    ] {
        assert!(
            contribution
                .bindings()
                .iter()
                .any(|binding| binding.surface() == surface
                    && binding.operation().as_str() == "code_symbol_search")
        );
    }
    assert!(contribution.bindings().iter().any(|binding| {
        binding.surface() == BindingSurface::Lsp
            && binding.operation().as_str() == "workspace/symbol"
    }));
}

#[test]
fn application_contribution_set_uses_registered_feedback_handlers() {
    let contributions = application_catalog_contributions().unwrap();
    let handlers = application_handler_descriptors().unwrap();
    let callable_code = callable_code_catalog_contribution().unwrap();
    let feedback = feedback_surface_catalog_contribution().unwrap();
    let feedback_handlers = feedback_surface_handler_descriptors().unwrap();

    assert!(contributions.contains(&callable_code));
    assert!(contributions.contains(&feedback));
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
        assert!(
            handlers.get(capability.use_case_id()).is_some(),
            "{} has a registered application handler",
            capability.capability_id()
        );
    }
    for capability in feedback.capabilities() {
        assert!(
            feedback_handlers
                .iter()
                .any(|handler| handler.operation().capability_id() == capability.capability_id()),
            "{} has a registered concrete feedback handler",
            capability.capability_id()
        );
        assert!(
            capability.availability().is_callable(),
            "{} is callable after its production owner was registered",
            capability.capability_id()
        );
        let provider_contribution = [
            GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
            PROXIMITY_CAPABILITY_ID_V1,
        ]
        .contains(&capability.capability_id().as_str());
        assert_eq!(
            capability.binding_ids().is_empty(),
            provider_contribution,
            "{} must use the combined advisory transport",
            capability.capability_id()
        );
    }
    assert!(feedback.bindings().iter().any(|binding| {
        binding.surface() == BindingSurface::Dashboard
            && feedback
                .capabilities()
                .iter()
                .any(|capability| capability.binding_ids().contains(binding.binding_id()))
    }));
    assert!(
        git_index_catalog_contribution()
            .unwrap()
            .bindings()
            .is_empty()
    );
}

#[test]
fn application_composition_excludes_planner_and_store_owned_surfaces() {
    // Cargo.toml already keeps this crate free of store/transport deps; this
    // composition check proves the public catalog API likewise exposes no
    // planner/model-runtime ownership.
    let contributions = application_catalog_contributions().unwrap();
    assert!(!contributions.is_empty());
    for capability in contributions
        .iter()
        .flat_map(|contribution| contribution.capabilities())
    {
        let capability_id = capability.capability_id().as_str();
        assert!(
            !capability_id.contains("planner")
                && !capability_id.contains("model-runtime")
                && !capability_id.contains("universal-retrieval"),
            "application catalog must not own {capability_id}"
        );
        let use_case = capability.use_case_id().as_str();
        assert!(
            !use_case.contains("planner") && !use_case.contains("dispatcher"),
            "application use cases must not own {use_case}"
        );
    }
}
