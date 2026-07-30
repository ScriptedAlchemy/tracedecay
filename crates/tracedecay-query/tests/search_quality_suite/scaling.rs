use std::hint::black_box;
use std::time::Instant;

use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkGrainV1, FileOccurrenceId, FreshnessCompatibilityV1,
};
use tracedecay_query::retrieval::lexical::{
    CodeLexicalProjectionAdapterV1, LexicalLane, LexicalLaneRetriever,
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
                .filter(|text| {
                    text.split(|character: char| {
                        !(character.is_ascii_alphanumeric()
                            || matches!(character, '_' | '-' | ':' | '.' | '/'))
                    })
                    .any(|term| term == "needle")
                })
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
