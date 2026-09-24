use tracedecay_contracts::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
};
use tracedecay_contracts::catalog_composition::build_application_catalog_snapshot;
use tracedecay_contracts::{
    application_catalog_contributions, application_handler_descriptors,
    callable_code_catalog_contribution, feedback_surface_catalog_contribution,
    feedback_surface_handler_descriptors,
    git::git_index_catalog_contribution,
    retrieval::catalog::{
        primitive_read_contribution, primitive_read_operation, symbol_search_contribution,
    },
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
        ]
        .contains(&capability.capability_id().as_str());
        assert_eq!(
            capability.binding_ids().is_empty(),
            provider_contribution,
            "{} has no direct bindings only when it is a producer contribution",
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
fn verified_graph_mcp_reads_have_application_primitive_admission_identity() {
    let contribution = primitive_read_contribution().unwrap();

    for operation_name in [
        "context",
        "node",
        "impact",
        "similar",
        "rename_preview",
        "port_status",
        "port_order",
        "todos",
    ] {
        let operation = primitive_read_operation(operation_name)
            .unwrap()
            .unwrap_or_else(|| panic!("{operation_name} primitive operation"));
        let capability = contribution
            .capabilities()
            .iter()
            .find(|capability| capability.capability_id() == operation.capability_id())
            .unwrap_or_else(|| panic!("{operation_name} primitive capability"));

        assert_eq!(capability.use_case_id(), operation.use_case_id());
        assert!(capability.availability().is_callable());
        assert!(contribution.bindings().iter().any(|binding| {
            binding.capability_id() == operation.capability_id()
                && binding.surface() == BindingSurface::Mcp
                && binding.operation().as_str() == operation_name
        }));
        assert!(!contribution.bindings().iter().any(|binding| {
            binding.capability_id() == operation.capability_id()
                && binding.surface() == BindingSurface::Http
        }));
    }
}

#[test]
fn similar_and_redundancy_use_the_current_protocol_revision_only() {
    let contribution = primitive_read_contribution().unwrap();
    for operation in ["similar", "redundancy"] {
        let mcp_bindings: Vec<_> = contribution
            .bindings()
            .iter()
            .filter(|binding| {
                binding.surface() == BindingSurface::Mcp
                    && binding.operation().as_str() == operation
            })
            .collect();
        assert_eq!(
            mcp_bindings.len(),
            1,
            "{operation} must keep one MCP (surface, operation) binding"
        );
        let binding = mcp_bindings[0];
        assert!(
            binding.protocol_revisions().contains(1),
            "{operation} must accept the current protocol revision"
        );
        assert!(
            !binding.protocol_revisions().contains(2),
            "{operation} must not keep a retired protocol revision"
        );
        assert_eq!(binding.protocol_revisions().minimum(), 1);
        assert_eq!(binding.protocol_revisions().maximum(), 1);
    }
}

#[test]
fn application_catalog_snapshot_admits_one_similar_redundancy_binding() {
    let snapshot = build_application_catalog_snapshot()
        .expect("catalog construction must succeed with one binding per surface-operation");
    for operation in ["similar", "redundancy"] {
        let capability_id = primitive_read_operation(operation)
            .unwrap()
            .unwrap_or_else(|| panic!("{operation} primitive operation"))
            .capability_id()
            .clone();
        let capability = snapshot
            .capability(&capability_id)
            .unwrap_or_else(|| panic!("{operation} is in the composed snapshot"));
        let mcp_bindings: Vec<_> = capability
            .binding_ids()
            .iter()
            .filter_map(|binding_id| snapshot.binding(binding_id))
            .filter(|binding| binding.surface() == BindingSurface::Mcp)
            .collect();
        assert_eq!(mcp_bindings.len(), 1, "{operation}");
        assert_eq!(mcp_bindings[0].operation().as_str(), operation);
        assert_eq!(mcp_bindings[0].protocol_revisions().minimum(), 1);
        assert_eq!(mcp_bindings[0].protocol_revisions().maximum(), 1);
    }
}
