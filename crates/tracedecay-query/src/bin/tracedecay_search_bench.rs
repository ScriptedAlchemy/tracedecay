//! Daemon-free, deterministic search-lane latency workload.
//!
//! Companion to `tracedecay-index-bench`: where that binary profiles the
//! indexing pipeline, this one profiles per-query evaluation cost of the
//! production exact and lexical lanes over a published lexical artifact.
//! It drives the same production build path once, then measures queries:
//!
//! 1. read the fixture corpus with the same ordered, `.gitignore`-blind walk;
//! 2. build and seal one clean generation through
//!    [`CodeIndexProductionOwnerV1::build_and_publish`];
//! 3. drain the sealed generation and ingest it into an isolated SQLite
//!    lexical artifact, finalize, and reopen it content-addressed — the
//!    exact reader shape `ProductionCodeIndexQueryOwnersV1` serves from;
//! 4. compose the production `ExactLane`/`LexicalLane` over that reader and
//!    run representative query classes (short token, high-cardinality term,
//!    phrase, path, exact identifier, typo-recovery) for a fixed iteration
//!    count, reporting per-class p50/p95/mean wall micros per phase
//!    (sanitize+parse, exact lane, lexical lane).
//!
//! The regex-shaped journey is intentionally absent: the exact and lexical
//! lanes have no regex evaluation; pattern scans are the grep tool's lane.
//!
//! Hermeticity matches `tracedecay-index-bench`: no socket, no daemon, no
//! operator profile; the only path written is a self-created scratch
//! directory under the system temp dir, removed before exit. With the
//! `hotpath` feature off the workload is identical and no report is written.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::BTreeSet;
use std::fmt;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracedecay_code_index::chunks::content_digest;
use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
    CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexProductionConfigV1,
    CodeIndexProductionOwnerV1, CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    CodeIndexRepositoryParseIdentityV1, VerifiedSealedLexicalPageBatchBoundsV1,
    VerifiedSealedLexicalPageBatchReadV1, VerifiedSealedLexicalPageSourceV1,
    VerifiedSealedLexicalPageV1, VerifiedSealedLexicalSourceReceiptV1,
};
use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
    ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
};
use tracedecay_domain::{
    AuthorizationRevision, ChunkerRevision, CodeGenerationId, ComponentRevision,
    ExactAdmissionRuleRevision, FileOccurrenceId, FreshnessCompatibilityV1, FreshnessVectorDigest,
    FusionProfileId, LanguageId, ManifestDigest, PolicyRevisionId, PrincipalId, PrivacyDomainId,
    ProjectId, ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1,
    ProjectionOutcomeV1, QueryNormalizationRevision, RepositoryDirtyStateV1, RepositoryId,
    RetrievalBudget, RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverBatch,
    RetrieverOutcome, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, ScoreDomainId, SensitivityLevelV1, SingleRootScopeV1,
    SnapshotFileDispositionV1, SourceFreshness, SourceInstanceKey, SourceNamespace, TemporalModeV1,
    TreeId, UtcMicros, VectorWatermark,
};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
    ExactLaneRetriever,
};
use tracedecay_query::retrieval::lexical::{
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactBuilderV1,
    CodeLexicalArtifactFinalizationStepV1, CodeLexicalArtifactReaderV1,
    CodeLexicalProjectionMetadataV1, LexicalLane, LexicalLaneRequest, LexicalLaneRetriever,
    MAX_FUZZY_TERM_EXPANSIONS_V1, lexical_query_parts,
};
use tracedecay_query::retrieval::{
    QUERY_EXACT_RULE_REVISION_V1, QUERY_LEXICAL_PROFILE_REVISION_V1, QUERY_LEXICAL_SCORE_DOMAIN_V1,
    QUERY_NORMALIZATION_REVISION_V1, QUERY_SANITIZER_REVISION_V1, RawRetrievalRequestV1,
};

/// Bumped whenever the workload shape changes, so a profile comparison
/// across a shape change is visibly not comparable.
const WORKLOAD_REVISION: &str = "search-bench.v1";
const DEFAULT_CORPUS_RELATIVE: &str = "benchmark_data/index-bench/corpus";
const CORPUS_ENV: &str = "TRACEDECAY_SEARCH_BENCH_CORPUS";
const REPLICAS_ENV: &str = "TRACEDECAY_SEARCH_BENCH_REPLICAS";

/// Sealed-source paging bounds, mirrored from `tracedecay-index-bench` so the
/// artifact this workload queries is built the same way the daemon builds it.
const MAX_PAGE_CHUNKS: usize = 64;
const MAX_PAGE_BYTES: usize = 512 * 1024;
const BATCH_MAX_PAGES: usize = 16;
const BATCH_MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;
const FINALIZATION_WORK_BUDGET: usize = 4_096;

