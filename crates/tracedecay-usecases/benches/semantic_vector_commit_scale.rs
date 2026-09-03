//! Peak-memory measurement for the semantic vector generation commit loop at
//! projection scale (issue #754).
//!
//! Drives the real `VectorGenerationStateMachineV1` through a full-rebuild
//! projection of `TD_SCALE_CHUNKS` chunks (default 120k) at `TD_SCALE_DIMS`
//! dimensions (default 768, the cataloged jina-v2-base-code width), committed
//! in `TD_SCALE_BATCH`-chunk batches (default 512, the durable stage bound).
//! Per-batch prepared corpora are generated streamingly so the bench's own
//! working set stays O(batch); every retained byte belongs to the subject.
//!
//! `TD_COMMIT_PATTERN` selects the drive shape:
//! - `adapter` (default): mirrors `GraphVectorGenerationStoreV1::
//!   commit_batch_records` — validate the batch against the unmodified
//!   machine (elided staged values), apply the decided effects in place, and
//!   attempt `publish_generation` after every batch.
//! - `machine`: plain retained-value `commit_batch` per batch and one
//!   terminal publish — the in-memory reference-model drive.
//!
//! Run:
//!   cargo bench -p tracedecay-usecases --bench semantic_vector_commit_scale
//! Output is CSV-ish milestones: batch, committed rows, VmRSS, VmHWM.

use std::fmt::Write as _;
use std::time::Instant;

#[path = "hotpath_coverage.rs"]
mod hotpath_coverage;

use sha2::{Digest, Sha256};
use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, build_batch_receipt, expected_request_digest,
};
use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision,
    CodeGenerationId, CodeSearchChunkId, ContentDigest, EmbeddingDeviceClassV1,
    EmbeddingDocumentCompositionV1, EmbeddingMetricV1, EmbeddingNormalizationV1,
    EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1,
    ManifestDigest, PrivacyDomainId, ProjectionBatchRequestV1, ProjectionOperationV1,
    ProjectionOutcomeV1, ProjectionReplayReasonV1,
};
use tracedecay_semantic::projector::{
    PreparedVectorGenerationV1, ProjectedChunkVectorV1, vector_output_digest,
};
use tracedecay_usecases::store::vector_generations::{
    BatchCommitDecisionV1, StagedVectorValueRetentionV1, VectorGenerationPlanV1,
    VectorGenerationStateMachineV1,
};

fn env_scale(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("canonical bench identity")
}

fn fixed_digest(label: u8) -> ManifestDigest {
    id(&format!("sha256:{}", format!("{label:02x}").repeat(32)))
}

fn content_digest(bytes: &[u8]) -> ContentDigest {
    let mut encoded = String::from("sha256:");
    for byte in Sha256::digest(bytes) {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string is infallible");
    }
    id(&encoded)
}

fn embedding_key(dimensions: u32) -> AdmittedEmbeddingProjectionKeyV1 {
    EmbeddingProjectionKeyV1 {
        model_artifact_digest: fixed_digest(1),
        tokenizer_digest: fixed_digest(2),
        config_digest: fixed_digest(3),
        query_instruction_digest: Some(fixed_digest(4)),
        document_instruction_digest: Some(fixed_digest(5)),
        document_composition: EmbeddingDocumentCompositionV1::SanitizedText,
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 512,
        inference_batch_size: 32,
        inference_batch_bytes: 512 * 1024,
        runtime_backend: "fastembed-ort".to_owned(),
        runtime_build_revision: "commit-scale-bench.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_owned(),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        privacy_domain: id::<PrivacyDomainId>("privacy.commit-scale-bench"),
        privacy_key_epoch: 1,
    }
    .admit()
    .expect("admitted bench embedding key")
}

fn chunk_id(index: usize) -> CodeSearchChunkId {
    id(&format!("chunk.v1.scale-{index:07}"))
}

