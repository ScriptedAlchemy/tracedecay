use tempfile::TempDir;

use super::*;

#[test]
fn graph_projection_reopens_with_identical_ordered_output() {
    let request = graph_request(8, 2);
    let edges = vec![
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed"),
            to_occurrence: id("symbol.middle"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.middle"),
            to_occurrence: id("symbol.target"),
            kind: RelationEdgeKindV1::Uses,
            authority: EdgeAuthorityV1::HeuristicCandidate,
            evidence_span: SourceSpan {
                start_byte: 1,
                end_byte: 2,
            },
        },
    ];
    let cancellation =
        CancellationSignal::active("cancellation.code-graph.reopen").expect("valid token");
    let temp = TempDir::new().expect("temporary graph directory");
    let path = temp.path().join("code.grafeo");
    let store =
        CodeGraphProjectionStore::open(&path, &cancellation).expect("open persistent graph");
    publish_projection(
        &store,
        &request,
        &edges,
        &["symbol.seed", "symbol.middle", "symbol.target"],
    );
    let before = read_projection(&store, &request, &cancellation);
    store.close().expect("close persistent graph");

    let reopened =
        CodeGraphProjectionStore::open(&path, &cancellation).expect("reopen persistent graph");
    let after = read_projection(&reopened, &request, &cancellation);

    assert_eq!(after, before);
    assert_eq!(
        serde_json::to_vec(&after).expect("serialize reopened output"),
        serde_json::to_vec(&before).expect("serialize original output")
    );
    result_order(
        &after,
        &["code-graph:symbol.middle", "code-graph:symbol.target"],
    );
    assert_eq!(
        after.evidence_by_occurrence[&id("code-graph:symbol.target")].weakest_authority,
        EdgeAuthorityV1::HeuristicCandidate
    );
}

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
    let store = CodeGraphProjectionStore::memory(&cancellation).expect("open memory graph");
    publish_projection(
        &store,
        &request_one,
        std::slice::from_ref(&edge_one),
        &["symbol.seed", "symbol.target-one"],
    );
    let reader_one = store
        .evidence_reader(
            &request_one.generation,
            None,
            freshness(FreshnessCompatibilityV1::Current),
            &cancellation,
        )
        .expect("open first generation reader");
    let before = complete_batch(
        reader_one
            .read_graph_evidence(&request_one)
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
        &store,
        &request_two,
        std::slice::from_ref(&edge_two),
        &["symbol.seed", "symbol.target-two"],
    );

    let frozen = complete_batch(
        reader_one
            .read_graph_evidence(&request_one)
            .expect("frozen reader remains readable"),
    );
    let current = read_projection(&store, &request_two, &cancellation);

    assert_eq!(frozen, before);
    result_order(&frozen, &["code-graph:symbol.target-one"]);
    result_order(&current, &["code-graph:symbol.target-two"]);
    assert_eq!(
        store
            .evidence_reader(
                &request_one.generation,
                None,
                freshness(FreshnessCompatibilityV1::Current),
                &cancellation,
            )
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
    let store = CodeGraphProjectionStore::memory(&active).expect("open memory graph");
    publish_projection(
        &store,
        &request,
        std::slice::from_ref(&edge),
        &["symbol.seed", "symbol.target"],
    );
    let before = read_projection(&store, &request, &active);

    let mut replacement = graph_request(8, 1);
    replacement.generation = id("generation.cancelled");
    replacement.seed_anchors = vec![binding(&replacement, "occ.seed", "symbol.seed")];
    let cancelled =
        CancellationSignal::active("cancellation.code-graph.cancelled").expect("valid token");
    assert!(cancelled.cancel(UtcMicros(42)));
    let replacement_chunks = projection_chunks(&replacement, &["symbol.seed", "symbol.other"]);
    let result = store.publish_code_graph(
        &replacement.generation,
        &[],
        &replacement_chunks,
        &cancelled,
    );

    assert_eq!(result.unwrap_err(), CodeGraphProjectionError::Cancelled);
    assert_eq!(read_projection(&store, &request, &active), before);
}