const SEALED_AT: i64 = 1_700_000_000_000_000;

/// The production fallback retrieval budget shape (`query-fallback`).
const BUDGET: RetrievalBudget = RetrievalBudget {
    max_candidates_per_lane: 32,
    max_fused_candidates: 32,
    max_hydrated_results: 16,
    max_hydration_bytes: 65_536,
    deadline_micros: None,
};

fn main() -> ExitCode {
    #[cfg(feature = "hotpath")]
    configure_hotpath();

    // Declared first so it drops last: the exit report must observe every
    // measured span. Nothing here may call `std::process::exit`.
    #[cfg(feature = "hotpath")]
    let _hotpath = hotpath::HotpathGuardBuilder::new("tracedecay-search-bench").build();

    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("tracedecay-search-bench: {message}");
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match run(&options) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("tracedecay-search-bench: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Same two guard defaults `tracedecay-index-bench` overrides, for the same
/// reasons: no socket ever, and no stdout report corrupting the summary.
#[cfg(feature = "hotpath")]
fn configure_hotpath() {
    if std::env::var_os("HOTPATH_METRICS_SERVER_OFF").is_none() {
        unsafe {
            std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
        }
    }
    let has_output_path = std::env::var_os("HOTPATH_OUTPUT_PATH")
        .is_some_and(|path| path.to_str().is_some_and(|path| !path.is_empty()));
    if !has_output_path {
        unsafe {
            std::env::set_var("HOTPATH_OUTPUT_FORMAT", "none");
            std::env::remove_var("HOTPATH_OUTPUT_PATH");
        }
    }
}

const USAGE: &str = "\
usage: tracedecay-search-bench [--corpus DIR] [--replicas N] [--iterations N]
                               [--warmups N] [--fuzzy-budget N]
                               [--class NAME]... [--term CLASS=QUERY]...

  --corpus DIR       fixture corpus to index and query
                     (default: $TRACEDECAY_SEARCH_BENCH_CORPUS, else
                     benchmark_data/index-bench/corpus beside this workspace)
  --replicas N       index the corpus N times under distinct logical path
                     prefixes (default: $TRACEDECAY_SEARCH_BENCH_REPLICAS, else 1)
  --iterations N     timed query iterations per class (default: 40)
  --warmups N        untimed warmup iterations per class (default: 3)
  --fuzzy-budget N   lexical typo-recovery budget (default: production 64)
  --class NAME       run only the named classes (repeatable; default: all)
  --term CLASS=QUERY override one class's query text (repeatable)
  -h, --help         print this message

Profiling: build with `--features production,hotpath` and set
HOTPATH_OUTPUT_FORMAT and HOTPATH_OUTPUT_PATH. With the feature off the
workload is identical and no report is written.";

/// Default query classes, written against the committed index-bench corpus.
/// Every default hits real corpus text so the measured work is candidate
/// scoring, not empty-result short-circuits.
const DEFAULT_CLASSES: [(&str, &str); 6] = [
    // One short technical token with moderate selectivity.
    ("short_token", "cursor"),
    // Terms present in nearly every fixture file: worst-case candidate count.
    ("high_cardinality", "entries sealed"),
    // Multi-word natural phrase, exercises the n-gram prefilter plus the
    // row-level substring confirmation and phrase document frequencies.
    ("phrase", "evidence into the accumulator"),
    // Path-shaped exact query addressing one logical path posting.
    ("path", "rust/outline_085.rs"),
    // One specific identifier: exact lane admission plus low-cardinality
    // lexical retrieval.
    ("exact_identifier", "OutlineState016"),
    // A misspelling within Levenshtein distance 1 of a corpus term, so
    // typo recovery must expand it through the vocabulary.
    ("fuzzy_typo", "acumulator"),
];

struct Options {
    corpus_root: PathBuf,
    replicas: usize,
    iterations: usize,
    warmups: usize,
    fuzzy_budget: u32,
    classes: Vec<(String, String)>,
}

impl Options {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut corpus_root: Option<PathBuf> = None;
        let mut replicas: Option<usize> = None;
        let mut iterations = 40usize;
        let mut warmups = 3usize;
        let mut fuzzy_budget = MAX_FUZZY_TERM_EXPANSIONS_V1;
        let mut selected: Vec<String> = Vec::new();
        let mut overrides: Vec<(String, String)> = Vec::new();
        let mut arguments = arguments.peekable();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "-h" | "--help" => return Ok(None),
                "--corpus" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--corpus needs a directory".to_owned())?;
                    corpus_root = Some(PathBuf::from(value));
                }
                "--replicas" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--replicas needs a count".to_owned())?;
                    replicas = Some(parse_count("--replicas", &value, 1)?);
                }
                "--iterations" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--iterations needs a count".to_owned())?;
                    iterations = parse_count("--iterations", &value, 1)?;
                }
                "--warmups" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--warmups needs a count".to_owned())?;
                    warmups = parse_count("--warmups", &value, 0)?;
                }
                "--fuzzy-budget" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--fuzzy-budget needs a count".to_owned())?;
                    fuzzy_budget = u32::try_from(parse_count("--fuzzy-budget", &value, 0)?)
                        .map_err(|_| "fuzzy budget does not fit u32".to_owned())?;
                }
                "--class" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--class needs a name".to_owned())?;
                    selected.push(value);
                }
                "--term" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--term needs CLASS=QUERY".to_owned())?;
                    let (class, query) = value
                        .split_once('=')
                        .ok_or_else(|| format!("--term {value:?} is not CLASS=QUERY"))?;
                    overrides.push((class.to_owned(), query.to_owned()));
                }
                other => return Err(format!("unrecognized argument {other:?}")),
            }
        }
        let corpus_root = corpus_root
            .or_else(|| std::env::var_os(CORPUS_ENV).map(PathBuf::from))
            .unwrap_or_else(default_corpus_root);
        let replicas = match replicas {
            Some(replicas) => replicas,
            None => match std::env::var(REPLICAS_ENV) {
                Ok(value) => parse_count(REPLICAS_ENV, &value, 1)?,
                Err(_) => 1,
            },
        };
        let mut classes: Vec<(String, String)> = DEFAULT_CLASSES
            .iter()
            .map(|(class, query)| ((*class).to_owned(), (*query).to_owned()))
            .collect();
        for (class, query) in overrides {
            match classes.iter_mut().find(|(name, _)| *name == class) {
                Some((_, existing)) => *existing = query,
                None => classes.push((class, query)),
            }
        }
        if !selected.is_empty() {
            for name in &selected {
                if !classes.iter().any(|(class, _)| class == name) {
                    return Err(format!("unknown query class {name:?}"));
                }
            }
            classes.retain(|(class, _)| selected.iter().any(|name| name == class));
        }
        Ok(Some(Self {
            corpus_root,
            replicas,
            iterations,
            warmups,
            fuzzy_budget,
            classes,
        }))
    }
}

