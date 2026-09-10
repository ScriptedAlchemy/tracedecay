use std::hint::black_box;
use std::time::Instant;

use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkGrainV1, FileOccurrenceId, FreshnessCompatibilityV1,
    RetrievalFailure, RetrieverOutcome, technical_tokens,
};
use tracedecay_query::retrieval::lexical::{
    CodeLexicalProjectionAdapterV1, LexicalLane, LexicalLaneRetriever,
    MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1,
};

use crate::candidate_producers::{chunk, complete, id, lexical_request, projection_metadata};

#[test]
#[ignore = "manual cold/warm lexical scaling benchmark"]
fn immutable_postings_cold_warm_scaling() {
    eprintln!("documents,cold_build_and_first_query_us,warm_query_avg_ns,full_scan_avg_ns");
    for (documents, repetitions) in [(1_000_u32, 100_u32), (10_000, 25), (50_000, 5)] {
        let generation = id::<CodeGenerationId>("generation.1");
        let target = documents / 2;
        let corpus = (0..documents)
            .map(|ordinal| {
                if ordinal == target {
                    format!("fn function_{ordinal}() {{ let needle = true; }}")
                } else {
                    format!("fn function_{ordinal}() {{ let unrelated_{ordinal} = true; }}")
                }
            })
            .collect::<Vec<_>>();
        let chunks = corpus
            .iter()
            .enumerate()
            .map(|(ordinal, text)| {
                chunk(
                    &generation,
                    ordinal as u32,
                    CodeSearchChunkGrainV1::SymbolBody,
                    text,
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
        let request = lexical_request("needle", &["needle"], &[], &[], 0, 8);

        let cold_started = Instant::now();
        let projection = CodeLexicalProjectionAdapterV1::new(metadata, chunks)
            .expect("generation-bound postings build");
        let lane = LexicalLane::new(projection);
        let first = complete(
            lane.retrieve_lexical(&request)
                .expect("first postings query"),
        );
        let cold_elapsed = cold_started.elapsed();
        assert_eq!(first.candidates.len(), 1);

        let warm_started = Instant::now();
        for _ in 0..repetitions {
            let batch = complete(
                lane.retrieve_lexical(&request)
                    .expect("warm postings query"),
            );
            assert_eq!(black_box(batch.candidates.len()), 1);
        }
        let warm_average = warm_started.elapsed().as_nanos() / u128::from(repetitions);

        let scan_started = Instant::now();
        for _ in 0..repetitions {
            let matches = corpus
                .iter()
                .filter(|text| technical_tokens(text).any(|(_, term)| term == "needle"))
                .count();
            assert_eq!(black_box(matches), 1);
        }
        let scan_average = scan_started.elapsed().as_nanos() / u128::from(repetitions);

        eprintln!(
            "{documents},{},{warm_average},{scan_average}",
            cold_elapsed.as_micros()
        );
    }
}

/// A term shared by more documents than the lane may hydrate per request
/// generates no candidates of its own once a rarer term is present: only the
/// rare term's documents are scored, so the batch cannot fill its cap from
/// common-word-only rows. Alone, the common term is still admitted.
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

    let mixed_outcome = lane
        .retrieve_lexical(&lexical_request(
            "needle shared_flag",
            &["needle", "shared_flag"],
            &[],
            &[],
            0,
            8,
        ))
        .expect("mixed-selectivity query");
    let RetrieverOutcome::Partial {
        value: mixed,
        reason:
            RetrievalFailure::CandidateSourcesPruned {
                term_sources,
                document_frequency_budget,
            },
    } = mixed_outcome
    else {
        panic!(
            "mixed-selectivity query must report the typed pruning partial, got {mixed_outcome:?}"
        );
    };
    assert_eq!(
        term_sources,
        vec![("shared_flag".to_owned(), u64::from(documents))],
    );
    assert_eq!(
        document_frequency_budget,
        MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1 as u64,
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
