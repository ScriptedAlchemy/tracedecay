use std::collections::BTreeSet;

use tracedecay_code_index::{
    chunks::content_digest,
    parallelism,
    production::{
        CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexProductionOwnerV1,
        CodeIndexRepositoryParseIdentityV1,
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
            sanitized_bytes: bytes,
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