fn parse_count(flag: &str, value: &str, minimum: usize) -> Result<usize, String> {
    let count = value
        .parse::<usize>()
        .map_err(|error| format!("invalid {flag} count {value:?}: {error}"))?;
    if count < minimum {
        return Err(format!("{flag} must be at least {minimum}"));
    }
    Ok(count)
}

fn default_corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(DEFAULT_CORPUS_RELATIVE)
}

// ---------------------------------------------------------------------------
// Corpus admission (identical walk to `tracedecay-index-bench`)
// ---------------------------------------------------------------------------

struct CorpusFile {
    relative_path: String,
    language: LanguageId,
    bytes: Vec<u8>,
}

fn load_corpus(root: &Path) -> Result<Vec<CorpusFile>, String> {
    let registry = StaticLanguageRegistry::new();
    let mut files = Vec::new();
    collect_corpus(root, root, &registry, &mut files)?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if files.is_empty() {
        return Err(format!(
            "corpus {} admitted no files with a known language extension",
            root.display()
        ));
    }
    Ok(files)
}

fn collect_corpus(
    root: &Path,
    directory: &Path,
    registry: &StaticLanguageRegistry,
    files: &mut Vec<CorpusFile>,
) -> Result<(), String> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| format!("read {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read {}: {error}", directory.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("stat {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_corpus(root, &path, registry, files)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(extension) = path.extension().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        let Some(descriptor) = registry.descriptor_for_extension(&extension.to_lowercase()) else {
            continue;
        };
        if !descriptor.capabilities.extraction {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("relativize {}: {error}", path.display()))?;
        let Some(relative_path) = relative.to_str() else {
            return Err(format!("corpus path {} is not Unicode", relative.display()));
        };
        let bytes =
            std::fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
        files.push(CorpusFile {
            relative_path: relative_path.replace('\\', "/"),
            language: descriptor.language.clone(),
            bytes,
        });
    }
    Ok(())
}

struct AdmittedFile {
    logical_path: String,
    language: LanguageId,
    bytes: Arc<[u8]>,
}

