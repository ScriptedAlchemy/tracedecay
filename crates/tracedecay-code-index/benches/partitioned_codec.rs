use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt::Debug,
    hint::black_box,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use ignore::WalkBuilder;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracedecay_code_index::{
    chunks::content_digest,
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
        CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexProductionConfigV1,
        CodeIndexProductionErrorV1, CodeIndexProductionOwnerV1, CodeIndexPublicationStoreErrorV1,
        CodeIndexPublishedGenerationV1, CodeIndexRepositoryParseIdentityV1,
        VerifiedSealedLexicalPageSourceV1,
    },
    projection::{
        ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
        ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
    },
};
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationId, FileOccurrenceId, LanguageId, ManifestDigest,
    PolicyRevisionId, PrivacyDomainId, ProjectId, ProjectionBatchRequestV1, ProjectionKeyV1,
    ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1, RepositoryDirtyStateV1,
    RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SensitivityLevelV1, SnapshotFileDispositionV1, UtcMicros,
};

const CORPUS_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../benchmark_data/index-bench/corpus"
);
const REPLICAS: usize = 10;
const WARMUPS: usize = 2;
const MEASURED: usize = 5;
const DEFAULT_HOTPATH_BYTES_PATH: &str = "/tmp/tracedecay-partitioned-codec-bytes.json";
const DEFAULT_HOTPATH_COUNT_PATH: &str = "/tmp/tracedecay-partitioned-codec-count.json";

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

#[derive(Clone)]
struct SourceFile {
    logical_path: String,
    language: LanguageId,
    bytes: Arc<[u8]>,
}

#[derive(Default)]
struct BenchmarkPublication;

impl CodeIndexAtomicPublicationPort for BenchmarkPublication {
    fn load_active(
        &self,
        _scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(None)
    }

    fn publish_atomically(
        &mut self,
        _scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        _generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        if expected_active_generation.is_some() {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        Ok(())
    }
}

struct ApplyingProjection;

impl CodeChunkProjectionSink for ApplyingProjection {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let decisions = request
            .changes
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
                request
                    .changes
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
                request
                    .changes
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
            .collect::<Vec<_>>();
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

struct ActiveControl;

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct EncodedFixture {
    manifest: Vec<u8>,
    segments: BTreeMap<String, Vec<u8>>,
}

#[derive(Serialize)]
struct Distribution {
    samples: usize,
    p50_ns: u64,
    p95_ns: u64,
}

#[derive(Serialize)]
struct Measurement {
    schema_version: u32,
    allocation_metric: &'static str,
    corpus_files: usize,
    corpus_bytes: usize,
    replicas: usize,
    measured_iterations: usize,
    manifest_size_bytes: usize,
    segment_count: usize,
    segment_bytes: usize,
    bytes_per_file: f64,
    vm_hwm_bytes: u64,
    encode_wall: Distribution,
    decode_and_open_wall: Distribution,
}

fn main() -> Result<(), Box<dyn Error>> {
    let count_allocations = std::env::args().any(|argument| argument == "--alloc-count");
    let output_path = configure_hotpath(count_allocations);
    let sources = replicated_sources()?;
    let corpus_bytes = sources.iter().map(|source| source.bytes.len()).sum();
    let generation = build_generation(&sources)?;
    let fixture = encode_once(&generation)?;
    for _ in 0..WARMUPS {
        black_box(encode_once(&generation)?);
        decode_and_open(&fixture)?;
    }

    reset_peak_rss()?;
    let guard = hotpath::HotpathGuardBuilder::new("partitioned-codec-bench")
        .format(hotpath::Format::Json)
        .output_path(output_path)
        .build();
    let mut encode_wall = Vec::with_capacity(MEASURED);
    let mut decode_wall = Vec::with_capacity(MEASURED);
    for _ in 0..MEASURED {
        let started = Instant::now();
        let encoded = hotpath::measure_block!(
            "code_index.generation.publish.segment_encode",
            encode_once(&generation)?
        );
        encode_wall.push(duration_ns(started.elapsed())?);
        assert_fixture_identity(&fixture, &encoded)?;

        let started = Instant::now();
        hotpath::measure_block!(
            "code_index.generation.decode.bundle",
            decode_and_open(&fixture)?
        );
        decode_wall.push(duration_ns(started.elapsed())?);
    }
    drop(guard);

    let segment_bytes = fixture.segments.values().map(Vec::len).sum::<usize>();
    let measurement = Measurement {
        schema_version: 1,
        allocation_metric: if count_allocations { "count" } else { "bytes" },
        corpus_files: sources.len(),
        corpus_bytes,
        replicas: REPLICAS,
        measured_iterations: MEASURED,
        manifest_size_bytes: fixture.manifest.len(),
        segment_count: fixture.segments.len(),
        segment_bytes,
        bytes_per_file: segment_bytes as f64 / sources.len() as f64,
        vm_hwm_bytes: proc_value("/proc/self/status", "VmHWM:")?.saturating_mul(1024),
        encode_wall: distribution(encode_wall),
        decode_and_open_wall: distribution(decode_wall),
    };
    println!("{}", serde_json::to_string_pretty(&measurement)?);
    Ok(())
}

fn configure_hotpath(count_allocations: bool) -> PathBuf {
    let output_path = std::env::var_os("HOTPATH_OUTPUT_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(if count_allocations {
                DEFAULT_HOTPATH_COUNT_PATH
            } else {
                DEFAULT_HOTPATH_BYTES_PATH
            })
        });
    unsafe {
        std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
        std::env::set_var("HOTPATH_REPORT", "functions-alloc");
        std::env::set_var("HOTPATH_OUTPUT_PATH", &output_path);
        std::env::set_var(
            "HOTPATH_ALLOC_METRIC",
            if count_allocations { "count" } else { "bytes" },
        );
    }
    output_path
}