fn chunk_digest(index: usize) -> ContentDigest {
    content_digest(format!("scale chunk body {index:07}").as_bytes())
}

/// SplitMix64-derived deterministic vector, finite by construction.
fn deterministic_vector(seed: usize, dimensions: usize) -> Vec<f32> {
    let mut state = seed as u64 ^ 0x9e37_79b9_7f4a_7c15;
    (0..dimensions)
        .map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut mixed = state;
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            mixed ^= mixed >> 31;
            ((mixed % 2_000_003) as f32 / 2_000_003.0) - 0.5
        })
        .collect()
}

/// One canonical full-rebuild batch: request, receipt, and vector rows for
/// chunks `[start, start + len)`, byte-identical across runs.
fn prepared_batch(
    embedding: &AdmittedEmbeddingProjectionKeyV1,
    generation: &CodeGenerationId,
    start: usize,
    len: usize,
    dimensions: usize,
) -> PreparedVectorGenerationV1 {
    let projection_key = embedding.projection_key().clone();
    let added = (start..start + len)
        .map(|index| ChangedCodeChunkV1 {
            chunk_id: chunk_id(index),
            prior_digest: None,
            current_digest: Some(chunk_digest(index)),
        })
        .collect::<Vec<_>>();
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: None,
        to_generation: generation.clone(),
        manifest_digest: fixed_digest(0),
        added_or_changed: added,
        deleted: Vec::new(),
        reused: Vec::new(),
    };
    changes.manifest_digest = changes.compute_digest().expect("changed set digest");
    let mut request = ProjectionBatchRequestV1 {
        request_digest: changes.manifest_digest.clone(),
        changes,
        previous_projection_key: None,
        target_projection_key: projection_key.clone(),
        replay_reason: ProjectionReplayReasonV1::FullRebuildIncompatible,
    };
    request.request_digest = expected_request_digest(&request).expect("request digest");

    let mut vectors = Vec::with_capacity(len);
    let mut decisions = Vec::with_capacity(len);
    for change in &request.changes.added_or_changed {
        let index_digest = change
            .current_digest
            .clone()
            .expect("bench change carries a digest");
        let seed = start + vectors.len();
        let values = deterministic_vector(seed, dimensions);
        let output_digest =
            vector_output_digest(&projection_key, &change.chunk_id, &index_digest, &values)
                .expect("vector output digest");
        decisions.push(ChunkProjectionDecisionV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest: None,
            current_chunk_digest: Some(index_digest.clone()),
            operation: ProjectionOperationV1::Added,
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: Some(output_digest.clone()),
        });
        vectors.push(ProjectedChunkVectorV1 {
            projection_key: projection_key.clone(),
            source_generation: generation.clone(),
            source_manifest_digest: request.changes.manifest_digest.clone(),
            chunk_id: change.chunk_id.clone(),
            chunk_digest: index_digest,
            values,
            output_digest,
        });
    }
    let receipt = build_batch_receipt(&request, &decisions).expect("bench batch receipt");
    PreparedVectorGenerationV1 {
        embedding_key: embedding.clone(),
        request,
        receipt,
        vectors,
        tombstones: Vec::new(),
    }
}

fn proc_status_kib(field: &str) -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .expect("proc status field")
}

fn report(label: &str, batch: usize, rows: usize, started: &Instant) {
    println!(
        "{label},batch={batch},rows={rows},vmrss_mib={},vmhwm_mib={},elapsed_s={:.1}",
        proc_status_kib("VmRSS:") / 1024,
        proc_status_kib("VmHWM:") / 1024,
        started.elapsed().as_secs_f64(),
    );
}

const COMMIT_SWEEP_LABEL: &str = "usecases.semantic.commit_scale.sweep";
const EXPECTED_HOTPATH_LABELS: &[&str] = &[COMMIT_SWEEP_LABEL];
const _: () = assert!(!EXPECTED_HOTPATH_LABELS.is_empty());

