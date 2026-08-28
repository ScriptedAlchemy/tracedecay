use std::{collections::BTreeSet, sync::Arc};

use tracedecay_code_index::{
    chunks::content_digest,
    parallelism,
    production::{
        CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexProductionOwnerV1,
        CodeIndexPublishedGenerationV1, CodeIndexRepositoryParseIdentityV1,
    },
};
use tracedecay_domain::{
    FileOccurrenceId, LanguageId, RepositoryDirtyStateV1, RepositoryId, SanitizationReceiptId,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1,
    UtcMicros, canonical_sha256,
};

use super::{
    ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, projection_key,
};
use crate::support::{RUST_SOURCE, id};

/// Read one `/proc/self/status` figure in KiB.
fn status_kib(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|line| line.starts_with(&format!("{field}:")))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn mib(kib: u64) -> f64 {
    kib as f64 / 1024.0
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Multi-file build request whose files differ in content and in cost, so a
/// parallel sweep genuinely reorders completion relative to snapshot order.
fn multi_file_request(file_count: usize, sealed_at: i64) -> CodeIndexBuildRequestV1 {
    let mut files = Vec::with_capacity(file_count);
    let mut captured = Vec::with_capacity(file_count);
    let mut receipts = Vec::with_capacity(file_count);
    for index in 0..file_count {
        // Vary body size so per-file parse/chunk cost varies widely.
        let body = RUST_SOURCE.repeat(1 + (index % 7));
        let source = format!("{body}\n// file {index}\n");
        let bytes = source.as_bytes().to_vec();
        let occurrence = id::<FileOccurrenceId>(&format!("file.equivalence.{index:04}"));
        files.push(SanitizedCodeFileV1 {
            file_occurrence_id: occurrence.clone(),
            logical_path: format!("src/equivalence/module_{index:04}.rs"),
            language: Some(id::<LanguageId>("rust")),
            content_digest: content_digest(&bytes),
            disposition: SnapshotFileDispositionV1::Present,
        });
        captured.push(CodeIndexCapturedFileV1 {
            file_occurrence_id: occurrence,
            sanitized_bytes: Arc::from(bytes),
            sensitivity_level: tracedecay_domain::SensitivityLevelV1::Public,
        });
        receipts.push(id::<SanitizationReceiptId>(&format!(
            "receipt.equivalence.{index:04}"
        )));
    }
    let identity = content_digest(
        files
            .iter()
            .map(|file| file.logical_path.clone())
            .collect::<Vec<_>>()
            .join("\n")
            .as_bytes(),
    );

    CodeIndexBuildRequestV1 {
        snapshot: SanitizedCodeSnapshotV1 {
            repository: id::<RepositoryId>("repository.production"),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
            sanitization_receipts: receipts,
            content_identity: identity,
            captured_at: UtcMicros(1_000_000),
            files,
        },
        captured_files: captured,
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            tree: None,
            dirty: RepositoryDirtyStateV1::Dirty,
        },
        sealed_at: UtcMicros(sealed_at),
        target_projection_key: projection_key(),
    }
}

fn sealed_bytes_at_width(width: usize, file_count: usize) -> Vec<u8> {
    parallelism::force_indexing_workers_for_test(width);
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(multi_file_request(file_count, 1_100_000), &ActiveControl)
        .expect("equivalence generation publishes");
    let bytes = generation.encode_sealed().expect("sealed encoding");
    parallelism::clear_forced_indexing_workers_for_test();
    bytes
}

pub(super) fn assert_parallel_and_sequential_generations_are_byte_identical() {
    const FILES: usize = 64;

    let sequential = sealed_bytes_at_width(1, FILES);
    let parallel = sealed_bytes_at_width(parallelism::indexing_worker_target(64), FILES);

    assert_eq!(
        sequential.len(),
        parallel.len(),
        "sealed generation length changed with indexing width"
    );
    assert!(
        sequential == parallel,
        "sealed generation bytes changed with indexing width"
    );
    assert_eq!(
        canonical_sha256(&sequential).expect("sequential digest"),
        canonical_sha256(&parallel).expect("parallel digest"),
    );
}

/// The row census a decoded generation exposes, so a width change that
/// silently dropped, duplicated, or reordered restored rows cannot pass by
/// re-encoding to the same length.
#[derive(Debug, PartialEq, Eq)]
struct DecodedCensus {
    generation_id: String,
    state_digest: String,
    chunks: usize,
    symbols: usize,
    imports: usize,
    edges: usize,
    edge_abstentions: usize,
    lineage: usize,
    snapshot_files: usize,
}

