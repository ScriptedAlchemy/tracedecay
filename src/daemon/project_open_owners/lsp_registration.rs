use std::collections::BTreeMap;

use tracedecay_lsp::analyzer::broker::AdmittedLspProvider;
use tracedecay_lsp::{
    ContextProjectionKind, GatewayCapabilities, SemanticCapability, TRACEDECAY_CONTEXT_REVISION,
};

pub(super) fn production_lsp_registration(
    admitted_providers: &[AdmittedLspProvider],
) -> (Vec<String>, GatewayCapabilities) {
    let revision = TRACEDECAY_CONTEXT_REVISION;
    let gateway_capabilities = GatewayCapabilities {
        supports_publish_diagnostics: true,
        supports_document_diagnostics: true,
        supports_workspace_diagnostics: true,
        supports_managed_diagnostics: true,
        // Multi-root admission is enabled only after the registrar mounts the
        // exact authorized scope-set storage.
        supports_workspace_folders: false,
        // The gateway implements the complete request protocol. The upstream
        // initialize response remains the authority for which methods can be
        // advertised to a particular session.
        semantic: SemanticCapability::ALL.into_iter().collect(),
        context_projections: BTreeMap::from([
            (ContextProjectionKind::diagnostics(), revision),
            (ContextProjectionKind::post_edit_impact(), revision),
            (ContextProjectionKind::affected_tests(), revision),
            (ContextProjectionKind::test_run_results(), revision),
        ]),
        supports_context_expansion: true,
    };
    (
        admitted_providers
            .iter()
            .map(|provider| provider.language.clone())
            .collect(),
        gateway_capabilities,
    )
}
