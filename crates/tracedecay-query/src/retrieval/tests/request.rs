use static_assertions::{assert_impl_all, assert_not_impl_any};
use tracedecay_domain::{
    EphemeralSanitizedQueryViewV1, PrincipalId, QueryNormalizationRevision, RetrievalBudget,
    RetrievalScope, RetrievalSnapshot, SanitizerRevision, SingleRootScopeV1, TemporalModeV1,
    UtcMicros, VectorWatermark,
};

use super::{digest_id, id};
use crate::retrieval::ports::RetrievalPortError;
use crate::retrieval::request::RawRetrievalRequestV1;

fn raw_request(query: String) -> RawRetrievalRequestV1 {
    RawRetrievalRequestV1::new(
        query,
        tracedecay_domain::RetrievalRequest {
            principal: id::<PrincipalId>("principal.fixture"),
            scope: RetrievalScope {
                privacy_domain: id("privacy.fixture"),
                root: SingleRootScopeV1 {
                    repository: id("repository.fixture"),
                    worktree: None,
                    reference: None,
                },
            },
            temporal_mode: TemporalModeV1::Current,
            snapshot: RetrievalSnapshot {
                watermarks: VectorWatermark::default(),
                freshness_digest: digest_id('f'),
                authorization_revision: id("authorization.v1"),
                captured_at: UtcMicros(7),
            },
            profile_id: id("profile.fixture.v1"),
            budget: RetrievalBudget {
                max_candidates_per_lane: 32,
                max_fused_candidates: 16,
                max_hydrated_results: 8,
                max_hydration_bytes: 65_536,
                deadline_micros: None,
            },
        },
    )
}

#[test]
fn raw_query_dto_sanitizes_immediately_without_leaking_into_request_or_debug() {
    let raw = raw_request("  private query  ".to_owned());
    assert!(!format!("{raw:?}").contains("private query"));

    let sanitized = raw
        .sanitize(
            id::<SanitizerRevision>("query-sanitizer.v1"),
            id::<QueryNormalizationRevision>("query-normalization.v1"),
        )
        .expect("raw request sanitizes");

    assert_eq!(sanitized.query_view().as_str(), "private query");
    assert!(!format!("{:?}", sanitized.query_view()).contains("private query"));
    let serialized =
        serde_json::to_string(sanitized.request()).expect("query-free request serializes");
    assert!(!serialized.contains("private query"));
    assert!(!serialized.contains("\"query\""));
}

#[test]
fn raw_query_dto_rejects_oversized_input_before_execution_state_exists() {
    let sanitize = |bytes: usize| {
        raw_request("x".repeat(bytes)).sanitize(
            id::<SanitizerRevision>("query-sanitizer.v1"),
            id::<QueryNormalizationRevision>("query-normalization.v1"),
        )
    };
    let limit = tracedecay_domain::MAX_EPHEMERAL_QUERY_VIEW_BYTES;
    match sanitize(limit + 1) {
        Err(RetrievalPortError::Contract(message)) => assert_eq!(
            message,
            "ephemeral sanitized query view violates the structural bounds for sanitized text"
        ),
        other => panic!(
            "oversized query must be a contract refusal: {:?}",
            other.err()
        ),
    }
    let at_limit = sanitize(limit).expect("a query at the byte limit is admitted");
    assert_eq!(at_limit.query_view().as_str(), "x".repeat(limit));
}

#[test]
fn query_boundary_types_have_typed_serde_and_clone_surface() {
    assert_not_impl_any!(RawRetrievalRequestV1: Clone, serde::Serialize);
    assert_impl_all!(RawRetrievalRequestV1: serde::de::DeserializeOwned);
    assert_not_impl_any!(
        EphemeralSanitizedQueryViewV1:
            Clone,
            serde::Serialize,
            serde::de::DeserializeOwned
    );
}
