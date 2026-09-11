//! Scratch harness (not committed): build the lexical artifact over an
//! already-sealed partitioned generation directory the way the runtime does
//! (64-page batches, prepare → append → commit, then bounded finalization
//! wakes) and report the wall/commit split, bytes written, and file size.
//!
//! usage: lexical_volume_probe <code-index-v1/<id> dir> <artifact path> [revision]
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::unwrap_used)]

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tracedecay_code_index::production::{
    CodeIndexExecutionControlV1, CodeIndexProductionErrorV1,
    VerifiedSealedLexicalPageBatchBoundsV1, VerifiedSealedLexicalPageBatchReadV1,
    VerifiedSealedLexicalPageSourceV1,
};
use tracedecay_domain::{
    ComponentRevision, FreshnessCompatibilityV1, ManifestDigest, ScoreDomainId, SourceFreshness,
    SourceInstanceKey, SourceNamespace, UtcMicros,
};
use tracedecay_query::retrieval::lexical::{
    CodeLexicalArtifactBuilderV1, CodeLexicalArtifactFinalizationStepV1,
    CodeLexicalArtifactReaderV1, CodeLexicalArtifactWriterRevisionV1,
    CodeLexicalProjectionMetadataV1,
};

const BATCH_PAGES: usize = 64;
const BATCH_BYTES: usize = 64 * 1024 * 1024;
const PAGE_CHUNKS: usize = 128;
const PAGE_BYTES: usize = 4 * 1024 * 1024;
const FINALIZATION_ROWS_PER_WAKE: usize = 128 * 4 * 1024;

struct ActiveControl;

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

