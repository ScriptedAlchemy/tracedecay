use super::*;

#[test]
fn graph_projection_bounds_convergent_fanout_by_nodes_and_edges() {
    let request = graph_request(64, 3);
    let first_layer: Vec<_> = (0..16).map(|index| format!("symbol.a{index:02}")).collect();
    let second_layer: Vec<_> = (0..16).map(|index| format!("symbol.b{index:02}")).collect();
    let mut symbols = vec!["symbol.seed".to_owned(), "symbol.sink".to_owned()];
    symbols.extend(first_layer.iter().cloned());
    symbols.extend(second_layer.iter().cloned());
    let chunks: Vec<_> = symbols
        .iter()
        .map(|symbol| projection_chunk(&request, &format!("chunk.{symbol}"), symbol))
        .collect();

    let mut edges = Vec::new();
    let mut push_edge = |from: &str, to: &str| {
        let offset = edges.len() as u64;
        edges.push(CanonicalRelationEdgeV1 {
            from_occurrence: id(from),
            to_occurrence: id(to),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: offset,
                end_byte: offset + 1,
            },
        });
    };
    for target in &first_layer {
        push_edge("symbol.seed", target);
    }
    for source in &first_layer {
        for target in &second_layer {
            push_edge(source, target);
        }
    }
    for source in &second_layer {
        push_edge(source, "symbol.sink");
    }

    let adapter = CodeGraphEvidenceAdapterV1::new(
        request.generation.clone(),
        None,
        freshness(FreshnessCompatibilityV1::Current),
        &edges,
        &chunks,
    )
    .expect("projection is valid");
    let batch = complete_batch(
        adapter
            .read_graph_evidence(&request)
            .expect("bounded traversal succeeds"),
    );

    assert_eq!(batch.coverage.examined, 288);
    assert_eq!(batch.coverage.eligible, 33);
    let sink = &batch.evidence_by_occurrence[&id("code-graph:symbol.sink")];
    assert_eq!(sink.path.len(), 3);
    assert_eq!(sink.path[0].to.as_str(), "symbol.a00");
    assert_eq!(sink.path[1].to.as_str(), "symbol.b00");
}

#[test]
fn graph_projection_preserves_improving_prefixes_through_a_later_bottleneck() {
    let request = graph_request(8, 3);
    let edge = |from: &str, to: &str, authority, start_byte| CanonicalRelationEdgeV1 {
        from_occurrence: id(from),
        to_occurrence: id(to),
        kind: RelationEdgeKindV1::Calls,
        authority,
        evidence_span: SourceSpan {
            start_byte,
            end_byte: start_byte + 1,
        },
    };
    let edges = vec![
        edge("symbol.seed", "symbol.a", EdgeAuthorityV1::SyntaxExact, 0),
        edge("symbol.seed", "symbol.z", EdgeAuthorityV1::SyntaxExact, 1),
        edge(
            "symbol.a",
            "symbol.join",
            EdgeAuthorityV1::HeuristicCandidate,
            2,
        ),
        edge("symbol.z", "symbol.join", EdgeAuthorityV1::SyntaxExact, 3),
        edge(
            "symbol.join",
            "symbol.target",
            EdgeAuthorityV1::HeuristicCandidate,
            4,
        ),
    ];

    let batch = projection_batch(
        &request,
        &edges,
        &[
            "symbol.seed",
            "symbol.a",
            "symbol.z",
            "symbol.join",
            "symbol.target",
        ],
    );

    let target = &batch.evidence_by_occurrence[&id("code-graph:symbol.target")];
    assert_eq!(target.path[0].to.as_str(), "symbol.a");
    assert_eq!(batch.coverage.examined, 6);
}

#[test]
fn graph_projection_preserves_cross_node_frontiers_through_a_later_bottleneck() {
    let request = graph_request(16, 4);
    let edge = |from: &str, to: &str, authority, start_byte| CanonicalRelationEdgeV1 {
        from_occurrence: id(from),
        to_occurrence: id(to),
        kind: RelationEdgeKindV1::Calls,
        authority,
        evidence_span: SourceSpan {
            start_byte,
            end_byte: start_byte + 1,
        },
    };
    let syntax = EdgeAuthorityV1::SyntaxExact;
    let edges = vec![
        edge("symbol.seed", "symbol.p1", syntax, 0),
        edge("symbol.seed", "symbol.p2", syntax, 1),
        edge("symbol.seed", "symbol.p3", syntax, 2),
        edge(
            "symbol.p1",
            "symbol.x",
            EdgeAuthorityV1::HeuristicCandidate,
            3,
        ),
        edge("symbol.p2", "symbol.y", EdgeAuthorityV1::NameResolved, 4),
        edge("symbol.p3", "symbol.x", syntax, 5),
        edge("symbol.x", "symbol.t", syntax, 6),
        edge("symbol.y", "symbol.t", syntax, 7),
        edge("symbol.t", "symbol.sink", EdgeAuthorityV1::NameResolved, 8),
    ];
    let batch = projection_batch(
        &request,
        &edges,
        &[
            "symbol.seed",
            "symbol.p1",
            "symbol.p2",
            "symbol.p3",
            "symbol.x",
            "symbol.y",
            "symbol.t",
            "symbol.sink",
        ],
    );

    let sink = &batch.evidence_by_occurrence[&id("code-graph:symbol.sink")];
    assert_eq!(sink.path[0].to.as_str(), "symbol.p2");
    assert_eq!(batch.coverage.examined, 12);
    assert_eq!(batch.coverage.eligible, 7);
}