fn replicated_sources() -> Result<Vec<SourceFile>, Box<dyn Error>> {
    let root = Path::new(CORPUS_ROOT);
    let mut base = WalkBuilder::new(root)
        .hidden(false)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .ok_or("corpus file has no UTF-8 extension")?;
            let language = language_for_extension(extension)?;
            let bytes: Arc<[u8]> = std::fs::read(path)?.into();
            Ok::<_, Box<dyn Error>>(SourceFile {
                logical_path: relative,
                language,
                bytes,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    base.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    let mut sources = Vec::with_capacity(base.len() * REPLICAS);
    for replica in 0..REPLICAS {
        sources.extend(base.iter().map(|source| SourceFile {
            logical_path: format!("replica/{replica:02}/{}", source.logical_path),
            language: source.language.clone(),
            bytes: Arc::clone(&source.bytes),
        }));
    }
    Ok(sources)
}

fn language_for_extension(extension: &str) -> Result<LanguageId, Box<dyn Error>> {
    let language = match extension {
        "go" => "go",
        "java" => "java",
        "py" => "python",
        "rs" => "rust",
        "ts" => "typescript",
        other => return Err(format!("unsupported benchmark extension {other}").into()),
    };
    Ok(id(language)?)
}

fn build_generation(
    sources: &[SourceFile],
) -> Result<Arc<CodeIndexPublishedGenerationV1>, Box<dyn Error>> {
    let mut files = Vec::with_capacity(sources.len());
    let mut captured_files = Vec::with_capacity(sources.len());
    let mut receipts = Vec::with_capacity(sources.len());
    let mut identity = Sha256::new();
    for (index, source) in sources.iter().enumerate() {
        let occurrence = id::<FileOccurrenceId>(&format!(
            "file.partitioned-codec.{}",
            &hex::encode(Sha256::digest(source.logical_path.as_bytes()))[..32]
        ))?;
        let digest = content_digest(&source.bytes);
        identity.update(source.logical_path.as_bytes());
        identity.update([0]);
        identity.update(digest.as_str().as_bytes());
        files.push(SanitizedCodeFileV1 {
            file_occurrence_id: occurrence.clone(),
            logical_path: source.logical_path.clone(),
            language: Some(source.language.clone()),
            content_digest: digest,
            disposition: SnapshotFileDispositionV1::Present,
        });
        captured_files.push(CodeIndexCapturedFileV1 {
            file_occurrence_id: occurrence,
            sanitized_bytes: Arc::clone(&source.bytes),
            sensitivity_level: SensitivityLevelV1::Public,
        });
        receipts.push(id::<SanitizationReceiptId>(&format!(
            "receipt.partitioned-codec.{index:04}"
        ))?);
    }
    let content_identity = content_digest(&identity.finalize());
    let request = CodeIndexBuildRequestV1 {
        snapshot: SanitizedCodeSnapshotV1 {
            repository: id("repository.partitioned-codec")?,
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: id("sanitizer.partitioned-codec.v1")?,
            sanitization_receipts: receipts,
            content_identity,
            captured_at: UtcMicros(1_000_000),
            files,
        },
        captured_files,
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            tree: None,
            dirty: RepositoryDirtyStateV1::Dirty,
        },
        sealed_at: UtcMicros(2_000_000),
        target_projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "lexical.partitioned-codec.v1".to_owned(),
            profile_digest: ManifestDigest::from_sha256_bytes(&Sha256::digest(
                b"partitioned-codec-bench",
            ))?,
        },
    };
    let mut owner = CodeIndexProductionOwnerV1::new(
        production_config()?,
        BenchmarkPublication,
        ApplyingProjection,
    )?;
    Ok(owner.build_and_publish(request, &ActiveControl)?)
}

