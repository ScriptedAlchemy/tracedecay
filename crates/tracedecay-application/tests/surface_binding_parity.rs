//! CLI/MCP/HTTP semantic parity for Git and feedback surface contracts.

use tracedecay_application::{
    ApplicationHandlerDescriptor, feedback_surface_catalog_contribution,
    feedback_surface_handler_descriptors, git_surface_catalog_contribution,
    git_surface_handler_descriptors,
};
use tracedecay_tool_catalog::{BindingSurface, CatalogContributionV1};

#[test]
fn git_and_feedback_bindings_have_cli_mcp_http_parity() {
    let git = git_surface_catalog_contribution().expect("git");
    let feedback = feedback_surface_catalog_contribution().expect("feedback");
    let git_handlers = git_surface_handler_descriptors().expect("git handlers");
    let feedback_handlers = feedback_surface_handler_descriptors().expect("feedback handlers");

    for (contribution, handlers) in [(&git, &git_handlers), (&feedback, &feedback_handlers)] {
        assert_surface_contract_parity(contribution, handlers);
    }
}

fn assert_surface_contract_parity(
    contribution: &CatalogContributionV1,
    handlers: &[ApplicationHandlerDescriptor],
) {
    let surfaces = [
        BindingSurface::Cli,
        BindingSurface::Mcp,
        BindingSurface::Http,
    ];

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
        assert_eq!(bindings.len(), surfaces.len());
        assert_eq!(capability.binding_ids().len(), surfaces.len());

        let operation = bindings[0].operation();
        for surface in surfaces {
            let binding = bindings
                .iter()
                .find(|binding| binding.surface() == surface)
                .unwrap_or_else(|| panic!("missing {operation} on {surface:?}"));
            assert_eq!(binding.operation(), operation);
            assert!(capability.binding_ids().contains(binding.binding_id()));
            assert!(binding.required_features().is_empty());
            assert!(!binding.is_alias());
        }
    }
}
