#[test]
fn distribution_missing_or_invalid_artifacts_use_typed_fallback_and_strict_unavailable() {
    struct TestSemanticRetrievalSelectionPolicyV1;
    impl SemanticRetrievalSelectionPolicyV1 for TestSemanticRetrievalSelectionPolicyV1 {
        fn select(
            &self,
            availability: SemanticRetrievalAvailabilityV1,
            requirement: SemanticRetrievalRequirementV1,
        ) -> SemanticRetrievalSelectionV1 {
            if availability == SemanticRetrievalAvailabilityV1::Ready {
                SemanticRetrievalSelectionV1::Semantic
            } else if requirement == SemanticRetrievalRequirementV1::FallbackAllowed {
                SemanticRetrievalSelectionV1::FrozenFallback
            } else {
                SemanticRetrievalSelectionV1::Unavailable
            }
        }
    }

    for (state, expected) in [
        (
            SemanticIndexStateV1::Unavailable,
            SemanticAbstentionV1::IndexUnavailable,
        ),
        (
            SemanticIndexStateV1::Incompatible,
            SemanticAbstentionV1::IndexIncompatible,
        ),
    ] {
        let query_view = query_view();
        let projection = projection();
        let request = request(&query_view, &projection, 1);
        let embedder = FakeQueryEmbedder::default();
        let vectors = FakeVectorReadPort::new(&request, Vec::new());
        let control = FixedExecutionControl::default();
        let lane = SemanticCodeRetriever::new(&embedder, &vectors, &control);
        let fallback = fallback();
        let fallback_identity = Arc::as_ptr(&fallback);

        let permissive =
            CalibratedSemanticQueryService::new(&lane, &TestSemanticRetrievalSelectionPolicyV1)
            .execute(
                SemanticLaneReadinessV1::Unavailable(state),
                SemanticQueryModeV1::FallbackAllowed,
                Arc::clone(&fallback),
            )
            .expect("artifact unavailability must preserve the declared fallback");
        assert!(matches!(
            &permissive,
            SemanticQueryServiceOutcomeV1::Fallback {
                abstention,
                ..
            } if abstention == &expected
        ));
        assert_eq!(Arc::as_ptr(permissive.fallback()), fallback_identity);
        assert_eq!(
            embedder.calls.get(),
            0,
            "unavailable artifacts cannot embed"
        );
        assert_eq!(vectors.scans.get(), 0, "unavailable artifacts cannot scan");

        assert_eq!(
            CalibratedSemanticQueryService::new(&lane, &TestSemanticRetrievalSelectionPolicyV1)
                .execute(
                    SemanticLaneReadinessV1::Unavailable(state),
                    SemanticQueryModeV1::StrictSemantic,
                    fallback,
                )
                .err(),
            Some(SemanticQueryServiceError::StrictUnavailable(expected))
        );
    }
}
