use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fmt::Debug;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::{
    CodeChunker, CodeFileChunksV1, DeterministicCodeChunker, content_digest,
};
use tracedecay_code_index::extract::{LanguageExtractor, NeverCancelled, TreeSitterExtractor};
use tracedecay_code_index::incremental::{GenerationChunkManifestV1, plan_chunk_increment};
use tracedecay_code_index::intake::{CodeIndexIntake, ReceiptBoundCodeFileV1, SanitizedCodeIntake};
use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionSinkErrorV1,
    ProjectionReceiptBuilderV1, ProjectionSinkReceiptV1, expected_request_digest,
    project_for_publication,
};
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChunkerRevision, CodeGenerationId, ExtractionBatchV1, FileOccurrenceId,
    LanguageDescriptorV1, LanguageId, ManifestDigest, PolicyRevisionId, ProjectId,
    ProjectionBatchReceiptV1, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1,
    ProjectionOperationV1, ProjectionOutcomeV1, ProjectionReplayReasonV1, RepositoryId,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    SnapshotFileDispositionV1, UtcMicros, ValidatedCodeFileV1,
};

const WORKLOAD_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/query-code-index/workload-v1.json"
);
const EXPECTED_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/query-code-index/expected-v1.json"
);
const DEFAULT_RESULT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmarks/query-code-index/result-provisional.json"
);
const CHUNKER_V1: &str = "chunker.query-benchmark.v1";
const CHUNKER_REPLAY: &str = "chunker.query-benchmark.replay";
const CHUNKER_INCOMPATIBLE: &str = "chunker.query-benchmark.incompatible";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkloadManifest {
    schema_version: u64,
    workload_id: String,
    harness_revision: String,
    seed: u64,
    repetitions: Repetitions,
    corpus: CorpusManifest,
    scales: Vec<ScaleManifest>,
    cases: Vec<CaseManifest>,
    runtime: RuntimeManifest,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Repetitions {
    warmups: u32,
    measured: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusManifest {
    source_files: Vec<String>,
    content_digest: String,
    descriptor_digest: String,
    language_strata: Vec<LanguageStratum>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LanguageStratum {
    language: String,
    files: u64,
    bytes: u64,
    chunks: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ScaleManifest {
    name: String,
    factor: usize,
    files: u64,
    bytes: u64,
    chunks: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseManifest {
    name: CaseName,
    cache_state: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeManifest {
    cargo_command: String,
    profile: String,
    platform: String,
    arrival_model: String,
    concurrency: u64,
    page_cache: String,
    parser_registry: String,
    clock: String,
    peak_rss: String,
    io_bytes: String,
    hardware_manifest: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum CaseName {
    Clean,
    WarmOneFile,
    Deletion,
    NoOp,
    ChunkerReplay,
    IncompatibleRebuild,
}

impl CaseName {
    const ALL: [Self; 6] = [
        Self::Clean,
        Self::WarmOneFile,
        Self::Deletion,
        Self::NoOp,
        Self::ChunkerReplay,
        Self::IncompatibleRebuild,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::WarmOneFile => "warm_one_file",
            Self::Deletion => "deletion",
            Self::NoOp => "no_op",
            Self::ChunkerReplay => "chunker_replay",
            Self::IncompatibleRebuild => "incompatible_rebuild",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExpectedManifest {
    schema_version: u64,
    workload_id: String,
    scales: Vec<ExpectedScale>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExpectedScale {
    name: String,
    cases: Vec<ExpectedCase>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExpectedCase {
    name: CaseName,
    files_parsed: u64,
    chunks_added_or_changed: u64,
    chunks_deleted: u64,
    chunks_reused: u64,
    projection_calls: u64,
    changed_ranges: u64,
    invalidated_chunks: u64,
    projection_operations: u64,
    full_rebuild_reason: Option<String>,
    input_bytes: u64,
    output_bytes: u64,
}

#[derive(Clone)]
struct WorkloadFile {
    logical_path: String,
    bytes: Vec<u8>,
}

#[derive(Clone)]
struct FileArtifact {
    source: WorkloadFile,
    extraction: ExtractionBatchV1,
    chunks: CodeFileChunksV1,
}

struct BuiltCorpus {
    artifacts: BTreeMap<String, FileArtifact>,
    manifest: GenerationChunkManifestV1,
}

impl BuiltCorpus {
    fn from_artifacts(
        generation_id: CodeGenerationId,
        artifacts: BTreeMap<String, FileArtifact>,
    ) -> Result<Self, String> {
        let files = artifacts
            .values()
            .map(|artifact| artifact.chunks.clone())
            .collect();
        let manifest = GenerationChunkManifestV1::new(generation_id, files)
            .map_err(|error| format!("build generation chunk manifest: {error}"))?;
        Ok(Self {
            artifacts,
            manifest,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSample {
    scale: String,
    case: CaseName,
    repetition: u32,
    files_parsed: u64,
    chunks_added_or_changed: u64,
    chunks_deleted: u64,
    chunks_reused: u64,
    projection_calls: u64,
    corpus_files: u64,
    corpus_bytes: u64,
    input_bytes: u64,
    output_bytes: u64,
    wall_ns: u64,
    event_to_ready_ns: u64,
    queue_delay_ns: u64,
    changed_ranges: u64,
    invalidated_chunks: u64,
    embedding_batches: Option<u64>,
    embedding_chunks: Option<u64>,
    projection_operations: u64,
    invalidation_amplification_per_changed_range: Option<f64>,
    projection_amplification_per_changed_range: Option<f64>,
    full_rebuild_reason: Option<String>,
    cpu_ticks: u64,
    cpu_ms: f64,
    peak_rss_bytes: u64,
    process_read_bytes: u64,
    process_write_bytes: u64,
    process_read_amplification_per_input_byte: Option<f64>,
    process_write_amplification_per_output_byte: Option<f64>,
}

impl RawSample {
    fn expected(&self) -> ExpectedCase {
        ExpectedCase {
            name: self.case,
            files_parsed: self.files_parsed,
            chunks_added_or_changed: self.chunks_added_or_changed,
            chunks_deleted: self.chunks_deleted,
            chunks_reused: self.chunks_reused,
            projection_calls: self.projection_calls,
            changed_ranges: self.changed_ranges,
            invalidated_chunks: self.invalidated_chunks,
            projection_operations: self.projection_operations,
            full_rebuild_reason: self.full_rebuild_reason.clone(),
            input_bytes: self.input_bytes,
            output_bytes: self.output_bytes,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct Distribution {
    samples: usize,
    min: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
    mean: f64,
}

impl Distribution {
    fn from_values(values: impl IntoIterator<Item = f64>) -> Result<Self, String> {
        let mut values = values.into_iter().collect::<Vec<_>>();
        if values.is_empty() {
            return Err("cannot summarize an empty sample set".to_owned());
        }
        values.sort_by(f64::total_cmp);
        let samples = values.len();
        let mean = values.iter().sum::<f64>() / samples as f64;
        Ok(Self {
            samples,
            min: values[0],
            p50: percentile(&values, 50),
            p95: percentile(&values, 95),
            p99: percentile(&values, 99),
            max: values[samples - 1],
            mean,
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CaseResult {
    scale: String,
    case: CaseName,
    cache_state: String,
    wall_ns: Distribution,
    event_to_ready_ns: Distribution,
    queue_delay_ns: Distribution,
    cpu_ms: Distribution,
    peak_rss_bytes: Distribution,
    process_read_bytes: Distribution,
    process_write_bytes: Distribution,
    process_read_amplification_per_input_byte: Option<Distribution>,
    process_write_amplification_per_output_byte: Option<Distribution>,
    input_bytes: u64,
    output_bytes: u64,
    files_parsed: u64,
    chunks_added_or_changed: u64,
    chunks_deleted: u64,
    chunks_reused: u64,
    projection_calls: u64,
    changed_ranges: u64,
    invalidated_chunks: u64,
    embedding_batches: Option<u64>,
    embedding_chunks: Option<u64>,
    projection_operations: u64,
    invalidation_amplification_per_changed_range: Option<f64>,
    projection_amplification_per_changed_range: Option<f64>,
    full_rebuild_reason: Option<String>,
    samples: Vec<RawSample>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct PlatformManifest {
    rustc: String,
    cargo: String,
    kernel: String,
    cpu_model: String,
    logical_cpus: usize,
    memory_total_bytes: u64,
    clock_ticks_per_second: u64,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkResult {
    schema_version: u64,
    evidence_status: &'static str,
    workload_id: String,
    harness_revision: String,
    workload_sha256: String,
    expected_sha256: String,
    captured_at_unix_micros: u128,
    seed: u64,
    repetitions: Repetitions,
    runtime: RuntimeManifest,
    platform: PlatformManifest,
    results: Vec<CaseResult>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CorpusPins {
    content_digest: String,
    descriptor_digest: String,
    current: ScaleManifest,
    ten_x: ScaleManifest,
    language_strata: Vec<LanguageStratum>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct Calibration {
    pins: CorpusPins,
    expected: ExpectedManifest,
}

#[derive(Clone, Copy)]
struct ProcessCounters {
    cpu_ticks: u64,
    read_bytes: u64,
    write_bytes: u64,
}

struct CountingSink {
    calls: u64,
}

impl CodeChunkProjectionSink for CountingSink {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        self.calls += 1;
        let decisions = projection_decisions(&request.changes);
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

fn main() {
    if let Err(error) = run_cli() {
        eprintln!("query code-index benchmark: {error}");
        std::process::exit(1);
    }
}

fn run_cli() -> Result<(), String> {
    let arguments = env::args()
        .skip(1)
        .filter(|argument| argument != "--bench")
        .collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => validate_only(),
        [argument] if argument == "--validate-only" => validate_only(),
        [argument] if argument == "--describe" => describe(),
        [argument] if argument == "--run" => run_measurement(Path::new(DEFAULT_RESULT_PATH)),
        [run, output, path] if run == "--run" && output == "--output" => {
            run_measurement(Path::new(path))
        }
        [sample, scale, case] if sample == "--sample" => run_sample_cli(scale, case),
        _ => Err(
            "usage: cargo bench --bench code_index_chunks -- [--validate-only|--describe|--run [--output PATH]]"
                .to_owned(),
        ),
    }
}

fn validate_only() -> Result<(), String> {
    let workload = load_workload()?;
    let expected = load_expected()?;
    validate_manifests(&workload, &expected)?;
    for scale in &workload.scales {
        for case in &workload.cases {
            let sample = execute_case(&workload, scale, case.name, 0)?;
            validate_sample(&expected, &sample)?;
        }
    }
    println!(
        "validated {} cases at current and exact 10x scales",
        workload.cases.len()
    );
    Ok(())
}

fn describe() -> Result<(), String> {
    let workload = load_workload()?;
    let calibration = calibrate(&workload)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&calibration)
            .map_err(|error| format!("serialize calibration: {error}"))?
    );
    Ok(())
}

fn run_sample_cli(scale_name: &str, case_name: &str) -> Result<(), String> {
    let workload = load_workload()?;
    let scale = workload
        .scales
        .iter()
        .find(|scale| scale.name == scale_name)
        .ok_or_else(|| format!("unknown scale {scale_name}"))?;
    let case = workload
        .cases
        .iter()
        .find(|case| case.name.as_str() == case_name)
        .ok_or_else(|| format!("unknown case {case_name}"))?;
    let sample = execute_case(&workload, scale, case.name, 0)?;
    println!(
        "{}",
        serde_json::to_string(&sample).map_err(|error| format!("serialize sample: {error}"))?
    );
    Ok(())
}

fn run_measurement(output_path: &Path) -> Result<(), String> {
    let workload_bytes = read_repository_file(WORKLOAD_PATH)?;
    let expected_bytes = read_repository_file(EXPECTED_PATH)?;
    let workload: WorkloadManifest = parse_json(&workload_bytes, WORKLOAD_PATH)?;
    let expected: ExpectedManifest = parse_json(&expected_bytes, EXPECTED_PATH)?;
    validate_manifests(&workload, &expected)?;

    let executable =
        env::current_exe().map_err(|error| format!("resolve benchmark executable: {error}"))?;
    let mut results = Vec::new();
    for scale in &workload.scales {
        for case in &workload.cases {
            for _ in 0..workload.repetitions.warmups {
                run_child_sample(&executable, scale, case.name)?;
            }
            let mut samples = Vec::with_capacity(workload.repetitions.measured as usize);
            for repetition in 0..workload.repetitions.measured {
                let mut sample = run_child_sample(&executable, scale, case.name)?;
                sample.repetition = repetition;
                validate_sample(&expected, &sample)?;
                samples.push(sample);
            }
            results.push(summarize_case(scale, case, samples)?);
        }
    }

    let result = BenchmarkResult {
        schema_version: 2,
        evidence_status: "provisional_baseline",
        workload_id: workload.workload_id.clone(),
        harness_revision: workload.harness_revision.clone(),
        workload_sha256: sha256_digest(&workload_bytes),
        expected_sha256: sha256_digest(&expected_bytes),
        captured_at_unix_micros: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system time predates Unix epoch: {error}"))?
            .as_micros(),
        seed: workload.seed,
        repetitions: workload.repetitions,
        runtime: workload.runtime.clone(),
        platform: platform_manifest()?,
        results,
    };
    let mut encoded = serde_json::to_vec_pretty(&result)
        .map_err(|error| format!("serialize benchmark result: {error}"))?;
    encoded.push(b'\n');
    fs::write(output_path, encoded)
        .map_err(|error| format!("write {}: {error}", output_path.display()))?;
    println!("{}", output_path.display());
    Ok(())
}

fn run_child_sample(
    executable: &Path,
    scale: &ScaleManifest,
    case: CaseName,
) -> Result<RawSample, String> {
    let output = Command::new(executable)
        .args(["--sample", &scale.name, case.as_str()])
        .output()
        .map_err(|error| format!("run {} {} sample: {error}", scale.name, case.as_str()))?;
    if !output.status.success() {
        return Err(format!(
            "{} {} sample failed:\n{}",
            scale.name,
            case.as_str(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("parse {} {} sample: {error}", scale.name, case.as_str()))
}

fn execute_case(
    workload: &WorkloadManifest,
    scale: &ScaleManifest,
    case: CaseName,
    repetition: u32,
) -> Result<RawSample, String> {
    let base_sources = load_base_sources(&workload.corpus.source_files)?;
    let sources = expand_sources(&base_sources, scale.factor);
    let descriptor = rust_descriptor()?;
    let extractor = TreeSitterExtractor::new();
    let prior = if case == CaseName::Clean {
        None
    } else {
        Some(build_fresh(
            &sources,
            generation(1)?,
            chunker_revision(CHUNKER_V1)?,
            &descriptor,
            &extractor,
        )?)
    };
    let corpus_bytes = sources.iter().map(|source| source.bytes.len() as u64).sum();

    reset_peak_rss()?;
    let counters_before = process_counters()?;
    let started = Instant::now();
    let (current, files_parsed, input_bytes, replay_reason) = match case {
        CaseName::Clean => (
            build_fresh(
                &sources,
                generation(1)?,
                chunker_revision(CHUNKER_V1)?,
                &descriptor,
                &extractor,
            )?,
            sources.len() as u64,
            corpus_bytes,
            ProjectionReplayReasonV1::InitialProjection,
        ),
        CaseName::WarmOneFile => {
            let prior = prior.as_ref().expect("prior generation");
            let mut current_sources = sources.clone();
            let changed = current_sources
                .first_mut()
                .ok_or_else(|| "warm workload has no source files".to_owned())?;
            changed
                .bytes
                .extend_from_slice(b"\n// query benchmark one-file edit\n");
            let input_bytes = changed.bytes.len() as u64;
            (
                rebuild_one_file(
                    prior,
                    &current_sources,
                    generation(2)?,
                    chunker_revision(CHUNKER_V1)?,
                    &descriptor,
                    &extractor,
                )?,
                1,
                input_bytes,
                ProjectionReplayReasonV1::SourceEdit,
            )
        }
        CaseName::Deletion => (
            carry_forward(
                prior.as_ref().expect("prior generation"),
                generation(2)?,
                sources.last().map(|source| source.logical_path.as_str()),
            )?,
            0,
            0,
            ProjectionReplayReasonV1::SourceEdit,
        ),
        CaseName::NoOp => (
            carry_forward(
                prior.as_ref().expect("prior generation"),
                generation(2)?,
                None,
            )?,
            0,
            0,
            ProjectionReplayReasonV1::VerificationReplay,
        ),
        CaseName::ChunkerReplay => (
            rechunk(
                prior.as_ref().expect("prior generation"),
                generation(2)?,
                chunker_revision(CHUNKER_REPLAY)?,
                &descriptor,
            )?,
            sources.len() as u64,
            corpus_bytes,
            ProjectionReplayReasonV1::VerificationReplay,
        ),
        CaseName::IncompatibleRebuild => (
            build_fresh(
                &sources,
                generation(2)?,
                chunker_revision(CHUNKER_INCOMPATIBLE)?,
                &descriptor,
                &extractor,
            )?,
            sources.len() as u64,
            corpus_bytes,
            ProjectionReplayReasonV1::FullRebuildIncompatible,
        ),
    };
    let changes = plan_chunk_increment(
        prior.as_ref().map(|prior| &prior.manifest),
        &current.manifest,
    )
    .map_err(|error| format!("plan {} increment: {error}", case.as_str()))?;
    let request = projection_request(&changes, case, replay_reason)?;
    let mut sink = CountingSink { calls: 0 };
    let handoff = project_for_publication(&mut sink, request)
        .map_err(|error| format!("project {} changes: {error}", case.as_str()))?;
    // Stop the clock before evidence accounting: serializing the full manifest
    // is measurement bookkeeping, not part of the measured sync journey.
    let wall_ns = u64::try_from(started.elapsed().as_nanos())
        .map_err(|_| "sample wall time overflowed u64".to_owned())?;
    let output_bytes = serde_json::to_vec(&(current.manifest.chunks(), &changes, handoff.receipt()))
        .map_err(|error| format!("serialize {} output evidence: {error}", case.as_str()))?
        .len() as u64;
    let changed_ranges = u64::from(case == CaseName::WarmOneFile);
    let invalidated_chunks = changes.deleted.len() as u64
        + changes
            .added_or_changed
            .iter()
            .filter(|change| change.prior_digest.is_some())
            .count() as u64;
    let projection_operations =
        changes.added_or_changed.len() as u64 + changes.deleted.len() as u64;
    let invalidation_amplification_per_changed_range =
        ratio_per_changed_range(invalidated_chunks, changed_ranges);
    let projection_amplification_per_changed_range =
        ratio_per_changed_range(projection_operations, changed_ranges);
    let full_rebuild_reason =
        (case == CaseName::IncompatibleRebuild).then(|| "chunker_incompatible".to_owned());
    let counters_after = process_counters()?;
    let cpu_ticks = counters_after
        .cpu_ticks
        .saturating_sub(counters_before.cpu_ticks);

    let process_read_bytes = counters_after
        .read_bytes
        .saturating_sub(counters_before.read_bytes);
    let process_write_bytes = counters_after
        .write_bytes
        .saturating_sub(counters_before.write_bytes);

    Ok(RawSample {
        scale: scale.name.clone(),
        case,
        repetition,
        files_parsed,
        chunks_added_or_changed: changes.added_or_changed.len() as u64,
        chunks_deleted: changes.deleted.len() as u64,
        chunks_reused: changes.reused.len() as u64,
        projection_calls: sink.calls,
        corpus_files: current.artifacts.len() as u64,
        corpus_bytes,
        input_bytes,
        output_bytes,
        wall_ns,
        event_to_ready_ns: wall_ns,
        queue_delay_ns: 0,
        changed_ranges,
        invalidated_chunks,
        embedding_batches: None,
        embedding_chunks: None,
        projection_operations,
        invalidation_amplification_per_changed_range,
        projection_amplification_per_changed_range,
        full_rebuild_reason,
        cpu_ticks,
        cpu_ms: cpu_ticks as f64 * 1_000.0 / clock_ticks_per_second()? as f64,
        peak_rss_bytes: process_peak_rss_kib()?.saturating_mul(1024),
        process_read_bytes,
        process_write_bytes,
        process_read_amplification_per_input_byte: ratio_per_bytes(process_read_bytes, input_bytes),
        process_write_amplification_per_output_byte: ratio_per_bytes(
            process_write_bytes,
            output_bytes,
        ),
    })
}

fn build_fresh(
    sources: &[WorkloadFile],
    generation_id: CodeGenerationId,
    chunker_revision: ChunkerRevision,
    descriptor: &LanguageDescriptorV1,
    extractor: &TreeSitterExtractor,
) -> Result<BuiltCorpus, String> {
    let chunker = chunker(generation_id.clone(), chunker_revision)?;
    let mut artifacts = BTreeMap::new();
    for source in sources {
        let artifact = build_artifact(source, &generation_id, descriptor, extractor, &chunker)?;
        artifacts.insert(source.logical_path.clone(), artifact);
    }
    BuiltCorpus::from_artifacts(generation_id, artifacts)
}

fn build_artifact(
    source: &WorkloadFile,
    generation_id: &CodeGenerationId,
    descriptor: &LanguageDescriptorV1,
    extractor: &TreeSitterExtractor,
    chunker: &DeterministicCodeChunker,
) -> Result<FileArtifact, String> {
    let file = receipt_bound_file(source, generation_id)?;
    let extraction = extractor
        .extract(&file, descriptor, &NeverCancelled)
        .map_err(|error| format!("extract {}: {error:?}", source.logical_path))?;
    let chunks = chunker
        .chunk_file(&file, extraction.batch(), descriptor, &NeverCancelled)
        .map_err(|error| format!("chunk {}: {error}", source.logical_path))?;
    let extraction = extraction.batch().clone();
    Ok(FileArtifact {
        source: source.clone(),
        extraction,
        chunks,
    })
}

fn rebuild_one_file(
    prior: &BuiltCorpus,
    sources: &[WorkloadFile],
    generation_id: CodeGenerationId,
    chunker_revision: ChunkerRevision,
    descriptor: &LanguageDescriptorV1,
    extractor: &TreeSitterExtractor,
) -> Result<BuiltCorpus, String> {
    let mut current = carry_forward(prior, generation_id.clone(), None)?;
    let source = sources
        .first()
        .ok_or_else(|| "one-file workload has no source".to_owned())?;
    let chunker = chunker(generation_id.clone(), chunker_revision)?;
    let artifact = build_artifact(source, &generation_id, descriptor, extractor, &chunker)?;
    current
        .artifacts
        .insert(source.logical_path.clone(), artifact);
    BuiltCorpus::from_artifacts(generation_id, current.artifacts)
}

fn carry_forward(
    prior: &BuiltCorpus,
    generation_id: CodeGenerationId,
    deleted_path: Option<&str>,
) -> Result<BuiltCorpus, String> {
    let mut artifacts = BTreeMap::new();
    for (path, artifact) in &prior.artifacts {
        if deleted_path == Some(path.as_str()) {
            continue;
        }
        let mut artifact = artifact.clone();
        rebind_artifact(&mut artifact, &generation_id)?;
        artifacts.insert(path.clone(), artifact);
    }
    BuiltCorpus::from_artifacts(generation_id, artifacts)
}

fn rebind_artifact(
    artifact: &mut FileArtifact,
    generation_id: &CodeGenerationId,
) -> Result<(), String> {
    let occurrence = file_occurrence(&artifact.source.logical_path, generation_id)?;
    artifact.extraction.generation_id = generation_id.clone();
    artifact.extraction.file_occurrence_id = occurrence.clone();
    artifact.chunks.document.generation_id = generation_id.clone();
    artifact.chunks.document.file_occurrence_id = occurrence.clone();
    for chunk in &mut artifact.chunks.chunks {
        chunk.anchor.generation_id = generation_id.clone();
        chunk.anchor.file_occurrence_id = occurrence.clone();
    }
    artifact
        .chunks
        .validate()
        .map_err(|error| format!("validate carried {}: {error}", artifact.source.logical_path))
}

fn rechunk(
    prior: &BuiltCorpus,
    generation_id: CodeGenerationId,
    chunker_revision: ChunkerRevision,
    descriptor: &LanguageDescriptorV1,
) -> Result<BuiltCorpus, String> {
    let chunker = chunker(generation_id.clone(), chunker_revision)?;
    let mut artifacts = BTreeMap::new();
    for artifact in prior.artifacts.values() {
        let mut extraction = artifact.extraction.clone();
        let file = receipt_bound_file(&artifact.source, &generation_id)?;
        extraction.generation_id = generation_id.clone();
        extraction.file_occurrence_id = file.file.file_occurrence_id.clone();
        let chunks = chunker
            .chunk_file(&file, &extraction, descriptor, &NeverCancelled)
            .map_err(|error| format!("rechunk {}: {error}", artifact.source.logical_path))?;
        artifacts.insert(
            artifact.source.logical_path.clone(),
            FileArtifact {
                source: artifact.source.clone(),
                extraction,
                chunks,
            },
        );
    }
    BuiltCorpus::from_artifacts(generation_id, artifacts)
}

fn chunker(
    generation_id: CodeGenerationId,
    chunker_revision: ChunkerRevision,
) -> Result<DeterministicCodeChunker, String> {
    Ok(DeterministicCodeChunker::new(
        generation_id,
        id::<RepositoryId>("repo.query-code-index-benchmark")?,
        id::<SanitizerRevision>("sanitizer.query-benchmark.v1")?,
        id::<PolicyRevisionId>("policy.query-benchmark.v1")?,
        chunker_revision,
        tracedecay_code_extraction::LanguageRegistry::new(),
    ))
}

fn receipt_bound_file(
    source: &WorkloadFile,
    generation_id: &CodeGenerationId,
) -> Result<ReceiptBoundCodeFileV1, String> {
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: file_occurrence(&source.logical_path, generation_id)?,
        logical_path: source.logical_path.clone(),
        language: Some(id::<LanguageId>("rust")?),
        content_digest: content_digest(&source.bytes),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let sanitizer_revision = id::<SanitizerRevision>("sanitizer.query-benchmark.v1")?;
    let intake = SanitizedCodeIntake::new(
        StaticLanguageRegistry::new(),
        sanitizer_revision.clone(),
        UtcMicros(1_000_000),
    );
    let capability = intake
        .admit(SanitizedCodeSnapshotV1 {
            repository: id::<RepositoryId>("repo.query-code-index-benchmark")?,
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision,
            sanitization_receipts: vec![id::<SanitizationReceiptId>(
                "receipt.query-code-index-benchmark.v1",
            )?],
            content_identity: content_digest(&source.bytes),
            captured_at: UtcMicros(1_000_000),
            files: vec![file.clone()],
        })
        .map_err(|error| format!("admit {}: {error:?}", source.logical_path))?;
    intake
        .bind_file(
            &capability,
            &id::<ProjectId>("project.query-code-index-benchmark")?,
            ValidatedCodeFileV1 {
                generation_id: generation_id.clone(),
                file,
                snapshot_digest: capability.snapshot().intake_digest.clone(),
                sanitized_bytes: source.bytes.clone(),
            },
        )
        .map_err(|error| format!("bind {}: {error:?}", source.logical_path))
}

fn file_occurrence(
    logical_path: &str,
    generation_id: &CodeGenerationId,
) -> Result<FileOccurrenceId, String> {
    let digest = sha256_hex(
        [
            logical_path.as_bytes(),
            b"\0",
            generation_id.as_str().as_bytes(),
        ]
        .concat()
        .as_slice(),
    );
    id::<FileOccurrenceId>(&format!("file.query-benchmark.{}", &digest[..32]))
}

fn generation(sequence: u64) -> Result<CodeGenerationId, String> {
    id(&format!("generation.v1.aaaaaaaa.{sequence:08}"))
}

fn chunker_revision(value: &str) -> Result<ChunkerRevision, String> {
    id(value)
}

fn projection_request(
    changes: &ChangedCodeChunkSetV1,
    case: CaseName,
    replay_reason: ProjectionReplayReasonV1,
) -> Result<ProjectionBatchRequestV1, String> {
    let target = projection_key(b"query-code-index-lexical-profile-v1")?;
    let previous_projection_key = (case != CaseName::Clean).then(|| target.clone());
    let mut request = ProjectionBatchRequestV1 {
        request_digest: digest_id::<ManifestDigest>(b"placeholder")?,
        changes: changes.clone(),
        previous_projection_key,
        target_projection_key: target,
        replay_reason,
    };
    request.request_digest =
        expected_request_digest(&request).map_err(|error| format!("request digest: {error}"))?;
    Ok(request)
}

fn projection_key(profile: &[u8]) -> Result<ProjectionKeyV1, String> {
    Ok(ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.query-benchmark.v1".to_owned(),
        profile_digest: digest_id(profile)?,
    })
}

fn projection_decisions(changes: &ChangedCodeChunkSetV1) -> Vec<ChunkProjectionDecisionV1> {
    changes
        .added_or_changed
        .iter()
        .map(|change| ChunkProjectionDecisionV1 {
            chunk_id: change.chunk_id.clone(),
            prior_chunk_digest: change.prior_digest.clone(),
            current_chunk_digest: change.current_digest.clone(),
            operation: if change.prior_digest.is_some() {
                ProjectionOperationV1::Updated
            } else {
                ProjectionOperationV1::Added
            },
            outcome: ProjectionOutcomeV1::Applied,
            output_digest: change.current_digest.clone(),
        })
        .chain(
            changes
                .deleted
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: None,
                    operation: ProjectionOperationV1::Deleted,
                    outcome: ProjectionOutcomeV1::Applied,
                    output_digest: None,
                }),
        )
        .chain(
            changes
                .reused
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: ProjectionOperationV1::Reused,
                    outcome: ProjectionOutcomeV1::Reused,
                    output_digest: None,
                }),
        )
        .collect()
}

fn load_workload() -> Result<WorkloadManifest, String> {
    let bytes = read_repository_file(WORKLOAD_PATH)?;
    parse_json(&bytes, WORKLOAD_PATH)
}

fn load_expected() -> Result<ExpectedManifest, String> {
    let bytes = read_repository_file(EXPECTED_PATH)?;
    parse_json(&bytes, EXPECTED_PATH)
}

fn read_repository_file(path: &str) -> Result<Vec<u8>, String> {
    fs::read(repository_root().join(path)).map_err(|error| format!("read {path}: {error}"))
}

fn parse_json<T: for<'de> Deserialize<'de>>(bytes: &[u8], path: &str) -> Result<T, String> {
    serde_json::from_slice(bytes).map_err(|error| format!("parse {path}: {error}"))
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_base_sources(paths: &[String]) -> Result<Vec<WorkloadFile>, String> {
    let root = repository_root();
    paths
        .iter()
        .map(|path| {
            let bytes = fs::read(root.join(path))
                .map_err(|error| format!("read workload source {path}: {error}"))?;
            Ok(WorkloadFile {
                logical_path: path.clone(),
                bytes,
            })
        })
        .collect()
}

fn expand_sources(base: &[WorkloadFile], factor: usize) -> Vec<WorkloadFile> {
    if factor == 1 {
        return base.to_vec();
    }
    let mut expanded = Vec::with_capacity(base.len() * factor);
    for replica in 0..factor {
        for source in base {
            expanded.push(WorkloadFile {
                logical_path: format!("replica/{replica:02}/{}", source.logical_path),
                bytes: source.bytes.clone(),
            });
        }
    }
    expanded
}

fn rust_descriptor() -> Result<LanguageDescriptorV1, String> {
    let registry = StaticLanguageRegistry::new();
    registry
        .descriptor(&id::<LanguageId>("rust")?)
        .cloned()
        .ok_or_else(|| "compiled language registry has no Rust descriptor".to_owned())
}

fn calibrate(workload: &WorkloadManifest) -> Result<Calibration, String> {
    let base = load_base_sources(&workload.corpus.source_files)?;
    let descriptor = rust_descriptor()?;
    let content_digest = corpus_digest(&base);
    let descriptor_digest = sha256_digest(
        &serde_json::to_vec(&descriptor)
            .map_err(|error| format!("serialize Rust descriptor: {error}"))?,
    );
    let mut scales = Vec::new();
    let mut expected_scales = Vec::new();
    for scale in &workload.scales {
        let sources = expand_sources(&base, scale.factor);
        let clean = build_fresh(
            &sources,
            generation(1)?,
            chunker_revision(CHUNKER_V1)?,
            &descriptor,
            &TreeSitterExtractor::new(),
        )?;
        scales.push(ScaleManifest {
            name: scale.name.clone(),
            factor: scale.factor,
            files: sources.len() as u64,
            bytes: sources.iter().map(|source| source.bytes.len() as u64).sum(),
            chunks: clean.manifest.chunks().len() as u64,
        });
        let mut cases = Vec::new();
        for case in &workload.cases {
            cases.push(execute_case(workload, scale, case.name, 0)?.expected());
        }
        expected_scales.push(ExpectedScale {
            name: scale.name.clone(),
            cases,
        });
    }
    let current = scales
        .iter()
        .find(|scale| scale.name == "current")
        .cloned()
        .ok_or_else(|| "calibration has no current scale".to_owned())?;
    let ten_x = scales
        .iter()
        .find(|scale| scale.name == "10x")
        .cloned()
        .ok_or_else(|| "calibration has no 10x scale".to_owned())?;
    Ok(Calibration {
        pins: CorpusPins {
            content_digest,
            descriptor_digest,
            current: current.clone(),
            ten_x,
            language_strata: vec![LanguageStratum {
                language: "rust".to_owned(),
                files: current.files,
                bytes: current.bytes,
                chunks: current.chunks,
            }],
        },
        expected: ExpectedManifest {
            schema_version: 1,
            workload_id: workload.workload_id.clone(),
            scales: expected_scales,
        },
    })
}

fn validate_manifests(
    workload: &WorkloadManifest,
    expected: &ExpectedManifest,
) -> Result<(), String> {
    if workload.schema_version != 1
        || workload.workload_id != "query-code-index-v1"
        || workload.harness_revision != "code-index-chunks.v2"
    {
        return Err("unsupported workload identity or revision".to_owned());
    }
    if workload.repetitions.warmups != 5 || workload.repetitions.measured != 30 {
        return Err("query requires exactly 5 warmups and 30 measured repetitions".to_owned());
    }
    let cases = workload
        .cases
        .iter()
        .map(|case| case.name)
        .collect::<BTreeSet<_>>();
    if cases != CaseName::ALL.into_iter().collect() || workload.cases.len() != CaseName::ALL.len() {
        return Err("workload must contain every query case exactly once".to_owned());
    }
    let calibration = calibrate(workload)?;
    if workload.corpus.content_digest != calibration.pins.content_digest
        || workload.corpus.descriptor_digest != calibration.pins.descriptor_digest
        || workload.corpus.language_strata != calibration.pins.language_strata
    {
        return Err("workload corpus, descriptor, or language-stratum pins drifted".to_owned());
    }
    let pinned_current = workload
        .scales
        .iter()
        .find(|scale| scale.name == "current")
        .ok_or_else(|| "workload has no current scale".to_owned())?;
    let pinned_ten_x = workload
        .scales
        .iter()
        .find(|scale| scale.name == "10x")
        .ok_or_else(|| "workload has no 10x scale".to_owned())?;
    if pinned_current.factor != 1
        || pinned_ten_x.factor != 10
        || pinned_current.files != calibration.pins.current.files
        || pinned_current.bytes != calibration.pins.current.bytes
        || pinned_current.chunks != calibration.pins.current.chunks
        || pinned_ten_x.files != calibration.pins.ten_x.files
        || pinned_ten_x.bytes != calibration.pins.ten_x.bytes
        || pinned_ten_x.chunks != calibration.pins.ten_x.chunks
    {
        return Err("current or exact 10x scale pins drifted".to_owned());
    }
    if expected.schema_version != 1
        || expected.workload_id != workload.workload_id
        || expected.scales != calibration.expected.scales
    {
        return Err("expected query case counts drifted".to_owned());
    }
    Ok(())
}

fn validate_sample(expected: &ExpectedManifest, sample: &RawSample) -> Result<(), String> {
    let expected_case = expected
        .scales
        .iter()
        .find(|scale| scale.name == sample.scale)
        .and_then(|scale| scale.cases.iter().find(|case| case.name == sample.case))
        .ok_or_else(|| format!("missing expected {} {}", sample.scale, sample.case.as_str()))?;
    let observed = sample.expected();
    if expected_case != &observed {
        return Err(format!(
            "{} {} counts drifted: expected {expected_case:?}, observed {observed:?}",
            sample.scale,
            sample.case.as_str()
        ));
    }
    Ok(())
}

fn summarize_case(
    scale: &ScaleManifest,
    case: &CaseManifest,
    samples: Vec<RawSample>,
) -> Result<CaseResult, String> {
    let expected = samples
        .first()
        .ok_or_else(|| format!("{} {} has no samples", scale.name, case.name.as_str()))?
        .expected();
    if samples.iter().any(|sample| sample.expected() != expected) {
        return Err(format!(
            "{} {} produced nondeterministic work counts",
            scale.name,
            case.name.as_str()
        ));
    }
    let first = samples.first().expect("samples checked nonempty");
    if samples.iter().any(|sample| {
        sample.changed_ranges != first.changed_ranges
            || sample.invalidated_chunks != first.invalidated_chunks
            || sample.embedding_batches != first.embedding_batches
            || sample.embedding_chunks != first.embedding_chunks
            || sample.projection_operations != first.projection_operations
            || sample.invalidation_amplification_per_changed_range
                != first.invalidation_amplification_per_changed_range
            || sample.projection_amplification_per_changed_range
                != first.projection_amplification_per_changed_range
            || sample.full_rebuild_reason != first.full_rebuild_reason
    }) {
        return Err(format!(
            "{} {} produced nondeterministic incremental metrics",
            scale.name,
            case.name.as_str()
        ));
    }
    Ok(CaseResult {
        scale: scale.name.clone(),
        case: case.name,
        cache_state: case.cache_state.clone(),
        wall_ns: Distribution::from_values(samples.iter().map(|sample| sample.wall_ns as f64))?,
        event_to_ready_ns: Distribution::from_values(
            samples.iter().map(|sample| sample.event_to_ready_ns as f64),
        )?,
        queue_delay_ns: Distribution::from_values(
            samples.iter().map(|sample| sample.queue_delay_ns as f64),
        )?,
        cpu_ms: Distribution::from_values(samples.iter().map(|sample| sample.cpu_ms))?,
        peak_rss_bytes: Distribution::from_values(
            samples.iter().map(|sample| sample.peak_rss_bytes as f64),
        )?,
        process_read_bytes: Distribution::from_values(
            samples
                .iter()
                .map(|sample| sample.process_read_bytes as f64),
        )?,
        process_write_bytes: Distribution::from_values(
            samples
                .iter()
                .map(|sample| sample.process_write_bytes as f64),
        )?,
        process_read_amplification_per_input_byte: optional_distribution(
            samples
                .iter()
                .map(|sample| sample.process_read_amplification_per_input_byte),
        )?,
        process_write_amplification_per_output_byte: optional_distribution(
            samples
                .iter()
                .map(|sample| sample.process_write_amplification_per_output_byte),
        )?,
        input_bytes: expected.input_bytes,
        output_bytes: expected.output_bytes,
        files_parsed: expected.files_parsed,
        chunks_added_or_changed: expected.chunks_added_or_changed,
        chunks_deleted: expected.chunks_deleted,
        chunks_reused: expected.chunks_reused,
        projection_calls: expected.projection_calls,
        changed_ranges: first.changed_ranges,
        invalidated_chunks: first.invalidated_chunks,
        embedding_batches: first.embedding_batches,
        embedding_chunks: first.embedding_chunks,
        projection_operations: first.projection_operations,
        invalidation_amplification_per_changed_range: first
            .invalidation_amplification_per_changed_range,
        projection_amplification_per_changed_range: first
            .projection_amplification_per_changed_range,
        full_rebuild_reason: first.full_rebuild_reason.clone(),
        samples,
    })
}

fn ratio_per_changed_range(value: u64, changed_ranges: u64) -> Option<f64> {
    (changed_ranges > 0).then(|| value as f64 / changed_ranges as f64)
}

fn ratio_per_bytes(value: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| value as f64 / denominator as f64)
}

fn optional_distribution(
    values: impl IntoIterator<Item = Option<f64>>,
) -> Result<Option<Distribution>, String> {
    let values = values.into_iter().collect::<Option<Vec<_>>>();
    values.map(Distribution::from_values).transpose()
}

fn percentile(values: &[f64], percentile: usize) -> f64 {
    let index = ((values.len() - 1) * percentile).div_ceil(100);
    values[index]
}

fn corpus_digest(sources: &[WorkloadFile]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.query-code-index-corpus.v1\0");
    for source in sources {
        hasher.update(source.logical_path.as_bytes());
        hasher.update([0]);
        hasher.update((source.bytes.len() as u64).to_be_bytes());
        hasher.update(&source.bytes);
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn digest_id<T>(bytes: &[u8]) -> Result<T, String>
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    id(&sha256_digest(bytes))
}

fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn id<T>(value: &str) -> Result<T, String>
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.to_owned()).map_err(|error| format!("invalid identity {value}: {error:?}"))
}

fn process_counters() -> Result<ProcessCounters, String> {
    Ok(ProcessCounters {
        cpu_ticks: process_cpu_ticks()?,
        read_bytes: proc_value("/proc/self/io", "read_bytes:")?,
        write_bytes: proc_value("/proc/self/io", "write_bytes:")?,
    })
}

fn process_cpu_ticks() -> Result<u64, String> {
    let stat = fs::read_to_string("/proc/self/stat")
        .map_err(|error| format!("read /proc/self/stat: {error}"))?;
    let after_name = stat
        .rfind(')')
        .and_then(|index| stat.get(index + 2..))
        .ok_or_else(|| "missing process-name terminator in /proc/self/stat".to_owned())?;
    let fields = after_name.split_whitespace().collect::<Vec<_>>();
    let user = fields
        .get(11)
        .ok_or_else(|| "missing utime in /proc/self/stat".to_owned())?
        .parse::<u64>()
        .map_err(|error| format!("parse process user ticks: {error}"))?;
    let system = fields
        .get(12)
        .ok_or_else(|| "missing stime in /proc/self/stat".to_owned())?
        .parse::<u64>()
        .map_err(|error| format!("parse process system ticks: {error}"))?;
    user.checked_add(system)
        .ok_or_else(|| "process CPU tick total overflowed u64".to_owned())
}

fn reset_peak_rss() -> Result<(), String> {
    let mut clear_refs = OpenOptions::new()
        .write(true)
        .open("/proc/self/clear_refs")
        .map_err(|error| format!("open /proc/self/clear_refs: {error}"))?;
    clear_refs
        .write_all(b"5\n")
        .map_err(|error| format!("reset process peak RSS: {error}"))
}

fn process_peak_rss_kib() -> Result<u64, String> {
    proc_value("/proc/self/status", "VmHWM:")
}

fn proc_value(path: &str, key: &str) -> Result<u64, String> {
    let contents = fs::read_to_string(path).map_err(|error| format!("read {path}: {error}"))?;
    contents
        .lines()
        .find_map(|line| {
            let (candidate, value) = line.split_once(':')?;
            if candidate.trim() != key.trim_end_matches(':') {
                return None;
            }
            value.split_whitespace().next()?.parse::<u64>().ok()
        })
        .ok_or_else(|| format!("missing or invalid {key} in {path}"))
}

fn clock_ticks_per_second() -> Result<u64, String> {
    let output = command_output("getconf", &["CLK_TCK"])?;
    let ticks = output
        .trim()
        .parse::<u64>()
        .map_err(|error| format!("parse getconf CLK_TCK: {error}"))?;
    if ticks == 0 {
        return Err("getconf CLK_TCK returned zero".to_owned());
    }
    Ok(ticks)
}

fn platform_manifest() -> Result<PlatformManifest, String> {
    Ok(PlatformManifest {
        rustc: command_output("rustc", &["--version"])?,
        cargo: command_output("cargo", &["--version"])?,
        kernel: command_output("uname", &["-srmo"])?,
        cpu_model: cpu_model()?,
        logical_cpus: std::thread::available_parallelism()
            .map_err(|error| format!("logical CPU count: {error}"))?
            .get(),
        memory_total_bytes: proc_value("/proc/meminfo", "MemTotal:")?.saturating_mul(1024),
        clock_ticks_per_second: clock_ticks_per_second()?,
    })
}

fn cpu_model() -> Result<String, String> {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo")
        .map_err(|error| format!("read /proc/cpuinfo: {error}"))?;
    cpuinfo
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "model name").then(|| value.trim().to_owned())
        })
        .ok_or_else(|| "missing CPU model in /proc/cpuinfo".to_owned())
}

fn command_output(command: &str, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new(command)
        .args(arguments)
        .output()
        .map_err(|error| format!("run {command}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| format!("{command} output is not UTF-8: {error}"))
}
