//! Canonical request-body bindings shared by callable query and page admission.

use tracedecay_application::CodeRelationRequest;
use tracedecay_application::retrieval::{
    CodeFacetRequest, CodeNavigationRequest, CodeTimelineRequest,
};
use tracedecay_application::{ExactOccurrenceRequest, PhraseSearchRequest};
use tracedecay_domain::{DomainError, ManifestDigest, canonical_sha256};

pub(in crate::daemon) fn exact_occurrence_page_body_digest(
    request: &ExactOccurrenceRequest,
) -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(
        "code_exact_occurrence",
        &request.literal,
        &request.kind,
        &request.scope,
        &request.meta.projection,
        &request.meta.order,
    ))
}

pub(in crate::daemon) fn phrase_search_page_body_digest(
    request: &PhraseSearchRequest,
) -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(
        "code_phrase_search",
        request.query.as_str(),
        &request.phrases,
        &request.field_filters,
        request.fuzzy_budget,
        &request.scope,
        &request.meta.projection,
        &request.meta.order,
    ))
}

pub(in crate::daemon) fn callees_page_body_digest(
    request: &CodeRelationRequest,
) -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(
        "code_callees",
        &request.node_id,
        request.maximum_depth,
        request.resolve_trait_dispatch,
        &request.scope,
        &request.meta.projection,
        &request.meta.order,
    ))
}

pub(in crate::daemon) fn facets_page_body_digest(
    request: &CodeFacetRequest,
) -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(
        "code_facets",
        request.dimension,
        &request.scope,
        &request.meta.projection,
        &request.meta.order,
    ))
}

pub(in crate::daemon) fn timeline_page_body_digest(
    request: &CodeTimelineRequest,
) -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(
        "code_timeline",
        &request.scope,
        &request.meta.projection,
        &request.meta.order,
    ))
}

pub(in crate::daemon) fn navigation_page_body_digest(
    operation: &'static str,
    request: &CodeNavigationRequest,
) -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(
        operation,
        &request.node_id,
        &request.scope,
        &request.meta.projection,
        &request.meta.order,
    ))
}