fn production_config() -> Result<CodeIndexProductionConfigV1, Box<dyn Error>> {
    Ok(CodeIndexProductionConfigV1 {
        project_id: id::<ProjectId>("project.partitioned-codec")?,
        repository: id::<RepositoryId>("repository.partitioned-codec")?,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.partitioned-codec.v1")?,
        policy_revision: id::<PolicyRevisionId>("policy.partitioned-codec.v1")?,
        chunker_revision: id::<ChunkerRevision>("chunker.partitioned-codec.v1")?,
        privacy_domain: id::<PrivacyDomainId>("privacy.partitioned-codec")?,
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    })
}

fn encode_once(
    generation: &CodeIndexPublishedGenerationV1,
) -> Result<EncodedFixture, CodeIndexProductionErrorV1> {
    let mut segments = BTreeMap::new();
    let manifest = generation.encode_partitioned_sealed(|digest, bytes| {
        segments.insert(digest.as_str().to_owned(), bytes.to_vec());
        Ok(())
    })?;
    Ok(EncodedFixture { manifest, segments })
}

fn decode_and_open(fixture: &EncodedFixture) -> Result<(), CodeIndexProductionErrorV1> {
    let decoded = CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
        &fixture.manifest,
        |digest, _, buffer| {
            let bytes = fixture.segments.get(digest.as_str()).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract("benchmark segment is missing".to_owned())
            })?;
            buffer.clear();
            buffer.extend_from_slice(bytes);
            Ok(())
        },
    )?
    .ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("benchmark manifest is incompatible".to_owned())
    })?;
    black_box(decoded);
    let source_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&fixture.manifest))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    let source = VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
        Cursor::new(Vec::<u8>::new()),
        &fixture.manifest,
        source_digest,
        |digest, _, buffer| {
            let bytes = fixture.segments.get(digest.as_str()).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract("benchmark segment is missing".to_owned())
            })?;
            buffer.clear();
            buffer.extend_from_slice(bytes);
            Ok(())
        },
        256,
        1024 * 1024,
    )?
    .ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("benchmark manifest is incompatible".to_owned())
    })?;
    black_box(source);
    Ok(())
}

fn assert_fixture_identity(
    expected: &EncodedFixture,
    actual: &EncodedFixture,
) -> Result<(), Box<dyn Error>> {
    if expected.manifest != actual.manifest || expected.segments != actual.segments {
        return Err("partitioned codec produced nondeterministic bytes".into());
    }
    Ok(())
}

fn distribution(mut values: Vec<u64>) -> Distribution {
    values.sort_unstable();
    Distribution {
        samples: values.len(),
        p50_ns: percentile(&values, 50),
        p95_ns: percentile(&values, 95),
    }
}

fn percentile(values: &[u64], percentile: usize) -> u64 {
    let index = ((values.len() - 1) * percentile).div_ceil(100);
    values[index]
}

fn duration_ns(duration: std::time::Duration) -> Result<u64, Box<dyn Error>> {
    Ok(u64::try_from(duration.as_nanos())?)
}

fn reset_peak_rss() -> Result<(), Box<dyn Error>> {
    std::fs::write("/proc/self/clear_refs", b"5\n")?;
    Ok(())
}

fn proc_value(path: &str, key: &str) -> Result<u64, Box<dyn Error>> {
    let contents = std::fs::read_to_string(path)?;
    contents
        .lines()
        .find_map(|line| {
            let (candidate, value) = line.split_once(':')?;
            if candidate != key.trim_end_matches(':') {
                return None;
            }
            value.split_whitespace().next()?.parse::<u64>().ok()
        })
        .ok_or_else(|| format!("missing {key} in {path}").into())
}

fn id<T>(value: &str) -> Result<T, T::Error>
where
    T: TryFrom<String>,
    T::Error: Debug,
{
    T::try_from(value.to_owned())
}