fn replicate(corpus: &[CorpusFile], replicas: usize) -> Vec<AdmittedFile> {
    let mut admitted = Vec::with_capacity(corpus.len().saturating_mul(replicas));
    for replica in 0..replicas {
        for file in corpus {
            let logical_path = if replica == 0 {
                file.relative_path.clone()
            } else {
                format!("replica{replica:02}/{}", file.relative_path)
            };
            admitted.push(AdmittedFile {
                logical_path,
                language: file.language.clone(),
                bytes: Arc::from(file.bytes.clone()),
            });
        }
    }
    // The snapshot contract requires canonical file order over the whole
    // admitted set, not per replica.
    admitted.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    admitted
}

// ---------------------------------------------------------------------------
// In-memory production authorities (identical to `tracedecay-index-bench`)
// ---------------------------------------------------------------------------

struct ActiveControl;

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct MemoryPublicationStore {
    active: Arc<
        Mutex<
            std::collections::BTreeMap<
                CodeIndexGenerationScopeV1,
                Arc<CodeIndexPublishedGenerationV1>,
            >,
        >,
    >,
}

impl CodeIndexAtomicPublicationPort for MemoryPublicationStore {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .map_err(|_| CodeIndexPublicationStoreErrorV1::CompareAndSwap)?
            .get(scope)
            .map(|generation| generation.as_ref().clone()))
    }

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| CodeIndexPublicationStoreErrorV1::CompareAndSwap)?;
        if active
            .get(scope)
            .map(|current| current.manifest().generation_id.clone())
            .as_ref()
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        active.insert(scope.clone(), generation);
        Ok(())
    }
}

struct ApplyingProjectionSink;

