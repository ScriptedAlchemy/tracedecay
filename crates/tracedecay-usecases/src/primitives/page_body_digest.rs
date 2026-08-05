//! Canonical query identities shared by page admission and cursor ownership.
//!
//! Continuation state is intentionally absent so a later page binds to the
//! same query. Page size remains authenticated because it changes the result
//! boundary represented by a cursor.

use tracedecay_application::retrieval::{
    ExactSymbolRequest, GraphImpactPrimitiveRequest, GraphRelationRequest, ImplementationsRequest,
    SignatureSearchRequest, SymbolSearchPrimitiveRequest, TypeHierarchyRequest,
};
use tracedecay_application::{ApplicationContractError, RetrievalRequestMeta};
use tracedecay_domain::{ManifestDigest, canonical_sha256};

use super::runtime::DiagnosticsPrimitiveRequest;

fn symbol_meta(meta: &RetrievalRequestMeta) -> impl serde::Serialize + '_ {
    (
        meta.page.page_size,
        &meta.temporal,
        &meta.projection,
        &meta.order,
    )
}

pub fn symbol_search_page_body_digest(
    request: &SymbolSearchPrimitiveRequest,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.primitive-page-body.v1",
        "code_symbol_search",
        request.query.as_bytes(),
        request.query.sanitizer_revision(),
        request.query.normalization_revision(),
        &request.scope,
        request.lazy_index_ignored_dependencies,
        symbol_meta(&request.meta),
    ))
    .map_err(Into::into)
}

pub fn signature_search_page_body_digest(
    request: &SignatureSearchRequest,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.primitive-page-body.v1",
        "code_signature_search",
        &request.returns,
        &request.params,
        request.is_async,
        &request.scope,
        symbol_meta(&request.meta),
    ))
    .map_err(Into::into)
}

pub fn implementations_page_body_digest(
    request: &ImplementationsRequest,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.primitive-page-body.v1",
        "code_implementations",
        &request.selector,
        &request.scope,
        symbol_meta(&request.meta),
    ))
    .map_err(Into::into)
}

pub fn type_hierarchy_page_body_digest(
    request: &TypeHierarchyRequest,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.primitive-page-body.v1",
        "code_type_hierarchy",
        &request.node_id,
        request.maximum_depth,
        &request.scope,
        symbol_meta(&request.meta),
    ))
    .map_err(Into::into)
}

pub fn callers_page_body_digest(
    request: &GraphRelationRequest,
) -> Result<ManifestDigest, ApplicationContractError> {
    graph_relation_page_body_digest("code_callers", request)
}

pub fn diagnostics_page_body_digest(
    request: &DiagnosticsPrimitiveRequest,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.primitive-page-body.v1",
        "diagnostics_read",
        &request.scope,
        request.maximum_diagnostics,
    ))
    .map_err(Into::into)
}

pub(crate) fn exact_symbol_page_body_digest(
    request: &ExactSymbolRequest,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.primitive-page-body.v1",
        "code_exact_symbol",
        &request.name,
        &request.scope,
        request.lazy_index_ignored_dependencies,
        symbol_meta(&request.meta),
    ))
    .map_err(Into::into)
}

pub(crate) fn callees_page_body_digest(
    request: &GraphRelationRequest,
) -> Result<ManifestDigest, ApplicationContractError> {
    graph_relation_page_body_digest("code_callees", request)
}

fn graph_relation_page_body_digest(
    operation: &'static str,
    request: &GraphRelationRequest,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.primitive-page-body.v1",
        operation,
        &request.node_id,
        request.maximum_depth,
        request.resolve_trait_dispatch,
        &request.scope,
        symbol_meta(&request.meta),
    ))
    .map_err(Into::into)
}

pub(crate) fn impact_page_body_digest(
    request: &GraphImpactPrimitiveRequest,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.primitive-page-body.v1",
        "code_impact",
        &request.node_id,
        request.maximum_depth,
        &request.scope,
        symbol_meta(&request.meta),
    ))
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use tracedecay_application::retrieval::{SignatureSearchRequest, SymbolGraphScope};
    use tracedecay_application::{
        OpaqueCursor, PageRequest, ResultProjection, RetrievalOrder, RetrievalRequestMeta,
    };

    use super::{diagnostics_page_body_digest, signature_search_page_body_digest};
    use crate::primitives::runtime::{DiagnosticsPrimitiveRequest, DiagnosticsPrimitiveScope};

    fn signature(page: PageRequest, returns: &str) -> SignatureSearchRequest {
        SignatureSearchRequest {
            returns: Some(returns.to_owned()),
            params: vec!["RequestContext".to_owned()],
            is_async: Some(true),
            scope: SymbolGraphScope {
                path_prefix: Some("src".to_owned()),
            },
            meta: RetrievalRequestMeta::current(
                page,
                ResultProjection::Evidence,
                RetrievalOrder::StableIdentity,
            ),
        }
    }

    #[test]
    fn symbol_digest_excludes_continuation_but_binds_page_size() {
        let first = signature(PageRequest::first(10).expect("page"), "Result");
        let same_size_resume = signature(
            PageRequest::new(10, Some(OpaqueCursor::new("opaque.next").expect("cursor")))
                .expect("page"),
            "Result",
        );
        assert_eq!(
            signature_search_page_body_digest(&first).expect("digest"),
            signature_search_page_body_digest(&same_size_resume).expect("digest")
        );

        let resized_resume = signature(
            PageRequest::new(200, Some(OpaqueCursor::new("opaque.next").expect("cursor")))
                .expect("page"),
            "Result",
        );
        assert_ne!(
            signature_search_page_body_digest(&first).expect("digest"),
            signature_search_page_body_digest(&resized_resume).expect("digest")
        );

        let other_query = signature(PageRequest::first(10).expect("page"), "Option");
        assert_ne!(
            signature_search_page_body_digest(&first).expect("digest"),
            signature_search_page_body_digest(&other_query).expect("digest")
        );
    }

    #[test]
    fn diagnostic_digest_excludes_continuation_but_binds_scope_and_page_size() {
        let first = DiagnosticsPrimitiveRequest {
            scope: DiagnosticsPrimitiveScope::Workspace,
            maximum_diagnostics: 10,
            cursor: None,
        };
        let retry = DiagnosticsPrimitiveRequest {
            scope: DiagnosticsPrimitiveScope::Workspace,
            maximum_diagnostics: 10,
            cursor: Some("opaque.next".to_owned()),
        };
        assert_eq!(
            diagnostics_page_body_digest(&first).expect("digest"),
            diagnostics_page_body_digest(&retry).expect("digest")
        );

        let resized = DiagnosticsPrimitiveRequest {
            scope: DiagnosticsPrimitiveScope::Workspace,
            maximum_diagnostics: 500,
            cursor: Some("opaque.next".to_owned()),
        };
        assert_ne!(
            diagnostics_page_body_digest(&first).expect("digest"),
            diagnostics_page_body_digest(&resized).expect("digest")
        );

        let other_scope = DiagnosticsPrimitiveRequest {
            scope: DiagnosticsPrimitiveScope::File("src/lib.rs".to_owned()),
            maximum_diagnostics: 10,
            cursor: None,
        };
        assert_ne!(
            diagnostics_page_body_digest(&first).expect("digest"),
            diagnostics_page_body_digest(&other_scope).expect("digest")
        );
    }
}
