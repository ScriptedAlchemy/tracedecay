use std::sync::Arc;

use tracedecay_domain::{
<<<<<<< ours
<<<<<<< ours
    CalibrationProfileId, ComponentRevision, EphemeralSanitizedQueryViewV1, PrincipalId,
    PublicRetrieverStatus, QueryMac, QueryNormalizationRevision, RepositoryId,
    RetrievalCursorKeyId, RetrieverKind, RetrieverOutcome, SanitizerRevision,
    ScoreDomainCalibrationV1, ScoreDomainId, TemporalModeV1,
=======
    CodeGenerationId, CodeSourceCursorBindingV1, ComponentRevision, EphemeralSanitizedQueryViewV1,
    GitOidV1, PrincipalId, QueryMac, QueryNormalizationRevision, RefId, RepositoryId,
    RetrievalCursorKeyId, RetrieverKind, RetrieverOutcome, SanitizerRevision, TemporalModeV1,
>>>>>>> theirs
=======
    CodeGenerationId, CodeSourceCursorBindingV1, ComponentRevision, EphemeralSanitizedQueryViewV1,
    GitOidV1, PrincipalId, QueryMac, QueryNormalizationRevision, RefId, RepositoryId,
    RetrievalCursorKeyId, RetrieverKind, RetrieverOutcome, SanitizerRevision, TemporalModeV1,
>>>>>>> theirs
};

use super::{batch, candidate, composition_lanes, id, no_caps, profile, request};
use crate::retrieval::fusion::{QueryDigestAuthenticationError, RetrievalCursorKeyringV1};
use crate::retrieval::{PreparedQueryBindingsV1, PreparedQueryErrorV1, PreparedQueryV1};
use crate::retrieval::{QueryAuthorityErrorV1, QueryAuthorityV1};

fn query_view() -> EphemeralSanitizedQueryViewV1 {
    EphemeralSanitizedQueryViewV1::sanitize(
        "authenticated fallback",
        id::<SanitizerRevision>("query-sanitizer.authority.v1"),
        id::<QueryNormalizationRevision>("query-normalization.authority.v1"),
    )
    .expect("sanitized query")
}