impl CodeChunkProjectionSink for ApplyingProjectionSink {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let mut decisions = Vec::with_capacity(
            request.changes.added_or_changed.len()
                + request.changes.deleted.len()
                + request.changes.reused.len(),
        );
        decisions.extend(request.changes.added_or_changed.iter().map(|change| {
            ChunkProjectionDecisionV1 {
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
            }
        }));
        decisions.extend(
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
        );
        decisions.extend(
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
        );
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Workload
// ---------------------------------------------------------------------------

fn run(options: &Options) -> Result<String, String> {
    let started = Instant::now();
    let control = ActiveControl;

    let corpus_started = Instant::now();
    let corpus = load_corpus(&options.corpus_root)?;
    let files = replicate(&corpus, options.replicas);
    let corpus_wall = corpus_started.elapsed();
    let admitted_bytes = files
        .iter()
        .map(|file| file.bytes.len() as u64)
        .sum::<u64>();

    let repository = identity::<RepositoryId>("repository.search-bench");
    let sanitizer_revision = identity::<SanitizerRevision>("sanitizer.search-bench.v1");
    let config = CodeIndexProductionConfigV1 {
        project_id: identity::<ProjectId>("project.search-bench"),
        repository: repository.clone(),
        sanitizer_revision: sanitizer_revision.clone(),
        policy_revision: identity::<PolicyRevisionId>("policy.search-bench.v1"),
        chunker_revision: identity::<ChunkerRevision>("chunker.search-bench.v1"),
        privacy_domain: identity::<PrivacyDomainId>("privacy.search-bench"),
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    };
    let mut owner = CodeIndexProductionOwnerV1::new(
        config,
        MemoryPublicationStore::default(),
        ApplyingProjectionSink,
    )
    .map_err(|error| format!("open production owner: {error}"))?;

    let request = build_request(&repository, &sanitizer_revision, &files);
    let generation_started = Instant::now();
    let generation = owner
        .build_and_publish(request, &control)
        .map_err(|error| format!("build generation: {error}"))?;
    let generation_wall = generation_started.elapsed();
    let chunk_count = generation.chunks().chunks().len() as u64;

    let seal_started = Instant::now();
    let sealed = generation
        .encode_sealed()
        .map_err(|error| format!("encode sealed generation: {error}"))?;
    let seal_wall = seal_started.elapsed();
    let sealed_len = sealed.len() as u64;
    let state_digest = sealed_state_digest(&sealed)?;

    let drain_started = Instant::now();
    let (pages, source_receipt) = drain_pages(&sealed, sealed_len, &state_digest, &control)?;
    let drain_wall = drain_started.elapsed();

    let scratch = Scratch::create()?;
    let artifact_path = scratch.path().join("lexical.sqlite");
    let metadata = projection_metadata(&generation, &repository);
    let ingest_started = Instant::now();
    let receipt = ingest_artifact(&artifact_path, metadata, &pages, &source_receipt, &control)?;
    let ingest_wall = ingest_started.elapsed();

    // Reopen content-addressed: the byte-identical reader shape the daemon
    // serves durable heads from after a restart. The daemon's publication
    // step content-addresses the finalized file at rest, so hash the same
    // bytes here rather than reusing the receipt's section identity.
    let open_started = Instant::now();
    let (file_digest, file_size_bytes) = hash_file(&artifact_path)?;
    let reader = CodeLexicalArtifactReaderV1::open_content_addressed(
        &artifact_path,
        &file_digest,
        file_size_bytes,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .map_err(|error| format!("reopen lexical artifact: {error}"))?;
    let open_wall = open_started.elapsed();

    let authority = CentralExactAdmissionAuthorityV1::new(
        ExactAdmissionRuleRevision::new(QUERY_EXACT_RULE_REVISION_V1)
            .map_err(|error| format!("exact rule revision: {error}"))?,
    );
    let exact_lane = ExactLane::new(authority.clone(), reader.exact_adapter(authority.clone()));
    let lexical_lane = LexicalLane::new(reader.clone());

    let generation_id = generation.manifest().generation_id.clone();
    let prototype = request_prototype(&generation, &repository)?;

    let mut class_reports = Vec::with_capacity(options.classes.len());
    for (class, query) in &options.classes {
        let report = run_class(RunClassArguments {
            class,
            query,
            options,
            prototype: &prototype,
            authority: &authority,
            exact_lane: &exact_lane,
            lexical_lane: &lexical_lane,
            generation: &generation_id,
        })?;
        class_reports.push(report);
    }

    drop(exact_lane);
    drop(lexical_lane);
    drop(reader);
    scratch.remove()?;

    let total_wall = started.elapsed();
    let report = serde_json::json!({
        "workload_revision": WORKLOAD_REVISION,
        "corpus_root": options.corpus_root.display().to_string(),
        "replicas": options.replicas,
        "corpus_files": corpus.len() as u64,
        "admitted_files": files.len() as u64,
        "admitted_bytes": admitted_bytes,
        "chunks": chunk_count,
        "sealed_bytes": sealed_len,
        "artifact_bytes": receipt.file_size_bytes(),
        "artifact_digest": receipt.artifact_digest().as_str(),
        "iterations": options.iterations,
        "warmups": options.warmups,
        "fuzzy_budget": options.fuzzy_budget,
        "peak_rss_bytes": peak_rss_bytes(),
        "build_wall_ms": {
            "corpus_load": millis(corpus_wall),
            "generation": millis(generation_wall),
            "seal_encode": millis(seal_wall),
            "sealed_page_drain": millis(drain_wall),
            "artifact_ingest_finalize": millis(ingest_wall),
            "artifact_reopen_verified": millis(open_wall),
            "total": millis(total_wall),
        },
        "classes": class_reports,
    });
    serde_json::to_string_pretty(&report).map_err(|error| format!("serialize summary: {error}"))
}

/// Query-independent request fields, cloned per iteration exactly as the
/// daemon clones its own per-request base.
struct RequestPrototypeV1 {
    principal: PrincipalId,
    scope: RetrievalScope,
    snapshot: RetrievalSnapshot,
    profile_id: FusionProfileId,
    sanitizer_revision: SanitizerRevision,
    normalization_revision: QueryNormalizationRevision,
    lexical_profile_revision: ComponentRevision,
    lexical_score_domain: ScoreDomainId,
}

impl RequestPrototypeV1 {
    fn request(&self) -> RetrievalRequest {
        RetrievalRequest {
            principal: self.principal.clone(),
            scope: self.scope.clone(),
            temporal_mode: TemporalModeV1::Current,
            snapshot: self.snapshot.clone(),
            profile_id: self.profile_id.clone(),
            budget: BUDGET,
        }
    }
}

fn request_prototype(
    generation: &CodeIndexPublishedGenerationV1,
    repository: &RepositoryId,
) -> Result<RequestPrototypeV1, String> {
    let manifest = generation.manifest();
    Ok(RequestPrototypeV1 {
        principal: identity::<PrincipalId>("principal.search-bench"),
        scope: RetrievalScope {
            privacy_domain: manifest.privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: repository.clone(),
                worktree: None,
                reference: None,
            },
        },
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(manifest.snapshot_digest.as_str())
                .map_err(|error| format!("freshness digest: {error}"))?,
            authorization_revision: identity::<AuthorizationRevision>(
                "authorization.search-bench.v1",
            ),
            captured_at: manifest.seal.sealed_at,
        },
        profile_id: identity::<FusionProfileId>("query-fallback"),
        sanitizer_revision: identity::<SanitizerRevision>(QUERY_SANITIZER_REVISION_V1),
        normalization_revision: identity::<QueryNormalizationRevision>(
            QUERY_NORMALIZATION_REVISION_V1,
        ),
        lexical_profile_revision: identity::<ComponentRevision>(QUERY_LEXICAL_PROFILE_REVISION_V1),
        lexical_score_domain: identity::<ScoreDomainId>(QUERY_LEXICAL_SCORE_DOMAIN_V1),
    })
}

