use super::*;

#[test]
fn graph_projection_breaks_equal_score_ties_by_canonical_full_path() {
    let mut request = graph_request(8, 1);
    request.seed_anchors = vec![
        binding(&request, "occ.seed-z", "symbol.seed-z"),
        binding(&request, "occ.seed-a", "symbol.seed-a"),
    ];
    let target = id::<SymbolOccurrenceId>("symbol.target");
    let edges = vec![
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed-z"),
            to_occurrence: target.clone(),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed-a"),
            to_occurrence: target,
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 1,
                end_byte: 2,
            },
        },
    ];

    let result = projection_batch(
        &request,
        &edges,
        &["symbol.seed-z", "symbol.seed-a", "symbol.target"],
    );

    result.validate().expect("batch remains valid");
    let evidence = &result.evidence_by_occurrence[&id("code-graph:symbol.target")];
    assert_eq!(evidence.path[0].from.as_str(), "symbol.seed-a");
}

#[test]
fn graph_projection_relaxes_a_same_seed_diamond_to_the_stronger_path() {
    let request = graph_request(8, 2);
    let edges = vec![
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed"),
            to_occurrence: id("symbol.a-weak"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.seed"),
            to_occurrence: id("symbol.z-strong"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 1,
                end_byte: 2,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.a-weak"),
            to_occurrence: id("symbol.target"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::HeuristicCandidate,
            evidence_span: SourceSpan {
                start_byte: 2,
                end_byte: 3,
            },
        },
        CanonicalRelationEdgeV1 {
            from_occurrence: id("symbol.z-strong"),
            to_occurrence: id("symbol.target"),
            kind: RelationEdgeKindV1::Calls,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: 3,
                end_byte: 4,
            },
        },
    ];

    let result = projection_batch(
        &request,
        &edges,
        &[
            "symbol.seed",
            "symbol.a-weak",
            "symbol.z-strong",
            "symbol.target",
        ],
    );

    result.validate().expect("batch remains valid");
    assert_eq!(
        result
            .candidates
            .iter()
            .filter(
                |candidate| candidate.source_occurrence_id.as_str() == "code-graph:symbol.target"
            )
            .count(),
        1
    );
    let evidence = &result.evidence_by_occurrence[&id("code-graph:symbol.target")];
    assert_eq!(evidence.path[0].to.as_str(), "symbol.z-strong");
    assert_eq!(evidence.weakest_authority, EdgeAuthorityV1::SyntaxExact);
}
