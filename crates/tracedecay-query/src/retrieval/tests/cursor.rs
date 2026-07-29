use tracedecay_domain::{
    CodeGenerationId, EphemeralSanitizedQueryViewV1, ManifestDigest, OptionalStagePublicStatus,
    ProjectionKeyV1, ProjectionKindV1, PublicRetrieverStatus, QueryMac, QueryNormalizationRevision,
    RetrievalAnchorId, RetrievalCursor, RetrievalCursorKeyId, RetrievalError, RetrievalFailure,
    RetrievalRequest, RetrieverContinuation, RetrieverKind, RetrieverOutcome, SanitizerRevision,
    SemanticRetrievalContinuationV1, UtcMicros, VectorGenerationIdV1,
};

use super::{batch, candidate, composition_lanes, id, no_caps, profile, request};
use crate::retrieval::fusion::{CompositionKernel, FusionStageInput, RetrievalCursorKeyringV1};

const NOW: UtcMicros = UtcMicros(10);
const CURSOR_TTL_MICROS: u64 = 100;

fn query_view() -> EphemeralSanitizedQueryViewV1 {
    EphemeralSanitizedQueryViewV1::sanitize(
        "deterministic retrieval",
        id::<SanitizerRevision>("query-sanitizer.v1"),
        id::<QueryNormalizationRevision>("query-normalization.v1"),
    )
    .expect("bounded sanitized query view")
}

fn keyring(request: &RetrievalRequest, key_epoch: u64) -> RetrievalCursorKeyringV1 {
    RetrievalCursorKeyringV1::new(
        request.scope.privacy_domain.clone(),
        id::<RetrievalCursorKeyId>(&format!("retrieval-key-{key_epoch}")),
        key_epoch,
        vec![7_u8; 32],
        CURSOR_TTL_MICROS,
    )
    .expect("retrieval cursor keyring")
}

fn composed_with_graph_outcome(
    graph: RetrieverOutcome<tracedecay_domain::RetrieverBatch<&'static str>>,
) -> (
    CompositionKernel,
    crate::retrieval::fusion::CompositionOutputV1,
) {
    let kernel = CompositionKernel::new(id("ranking.fixture.v1"));
    let output = kernel
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(Vec::new(), "empty")),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(
                            vec![
                                candidate(RetrieverKind::Lexical, "first", 900_000, 0),
                                candidate(RetrieverKind::Lexical, "second", 800_000, 1),
                                candidate(RetrieverKind::Lexical, "third", 700_000, 2),
                            ],
                            "lexical",
                        )),
                    ),
                    (RetrieverKind::Graph, graph),
                ]),
            },
            &no_caps(),
        )
        .unwrap();
    (kernel, output)
}