struct RunClassArguments<'a, E, L> {
    class: &'a str,
    query: &'a str,
    options: &'a Options,
    prototype: &'a RequestPrototypeV1,
    authority: &'a CentralExactAdmissionAuthorityV1,
    exact_lane: &'a E,
    lexical_lane: &'a L,
    generation: &'a CodeGenerationId,
}

struct IterationMicros {
    prepare: u64,
    exact: u64,
    lexical: u64,
    total: u64,
}

fn run_class<E, L>(arguments: RunClassArguments<'_, E, L>) -> Result<serde_json::Value, String>
where
    E: ExactLaneRetriever,
    L: LexicalLaneRetriever,
{
    let RunClassArguments {
        class,
        query,
        options,
        prototype,
        authority,
        exact_lane,
        lexical_lane,
        generation,
    } = arguments;
    let mut samples = Vec::with_capacity(options.iterations);
    let mut last_outcomes = None;
    for iteration in 0..(options.warmups + options.iterations) {
        let iteration_started = Instant::now();
        let sanitized = RawRetrievalRequestV1::new(query.to_owned(), prototype.request())
            .sanitize(
                prototype.sanitizer_revision.clone(),
                prototype.normalization_revision.clone(),
            )
            .map_err(|error| format!("sanitize {class}: {error}"))?;
        let request = sanitized.request();
        let query_view = sanitized.query_view();
        let literals = authority.parse_literals(query_view, request);
        let prepare_wall = iteration_started.elapsed();

        let exact_started = Instant::now();
        let exact_outcome = exact_lane
            .retrieve_exact(&ExactLaneRequest {
                base: request.clone(),
                query_view,
                generation: generation.clone(),
                literals,
                budget: request.budget,
            })
            .map_err(|error| format!("exact lane {class}: {error}"))?;
        let exact_wall = exact_started.elapsed();

        let lexical_started = Instant::now();
        let parts = lexical_query_parts(query_view.as_str())
            .map_err(|error| format!("lexical parts {class}: {error}"))?;
        let lexical_outcome = lexical_lane
            .retrieve_lexical(&LexicalLaneRequest {
                base: request.clone(),
                query_view,
                generation: generation.clone(),
                whole_terms: parts.whole_terms,
                subtokens: parts.subtokens,
                phrases: parts.phrases,
                field_filters: Vec::new(),
                fuzzy_budget: options.fuzzy_budget,
                lexical_profile_revision: prototype.lexical_profile_revision.clone(),
                score_domain: prototype.lexical_score_domain.clone(),
                budget: request.budget,
            })
            .map_err(|error| format!("lexical lane {class}: {error}"))?;
        let lexical_wall = lexical_started.elapsed();

        if iteration >= options.warmups {
            samples.push(IterationMicros {
                prepare: micros(prepare_wall),
                exact: micros(exact_wall),
                lexical: micros(lexical_wall),
                total: micros(iteration_started.elapsed()),
            });
        }
        last_outcomes = Some((exact_outcome, lexical_outcome));
    }
    let (exact_outcome, lexical_outcome) =
        last_outcomes.ok_or_else(|| format!("query class {class} ran no iterations"))?;
    Ok(serde_json::json!({
        "class": class,
        "query_bytes": query.len(),
        "exact": outcome_facts(&exact_outcome),
        "lexical": outcome_facts(&lexical_outcome),
        "micros": {
            "prepare": phase_stats(&samples, |sample| sample.prepare),
            "exact_lane": phase_stats(&samples, |sample| sample.exact),
            "lexical_lane": phase_stats(&samples, |sample| sample.lexical),
            "total": phase_stats(&samples, |sample| sample.total),
        },
    }))
}

fn outcome_facts<E>(outcome: &RetrieverOutcome<RetrieverBatch<E>>) -> serde_json::Value {
    match outcome {
        RetrieverOutcome::Complete(batch) => serde_json::json!({
            "outcome": "complete",
            "candidates": batch.candidates.len(),
            "examined": batch.coverage.examined,
            "eligible": batch.coverage.eligible,
            "excluded": batch.coverage.excluded,
            "capped": batch.coverage.capped,
        }),
        RetrieverOutcome::Partial { value, .. } => serde_json::json!({
            "outcome": "partial",
            "candidates": value.candidates.len(),
            "examined": value.coverage.examined,
        }),
        RetrieverOutcome::Stale(_) => serde_json::json!({ "outcome": "stale" }),
        RetrieverOutcome::Cancelled => serde_json::json!({ "outcome": "cancelled" }),
        RetrieverOutcome::Denied => serde_json::json!({ "outcome": "denied" }),
        RetrieverOutcome::BudgetExceeded(_) => serde_json::json!({ "outcome": "budget_exceeded" }),
        RetrieverOutcome::TimedOut(_) => serde_json::json!({ "outcome": "timed_out" }),
        RetrieverOutcome::Unavailable(failure) => serde_json::json!({
            "outcome": "unavailable",
            "detail": format!("{failure:?}"),
        }),
    }
}

fn phase_stats(
    samples: &[IterationMicros],
    phase: impl Fn(&IterationMicros) -> u64,
) -> serde_json::Value {
    let mut values = samples.iter().map(&phase).collect::<Vec<_>>();
    values.sort_unstable();
    let count = values.len();
    let sum = values.iter().sum::<u64>();
    let mean = if count == 0 { 0 } else { sum / count as u64 };
    serde_json::json!({
        "p50": percentile(&values, 50),
        "p95": percentile(&values, 95),
        "mean": mean,
        "min": values.first().copied().unwrap_or_default(),
        "max": values.last().copied().unwrap_or_default(),
    })
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * percent).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn build_request(
    repository: &RepositoryId,
    sanitizer_revision: &SanitizerRevision,
    files: &[AdmittedFile],
) -> CodeIndexBuildRequestV1 {
    let mut snapshot_files = Vec::with_capacity(files.len());
    let mut captured_files = Vec::with_capacity(files.len());
    let mut identity_hash = Vec::new();
    for (ordinal, file) in files.iter().enumerate() {
        let digest = content_digest(&file.bytes);
        identity_hash.extend_from_slice(digest.as_str().as_bytes());
        let file_occurrence_id =
            identity::<FileOccurrenceId>(&format!("file.search-bench.{ordinal:06}"));
        snapshot_files.push(SanitizedCodeFileV1 {
            file_occurrence_id: file_occurrence_id.clone(),
            logical_path: file.logical_path.clone(),
            language: Some(file.language.clone()),
            content_digest: digest,
            disposition: SnapshotFileDispositionV1::Present,
        });
        captured_files.push(CodeIndexCapturedFileV1 {
            file_occurrence_id,
            sanitized_bytes: Arc::clone(&file.bytes),
            sensitivity_level: SensitivityLevelV1::Public,
        });
    }
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: repository.clone(),
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: sanitizer_revision.clone(),
        sanitization_receipts: vec![identity::<SanitizationReceiptId>("receipt.search-bench")],
        content_identity: content_digest(&identity_hash),
        captured_at: UtcMicros(SEALED_AT - 1_000_000),
        files: snapshot_files,
    };
    CodeIndexBuildRequestV1 {
        snapshot,
        captured_files,
        changed_files: files
            .iter()
            .map(|file| file.logical_path.clone())
            .collect::<BTreeSet<_>>(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            tree: Some(identity::<TreeId>("tree.search-bench.clean")),
            dirty: RepositoryDirtyStateV1::Clean,
        },
        sealed_at: UtcMicros(SEALED_AT),
        target_projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "lexical.v1".to_owned(),
            profile_digest: identity::<ManifestDigest>(&format!("sha256:{}", "e".repeat(64))),
        },
    }
}

