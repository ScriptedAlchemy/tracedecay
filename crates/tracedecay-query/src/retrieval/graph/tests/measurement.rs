use std::time::Instant;

use super::*;

fn nearest_rank(samples: &mut [u128], percentile: usize) -> u128 {
    samples.sort_unstable();
    samples[(samples.len() * percentile).div_ceil(100) - 1]
}

#[test]
#[ignore = "manual measurement only; run with --ignored --nocapture"]
fn manual_measure_code_graph_projection_and_traversal() {
    const SAMPLES: usize = 25;
    const SYMBOLS: [&str; 16] = [
        "symbol.seed",
        "symbol.n01",
        "symbol.n02",
        "symbol.n03",
        "symbol.n04",
        "symbol.n05",
        "symbol.n06",
        "symbol.n07",
        "symbol.n08",
        "symbol.n09",
        "symbol.n10",
        "symbol.n11",
        "symbol.n12",
        "symbol.n13",
        "symbol.n14",
        "symbol.n15",
    ];
    let request = graph_request(64, 4);
    let chunks: Vec<_> = SYMBOLS
        .iter()
        .map(|symbol| projection_chunk(&request, &format!("chunk.{symbol}"), symbol))
        .collect();
    let edges: Vec<_> = SYMBOLS
        .iter()
        .enumerate()
        .flat_map(|(index, from)| {
            [1usize, 2]
                .into_iter()
                .filter_map(move |step| SYMBOLS.get(index + step).map(|to| (from, to, step)))
                .map(move |(from, to, step)| CanonicalRelationEdgeV1 {
                    from_occurrence: id(from),
                    to_occurrence: id(to),
                    kind: if step == 1 {
                        RelationEdgeKindV1::Calls
                    } else {
                        RelationEdgeKindV1::Uses
                    },
                    authority: EdgeAuthorityV1::SyntaxExact,
                    evidence_span: SourceSpan {
                        start_byte: (index * 2 + step) as u64,
                        end_byte: (index * 2 + step + 1) as u64,
                    },
                })
        })
        .collect();

    let mut builds = Vec::with_capacity(SAMPLES);
    let mut traversals = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        let adapter = CodeGraphEvidenceAdapterV1::new(
            request.generation.clone(),
            None,
            freshness(FreshnessCompatibilityV1::Current),
            &edges,
            &chunks,
        )
        .expect("representative projection builds");
        builds.push(started.elapsed().as_micros());

        let started = Instant::now();
        let batch = complete_batch(
            adapter
                .read_graph_evidence(&request)
                .expect("representative traversal succeeds"),
        );
        traversals.push(started.elapsed().as_micros());
        assert!(!batch.candidates.is_empty());
    }

    println!(
        "manual graph projection/traversal (µs): build p50={} p95={}; traversal p50={} p95={}",
        nearest_rank(&mut builds, 50),
        nearest_rank(&mut builds, 95),
        nearest_rank(&mut traversals, 50),
        nearest_rank(&mut traversals, 95),
    );
}
