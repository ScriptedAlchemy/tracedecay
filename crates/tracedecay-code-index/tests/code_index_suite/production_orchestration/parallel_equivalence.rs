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
    EdgeAuthorityV1, FileOccurrenceId, LanguageId, RepositoryDirtyStateV1, RepositoryId,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    SnapshotFileDispositionV1, UtcMicros, canonical_sha256,
};

use super::{
    ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, projection_key,
};
use crate::support::{RUST_SOURCE, id};

/// One module's source: a body whose size varies with `index`, so per-file
/// parse and chunk cost varies widely, plus a uniquely named helper and an
/// imported call into the next module's helper.
fn module_source(index: usize, file_count: usize) -> String {
    let body = RUST_SOURCE.repeat(1 + (index % 7));
    let neighbour = (index + 1) % file_count;
    format!(
        "{body}\n\
         use crate::equivalence::module_{neighbour:04}::equivalence_helper_{neighbour:04};\n\
         pub fn equivalence_helper_{index:04}() {{}}\n\
         pub fn equivalence_caller_{index:04}() {{ equivalence_helper_{neighbour:04}(); }}\n\
         // file {index}\n"
    )
}

/// Multi-file build request whose files differ in content and in cost, so a
/// parallel sweep genuinely reorders completion relative to snapshot order,
/// and whose modules cross-reference each other, so sealing has real cross-file
/// references to resolve against the whole file set.
fn multi_file_request(file_count: usize, sealed_at: i64) -> CodeIndexBuildRequestV1 {
    let mut files = Vec::with_capacity(file_count);
    let mut captured = Vec::with_capacity(file_count);
    let mut receipts = Vec::with_capacity(file_count);
    for index in 0..file_count {
        let bytes = module_source(index, file_count).into_bytes();
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

/// Build the equivalence generation with the indexing pool forced to `width`,
/// then read from it at that same width — encoding fans out too, so the width
/// must still be in force when the caller takes what it wants to compare.
fn at_width<R>(
    width: usize,
    file_count: usize,
    read: impl FnOnce(&CodeIndexPublishedGenerationV1) -> R,
) -> R {
    parallelism::force_indexing_workers_for_test(width);
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(multi_file_request(file_count, 1_100_000), &ActiveControl)
        .expect("equivalence generation publishes");
    let read = read(&generation);
    parallelism::clear_forced_indexing_workers_for_test();
    read
}

fn sealed_bytes_at_width(width: usize, file_count: usize) -> Vec<u8> {
    at_width(width, file_count, |generation| {
        generation.encode_sealed().expect("sealed encoding")
    })
}

/// Every cross-file edge sealing bound, in the generation's own edge order.
fn name_resolved_edges(generation: &CodeIndexPublishedGenerationV1) -> Vec<String> {
    generation
        .edges()
        .iter()
        .filter(|edge| edge.authority == EdgeAuthorityV1::NameResolved)
        .map(|edge| {
            format!(
                "{}|{}|{:?}|{}..{}",
                edge.from_occurrence.as_str(),
                edge.to_occurrence.as_str(),
                edge.kind,
                edge.evidence_span.start_byte,
                edge.evidence_span.end_byte,
            )
        })
        .collect()
}

/// Cross-file references only resolve at sealing, where every file's symbols
/// finally exist together, and that resolution now fans out per file. A memo
/// that leaked between files, or a concatenation that lost file order, would
/// change which edges bind or where they sort — so the resolved edge vector
/// must be identical at width 1 and at full machine width. The count is pinned
/// so a fixture that stopped producing cross-file references cannot make this
/// pass by comparing two empty vectors.
pub(super) fn assert_cross_file_resolution_is_width_invariant() {
    const FILES: usize = 64;

    let sequential = at_width(1, FILES, name_resolved_edges);
    let parallel = at_width(
        parallelism::indexing_worker_target(64),
        FILES,
        name_resolved_edges,
    );

    assert_eq!(
        sequential.len(),
        FILES,
        "fixture stopped exercising cross-file resolution: {sequential:?}"
    );
    assert_eq!(
        sequential, parallel,
        "cross-file reference resolution changed with indexing width"
    );
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