fn sealed_state_digest(sealed: &[u8]) -> Result<ManifestDigest, String> {
    let envelope: serde_json::Value = serde_json::from_slice(sealed)
        .map_err(|error| format!("decode sealed generation envelope: {error}"))?;
    let digest = envelope
        .get("state_digest")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "sealed generation envelope has no state digest".to_owned())?;
    ManifestDigest::try_from(digest.to_owned())
        .map_err(|error| format!("sealed generation state digest: {error:?}"))
}

fn drain_pages(
    sealed: &[u8],
    sealed_len: u64,
    state_digest: &ManifestDigest,
    control: &ActiveControl,
) -> Result<
    (
        Vec<VerifiedSealedLexicalPageV1>,
        VerifiedSealedLexicalSourceReceiptV1,
    ),
    String,
> {
    let bounds =
        VerifiedSealedLexicalPageBatchBoundsV1::new(BATCH_MAX_PAGES, BATCH_MAX_RETAINED_BYTES)
            .map_err(|error| format!("sealed lexical batch bounds: {error}"))?;
    let mut source = VerifiedSealedLexicalPageSourceV1::open(
        Cursor::new(sealed.to_vec()),
        sealed_len,
        state_digest.clone(),
        MAX_PAGE_CHUNKS,
        MAX_PAGE_BYTES,
        control,
    )
    .map_err(|error| format!("open sealed lexical page source: {error}"))?;
    let mut pages = Vec::new();
    loop {
        let read = source
            .next_page_batch_if(control, bounds, |staged| {
                NonZeroUsize::new(staged.len())
                    .ok_or_else(|| "sealed lexical batch staged no pages".to_owned())
            })
            .map_err(|error| format!("stage sealed lexical page batch: {error}"))?
            .map_err(|error| format!("admit sealed lexical page batch: {error}"))?;
        match read {
            VerifiedSealedLexicalPageBatchReadV1::Pages(batch) => pages.extend(batch),
            VerifiedSealedLexicalPageBatchReadV1::Complete(receipt) => {
                return Ok((pages, receipt));
            }
        }
    }
}

