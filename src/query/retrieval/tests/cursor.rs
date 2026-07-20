use tracedecay_domain::{RetrievalError, RetrievalFailure, RetrieverKind, RetrieverOutcome};

use super::{batch, candidate, composition_lanes, id, no_caps, profile, request};
use crate::query::retrieval::fusion::{CompositionKernel, FusionStageInput};

fn composed_with_graph_outcome(
    graph: RetrieverOutcome<tracedecay_domain::RetrieverBatch<&'static str>>,
) -> (
    CompositionKernel,
    crate::query::retrieval::fusion::CompositionOutputV1,
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
    let first = kernel.paginate(&request(), &output, 2, None).unwrap();

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
        .paginate(&request(), &output, 2, Some(&cursor))
        .unwrap();
    assert_eq!(second.ranked_candidates[0].final_ordinal, 2);
    assert!(second.cursor.is_none());
}

#[test]
fn cursor_rejects_a_differently_completed_candidate_set() {
    let (kernel, output) =
        composed_with_graph_outcome(RetrieverOutcome::Complete(batch(Vec::new(), "empty")));
    let cursor = kernel
        .paginate(&request(), &output, 2, None)
        .unwrap()
        .cursor
        .unwrap();

    let mut changed = output;
    changed.ranked_candidates.pop();
    assert_eq!(
        kernel.paginate(&request(), &changed, 2, Some(&cursor)),
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
    let denied_cursor = kernel
        .paginate(&request(), &denied, 2, None)
        .unwrap()
        .cursor
        .unwrap();
    let unavailable_cursor = kernel
        .paginate(&request(), &unavailable, 2, None)
        .unwrap()
        .cursor
        .unwrap();
    assert_eq!(denied_cursor, unavailable_cursor);
}
