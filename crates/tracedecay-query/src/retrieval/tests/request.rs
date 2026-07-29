use static_assertions::assert_not_impl_any;
use tracedecay_domain::{
    EphemeralSanitizedQueryViewV1, PrincipalId, QueryNormalizationRevision, RetrievalBudget,
    RetrievalScope, RetrievalSnapshot, SanitizerRevision, SingleRootScopeV1, TemporalModeV1,
    UtcMicros, VectorWatermark,
};

use super::{digest_id, id};
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
fn raw_query_dto_is_deserializable_only_at_the_boundary() {
    let request = raw_request("private boundary query".to_owned());
    let mut request_json =
        serde_json::to_value(super::request()).expect("query-free request serializes");
    request_json
        .as_object_mut()
        .expect("request is a JSON object")
        .insert(
            "query".to_owned(),
            serde_json::Value::String("private boundary query".to_owned()),
        );
    let decoded: RawRetrievalRequestV1 =
        serde_json::from_value(request_json).expect("boundary DTO deserializes");
    assert_eq!(format!("{request:?}"), format!("{decoded:?}"));
    assert!(!format!("{decoded:?}").contains("private boundary query"));
}

#[test]
fn raw_query_dto_rejects_oversized_input_before_execution_state_exists() {
    let raw = raw_request("x".repeat(tracedecay_domain::MAX_EPHEMERAL_QUERY_VIEW_BYTES + 1));
    assert!(
        raw.sanitize(
            id::<SanitizerRevision>("query-sanitizer.v1"),
            id::<QueryNormalizationRevision>("query-normalization.v1"),
        )
        .is_err()
    );
}

#[test]
fn query_view_source_has_no_clone_or_serde_surface() {
    assert_not_impl_any!(
        EphemeralSanitizedQueryViewV1:
            Clone,
            serde::Serialize,
            serde::de::DeserializeOwned
    );

    let boundary_source = include_str!("../request.rs");
    let raw_start = boundary_source
        .find("pub struct RawRetrievalRequestV1")
        .expect("raw DTO declaration");
    let raw_prefix = &boundary_source[..raw_start];
    let raw_derive_start = raw_prefix.rfind("#[derive").expect("raw DTO derive");
    let raw_declaration = &boundary_source[raw_derive_start..raw_start];
    assert!(!raw_declaration.contains("Clone"));
    assert!(!raw_declaration.contains("Serialize"));
    assert!(raw_declaration.contains("Deserialize"));
}
