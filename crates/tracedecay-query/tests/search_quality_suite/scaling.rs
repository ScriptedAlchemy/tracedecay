use tracedecay_domain::{RetrievalFailure, RetrieverOutcome};
use tracedecay_query::retrieval::lexical::{LexicalLaneRetriever, MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1};

use crate::candidate_producers::{complete, real_lexical_source_fixture_from_sources, sealed_artifact};

/// A term shared by more documents than the lane may hydrate per request
/// generates no candidates of its own once a rarer term is present: only the
/// rare term's documents are scored, so the batch cannot fill its cap from
/// common-word-only rows. The lane reports that pruning as a typed `Partial`
/// outcome naming the pruned term rather than silently truncating. Alone, the
/// common term is still admitted and the retrieval is complete.
#[test]
fn common_term_candidates_are_bounded_by_the_rarest_source() {
    let files = 128;
    let functions_per_file = MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1 / files + 1;
    let fixture = real_lexical_source_fixture_from_sources(
        (0..files)
            .map(|file| {
                let source = (0..functions_per_file)
                    .map(|function| {
                        if file == files / 2 && function == 0 {
                            format!("pub fn function_{function}() {{ let needle = shared_flag; }}\n")
                        } else {
                            format!("pub fn function_{function}() {{ let shared_flag = true; }}\n")
                        }
                    })
                    .collect::<String>();
                (
                    format!("file.scaling.{file:03}"),
                    format!("src/scaling_{file:03}.rs"),
                    source.into_bytes(),
                )
            })
            .collect(),
    );
    let artifact = sealed_artifact(&fixture, fixture.metadata.clone());
    let lane = artifact.lane();

    let common_only = complete(
        lane.retrieve_lexical(&artifact.request("shared_flag", &["shared_flag"], &[], &[], 0, 8))
            .expect("common-only query"),
    );
    assert_eq!(common_only.candidates.len(), 8);
    let documents = common_only.coverage.eligible;
    assert!(
        documents > MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1 as u64,
        "the common term must exceed the document-frequency budget: {documents}"
    );

    let RetrieverOutcome::Partial {
        value: mixed,
        reason,
    } = lane
        .retrieve_lexical(&artifact.request(
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
            term_sources: vec![("shared_flag".to_owned(), documents)],
            document_frequency_budget: MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1 as u64,
        }
    );
    assert!(
        !mixed.candidates.is_empty() && mixed.candidates.len() == mixed.coverage.eligible as usize,
        "only the rare term's documents may be hydrated: {:?}",
        mixed.coverage
    );
    assert!(mixed.coverage.eligible < 8);
}