#[test]
fn overflow_cursor_resumes_the_frozen_candidate_set() {
    let (kernel, output) =
        composed_with_graph_outcome(RetrieverOutcome::Complete(batch(Vec::new(), "empty")));
    let request = request();
    let query_view = query_view();
    let keyring = keyring(&request, 7);
    let first = kernel
        .paginate_at(&request, &query_view, &keyring, &output, 2, None, NOW)
        .unwrap();

    assert_eq!(
        first
            .ranked_candidates
            .iter()
            .map(|ranked| ranked.final_ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    let cursor = first.cursor.expect("overflow cursor");
    assert_eq!(cursor.next_ordinal, 2);

    let second = kernel
        .paginate_at(
            &request,
            &query_view,
            &keyring,
            &output,
            2,
            Some(&cursor),
            NOW,
        )
        .unwrap();
    assert_eq!(second.ranked_candidates[0].final_ordinal, 2);
    assert!(second.cursor.is_none());
}

#[test]
fn cursor_rejects_a_differently_completed_candidate_set() {
    let (kernel, output) =
        composed_with_graph_outcome(RetrieverOutcome::Complete(batch(Vec::new(), "empty")));
    let request = request();
    let query_view = query_view();
    let keyring = keyring(&request, 7);
    let cursor = kernel
        .paginate_at(&request, &query_view, &keyring, &output, 2, None, NOW)
        .unwrap()
        .cursor
        .unwrap();

    let mut changed = output;
    changed.ranked_candidates.pop();
    assert_eq!(
        kernel.paginate_at(
            &request,
            &query_view,
            &keyring,
            &changed,
            2,
            Some(&cursor),
            NOW,
        ),
        Err(RetrievalError::CursorSetMismatch)
    );
}

#[test]
fn denied_and_unavailable_optional_lanes_have_identical_public_cursor_bytes() {
    let (kernel, denied) = composed_with_graph_outcome(RetrieverOutcome::Denied);
    let unavailable_failure = RetrievalFailure::AuthorityUnavailable {
        detail: "internal authority detail".to_owned(),
    };
    let (_, unavailable) =
        composed_with_graph_outcome(RetrieverOutcome::Unavailable(unavailable_failure));

    assert_eq!(denied.ranked_candidates, unavailable.ranked_candidates);
    assert_eq!(
        denied.public_lane_statuses,
        unavailable.public_lane_statuses
    );
    let request = request();
    let query_view = query_view();
    let keyring = keyring(&request, 7);
    let denied_cursor = kernel
        .paginate_at(&request, &query_view, &keyring, &denied, 2, None, NOW)
        .unwrap()
        .cursor
        .unwrap();
    let unavailable_cursor = kernel
        .paginate_at(&request, &query_view, &keyring, &unavailable, 2, None, NOW)
        .unwrap()
        .cursor
        .unwrap();
    assert_eq!(denied_cursor, unavailable_cursor);
}

#[test]
fn cursor_query_identity_is_bound_to_the_privacy_domain_and_key_epoch() {
    let (kernel, output) =
        composed_with_graph_outcome(RetrieverOutcome::Complete(batch(Vec::new(), "empty")));
    let request = request();
    let query_view = query_view();
    let current_key = keyring(&request, 7);
    let cursor = kernel
        .paginate_at(&request, &query_view, &current_key, &output, 2, None, NOW)
        .expect("cursor is minted")
        .cursor
        .expect("overflow cursor");

    assert_eq!(
        cursor.query_digest.privacy_domain,
        request.scope.privacy_domain
    );
    assert_eq!(cursor.query_digest.key_epoch, 7);
    assert!(cursor.query_digest.mac.as_str().starts_with("hmac-sha256:"));

    let rotated_key = keyring(&request, 8);
    assert_ne!(
        current_key
            .digest_active_query(&request, &query_view)
            .expect("current digest"),
        rotated_key
            .digest_active_query(&request, &query_view)
            .expect("rotated digest")
    );
    assert_eq!(
        kernel.paginate_at(
            &request,
            &query_view,
            &rotated_key,
            &output,
            2,
            Some(&cursor),
            NOW,
        ),
        Err(RetrievalError::CursorKeyUnavailable)
    );
}

#[test]
fn pr9_cursor_mac_authenticates_the_semantic_continuation_envelope() {
    let (kernel, output) =
        composed_with_graph_outcome(RetrieverOutcome::Complete(batch(Vec::new(), "empty")));
    let request = request();
    let query_view = query_view();
    let keys = keyring(&request, 7);
    let mut cursor = kernel
        .paginate_at(&request, &query_view, &keys, &output, 2, None, NOW)
        .unwrap()
        .cursor
        .unwrap();
    let mut statuses = cursor.public_lane_statuses.clone();
    statuses.insert(RetrieverKind::Semantic, PublicRetrieverStatus::Complete);
    cursor.semantic = Some(SemanticRetrievalContinuationV1 {
        profile_id: id("profile.semantic.cursor.v1"),
        profile_digest: super::digest_id('c'),
        code_generation: id::<CodeGenerationId>("code-generation.cursor.v1"),
        vector_generation: VectorGenerationIdV1::new(super::digest_id::<ManifestDigest>('a')),
        projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Embedding,
            schema_revision: "projection.cursor.v1".to_owned(),
            profile_digest: super::digest_id('b'),
        },
        search_index_key: tracedecay_domain::SemanticSearchIndexProfileV1::exact_flat_v1()
            .and_then(|profile| profile.index_key())
            .expect("search index key"),
        candidate_set_digest: cursor.candidate_set_digest.clone(),
        public_lane_statuses: statuses,
        lane_checkpoints: Vec::new(),
        ranking_revision: id("ranking.semantic.cursor.v1"),
        rerank: OptionalStagePublicStatus::NotRequested,
        ordered_candidate_anchors: vec![
            id::<RetrievalAnchorId>("anchor.semantic.cursor.0"),
            id::<RetrievalAnchorId>("anchor.semantic.cursor.1"),
        ],
        next_ordinal: 2,
    });
    keys.resign_cursor(&mut cursor).unwrap();

    kernel
        .paginate_at(&request, &query_view, &keys, &output, 2, Some(&cursor), NOW)
        .expect("PR9 validates first through the shared cursor MAC");

    let mut missing_continuation = cursor.clone();
    missing_continuation.semantic = None;
    assert_eq!(
        kernel.paginate_at(
            &request,
            &query_view,
            &keys,
            &output,
            2,
            Some(&missing_continuation),
            NOW,
        ),
        Err(RetrievalError::CursorAuthenticationFailed)
    );

    cursor
        .semantic
        .as_mut()
        .expect("semantic continuation")
        .next_ordinal = 3;
    assert_eq!(
        kernel.paginate_at(&request, &query_view, &keys, &output, 2, Some(&cursor), NOW,),
        Err(RetrievalError::CursorAuthenticationFailed)
    );
}

fn resume(
    kernel: &CompositionKernel,
    request: &RetrievalRequest,
    query_view: &EphemeralSanitizedQueryViewV1,
    keyring: &RetrievalCursorKeyringV1,
    output: &crate::retrieval::fusion::CompositionOutputV1,
    cursor: &RetrievalCursor,
    now: UtcMicros,
) -> Result<crate::retrieval::fusion::CompositionPageV1, RetrievalError> {
    kernel.paginate_at(request, query_view, keyring, output, 2, Some(cursor), now)
}

#[test]
fn cursor_mac_authenticates_every_payload_field_before_binding() {
    let (kernel, output) =
        composed_with_graph_outcome(RetrieverOutcome::Complete(batch(Vec::new(), "empty")));
    let request = request();
    let query_view = query_view();
    let mut keys = keyring(&request, 7);
    let cursor = kernel
        .paginate_at(&request, &query_view, &keys, &output, 2, None, NOW)
        .unwrap()
        .cursor
        .unwrap();
    keys.retain(id("retrieval-key-alias"), 7, vec![8_u8; 32])
        .unwrap();
    keys.retain(id("retrieval-key-7"), 8, vec![9_u8; 32])
        .unwrap();

    let assert_authentication_failure = |tampered: RetrievalCursor| {
        assert_eq!(
            resume(
                &kernel,
                &request,
                &query_view,
                &keys,
                &output,
                &tampered,
                NOW,
            ),
            Err(RetrievalError::CursorAuthenticationFailed)
        );
    };

    let mut tampered = cursor.clone();
    tampered.key_id = id("retrieval-key-alias");
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.key_epoch = 8;
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.privacy_domain = id("privacy.other");
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.query_digest.mac = QueryMac::new(format!("hmac-sha256:{}", "1".repeat(64))).unwrap();
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.profile_id = id("profile.other");
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.snapshot_digest = id(format!("sha256:{}", "2".repeat(64)).as_str());
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.freshness_digest = id(format!("sha256:{}", "3".repeat(64)).as_str());
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.authorization_revision = id("authorization.other");
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.candidate_set_digest = id(format!("sha256:{}", "4".repeat(64)).as_str());
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered
        .public_lane_statuses
        .insert(RetrieverKind::Graph, PublicRetrieverStatus::Partial);
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.lane_checkpoints.push(RetrieverContinuation {
        lane: RetrieverKind::Graph,
        checkpoint_digest: id(format!("sha256:{}", "5".repeat(64)).as_str()),
        exhausted: false,
    });
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.ranking_revision = id("ranking.other");
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.next_ordinal += 1;
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.expiry = UtcMicros(tampered.expiry.0 + 1);
    assert_authentication_failure(tampered);
    let mut tampered = cursor.clone();
    tampered.signature = QueryMac::new(format!("hmac-sha256:{}", "6".repeat(64))).unwrap();
    assert_authentication_failure(tampered);
}

#[test]
fn cursor_rotation_retains_old_keys_until_policy_revokes_them() {
    let (kernel, output) =
        composed_with_graph_outcome(RetrieverOutcome::Complete(batch(Vec::new(), "empty")));
    let request = request();
    let query_view = query_view();
    let mut keys = keyring(&request, 7);
    let old_cursor = kernel
        .paginate_at(&request, &query_view, &keys, &output, 2, None, NOW)
        .unwrap()
        .cursor
        .unwrap();

    keys.rotate(id("retrieval-key-8"), 8, vec![8_u8; 32])
        .unwrap();
    assert!(
        resume(
            &kernel,
            &request,
            &query_view,
            &keys,
            &output,
            &old_cursor,
            NOW,
        )
        .is_ok()
    );

    keys.revoke(&old_cursor.key_id, old_cursor.key_epoch)
        .unwrap();
    assert_eq!(
        resume(
            &kernel,
            &request,
            &query_view,
            &keys,
            &output,
            &old_cursor,
            NOW,
        ),
        Err(RetrievalError::CursorKeyRevoked)
    );
}

#[test]
fn cursor_rejects_cross_domain_and_snapshot_replay_after_authentication() {
    let (kernel, output) =
        composed_with_graph_outcome(RetrieverOutcome::Complete(batch(Vec::new(), "empty")));
    let request = request();
    let query_view = query_view();
    let keys = keyring(&request, 7);
    let cursor = kernel
        .paginate_at(&request, &query_view, &keys, &output, 2, None, NOW)
        .unwrap()
        .cursor
        .unwrap();

    let mut cross_domain = request.clone();
    cross_domain.scope.privacy_domain = id("privacy.other");
    assert_eq!(
        resume(
            &kernel,
            &cross_domain,
            &query_view,
            &keys,
            &output,
            &cursor,
            NOW,
        ),
        Err(RetrievalError::CursorSetMismatch)
    );

    let mut changed_snapshot = request.clone();
    changed_snapshot.snapshot.captured_at = UtcMicros(8);
    assert_eq!(
        resume(
            &kernel,
            &changed_snapshot,
            &query_view,
            &keys,
            &output,
            &cursor,
            NOW,
        ),
        Err(RetrievalError::CursorSetMismatch)
    );
}

#[test]
fn cursor_expiry_is_finite_and_exclusive_at_the_exact_boundary() {
    let (kernel, output) =
        composed_with_graph_outcome(RetrieverOutcome::Complete(batch(Vec::new(), "empty")));
    let request = request();
    let query_view = query_view();
    let keys = keyring(&request, 7);
    let cursor = kernel
        .paginate_at(&request, &query_view, &keys, &output, 2, None, NOW)
        .unwrap()
        .cursor
        .unwrap();

    assert_eq!(cursor.expiry, UtcMicros(NOW.0 + CURSOR_TTL_MICROS as i64));
    assert!(
        resume(
            &kernel,
            &request,
            &query_view,
            &keys,
            &output,
            &cursor,
            UtcMicros(cursor.expiry.0 - 1),
        )
        .is_ok()
    );
    assert_eq!(
        resume(
            &kernel,
            &request,
            &query_view,
            &keys,
            &output,
            &cursor,
            cursor.expiry,
        ),
        Err(RetrievalError::CursorExpired)
    );
}