fn main() {
    // First statement on purpose: may set Hotpath environment for the guard,
    // which is sound only before any other thread exists.
    let coverage = hotpath_coverage::init("tracedecay-semantic-vector-commit-scale");
    let chunks = env_scale("TD_SCALE_CHUNKS", 120_000);
    let dimensions = env_scale("TD_SCALE_DIMS", 768);
    let batch_len = env_scale("TD_SCALE_BATCH", 512);
    let pattern = std::env::var("TD_COMMIT_PATTERN").unwrap_or_else(|_| "adapter".to_owned());
    let embedding = embedding_key(u32::try_from(dimensions).expect("bench dimension width"));
    let generation: CodeGenerationId = id("code-generation.commit-scale");
    let plan = VectorGenerationPlanV1 {
        target_projection_key: embedding.projection_key().clone(),
        source_generation: generation.clone(),
        source_manifest_digest: fixed_digest(9),
        expected_chunk_ids: (0..chunks).map(chunk_id).collect::<Vec<_>>().into(),
        base_generation: None,
    };
    println!(
        "config,pattern={pattern},chunks={chunks},dims={dimensions},batch={batch_len},\
         float_mib={}",
        chunks * dimensions * 4 / (1024 * 1024)
    );
    let started = Instant::now();
    let mut state = match pattern.as_str() {
        "adapter" => VectorGenerationStateMachineV1::with_staged_value_retention(
            StagedVectorValueRetentionV1::Elided,
        ),
        _ => VectorGenerationStateMachineV1::new(),
    };
    let build = state.begin_generation(plan).expect("begin bench build");
    let mut checkpoint = None;
    let batch_count = chunks.div_ceil(batch_len);
    let report_every = (batch_count / 8).max(1);
    report("begin", 0, 0, &started);
    let mut committed_rows = 0_usize;
    let mut publication = None;
    hotpath::measure_block!(COMMIT_SWEEP_LABEL, {
        for batch_index in 0..batch_count {
            let start = batch_index * batch_len;
            let len = batch_len.min(chunks - start);
            let prepared = prepared_batch(&embedding, &generation, start, len, dimensions);
            match pattern.as_str() {
                "adapter" => {
                    // Mirror of `commit_batch_records`: decide against the
                    // unmodified machine (the durable append happens between the
                    // halves in production), apply in place, then attempt the
                    // per-batch publication probe.
                    let staged_commit = match state
                        .validate_batch(&build, checkpoint.as_ref(), &prepared)
                        .expect("bench batch validates")
                    {
                        BatchCommitDecisionV1::Replay(_) => panic!("bench batches never replay"),
                        BatchCommitDecisionV1::Commit(staged_commit) => staged_commit,
                    };
                    checkpoint = Some(
                        state
                            .apply_batch(&build, staged_commit)
                            .expect("bench batch commits"),
                    );
                    publication = state.publish_generation(&build).ok();
                }
                "machine" => {
                    checkpoint = Some(
                        state
                            .commit_batch(&build, checkpoint.as_ref(), prepared)
                            .expect("bench batch commits"),
                    );
                }
                other => panic!("unknown TD_COMMIT_PATTERN {other:?}"),
            }
            committed_rows += len;
            if (batch_index + 1) % report_every == 0 {
                report("commit", batch_index + 1, committed_rows, &started);
            }
        }
        if pattern == "machine" {
            publication = Some(state.publish_generation(&build).expect("bench publication"));
        }
    });
    let publication = publication.expect("bench run publishes");
    report("published", batch_count, committed_rows, &started);
    let generation_rows = state
        .generation(&publication.generation_id)
        .expect("published generation")
        .vectors()
        .len();
    assert_eq!(generation_rows, chunks, "published corpus is complete");
    drop(state);
    report("dropped", batch_count, committed_rows, &started);
    hotpath_coverage::finish(coverage, EXPECTED_HOTPATH_LABELS);
}