fn projection_metadata(
    generation: &CodeIndexPublishedGenerationV1,
    repository: &RepositoryId,
) -> CodeLexicalProjectionMetadataV1 {
    CodeLexicalProjectionMetadataV1 {
        generation: generation.manifest().generation_id.clone(),
        repository_id: Some(repository.clone()),
        logical_paths: generation
            .snapshot()
            .files
            .iter()
            .map(|file| (file.file_occurrence_id.clone(), file.logical_path.clone()))
            .collect(),
        freshness: SourceFreshness {
            source_namespace: identity::<SourceNamespace>("ns.code.search-bench"),
            source_instance: identity::<SourceInstanceKey>("instance.search-bench"),
            source_watermark: Some(1),
            projection_watermark: Some(1),
            observed_at: UtcMicros(SEALED_AT),
            source_generation: Some(1),
            generation_lag: Some(0),
            compatibility: FreshnessCompatibilityV1::Current,
            policy_revision: identity("policy.search-bench.v1"),
        },
        exact_retriever_revision: identity::<ComponentRevision>("retriever.exact.search-bench.v1"),
        lexical_retriever_revision: identity::<ComponentRevision>(
            "retriever.lexical.search-bench.v1",
        ),
        exact_score_domain: identity::<ScoreDomainId>("score.exact.search-bench.v1"),
    }
}

fn ingest_artifact(
    artifact_path: &Path,
    metadata: CodeLexicalProjectionMetadataV1,
    pages: &[VerifiedSealedLexicalPageV1],
    source_receipt: &VerifiedSealedLexicalSourceReceiptV1,
    control: &ActiveControl,
) -> Result<tracedecay_query::retrieval::lexical::VerifiedCodeLexicalArtifactV1, String> {
    let mut builder = CodeLexicalArtifactBuilderV1::create(artifact_path, metadata)
        .map_err(|error| format!("create lexical artifact: {error}"))?;
    for batch in pages.chunks(BATCH_MAX_PAGES) {
        builder
            .append_pages(batch, control)
            .map_err(|error| format!("append lexical artifact batch: {error}"))?;
    }
    loop {
        match builder
            .advance_finalization(source_receipt, FINALIZATION_WORK_BUDGET, control)
            .map_err(|error| format!("finalize lexical artifact: {error}"))?
        {
            CodeLexicalArtifactFinalizationStepV1::Pending { .. } => {}
            CodeLexicalArtifactFinalizationStepV1::Ready(receipt) => return Ok(*receipt),
        }
    }
}

// ---------------------------------------------------------------------------
// Scratch directory
// ---------------------------------------------------------------------------

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn create() -> Result<Self, String> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "tracedecay-search-bench-{}-{unique:x}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)
            .map_err(|error| format!("create scratch {}: {error}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn remove(self) -> Result<(), String> {
        std::fs::remove_dir_all(&self.path)
            .map_err(|error| format!("remove scratch {}: {error}", self.path.display()))
    }
}

fn hash_file(path: &Path) -> Result<(ManifestDigest, u64), String> {
    use sha2::Digest as _;

    let bytes = std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let digest = ManifestDigest::from_sha256_bytes(&sha2::Sha256::digest(&bytes))
        .map_err(|error| format!("artifact digest: {error}"))?;
    Ok((digest, bytes.len() as u64))
}

fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        let Some(value) = line.strip_prefix("VmHWM:") else {
            continue;
        };
        let kilobytes = value.split_whitespace().next()?.parse::<u64>().ok()?;
        return kilobytes.checked_mul(1024);
    }
    None
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn identity<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap_or_else(|error| {
        panic!("deterministic benchmark identity {value:?} must be valid: {error:?}")
    })
}
