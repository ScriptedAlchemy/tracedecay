use std::sync::Arc;

use tracedecay_domain::{
    ComponentRevision, EphemeralSanitizedQueryViewV1, QueryNormalizationRevision,
    RetrievalCursorKeyId, RetrieverKind, RetrieverOutcome, SanitizerRevision,
};

use super::{batch, composition_lanes, id, no_caps, profile, request};
use crate::query::retrieval::Pr9QueryAuthorityV1;
use crate::query::retrieval::fusion::RetrievalCursorKeyringV1;

fn query_view() -> EphemeralSanitizedQueryViewV1 {
    EphemeralSanitizedQueryViewV1::sanitize(
        "authenticated fallback",
        id::<SanitizerRevision>("query-sanitizer.authority.v1"),
        id::<QueryNormalizationRevision>("query-normalization.authority.v1"),
    )
    .expect("sanitized query")
}

fn authority() -> Pr9QueryAuthorityV1 {
    let request = request();
    Pr9QueryAuthorityV1::new(
        profile(),
        no_caps(),
        id::<ComponentRevision>("ranking.authority.v1"),
        RetrievalCursorKeyringV1::new(
            request.scope.privacy_domain,
            id::<RetrievalCursorKeyId>("retrieval-key.authority.v1"),
            1,
            vec![7_u8; 32],
            1_000_000,
        )
        .expect("keyring"),
    )
    .expect("authority")
}

fn empty_pr9_lanes() -> Vec<crate::query::retrieval::fusion::CompositionLaneInput> {
    composition_lanes(vec![
        (
            RetrieverKind::ExactLiteral,
            RetrieverOutcome::Complete(batch(Vec::new(), "exact")),
        ),
        (
            RetrieverKind::Lexical,
            RetrieverOutcome::Complete(batch(Vec::new(), "lexical")),
        ),
        (
            RetrieverKind::Graph,
            RetrieverOutcome::Complete(batch(Vec::new(), "graph")),
        ),
    ])
}

#[test]
fn authenticated_pr9_fallback_is_byte_stable_and_carries_only_pr9_lanes() {
    let authority = authority();
    let request = request();
    let query = query_view();
    let first = authority
        .compose(&request, &query, empty_pr9_lanes(), 8, None)
        .expect("compose");
    let second = authority
        .compose(&request, &query, empty_pr9_lanes(), 8, None)
        .expect("repeat compose");

    assert_eq!(first, second);
    assert_eq!(
        first
            .fallback
            .public_pr9_lane_coverage
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        RetrieverKind::PR9_FALLBACK_LANES
    );
    first.fallback.validate().expect("canonical fallback");
    assert_eq!(
        serde_json::to_vec(first.fallback.as_ref()).expect("first bytes"),
        serde_json::to_vec(second.fallback.as_ref()).expect("second bytes"),
    );
}

#[test]
fn semantic_handoff_reuses_the_authenticated_query_and_fallback() {
    let authority = authority();
    let request = request();
    let authorized = authority
        .compose(&request, &query_view(), empty_pr9_lanes(), 8, None)
        .expect("compose");

    assert_eq!(
        authority
            .authenticate_query(&request, &query_view())
            .expect("digest"),
        authorized.query_digest
    );
    assert!(Arc::strong_count(&authorized.fallback) >= 1);
}
