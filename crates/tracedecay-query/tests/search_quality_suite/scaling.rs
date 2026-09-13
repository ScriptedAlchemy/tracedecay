use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkGrainV1, FileOccurrenceId, FreshnessCompatibilityV1,
    RetrievalFailure, RetrieverOutcome,
};
use tracedecay_query::retrieval::lexical::{
    CodeLexicalProjectionAdapterV1, LexicalLane, LexicalLaneRetriever,
    MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1,
};

use crate::candidate_producers::{chunk, complete, id, lexical_request, projection_metadata};

/// A term shared by more documents than the lane may hydrate per request
/// generates no candidates of its own once a rarer term is present: only the
/// rare term's documents are scored, so the batch cannot fill its cap from
/// common-word-only rows. The lane reports that pruning as a typed `Partial`
/// outcome naming the pruned term rather than silently truncating. Alone, the
/// common term is still admitted and the retrieval is complete.
#[test]
fn common_term_candidates_are_bounded_by_the_rarest_source() {
    let documents = (MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1 + 1) as u32;
    let generation = id::<CodeGenerationId>("generation.1");
    let target = documents / 2;
    let chunks = (0..documents)
        .map(|ordinal| {
            let text = if ordinal == target {
                format!("fn function_{ordinal}() {{ let needle = shared_flag; }}")
            } else {
                format!("fn function_{ordinal}() {{ let shared_flag = true; }}")
            };
            chunk(
                &generation,
                ordinal,
                CodeSearchChunkGrainV1::SymbolBody,
                &text,
                &[],
                &[],
            )
        })
        .collect();
    let mut metadata = projection_metadata(&generation, FreshnessCompatibilityV1::Current);
    metadata.logical_paths.extend((0..documents).map(|ordinal| {
        (
            id::<FileOccurrenceId>(&format!("file.{ordinal}")),
            format!("src/file-{ordinal}.rs"),
        )
    }));
    let lane = LexicalLane::new(
        CodeLexicalProjectionAdapterV1::new(metadata, chunks).expect("postings build"),
    );

    let RetrieverOutcome::Partial {
        value: mixed,
        reason,
    } = lane
        .retrieve_lexical(&lexical_request(
            "needle shared_flag",
            &["needle", "shared_flag"],
            &[],
            &[],
            0,
            8,
        ))
        .expect("mixed-selectivity query")
    else {
        panic!("pruning the common term must be reported as a partial retrieval");
    };
    assert_eq!(
        reason,
        RetrievalFailure::CandidateSourcesPruned {
            term_sources: vec![("shared_flag".to_owned(), u64::from(documents))],
            document_frequency_budget: MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1 as u64,
        }
    );
    assert_eq!(
        mixed.candidates.len(),
        1,
        "only the rare term's document may be hydrated: {:?}",
        mixed.coverage
    );
    assert_eq!(mixed.coverage.eligible, 1);

    let common_only = complete(
        lane.retrieve_lexical(&lexical_request(
            "shared_flag",
            &["shared_flag"],
            &[],
            &[],
            0,
            8,
        ))
        .expect("common-only query"),
    );
    assert_eq!(common_only.candidates.len(), 8);
    assert_eq!(common_only.coverage.eligible, u64::from(documents));
}