fn authority() -> QueryAuthorityV1 {
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

fn authority_with_keyring(keyring: RetrievalCursorKeyringV1) -> QueryAuthorityV1 {
    QueryAuthorityV1::new(
        profile(),
        no_caps(),
        id::<ComponentRevision>("ranking.authority.v1"),
        keyring,
    )
    .expect("authority")
}

#[test]
fn prepared_query_cursor_resumes_only_the_authenticated_generation_and_candidate_set() {
    let generation = CodeGenerationId::new("generation.prepared-query.v1").expect("generation");
    let bindings = PreparedQueryBindingsV1::new(
        "code_index_branch_diff.v1",
        tracedecay_domain::canonical_sha256(&"scope.prepared-query").expect("scope digest"),
        generation.clone(),
        tracedecay_domain::canonical_sha256(&"query.prepared-query").expect("query digest"),
    )
    .expect("bindings");
    let items = vec!["first".to_owned(), "second".to_owned(), "third".to_owned()];
    let first = PreparedQueryV1::prepare(Arc::new(authority()), request(), None)
        .expect("prepare first page")
        .paginate(
            &bindings,
            items.clone(),
            1,
            tracedecay_domain::UtcMicros(10),
        )
        .expect("first page");
    let cursor = first.next_cursor.expect("continuation");
    let resumed = PreparedQueryV1::prepare(Arc::new(authority()), request(), Some(&cursor))
        .expect("authenticate continuation")
        .paginate(
            &bindings,
            items.clone(),
            1,
            tracedecay_domain::UtcMicros(11),
        )
        .expect("resume page");
    assert_eq!(resumed.items, ["second"]);

    let changed_generation = PreparedQueryBindingsV1::new(
        "code_index_branch_diff.v1",
        tracedecay_domain::canonical_sha256(&"scope.prepared-query").expect("scope digest"),
        CodeGenerationId::new("generation.prepared-query.v2").expect("changed generation"),
        tracedecay_domain::canonical_sha256(&"query.prepared-query").expect("query digest"),
    )
    .expect("changed generation bindings");
    assert_eq!(
        PreparedQueryV1::prepare(Arc::new(authority()), request(), Some(&cursor))
            .expect("authenticate continuation")
            .paginate(
                &changed_generation,
                items.clone(),
                1,
                tracedecay_domain::UtcMicros(11),
            ),
        Err(PreparedQueryErrorV1::Stale)
    );
    assert_eq!(
        PreparedQueryV1::prepare(Arc::new(authority()), request(), Some(&cursor))
            .expect("authenticate continuation")
            .paginate(
                &bindings,
                vec!["first".to_owned(), "changed".to_owned()],
                1,
                tracedecay_domain::UtcMicros(11),
            ),
        Err(PreparedQueryErrorV1::Stale)
    );
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

<<<<<<< ours
<<<<<<< ours
fn federated_authority() -> QueryAuthorityV1 {
    let mut profile = profile();
    profile.calibrations = RetrieverKind::ALL_LANES
        .into_iter()
        .map(|lane| {
            (
                lane,
                id::<CalibrationProfileId>(&format!("calibration.{}.v1", lane.as_str())),
            )
        })
        .collect();
    profile.score_domain_calibrations = RetrieverKind::ALL_LANES
        .into_iter()
        .map(|lane| {
            let score_domain = id::<ScoreDomainId>(&format!("score.{}.v1", lane.as_str()));
            (
                score_domain.clone(),
                ScoreDomainCalibrationV1 {
                    calibration_profile_id: id(&format!("calibration.{}.v1", lane.as_str())),
                    score_domain,
                    raw_min_micros: 0,
                    raw_max_micros: 1_000_000,
                },
            )
        })
        .collect();
    profile.weights_micros = RetrieverKind::ALL_LANES
        .into_iter()
        .map(|lane| (lane, 100_000))
        .collect();
    let request = request();
    QueryAuthorityV1::new_federated(
        profile,
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
    .expect("federated authority")
}

fn empty_federated_lanes() -> Vec<crate::retrieval::fusion::CompositionLaneInput> {
    composition_lanes(
        RetrieverKind::ALL_LANES
            .into_iter()
            .map(|lane| {
                (
                    lane,
                    RetrieverOutcome::Complete(batch(Vec::new(), lane.as_str())),
                )
            })
            .collect(),
    )
}

#[test]
fn federated_authority_composes_every_lane_without_fallback_projection() {
    let authority = federated_authority();
    let request = request();
    let authorized = authority
        .compose_federated(&request, &query_view(), empty_federated_lanes(), 8, None)
        .expect("federated composition");

    assert_eq!(
        authorized
            .composition
            .public_lane_statuses
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        RetrieverKind::ALL_LANES
    );
    assert!(
        authorized
            .composition
            .public_lane_statuses
            .values()
            .all(|status| *status == PublicRetrieverStatus::Complete)
    );
    assert!(authorized.page.ranked_candidates.is_empty());
    assert!(authorized.page.cursor.is_none());
    assert_eq!(
        authority.compose(&request, &query_view(), empty_foreground_lanes(), 8, None,),
        Err(QueryAuthorityErrorV1::AuthorityModeMismatch)
    );
}

#[test]
fn federated_authority_rejects_missing_or_duplicate_lanes() {
    let authority = federated_authority();
    let request = request();
    let query = query_view();
    let mut missing = empty_federated_lanes();
    missing.retain(|lane| lane.lane != RetrieverKind::Diagnostic);
    assert_eq!(
        authority.compose_federated(&request, &query, missing, 8, None),
        Err(QueryAuthorityErrorV1::LaneSetMismatch)
    );

    let mut duplicate = empty_federated_lanes();
    duplicate.push(duplicate[0].clone());
    assert_eq!(
        authority.compose_federated(&request, &query, duplicate, 8, None),
        Err(QueryAuthorityErrorV1::LaneSetMismatch)
    );
=======
=======
>>>>>>> theirs
fn paged_foreground_lanes() -> Vec<crate::retrieval::fusion::CompositionLaneInput> {
    composition_lanes(vec![
        (
            RetrieverKind::ExactLiteral,
            RetrieverOutcome::Complete(batch(Vec::new(), "exact")),
        ),
        (
            RetrieverKind::Lexical,
            RetrieverOutcome::Complete(batch(
                vec![
                    candidate(RetrieverKind::Lexical, "first", 900_000, 0),
                    candidate(RetrieverKind::Lexical, "second", 800_000, 1),
                ],
                "lexical",
            )),
        ),
        (
            RetrieverKind::Graph,
            RetrieverOutcome::Complete(batch(Vec::new(), "graph")),
        ),
    ])
}

#[test]
fn exact_code_source_binding_is_authenticated_with_the_query_cursor() {
    let authority = authority();
    let request = request();
    let query = query_view();
    let mut cursor = authority
        .compose(&request, &query, paged_foreground_lanes(), 1, None)
        .expect("compose first page")
        .fallback
        .cursor
        .clone()
        .expect("continuation cursor");
    let binding = CodeSourceCursorBindingV1 {
        reference: RefId::new("refs/heads/feature").expect("reference"),
        commit: GitOidV1::new("1".repeat(40)).expect("commit"),
        tree: GitOidV1::new("2".repeat(40)).expect("tree"),
        generation: CodeGenerationId::new("generation.feature").expect("generation"),
    };

    authority
        .bind_code_source_cursor(&mut cursor, binding.clone())
        .expect("bind exact source");
    authority
        .verify_code_source_cursor(&cursor, &binding)
        .expect("authenticated exact source");

    let mut mismatches = Vec::new();
    let mut changed = binding.clone();
    changed.reference = RefId::new("refs/heads/other").expect("wrong reference");
    mismatches.push(changed);
    let mut changed = binding.clone();
    changed.commit = GitOidV1::new("3".repeat(40)).expect("wrong commit");
    mismatches.push(changed);
    let mut changed = binding.clone();
    changed.tree = GitOidV1::new("4".repeat(40)).expect("wrong tree");
    mismatches.push(changed);
    let mut changed = binding;
    changed.generation = CodeGenerationId::new("generation.other").expect("wrong generation");
    mismatches.push(changed);
    for mismatch in mismatches {
        assert!(matches!(
            authority.verify_code_source_cursor(&cursor, &mismatch),
            Err(QueryAuthorityErrorV1::Retrieval(
                tracedecay_domain::RetrievalError::CursorSetMismatch
            ))
        ));
    }
<<<<<<< ours
>>>>>>> theirs
=======
>>>>>>> theirs
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
            .public_fallback_lane_coverage
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        RetrieverKind::QUERY_FALLBACK_LANES
    );
    assert_eq!(
        first
            .fallback_lanes
            .iter()
            .map(|lane| lane.lane)
            .collect::<Vec<_>>(),
        RetrieverKind::QUERY_FALLBACK_LANES
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
        Err(QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::KeyUnavailable,
        ))
    );

    let mut tampered = old_digest.clone();
    tampered.mac = QueryMac::new(format!("hmac-sha256:{}", "9".repeat(64))).expect("tampered MAC");
    assert_eq!(
        authority.verify_authenticated_query(&old_key, &request, &query, &tampered),
        Err(QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::AuthenticationFailed,
        ))
    );

    let mut wrong_scope = request.clone();
    wrong_scope.scope.privacy_domain = id("privacy.authority.other");
    assert_eq!(
        authority.verify_authenticated_query(&old_key, &wrong_scope, &query, &old_digest),
        Err(QueryAuthorityErrorV1::QueryAuthentication(
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
        Err(QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::AuthenticationFailed,
        ))
    );
}

#[test]
fn prepared_cursor_authentication_binds_principal_scope_and_temporal_mode() {
    let request = request();
    let active_key = id::<RetrievalCursorKeyId>("retrieval-key.authority.v1");
    let authority = authority();
    let payload = br#"{"cursor":"prepared"}"#;
    let digest = authority
        .authenticate_prepared_cursor_payload(&request, payload)
        .expect("prepared cursor digest");

    let mut mismatches = Vec::new();
    let mut principal = request.clone();
    principal.principal = id::<PrincipalId>("principal.other");
    mismatches.push(principal);
    let mut scope = request.clone();
    scope.scope.root.repository = id::<RepositoryId>("repository.other");
    mismatches.push(scope);
    let mut temporal = request.clone();
    temporal.temporal_mode = TemporalModeV1::Evolution;
    mismatches.push(temporal);

    for mismatch in mismatches {
        assert_eq!(
            authority.verify_prepared_cursor_payload(&active_key, &mismatch, payload, &digest),
            Err(QueryAuthorityErrorV1::QueryAuthentication(
                QueryDigestAuthenticationError::AuthenticationFailed,
            ))
        );
    }
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
        Err(QueryAuthorityErrorV1::QueryAuthentication(
            QueryDigestAuthenticationError::KeyRevoked,
        ))
    );
}
