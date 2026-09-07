use super::*;

#[test]
fn published_generation_replacement_keeps_existing_reader_frozen() {
    let request_one = graph_request(8, 1);
    let edge_one = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.target-one"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: 0,
            end_byte: 1,
        },
    };
    let cancellation =
        CancellationSignal::active("cancellation.code-graph.generations").expect("valid token");
    let publisher =
        HermeticCodeGraphProjectionStore::memory(&cancellation).expect("open memory graph");
    publish_projection(
        &publisher,
        &request_one,
        std::slice::from_ref(&edge_one),
        &["symbol.seed", "symbol.target-one"],
    );
    let store_one = publisher
        .verified_store(&request_one.generation)
        .expect("open first verified generation");
    let reader_one = store_one
        .evidence_reader(
            &request_one.generation,
            None,
            freshness(FreshnessCompatibilityV1::Current),
            &cancellation,
        )
        .expect("open first generation reader");
    let before = complete_batch(
        reader_one
            .read_graph_evidence(&request_one, super::graph_control())
            .expect("read first generation"),
    );

    let mut request_two = graph_request(8, 1);
    request_two.generation = id("generation.2");
    request_two.seed_anchors = vec![binding(&request_two, "occ.seed", "symbol.seed")];
    let edge_two = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.target-two"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::NameResolved,
        evidence_span: SourceSpan {
            start_byte: 2,
            end_byte: 3,
        },
    };
    publish_projection(
        &publisher,
        &request_two,
        std::slice::from_ref(&edge_two),
        &["symbol.seed", "symbol.target-two"],
    );
    let store_two = publisher
        .verified_store(&request_two.generation)
        .expect("open second verified generation");

    let frozen = complete_batch(
        reader_one
            .read_graph_evidence(&request_one, super::graph_control())
            .expect("frozen reader remains readable"),
    );
    let current = read_projection(&store_two, &request_two, &cancellation);

    assert_eq!(frozen, before);
    result_order(&frozen, &["code-graph:symbol.target-one"]);
    result_order(&current, &["code-graph:symbol.target-two"]);
    assert_eq!(
        publisher
            .verified_store(&request_one.generation)
            .unwrap_err(),
        CodeGraphProjectionError::GenerationMismatch
    );
}

#[test]
fn cancelled_generation_publication_preserves_prior_generation() {
    let request = graph_request(8, 1);
    let edge = CanonicalRelationEdgeV1 {
        from_occurrence: id("symbol.seed"),
        to_occurrence: id("symbol.target"),
        kind: RelationEdgeKindV1::Calls,
        authority: EdgeAuthorityV1::SyntaxExact,
        evidence_span: SourceSpan {
            start_byte: 0,
            end_byte: 1,
        },
    };
    let active = CancellationSignal::active("cancellation.code-graph.prior").expect("valid token");
    let publisher = HermeticCodeGraphProjectionStore::memory(&active).expect("open memory graph");
    publish_projection(
        &publisher,
        &request,
        std::slice::from_ref(&edge),
        &["symbol.seed", "symbol.target"],
    );
    let before_store = publisher
        .verified_store(&request.generation)
        .expect("open prior verified generation");
    let before = read_projection(&before_store, &request, &active);

    let mut replacement = graph_request(8, 1);
    replacement.generation = id("generation.cancelled");
    replacement.seed_anchors = vec![binding(&replacement, "occ.seed", "symbol.seed")];
    let cancelled =
        CancellationSignal::active("cancellation.code-graph.cancelled").expect("valid token");
    assert!(cancelled.cancel(UtcMicros(42)));
    let replacement_chunks = projection_chunks(&replacement, &["symbol.seed", "symbol.other"]);
    let result = publisher.publish_code_graph(
        &replacement.generation,
        &[],
        &replacement_chunks,
        &cancelled,
    );

    assert_eq!(result.unwrap_err(), CodeGraphProjectionError::Cancelled);
    let after_store = publisher
        .verified_store(&request.generation)
        .expect("prior verified generation remains current");
    assert_eq!(read_projection(&after_store, &request, &active), before);
}
