use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use tracedecay_domain::{
    Pr9FallbackSubpayload, PublicRetrieverStatus, RetrieverBatch, RetrieverKind, RetrieverOutcome,
};
use tracedecay_query::retrieval::RetrievalPortError;
use tracedecay_query::retrieval::semantic::{
    CalibratedSemanticQueryService, CodeSemanticEvidenceV1, SemanticAbstentionV1,
    SemanticIndexStateV1, SemanticLaneReadinessV1, SemanticLaneRetriever, SemanticQueryModeV1,
    SemanticQueryServiceError, SemanticQueryServiceOutcomeV1,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn fallback() -> Arc<Pr9FallbackSubpayload> {
    let mut fallback = Pr9FallbackSubpayload {
        profile_id: id("profile.pr9.semantic-contract.v1"),
        ordered_candidates: Vec::new(),
        public_pr9_lane_coverage: BTreeMap::from([
            (RetrieverKind::ExactLiteral, PublicRetrieverStatus::Complete),
            (RetrieverKind::Lexical, PublicRetrieverStatus::Complete),
            (RetrieverKind::Graph, PublicRetrieverStatus::Complete),
        ]),
        freshness: Vec::new(),
        cursor: None,
        digest: id(&format!("sha256:{}", "0".repeat(64))),
    };
    fallback.digest = fallback.compute_digest().expect("fallback digest");
    Arc::new(fallback)
}

struct NeverCalledSemanticLane;

impl SemanticLaneRetriever for NeverCalledSemanticLane {
    fn retrieve_semantic(
        &self,
        _request: &tracedecay_query::retrieval::semantic::SemanticRetrievalRequestV1<'_>,
    ) -> Result<RetrieverOutcome<RetrieverBatch<CodeSemanticEvidenceV1>>, RetrievalPortError> {
        panic!("a non-ready semantic lane must never be invoked")
    }
}

#[test]
fn semantic_warmup_preserves_foreground_fallback_bytes() {
    let fallback = fallback();
    let fallback_bytes = serde_json::to_vec(fallback.as_ref()).expect("serialize fallback");
    let service = CalibratedSemanticQueryService::new(&NeverCalledSemanticLane);

    for (state, expected) in [
        (
            SemanticIndexStateV1::Unavailable,
            SemanticAbstentionV1::IndexUnavailable,
        ),
        (
            SemanticIndexStateV1::Indexing,
            SemanticAbstentionV1::Indexing,
        ),
        (
            SemanticIndexStateV1::Degraded,
            SemanticAbstentionV1::IndexDegraded,
        ),
        (
            SemanticIndexStateV1::Failed,
            SemanticAbstentionV1::IndexFailed,
        ),
        (
            SemanticIndexStateV1::Stale,
            SemanticAbstentionV1::IndexStale,
        ),
        (
            SemanticIndexStateV1::Incompatible,
            SemanticAbstentionV1::IndexIncompatible,
        ),
    ] {
        let outcome = service
            .execute(
                SemanticLaneReadinessV1::Unavailable(state),
                SemanticQueryModeV1::FallbackAllowed,
                Arc::clone(&fallback),
            )
            .expect("foreground retrieval remains available");
        let SemanticQueryServiceOutcomeV1::Fallback { abstention, .. } = &outcome else {
            panic!("non-ready semantic state must use the unchanged foreground fallback");
        };
        assert_eq!(abstention, &expected);
        assert_eq!(
            serde_json::to_vec(outcome.fallback().as_ref()).expect("serialize returned fallback"),
            fallback_bytes
        );
        assert!(matches!(
            service.execute(
                SemanticLaneReadinessV1::Unavailable(state),
                SemanticQueryModeV1::StrictSemantic,
                Arc::clone(&fallback),
            ),
            Err(SemanticQueryServiceError::StrictUnavailable(ref reason)) if reason == &expected
        ));
    }
}