fn write_bytes() -> u64 {
    let io = std::fs::read_to_string("/proc/self/io").unwrap();
    io.lines()
        .find_map(|line| line.strip_prefix("write_bytes:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap()
}

/// Reopen a sealed artifact through the production reader and hydrate every
/// `sample_every`-th row by chunk id, printing how many resolved and the
/// wall time. Exercises the row codec + string dictionary on real data.
fn verify_reader(artifact_path: &Path, sample_every: usize) {
    let control = ActiveControl;
    let bytes = std::fs::read(artifact_path).unwrap();
    let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes)).unwrap();
    drop(bytes);
    let size = std::fs::metadata(artifact_path).unwrap().len();
    let _ = tracedecay_private_fs::make_private_file(artifact_path).unwrap();
    let opened = Instant::now();
    let reader = CodeLexicalArtifactReaderV1::open_content_addressed(
        artifact_path,
        &digest,
        size,
        tracedecay_query::retrieval::lexical::CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .unwrap();
    let open_wall = opened.elapsed();
    let inspect = rusqlite::Connection::open_with_flags(
        artifact_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let chunk_ids: Vec<String> = inspect
        .prepare("SELECT chunk_id FROM rows WHERE document_id % ?1 = 0 ORDER BY document_id")
        .unwrap()
        .query_map([sample_every as i64], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let started = Instant::now();
    let mut resolved = 0usize;
    let mut with_symbol = 0usize;
    for chunk_id in &chunk_ids {
        let chunk = tracedecay_domain::CodeSearchChunkId::new(chunk_id.clone()).unwrap();
        let occurrence = reader
            .occurrence_by_chunk(&chunk)
            .unwrap()
            .expect("row present");
        assert_eq!(occurrence.chunk.as_str(), chunk_id);
        assert!(!occurrence.logical_path.is_empty());
        resolved += 1;
        with_symbol += usize::from(occurrence.symbol.is_some());
    }
    println!(
        "reader: revision={} open={:?} hydrated={resolved} with_symbol={with_symbol} wall={:?} ({:.1} us/row)",
        reader.verified_artifact().section_digests().len(),
        open_wall,
        started.elapsed(),
        started.elapsed().as_secs_f64() * 1e6 / resolved.max(1) as f64
    );
}

fn main() {
    #[cfg(feature = "hotpath")]
    let _hotpath = hotpath::HotpathGuardBuilder::new("lexical-volume-probe").build();

    let mut args = std::env::args().skip(1);
    let first = args.next().expect("generation dir or --open");
    if first == "--open" {
        verify_reader(&PathBuf::from(args.next().expect("artifact path")), 97);
        return;
    }
    let generation_dir = PathBuf::from(first);
    let artifact_path = PathBuf::from(args.next().expect("artifact path"));
    let revision = match args.next().as_deref() {
        None | Some("14") => CodeLexicalArtifactWriterRevisionV1::default(),
        Some("13") => CodeLexicalArtifactWriterRevisionV1::V13,
        Some("12") => CodeLexicalArtifactWriterRevisionV1::V12,
        Some("11") => CodeLexicalArtifactWriterRevisionV1::V11,
        Some(other) => panic!("unknown revision {other}"),
    };
    let control = ActiveControl;

    let manifest_path = std::fs::read_dir(generation_dir.join("code-generations-v1"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("generation-") && name.ends_with(".json"))
        })
        .expect("generation manifest");
    let manifest_bytes = std::fs::read(&manifest_path).unwrap();
    let state_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&manifest_bytes)).unwrap();
    let segments_root = generation_dir.join("code-generation-segments-v1");
    let read_segment = move |digest: &ManifestDigest,
                             expected: u64,
                             buffer: &mut Vec<u8>|
          -> Result<(), CodeIndexProductionErrorV1> {
        let hex = digest.as_str().strip_prefix("sha256:").unwrap();
        let path = segments_root.join(format!("segment-{hex}.json"));
        let bytes = std::fs::read(&path).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!("segment {}: {error}", path.display()))
        })?;
        assert_eq!(bytes.len() as u64, expected);
        buffer.clear();
        buffer.extend_from_slice(&bytes);
        Ok(())
    };
    let opened = Instant::now();
    let mut source = VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
        File::open(&manifest_path).unwrap(),
        &manifest_bytes,
        state_digest,
        read_segment,
        PAGE_CHUNKS,
        PAGE_BYTES,
    )
    .unwrap()
    .expect("partitioned sealed source");
    println!("source_open_ms={}", opened.elapsed().as_millis());

    let metadata = {
        let sealed = source.metadata();
        CodeLexicalProjectionMetadataV1 {
            generation: sealed.manifest().generation_id.clone(),
            repository_id: Some(sealed.snapshot().repository.clone()),
            logical_paths: sealed
                .snapshot()
                .files
                .iter()
                .map(|file| (file.file_occurrence_id.clone(), file.logical_path.clone()))
                .collect(),
            freshness: SourceFreshness {
                source_namespace: SourceNamespace::try_from("ns.code.probe".to_owned()).unwrap(),
                source_instance: SourceInstanceKey::try_from("instance.probe".to_owned()).unwrap(),
                source_watermark: Some(1),
                projection_watermark: Some(1),
                observed_at: UtcMicros(1_700_000_000_000_000),
                source_generation: Some(1),
                generation_lag: Some(0),
                compatibility: FreshnessCompatibilityV1::Current,
                policy_revision: ComponentRevision::new("policy.probe.v1").unwrap(),
            },
            exact_retriever_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_EXACT_RETRIEVER_REVISION_V1,
            )
            .unwrap(),
            lexical_retriever_revision: ComponentRevision::new(
                tracedecay_query::retrieval::QUERY_LEXICAL_RETRIEVER_REVISION_V1,
            )
            .unwrap(),
            exact_score_domain: ScoreDomainId::new(
                tracedecay_query::retrieval::QUERY_EXACT_SCORE_DOMAIN_V1,
            )
            .unwrap(),
        }
    };

    let _ = std::fs::remove_file(&artifact_path);
    let started = Instant::now();
    let written_before = write_bytes();
    let mut builder = CodeLexicalArtifactBuilderV1::create_with_memory_budget_and_format_revision(
        &artifact_path,
        metadata,
        tracedecay_query::retrieval::lexical::CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        revision,
    )
    .unwrap();
    let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(BATCH_PAGES, BATCH_BYTES).unwrap();
    let mut batches = 0usize;
    let mut pages_total = 0usize;
    let mut stage_wall = Duration::ZERO;
    let mut prepare_wall = Duration::ZERO;
    let mut append_wall = Duration::ZERO;
    let mut max_append = Duration::ZERO;
    let receipt = loop {
        let stage_started = Instant::now();
        let mut prepare_elapsed = Duration::ZERO;
        let mut append_elapsed = Duration::ZERO;
        let read =
            source
                .next_page_batch_if(&control, bounds, |pages| {
                    let prepare_started = Instant::now();
                    let prepared = builder.prepare_admissible_page_prefix(pages, &control)?;
                    prepare_elapsed = prepare_started.elapsed();
                    let append_started = Instant::now();
                    builder.append_prepared_pages(prepared.prepared_pages(), &control)?;
                    append_elapsed = append_started.elapsed();
                    Ok::<
                        NonZeroUsize,
                        tracedecay_query::retrieval::lexical::CodeLexicalArtifactErrorV1,
                    >(prepared.accepted_prefix())
                })
                .unwrap()
                .unwrap();
        match read {
            VerifiedSealedLexicalPageBatchReadV1::Pages(pages) => {
                batches += 1;
                pages_total += pages.len();
                stage_wall += stage_started.elapsed() - prepare_elapsed - append_elapsed;
                prepare_wall += prepare_elapsed;
                append_wall += append_elapsed;
                max_append = max_append.max(append_elapsed);
                if batches.is_multiple_of(8) {
                    eprintln!(
                        "batch {batches}: pages={} prepare={:?} append={:?} file={} MiB",
                        pages.len(),
                        prepare_elapsed,
                        append_elapsed,
                        std::fs::metadata(&artifact_path).unwrap().len() / 1_048_576
                    );
                }
            }
            VerifiedSealedLexicalPageBatchReadV1::Complete(receipt) => break receipt,
        }
    };
    let source_phase_wall = started.elapsed();
    let written_after_source = write_bytes();
    println!(
        "source_phase: batches={batches} pages={pages_total} wall={:.3}s stage={:.3}s prepare={:.3}s append(commit)={:.3}s max_append={:.3}s mean_append={:.3}s written={} MiB file={} MiB",
        source_phase_wall.as_secs_f64(),
        stage_wall.as_secs_f64(),
        prepare_wall.as_secs_f64(),
        append_wall.as_secs_f64(),
        max_append.as_secs_f64(),
        append_wall.as_secs_f64() / batches.max(1) as f64,
        (written_after_source - written_before) / 1_048_576,
        std::fs::metadata(&artifact_path).unwrap().len() / 1_048_576
    );

    let finalization_started = Instant::now();
    let mut wakes = 0usize;
    let mut last_phase = String::new();
    let mut phase_started = Instant::now();
    let verified = loop {
        wakes += 1;
        match builder
            .advance_finalization(&receipt, FINALIZATION_ROWS_PER_WAKE, &control)
            .unwrap()
        {
            CodeLexicalArtifactFinalizationStepV1::Pending { phase, .. } => {
                let phase = format!("{phase:?}");
                if phase != last_phase {
                    if !last_phase.is_empty() {
                        println!(
                            "finalization phase {last_phase}: {:.3}s",
                            phase_started.elapsed().as_secs_f64()
                        );
                    }
                    last_phase = phase;
                    phase_started = Instant::now();
                }
            }
            CodeLexicalArtifactFinalizationStepV1::Ready(verified) => break verified,
        }
    };
    println!(
        "finalization phase {last_phase}: {:.3}s",
        phase_started.elapsed().as_secs_f64()
    );
    let written_after = write_bytes();
    println!(
        "finalization: wakes={wakes} wall={:.3}s written={} MiB",
        finalization_started.elapsed().as_secs_f64(),
        (written_after - written_after_source) / 1_048_576
    );
    println!(
        "total: wall={:.3}s written={} MiB file_size={} bytes artifact_digest={} sections={:?}",
        started.elapsed().as_secs_f64(),
        (written_after - written_before) / 1_048_576,
        verified.file_size_bytes(),
        verified.artifact_digest().as_str(),
        verified
            .section_digests()
            .iter()
            .map(|section| (section.name.clone(), section.row_count))
            .collect::<Vec<_>>()
    );
    drop(builder);
    verify_reader(&artifact_path, 97);
}
