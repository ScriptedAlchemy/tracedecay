use std::sync::Arc;

use tracedecay_domain::{
    ComponentRevision, EphemeralSanitizedQueryViewV1, QueryMac, QueryNormalizationRevision,
    RetrievalCursorKeyId, RetrieverKind, RetrieverOutcome, SanitizerRevision,
};

use super::{batch, composition_lanes, id, no_caps, profile, request};
use crate::retrieval::fusion::{QueryDigestAuthenticationError, RetrievalCursorKeyringV1};
use crate::retrieval::{Pr9QueryAuthorityErrorV1, Pr9QueryAuthorityV1};

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
    authority_with_keyring(
        RetrievalCursorKeyringV1::new(
            request.scope.privacy_domain,
            id::<RetrievalCursorKeyId>("retrieval-key.authority.v1"),
            1,
            vec![7_u8; 32],
            1_000_000,
        )
        .expect("keyring"),
    )
}

fn authority_with_keyring(keyring: RetrievalCursorKeyringV1) -> Pr9QueryAuthorityV1 {
    Pr9QueryAuthorityV1::new(
        profile(),
        no_caps(),
        id::<ComponentRevision>("ranking.authority.v1"),
        keyring,
    )
    .expect("authority")
}

fn empty_foreground_lanes() -> Vec<crate::retrieval::fusion::CompositionLaneInput> {
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
fn authenticated_foreground_fallback_is_byte_stable_and_lane_bounded() {
    let authority = authority();
    let request = request();
    let query = query_view();
    let first = authority
        .compose(&request, &query, empty_foreground_lanes(), 8, None)
        .expect("compose");
    let second = authority
        .compose(&request, &query, empty_foreground_lanes(), 8, None)
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
    assert_eq!(
        first
            .pr9_lanes
            .iter()
            .map(|lane| lane.lane)
            .collect::<Vec<_>>(),
        RetrieverKind::PR9_FALLBACK_LANES
    );
    assert_eq!(first.page_size, 8);
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
        .compose(&request, &query_view(), empty_foreground_lanes(), 8, None)
        .expect("compose");

    assert_eq!(
        authority
            .authenticate_query(&request, &query_view())
            .expect("digest"),
        authorized.query_digest
    );
    assert!(Arc::strong_count(&authorized.fallback) >= 1);
}

#[test]
fn retained_query_key_verifies_without_fallback_key_guessing() {
    let request = request();
    let query = query_view();
    let old_key = id::<RetrievalCursorKeyId>("retrieval-key.authority.old");
    let mut keys = RetrievalCursorKeyringV1::new(
        request.scope.privacy_domain.clone(),
        old_key.clone(),
        7,
        vec![7_u8; 32],
        1_000_000,
    )
    .expect("old key");
    let old_digest = keys
        .digest_active_query(&request, &query)
        .expect("old digest");
    keys.rotate(
        id::<RetrievalCursorKeyId>("retrieval-key.authority.active"),
        8,
        vec![8_u8; 32],
    )
    .expect("rotation");
    let authority = authority_with_keyring(keys);

    authority
        .verify_authenticated_query(&old_key, &request, &query, &old_digest)
        .expect("retained exact key verifies");
    assert_eq!(
        authority.verify_authenticated_query(
            &id::<RetrievalCursorKeyId>("retrieval-key.authority.unknown"),
            &request,
            &query,
            &old_digest,
        ),
        Err(Pr9QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::KeyUnavailable,
        ))
    );

    let mut tampered = old_digest.clone();
    tampered.mac = QueryMac::new(format!("hmac-sha256:{}", "9".repeat(64))).expect("tampered MAC");
    assert_eq!(
        authority.verify_authenticated_query(&old_key, &request, &query, &tampered),
        Err(Pr9QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::AuthenticationFailed,
        ))
    );

    let mut wrong_scope = request.clone();
    wrong_scope.scope.privacy_domain = id("privacy.authority.other");
    assert_eq!(
        authority.verify_authenticated_query(&old_key, &wrong_scope, &query, &old_digest),
        Err(Pr9QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::PrivacyDomainMismatch,
        ))
    );
}

#[test]
fn prepared_cursor_authentication_binds_the_selected_key_id() {
    let request = request();
    let active_key = id::<RetrievalCursorKeyId>("retrieval-key.prepared.active");
    let alternate_key = id::<RetrievalCursorKeyId>("retrieval-key.prepared.alternate");
    let mut keys = RetrievalCursorKeyringV1::new(
        request.scope.privacy_domain.clone(),
        active_key.clone(),
        7,
        vec![7_u8; 32],
        1_000_000,
    )
    .expect("active key");
    keys.retain(alternate_key.clone(), 7, vec![7_u8; 32])
        .expect("same-epoch alternate key");
    let authority = authority_with_keyring(keys);
    let payload = br#"{"cursor":"prepared"}"#;
    let digest = authority
        .authenticate_prepared_cursor_payload(&request, payload)
        .expect("prepared cursor digest");

    authority
        .verify_prepared_cursor_payload(&active_key, &request, payload, &digest)
        .expect("selected key verifies");
    assert_eq!(
        authority.verify_prepared_cursor_payload(&alternate_key, &request, payload, &digest,),
        Err(Pr9QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::AuthenticationFailed,
        ))
    );
}

#[test]
fn revoked_retained_query_key_is_rejected() {
    let request = request();
    let query = query_view();
    let old_key = id::<RetrievalCursorKeyId>("retrieval-key.authority.revoked");
    let mut keys = RetrievalCursorKeyringV1::new(
        request.scope.privacy_domain.clone(),
        old_key.clone(),
        7,
        vec![7_u8; 32],
        1_000_000,
    )
    .expect("old key");
    let old_digest = keys
        .digest_active_query(&request, &query)
        .expect("old digest");
    keys.rotate(
        id::<RetrievalCursorKeyId>("retrieval-key.authority.active"),
        8,
        vec![8_u8; 32],
    )
    .expect("rotation");
    keys.revoke(&old_key, 7).expect("revocation");
    let authority = authority_with_keyring(keys);

    assert_eq!(
        authority.verify_authenticated_query(&old_key, &request, &query, &old_digest),
        Err(Pr9QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::KeyRevoked,
        ))
    );
}