fn decode_at_width(width: usize, sealed: &[u8]) -> (Vec<u8>, DecodedCensus) {
    parallelism::force_indexing_workers_for_test(width);
    let generation =
        CodeIndexPublishedGenerationV1::decode_sealed(sealed).expect("sealed generation decodes");
    let census = DecodedCensus {
        generation_id: generation.manifest().generation_id.as_str().to_owned(),
        state_digest: generation
            .projection()
            .publication_digest()
            .as_str()
            .to_owned(),
        chunks: generation.chunks().chunks().len(),
        symbols: generation.symbols().symbols.len(),
        imports: generation.imports().len(),
        edges: generation.edges().len(),
        edge_abstentions: generation.edge_abstentions().len(),
        lineage: generation.lineage().len(),
        snapshot_files: generation.snapshot().files.len(),
    };
    // Re-encoding is canonical, so identical re-encoded bytes prove the whole
    // decoded state — every restored row, in order — is identical, not just
    // the fields the census names.
    let reencoded = generation.encode_sealed().expect("sealed re-encoding");
    parallelism::clear_forced_indexing_workers_for_test();
    (reencoded, census)
}

/// Width is sizing policy on the way in as well as on the way out. Restoring
/// each file's exact-extraction authority fans out across the indexing pool,
/// so a generation decoded with that sweep running inline must restore exactly
/// the state a full-width decode restores — same rows, same order, same bytes.
pub(super) fn assert_parallel_and_sequential_decodes_are_byte_identical() {
    const FILES: usize = 64;

    let sealed = sealed_bytes_at_width(1, FILES);

    let (sequential_bytes, sequential_census) = decode_at_width(1, &sealed);
    let (parallel_bytes, parallel_census) =
        decode_at_width(parallelism::indexing_worker_target(64), &sealed);

    assert_eq!(
        sequential_census, parallel_census,
        "decoded generation census changed with indexing width"
    );
    assert!(
        sequential_bytes == parallel_bytes,
        "re-encoded generation bytes changed with decode width"
    );
    // A width-1 decode must reproduce the exact bytes it was handed, so the
    // sequential path is pinned to the seal itself and not merely to itself.
    assert!(
        sequential_bytes == sealed,
        "width-1 decode did not round-trip the sealed bytes"
    );
    assert_eq!(
        canonical_sha256(&sequential_bytes).expect("sequential digest"),
        canonical_sha256(&parallel_bytes).expect("parallel digest"),
    );
}

/// Sealed-decode measurement harness. Not a contract — nothing here asserts on
/// a timing or a memory figure.
///
/// `VmHWM` only ever rises within a process, so a peak-RSS figure is only
/// comparable across widths when each width runs in its own process. Run one
/// width per invocation:
///
/// ```text
/// TRACEDECAY_DECODE_WIDTH=1 TRACEDECAY_DECODE_FILES=4000 \
///   cargo test -p tracedecay-code-index --all-features --profile perf \
///   --test code_index_suite -- --ignored --nocapture --exact \
///   production_orchestration::sealed_decode_width_probe
/// ```
///
/// `TRACEDECAY_DECODE_WIDTH=0` (the default) uses the host width.
pub(super) fn run_sealed_decode_width_probe() {
    let files = env_usize("TRACEDECAY_DECODE_FILES", 4_000);
    let width = env_usize("TRACEDECAY_DECODE_WIDTH", 0);

    // Build and seal at full width; only the decode is under measurement.
    let sealed = sealed_bytes_at_width(parallelism::indexing_worker_target(64), files);

    if width > 0 {
        parallelism::force_indexing_workers_for_test(width);
    }
    let effective = parallelism::indexing_workers();

    let before_rss = status_kib("VmRSS").unwrap_or(0);
    let started = std::time::Instant::now();
    let generation =
        CodeIndexPublishedGenerationV1::decode_sealed(&sealed).expect("sealed generation decodes");
    let decode_wall = started.elapsed();
    let after_rss = status_kib("VmRSS").unwrap_or(0);
    let peak_rss = status_kib("VmHWM").unwrap_or(0);

    let chunks = generation.chunks().chunks().len();
    let symbols = generation.symbols().symbols.len();
    drop(generation);
    parallelism::clear_forced_indexing_workers_for_test();

    println!("=== sealed decode width probe ===");
    println!("files              {files}");
    println!("chunks             {chunks}");
    println!("symbols            {symbols}");
    println!("sealed bytes       {}", sealed.len());
    println!("effective width    {effective}");
    println!("decode wall        {decode_wall:?}");
    println!("VmRSS before       {:.1} MiB", mib(before_rss));
    println!("VmRSS after        {:.1} MiB", mib(after_rss));
    println!(
        "VmRSS delta        {:.1} MiB",
        mib(after_rss.saturating_sub(before_rss))
    );
    println!("VmHWM (peak)       {:.1} MiB", mib(peak_rss));
}
