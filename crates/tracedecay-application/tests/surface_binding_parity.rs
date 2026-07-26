//! Declared transport parity for Git and feedback surface contracts.

use tracedecay_application::{
    ApplicationHandlerDescriptor, configuration_surface_catalog_contribution,
    configuration_surface_handler_descriptors, feedback_surface_catalog_contribution,
    feedback_surface_handler_descriptors, git_surface_catalog_contribution,
    git_surface_handler_descriptors,
};
use tracedecay_tool_catalog::{BindingSurface, CatalogContributionV1};

#[test]
fn git_and_feedback_bindings_have_declared_surface_parity() {
    const TRANSPORT_SURFACES: [BindingSurface; 3] = [
        BindingSurface::Cli,
        BindingSurface::Mcp,
        BindingSurface::Http,
    ];
    const CLI_MCP_SURFACES: [BindingSurface; 2] = [BindingSurface::Cli, BindingSurface::Mcp];
    const ADVISORY_SURFACES: [BindingSurface; 4] = [
        BindingSurface::Cli,
        BindingSurface::Mcp,
        BindingSurface::Http,
        BindingSurface::Lsp,
    ];
    const ADVISORY_CAPABILITIES: [&str; 3] = [
        "capability.application.feedback.github-review-ingest",
        "capability.application.feedback.ci-failure-localize",
        "capability.application.feedback.proximity",
    ];
    let git = git_surface_catalog_contribution().expect("git");
    let feedback = feedback_surface_catalog_contribution().expect("feedback");
    let git_handlers = git_surface_handler_descriptors().expect("git handlers");
    let feedback_handlers = feedback_surface_handler_descriptors().expect("feedback handlers");

    assert_surface_contract_parity(&git, &git_handlers, &CLI_MCP_SURFACES, &[]);
    let advisory_overrides =
        ADVISORY_CAPABILITIES.map(|capability_id| (capability_id, ADVISORY_SURFACES.as_slice()));
    assert_surface_contract_parity(
        &feedback,
        &feedback_handlers,
        &TRANSPORT_SURFACES,
        &advisory_overrides,
    );
}

#[test]
fn configuration_bindings_have_declared_surface_parity() {
    let configuration =
        configuration_surface_catalog_contribution().expect("configuration contribution");
    let handlers = configuration_surface_handler_descriptors().expect("configuration handlers");

    assert_surface_contract_parity(
        &configuration,
        &handlers,
        &[
            BindingSurface::Cli,
            BindingSurface::Mcp,
            BindingSurface::Http,
        ],
        &[],
    );
}

fn assert_surface_contract_parity(
    contribution: &CatalogContributionV1,
    handlers: &[ApplicationHandlerDescriptor],
    default_surfaces: &[BindingSurface],
    surface_overrides: &[(&str, &[BindingSurface])],
) {
    for capability in contribution.capabilities() {
        let handler = handlers
            .iter()
            .find(|handler| handler.operation().capability_id() == capability.capability_id())
            .expect("capability has one application handler descriptor");
        assert_eq!(handler.request_schema(), capability.request_schema());
        assert_eq!(handler.result_schema(), capability.result_schema());

        let bindings: Vec<_> = contribution
            .bindings()
            .iter()
            .filter(|binding| binding.capability_id() == capability.capability_id())
            .collect();
        let surfaces = surface_overrides
            .iter()
            .find(|(capability_id, _)| *capability_id == capability.capability_id().as_str())
            .map_or(default_surfaces, |(_, surfaces)| *surfaces);
        assert_eq!(bindings.len(), surfaces.len());
        assert_eq!(capability.binding_ids().len(), surfaces.len());

        let operation = bindings[0].operation();
        for surface in surfaces {
            let binding = bindings
                .iter()
                .find(|binding| binding.surface() == *surface)
                .unwrap_or_else(|| panic!("missing {operation} on {surface:?}"));
            assert_eq!(binding.operation(), operation);
            assert!(capability.binding_ids().contains(binding.binding_id()));
            assert!(binding.required_features().is_empty());
            assert!(!binding.is_alias());
        }
    }
}
