use std::time::Instant;

use super::*;

#[cfg(target_os = "linux")]
fn process_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kibibytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kibibytes.checked_mul(1024)
}

#[cfg(not(target_os = "linux"))]
fn process_rss_bytes() -> Option<u64> {
    None
}

#[test]
#[ignore = "manual measurement only; run with --ignored --nocapture"]
fn manual_measure_code_graph_projection_and_traversal() {
    const SYMBOL_COUNT: usize = 50_000;
    const FANOUT: usize = 2;
    let request = graph_request(64, 4);
    let symbols: Vec<_> = std::iter::once("symbol.seed".to_owned())
        .chain((1..SYMBOL_COUNT).map(|index| format!("symbol.n{index:05}")))
        .collect();
    let chunks: Vec<_> = symbols
        .iter()
        .map(|symbol| projection_chunk(&request, &format!("chunk.{symbol}"), symbol))
        .collect();
    let mut edges = Vec::with_capacity(SYMBOL_COUNT * FANOUT);
    for (index, from) in symbols.iter().enumerate() {
        for step in 1..=FANOUT {
            if let Some(to) = symbols.get(index + step) {
                edges.push(CanonicalRelationEdgeV1 {
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
                });
            }
        }
    }

    let rss_before = process_rss_bytes();
    let started = Instant::now();
    let adapter = CodeGraphEvidenceReader::new(
        request.generation.clone(),
        None,
        freshness(FreshnessCompatibilityV1::Current),
        &edges,
        &chunks,
    )
    .expect("representative projection builds");
    let build_elapsed = started.elapsed();
    let rss_after = process_rss_bytes();

    let started = Instant::now();
    let batch = complete_batch(
        adapter
            .read_graph_evidence(&request)
            .expect("representative traversal succeeds"),
    );
    let traversal_elapsed = started.elapsed();
    result_order(
        &batch,
        &[
            "code-graph:symbol.n00001",
            "code-graph:symbol.n00002",
            "code-graph:symbol.n00003",
            "code-graph:symbol.n00004",
            "code-graph:symbol.n00005",
            "code-graph:symbol.n00006",
            "code-graph:symbol.n00007",
            "code-graph:symbol.n00008",
        ],
    );
    assert_eq!(
        batch.coverage,
        RetrieverCoverage {
            examined: 14,
            eligible: 8,
            excluded: 0,
            capped: 0,
            unknown: 0,
        }
    );

    println!(
        "manual graph projection: symbols={SYMBOL_COUNT} edges={} build_ms={} \
         traversal_us={} rss_before_bytes={} rss_after_bytes={} rss_delta_bytes={}",
        edges.len(),
        build_elapsed.as_millis(),
        traversal_elapsed.as_micros(),
        rss_before.unwrap_or_default(),
        rss_after.unwrap_or_default(),
        rss_after
            .zip(rss_before)
            .map(|(after, before)| after.saturating_sub(before))
            .unwrap_or_default(),
    );
}
