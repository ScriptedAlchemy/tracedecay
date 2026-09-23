use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::{
    DeterministicCodeChunker, ExtractionAdmittedCodeSearchChunkV1, content_digest,
};
use tracedecay_code_index::clones::CloneNormalizationClassV1;
use tracedecay_code_index::extract::{LanguageExtractor, NeverCancelled, TreeSitterExtractor};
use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
    CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexInterruptionV1,
    CodeIndexProductionConfigV1, CodeIndexProductionErrorV1, CodeIndexProductionOwnerV1,
    CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    CodeIndexRepositoryParseIdentityV1, SealedGenerationSegmentPublicationV1,
    VerifiedSealedLexicalCursorV1, VerifiedSealedLexicalPageBatchBoundsV1,
    VerifiedSealedLexicalPageBatchReadV1, VerifiedSealedLexicalPageReadV1,
    VerifiedSealedLexicalPageSourceV1, VerifiedSealedLexicalPageV1,
    VerifiedSealedLexicalSourceReceiptV1, VerifiedSealedLexicalSymbolDisplayV1,
};
use tracedecay_code_index::projection::{
    ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
    ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
};
use tracedecay_domain::{
    BoundedSanitizedText, ChunkerRevision, CodeGenerationId, CodeSearchChunkAnchorV1,
    CodeSearchChunkGrainV1, CodeSearchChunkId, CodeSearchChunkV1, ComponentRevision, ContentDigest,
    EphemeralSanitizedQueryViewV1, ExactAdmissionProof, ExactAdmissionRuleRevision,
    ExactAdmissionValidator, ExactFieldV1, ExactTechnicalTermKindV1, ExactTechnicalTermV1,
    FileOccurrenceId, FreshnessCompatibilityV1, LanguageDescriptorRevision, ManifestDigest,
    PolicyRevisionId, PrincipalId, PrivacyDomainId, ProjectId, ProjectionBatchRequestV1,
    ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1,
    QueryNormalizationRevision, RepositoryDirtyStateV1, RepositoryId, RetrievalBudget,
    RetrievalError, RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverCoverage,
    RetrieverOutcome, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, ScoreDomainId, SensitivityDecision, SensitivityLevelV1, SingleRootScopeV1,
    SnapshotFileDispositionV1, SourceFreshness, SourceInstanceKey, SourceNamespace, SourceSpan,
    SymbolOccurrenceId, TemporalModeV1, UtcMicros, ValidatedCodeFileV1, VectorWatermark,
};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
    ExactLaneRetriever, ExactLiteralV1,
};
use tracedecay_query::retrieval::lexical::{
    CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CloneFingerprintCancellationPointV1,
    CloneFingerprintPartialReasonV1, CloneNearMatchExtentV1, CloneSelectedBlockContainmentClassV1,
    CloneSelectedBlockV1, CodeLexicalArtifactBatchLimitV1, CodeLexicalArtifactBuilderV1,
    CodeLexicalArtifactErrorV1, CodeLexicalArtifactFinalizationStepV1, CodeLexicalArtifactReaderV1,
    CodeLexicalProjectionAdapterV1, CodeLexicalProjectionBuildStepV1, CodeLexicalProjectionBuildV1,
    CodeLexicalCloneRouteV1, CodeLexicalProjectionMetadataV1, LexicalFieldFilterV1, LexicalFieldV1, LexicalLane,
    LexicalLaneRequest, LexicalLaneRetriever, LexicalProximityV1, LexicalSpellingVariantV1,
    MAX_CLONE_EXACT_PAGE_MEMBERS_V1, MAX_FUZZY_TERM_EXPANSIONS_V1,
    MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1, MAX_LEXICAL_QUERY_TERM_BYTES_V1,
    VerifiedCodeLexicalArtifactV1,
};
use tracedecay_query::retrieval::ports::{
    ExactTermPostingReadPort, LexicalPostingReadPort, RetrievalExecutionControl, RetrievalPortError,
};
use tracedecay_query::retrieval::{QUERY_EXACT_SCORE_DOMAIN_V1, QUERY_LEXICAL_SCORE_DOMAIN_V1};

/// The request authority every uncancelled fixture request runs under.
pub(crate) struct FixtureRetrievalExecutionControl;

impl RetrievalExecutionControl for FixtureRetrievalExecutionControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn elapsed_micros(&self) -> u64 {
        0
    }
}

pub(crate) static ACTIVE_CONTROL: FixtureRetrievalExecutionControl =
    FixtureRetrievalExecutionControl;

struct ArtifactControl {
    cancelled: bool,
}

impl CodeIndexExecutionControlV1 for ArtifactControl {
    fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct CancelsAfterChecks {
    checks: AtomicUsize,
    cancel_at: usize,
}

impl CodeIndexExecutionControlV1 for CancelsAfterChecks {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::SeqCst).saturating_add(1) >= self.cancel_at
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct ArtifactPublicationStore {
    active: Arc<Mutex<BTreeMap<CodeIndexGenerationScopeV1, Arc<CodeIndexPublishedGenerationV1>>>>,
}

impl CodeIndexAtomicPublicationPort for ArtifactPublicationStore {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .expect("artifact publication lock")
            .get(scope)
            .map(Arc::clone))
    }

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self.active.lock().expect("artifact publication lock");
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

struct ArtifactProjectionSink;

impl CodeChunkProjectionSink for ArtifactProjectionSink {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let mut decisions = request
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
            .collect::<Vec<_>>();
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
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

struct CancelAtObservation {
    cancellation_observation: usize,
    observations: AtomicUsize,
}

struct CancelAtObservationWithJournalProbe {
    cancellation_observation: usize,
    observations: AtomicUsize,
    journal_path: PathBuf,
    journal_seen: AtomicBool,
}

struct CancelOnBackgroundObservation {
    caller: std::thread::ThreadId,
}

impl CancelOnBackgroundObservation {
    fn new() -> Self {
        Self {
            caller: std::thread::current().id(),
        }
    }
}

impl CodeIndexExecutionControlV1 for CancelOnBackgroundObservation {
    fn is_cancelled(&self) -> bool {
        std::thread::current().id() != self.caller
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

impl CancelAtObservation {
    fn new(cancellation_observation: usize) -> Self {
        Self {
            cancellation_observation,
            observations: AtomicUsize::new(0),
        }
    }

    /// How many times the controlled operation consulted this authority.
    fn observations(&self) -> usize {
        self.observations.load(Ordering::SeqCst)
    }
}

/// The same cancel-at-observation semantics for lane-level request control:
/// the `cancellation_observation`-th consultation and every later one report
/// cancellation.
impl RetrievalExecutionControl for CancelAtObservation {
    fn is_cancelled(&self) -> bool {
        CodeIndexExecutionControlV1::is_cancelled(self)
    }

    fn elapsed_micros(&self) -> u64 {
        0
    }
}

impl CancelAtObservationWithJournalProbe {
    fn new(artifact_path: &Path, cancellation_observation: usize) -> Self {
        let mut journal_path = artifact_path.as_os_str().to_owned();
        journal_path.push("-journal");
        Self {
            cancellation_observation,
            observations: AtomicUsize::new(0),
            journal_path: PathBuf::from(journal_path),
            journal_seen: AtomicBool::new(false),
        }
    }

    fn journal_seen(&self) -> bool {
        self.journal_seen.load(Ordering::SeqCst)
    }
}

impl CodeIndexExecutionControlV1 for CancelAtObservation {
    fn is_cancelled(&self) -> bool {
        let observations = self
            .observations
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        observations >= self.cancellation_observation
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

impl CodeIndexExecutionControlV1 for CancelAtObservationWithJournalProbe {
    fn is_cancelled(&self) -> bool {
        let observations = self
            .observations
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        if observations < self.cancellation_observation {
            return false;
        }
        self.journal_seen
            .store(self.journal_path.exists(), Ordering::SeqCst);
        true
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct CancelAfterAcceptedPage {
    page_accepted: std::sync::atomic::AtomicBool,
}

impl CancelAfterAcceptedPage {
    fn mark_page_accepted(&self) {
        self.page_accepted.store(true, Ordering::SeqCst);
    }
}

impl CodeIndexExecutionControlV1 for CancelAfterAcceptedPage {
    fn is_cancelled(&self) -> bool {
        self.page_accepted.load(Ordering::SeqCst)
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

/// A bounded work budget that exhausts after a fixed number of deadline
/// observations, mirroring the production activations that failed with
/// "the read port exceeded its bounded work budget".
struct BudgetExhaustedAtObservation {
    exhaustion_observation: usize,
    observations: AtomicUsize,
}

/// Mutates the named artifact only at a production reader control checkpoint.
/// The reader must keep serving the already-open verified file or refuse the
/// replaced pathname; it must never hash one file and serve another.
struct ReplaceArtifactAtObservation {
    target: PathBuf,
    replacement: PathBuf,
    replacement_observation: usize,
    observations: AtomicUsize,
}

impl ReplaceArtifactAtObservation {
    fn new(target: PathBuf, replacement: PathBuf, replacement_observation: usize) -> Self {
        Self {
            target,
            replacement,
            replacement_observation,
            observations: AtomicUsize::new(0),
        }
    }
}

impl CodeIndexExecutionControlV1 for ReplaceArtifactAtObservation {
    fn is_cancelled(&self) -> bool {
        let observation = self
            .observations
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        if observation == self.replacement_observation {
            std::fs::rename(&self.replacement, &self.target)
                .expect("atomically replace the named artifact during reader control");
        }
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

impl BudgetExhaustedAtObservation {
    fn new(exhaustion_observation: usize) -> Self {
        Self {
            exhaustion_observation,
            observations: AtomicUsize::new(0),
        }
    }
}

impl CodeIndexExecutionControlV1 for BudgetExhaustedAtObservation {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        let observations = self
            .observations
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        observations >= self.exhaustion_observation
    }
}

/// A partitioned sealed generation held in memory: the manifest, its content
/// address, and every published segment under its digest.
#[derive(Clone)]
struct RealLexicalSourceFixture {
    manifest: Vec<u8>,
    segments: Arc<BTreeMap<String, Vec<u8>>>,
    state_digest: ManifestDigest,
    generation: Arc<CodeIndexPublishedGenerationV1>,
    metadata: CodeLexicalProjectionMetadataV1,
}

impl RealLexicalSourceFixture {
    fn open_source(&self, maximum_page_chunks: usize) -> VerifiedSealedLexicalPageSourceV1 {
        let segments = Arc::clone(&self.segments);
        VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
            &self.manifest,
            self.state_digest.clone(),
            move |digest, _, buffer, _control| {
                let bytes = segments.get(digest.as_str()).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract("fixture segment is missing".to_owned())
                })?;
                buffer.clear();
                buffer.extend_from_slice(bytes);
                Ok(())
            },
            maximum_page_chunks,
            1024 * 1024,
        )
        .expect("verified sealed lexical source")
    }
}

fn real_lexical_source_fixture() -> RealLexicalSourceFixture {
    real_lexical_source_fixture_with_files(1)
}

/// The in-memory projection over every admitted chunk of `generation`,
/// carrying the generation's own extracted qualified names, the same
/// authority the sealed-page artifact path reads per chunk.
fn generation_backed_projection(
    metadata: CodeLexicalProjectionMetadataV1,
    generation: &CodeIndexPublishedGenerationV1,
) -> CodeLexicalProjectionAdapterV1 {
    let chunks = generation
        .admitted_chunks()
        .expect("published generation admitted chunks")
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    let symbol_displays = generation
        .symbols()
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.occurrence.clone(),
                VerifiedSealedLexicalSymbolDisplayV1::from(symbol.as_ref()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    CodeLexicalProjectionAdapterV1::new_admitted(metadata, chunks, symbol_displays)
        .expect("generation-backed lexical projection")
}

/// One real production corpus with `file_count` TypeScript files. The first
/// file keeps the original single-file identity; the rest share its token
/// shape (identical per-field token counts) under distinct symbols so BM25
/// scores tie across files without content-identical chunks.
fn real_lexical_source_fixture_with_files(file_count: usize) -> RealLexicalSourceFixture {
    assert!(file_count >= 1, "fixture needs at least one file");
    let identity_source = b"import type { Widget } from \"widget-kit\";\nexport function render(value: Widget) { return value; }\n";
    let mut sources = (0..file_count)
        .map(|ordinal| {
            if ordinal == 0 {
                (
                    "file.artifact".to_owned(),
                    "src/artifact.ts".to_owned(),
                    identity_source.to_vec(),
                )
            } else {
                // Keep the original small-fixture identities stable.
                (
                    format!("file.artifact.{ordinal:02}"),
                    format!("src/artifact_{ordinal:02}.ts"),
                    format!(
                        "import type {{ Widget }} from \"widget-kit\";\nexport function render_{ordinal:02}(value: Widget) {{ return value; }}\n"
                    )
                    .into_bytes(),
                )
            }
        })
        .collect::<Vec<_>>();
    sources.sort_by(|left, right| left.1.cmp(&right.1));
    real_lexical_source_fixture_from_sources(sources)
}

fn real_lexical_source_fixture_from_sources(
    source_inputs: Vec<(String, String, Vec<u8>)>,
) -> RealLexicalSourceFixture {
    assert!(!source_inputs.is_empty(), "fixture needs at least one file");
    let repository = id::<RepositoryId>("repository.artifact");
    let sanitizer_revision = id::<SanitizerRevision>("sanitizer.v1");
    let languages = StaticLanguageRegistry::new();
    let sources = source_inputs
        .into_iter()
        .map(|(file_id, logical_path, source)| {
            let extension = Path::new(&logical_path)
                .extension()
                .and_then(|extension| extension.to_str())
                .expect("fixture source extension");
            let language = languages
                .descriptor_for_extension(extension)
                .expect("compiled fixture language descriptor")
                .language
                .clone();
            let file = SanitizedCodeFileV1 {
                file_occurrence_id: id::<FileOccurrenceId>(&file_id),
                logical_path,
                language: Some(language),
                content_digest: content_digest(&source),
                disposition: SnapshotFileDispositionV1::Present,
            };
            (file, source)
        })
        .collect::<Vec<_>>();
    let identity_source = sources
        .first()
        .map(|(_, source)| source.as_slice())
        .expect("non-empty fixture sources");
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: repository.clone(),
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: sanitizer_revision.clone(),
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.artifact")],
        content_identity: content_digest(identity_source),
        captured_at: UtcMicros(1_000_000),
        files: sources.iter().map(|(file, _)| file.clone()).collect(),
    };
    let request = CodeIndexBuildRequestV1 {
        snapshot,
        captured_files: sources
            .iter()
            .map(|(file, source)| CodeIndexCapturedFileV1 {
                file_occurrence_id: file.file_occurrence_id.clone(),
                sanitized_bytes: Arc::from(source.clone()),
                sensitivity_level: SensitivityLevelV1::Public,
            })
            .collect(),
        changed_files: sources
            .iter()
            .map(|(file, _)| file.logical_path.clone())
            .collect::<BTreeSet<_>>(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            tree: None,
            dirty: RepositoryDirtyStateV1::Dirty,
        },
        sealed_at: UtcMicros(1_100_000),
        target_projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "lexical.v1".to_owned(),
            profile_digest: digest_id('e'),
        },
    };
    let config = CodeIndexProductionConfigV1 {
        project_id: id::<ProjectId>("project.artifact"),
        repository: repository.clone(),
        sanitizer_revision,
        policy_revision: id::<PolicyRevisionId>("policy.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        privacy_domain: id::<PrivacyDomainId>("privacy.artifact"),
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    };
    let mut owner = CodeIndexProductionOwnerV1::new(
        config,
        ArtifactPublicationStore::default(),
        ArtifactProjectionSink,
    )
    .expect("artifact production owner");
    let generation = owner
        .build_and_publish(request, &ArtifactControl { cancelled: false })
        .expect("production generation");
    let mut segments = BTreeMap::new();
    let mut evidence_pack = Vec::new();
    let manifest = generation
        .encode_partitioned_sealed(|publication| {
            match publication {
                SealedGenerationSegmentPublicationV1::File { digest, bytes } => {
                    segments.insert(digest.as_str().to_owned(), bytes.to_vec());
                }
                SealedGenerationSegmentPublicationV1::GenerationEvidencePage { bytes, .. } => {
                    evidence_pack.extend_from_slice(bytes);
                }
                SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                    segment_digest,
                    ..
                } => {
                    segments.insert(
                        segment_digest.as_str().to_owned(),
                        std::mem::take(&mut evidence_pack),
                    );
                }
            }
            Ok(())
        })
        .expect("sealed production generation");
    let state_digest =
        ManifestDigest::from_sha256_bytes(&Sha256::digest(&manifest)).expect("manifest digest");
    let logical_paths = generation
        .snapshot()
        .files
        .iter()
        .map(|file| (file.file_occurrence_id.clone(), file.logical_path.clone()))
        .collect();
    let metadata = CodeLexicalProjectionMetadataV1 {
        generation: generation.manifest().generation_id.clone(),
        repository_id: Some(repository),
        logical_paths,
        freshness: freshness(FreshnessCompatibilityV1::Current),
        exact_retriever_revision: id::<ComponentRevision>("retriever.exact.v1"),
        lexical_retriever_revision: id::<ComponentRevision>("retriever.lexical.v1"),
        exact_score_domain: id::<ScoreDomainId>(QUERY_EXACT_SCORE_DOMAIN_V1),
        clone_route: Some(CodeLexicalCloneRouteV1 {
            project_id: generation.manifest().project_id.clone(),
            worktree_id: generation.snapshot().worktree.clone(),
            snapshot_digest: generation.manifest().snapshot_digest.clone(),
        }),
    };
    RealLexicalSourceFixture {
        manifest,
        segments: Arc::new(segments),
        state_digest,
        generation,
        metadata,
    }
}

fn real_verified_pages_with_maximum_page_chunks(
    maximum_page_chunks: usize,
) -> (
    RealLexicalSourceFixture,
    Vec<VerifiedSealedLexicalPageV1>,
    VerifiedSealedLexicalSourceReceiptV1,
) {
    let fixture = real_lexical_source_fixture();
    let (pages, receipt) = drain_verified_pages(&fixture, maximum_page_chunks);
    assert!(!pages.is_empty(), "production source emits lexical pages");
    assert!(
        pages.iter().any(|page| !page.imports().is_empty()),
        "production source emits parser-validated import evidence"
    );
    (fixture, pages, receipt)
}

fn page_symbol_displays(
    pages: &[VerifiedSealedLexicalPageV1],
) -> BTreeMap<SymbolOccurrenceId, VerifiedSealedLexicalSymbolDisplayV1> {
    pages
        .iter()
        .flat_map(|page| page.symbol_displays().iter().flatten())
        .map(|display| (display.occurrence().clone(), display.clone()))
        .collect()
}

fn drain_verified_pages(
    fixture: &RealLexicalSourceFixture,
    maximum_page_chunks: usize,
) -> (
    Vec<VerifiedSealedLexicalPageV1>,
    VerifiedSealedLexicalSourceReceiptV1,
) {
    let control = ArtifactControl { cancelled: false };
    let mut source = fixture.open_source(maximum_page_chunks);
    let mut pages = Vec::new();
    let receipt = loop {
        match source.next_page(&control).expect("verified lexical page") {
            VerifiedSealedLexicalPageReadV1::Page(page) => pages.push(page),
            VerifiedSealedLexicalPageReadV1::Complete(receipt) => break receipt,
        }
    };
    (pages, receipt)
}

fn build_clone_artifact(
    fixture: &RealLexicalSourceFixture,
) -> (
    tempfile::TempDir,
    Vec<VerifiedSealedLexicalPageV1>,
    CodeLexicalArtifactReaderV1,
) {
    let (pages, receipt) = drain_verified_pages(fixture, 128);
    let directory = tempfile::tempdir().expect("clone artifact tempdir");
    let path = directory.path().join("clone-artifact-v16.sqlite");
    let control = ArtifactControl { cancelled: false };
    let verified = {
        let mut builder = CodeLexicalArtifactBuilderV1::create(&path, fixture.metadata.clone())
            .expect("create clone artifact");
        for page in &pages {
            builder
                .append_page(page, &control)
                .expect("append clone artifact page");
        }
        finish_staged_artifact(&mut builder, &receipt, &control)
    };
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &path,
        &verified,
        &fixture.metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("open clone artifact");
    (directory, pages, reader)
}

fn real_verified_pages() -> (
    RealLexicalSourceFixture,
    Vec<VerifiedSealedLexicalPageV1>,
    VerifiedSealedLexicalSourceReceiptV1,
) {
    real_verified_pages_with_maximum_page_chunks(128)
}

fn page_batch_identities(pages: &[VerifiedSealedLexicalPageV1]) -> Vec<(u64, String, Vec<u8>)> {
    pages
        .iter()
        .map(|page| {
            (
                page.page_ordinal(),
                page.page_digest().as_str().to_owned(),
                page.next_cursor()
                    .persisted_bytes()
                    .expect("persist page cursor"),
            )
        })
        .collect()
}

#[test]
fn sealed_source_rejected_batch_retries_byte_identical_pages_and_cursor() {
    let fixture = real_lexical_source_fixture();
    let control = ArtifactControl { cancelled: false };
    let mut source = fixture.open_source(1);
    let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(2, 64 * 1024 * 1024)
        .expect("two-page batch bounds");
    let cursor_before = source.cursor().persisted_bytes().expect("initial cursor");
    let mut rejected_identities = None;
    let rejected = source
        .next_page_batch_if(&control, bounds, |pages| {
            rejected_identities = Some(page_batch_identities(pages));
            Err("reject staged batch")
        })
        .expect("stage rejected batch");
    assert!(matches!(rejected, Err("reject staged batch")));
    assert_eq!(
        source.cursor().persisted_bytes().expect("rejected cursor"),
        cursor_before,
        "callback rejection must retain the byte-exact source cursor"
    );

    let retried = source
        .next_page_batch_if(&control, bounds, |pages| {
            Ok::<_, &'static str>(NonZeroUsize::new(pages.len()).expect("non-empty source batch"))
        })
        .expect("retry staged batch")
        .expect("accept retried batch");
    let VerifiedSealedLexicalPageBatchReadV1::Pages(retried_pages) = retried else {
        panic!("fixture must emit a retried page batch");
    };
    assert_eq!(
        page_batch_identities(&retried_pages),
        rejected_identities.expect("rejected page identities"),
        "retry must reproduce the exact ordered pages"
    );
    assert_eq!(
        source.cursor().persisted_bytes().expect("accepted cursor"),
        retried_pages
            .last()
            .expect("retried pages")
            .next_cursor()
            .persisted_bytes()
            .expect("final accepted cursor"),
        "acceptance advances exactly to the final page"
    );
}

#[test]
fn sealed_source_batch_bounds_and_completion_never_advance_empty_work() {
    assert!(VerifiedSealedLexicalPageBatchBoundsV1::new(0, 1).is_err());
    assert!(VerifiedSealedLexicalPageBatchBoundsV1::new(1, 0).is_err());

    let fixture = real_lexical_source_fixture();
    let control = ArtifactControl { cancelled: false };
    let (pages, _) = drain_verified_pages(&fixture, 1);
    let first_page_bound =
        std::mem::size_of::<VerifiedSealedLexicalPageV1>() + pages[0].retained_owned_bytes() - 1;
    let too_small = VerifiedSealedLexicalPageBatchBoundsV1::new(1, first_page_bound)
        .expect("sub-page batch bound");
    let mut source = fixture.open_source(1);
    let cursor_before = source.cursor().persisted_bytes().expect("initial cursor");
    let callbacks = AtomicUsize::new(0);
    assert!(
        source
            .next_page_batch_if(&control, too_small, |_| {
                callbacks.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(NonZeroUsize::MIN)
            })
            .is_err(),
        "a first page above the retained-byte bound is a typed source error"
    );
    assert_eq!(callbacks.load(Ordering::SeqCst), 0);
    assert_eq!(
        source.cursor().persisted_bytes().expect("refused cursor"),
        cursor_before
    );

    let bounds =
        VerifiedSealedLexicalPageBatchBoundsV1::new(2, 64 * 1024 * 1024).expect("drain bounds");
    loop {
        let before = callbacks.load(Ordering::SeqCst);
        let read = source
            .next_page_batch_if(&control, bounds, |pages| {
                callbacks.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(NonZeroUsize::new(pages.len()).expect("non-empty source batch"))
            })
            .expect("drain source")
            .expect("accept source batch");
        match read {
            VerifiedSealedLexicalPageBatchReadV1::Pages(pages) => {
                assert!(!pages.is_empty(), "page batches are never empty");
                assert_eq!(callbacks.load(Ordering::SeqCst), before + 1);
            }
            VerifiedSealedLexicalPageBatchReadV1::Complete(_) => {
                assert_eq!(
                    callbacks.load(Ordering::SeqCst),
                    before,
                    "completion must bypass the page callback"
                );
                break;
            }
        }
    }
}

fn finish_staged_artifact(
    builder: &mut CodeLexicalArtifactBuilderV1,
    source_receipt: &VerifiedSealedLexicalSourceReceiptV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> VerifiedCodeLexicalArtifactV1 {
    loop {
        match builder
            .advance_finalization(source_receipt, 4_096, control)
            .expect("finalize staged lexical artifact")
        {
            CodeLexicalArtifactFinalizationStepV1::Pending { .. } => {}
            CodeLexicalArtifactFinalizationStepV1::Ready(receipt) => return *receipt,
        }
    }
}

fn stored_base_section_receipts(path: &Path) -> Vec<Vec<u8>> {
    let connection = rusqlite::Connection::open(path).expect("open artifact receipt inspection");
    let mut statement = connection
        .prepare("SELECT base_sections_receipt FROM source_pages ORDER BY page_ordinal")
        .expect("prepare ordered base-section receipt query");
    statement
        .query_map([], |row| row.get(0))
        .expect("query ordered base-section receipts")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect ordered base-section receipts")
}

/// Fixture-only source driver retained for legacy regression setup. Production
/// finalization receives the source receipt and never owns a source reader.
trait TestArtifactSourceStaging {
    fn rebuild_and_finalize(
        &mut self,
        source: &mut VerifiedSealedLexicalPageSourceV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedCodeLexicalArtifactV1, CodeLexicalArtifactErrorV1>;
}

impl TestArtifactSourceStaging for CodeLexicalArtifactBuilderV1 {
    fn rebuild_and_finalize(
        &mut self,
        source: &mut VerifiedSealedLexicalPageSourceV1,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedCodeLexicalArtifactV1, CodeLexicalArtifactErrorV1> {
        let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(16, 32 * 1024 * 1024)
            .map_err(|error| CodeLexicalArtifactErrorV1::Corrupt(error.to_string()))?;
        let receipt = loop {
            let admitted = source
                .next_page_batch_if(control, bounds, |pages| {
                    let prepared = self.prepare_admissible_page_prefix(pages, control)?;
                    let accepted = prepared.accepted_prefix();
                    self.append_prepared_pages(prepared.prepared_pages(), control)?;
                    Ok(accepted)
                })
                .map_err(|error| match error {
                    CodeIndexProductionErrorV1::Interrupted(interruption) => {
                        CodeLexicalArtifactErrorV1::Interrupted(interruption)
                    }
                    error => CodeLexicalArtifactErrorV1::Corrupt(error.to_string()),
                })?;
            match admitted? {
                VerifiedSealedLexicalPageBatchReadV1::Pages(_) => {}
                VerifiedSealedLexicalPageBatchReadV1::Complete(receipt) => break receipt,
            }
        };
        Ok(finish_staged_artifact(self, &receipt, control))
    }
}

pub(crate) use tracedecay_domain::test_fixtures::id;

pub(crate) fn digest_id<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("valid fixture digest")
}

pub(crate) fn budget(max_candidates_per_lane: u32) -> RetrievalBudget {
    RetrievalBudget {
        max_candidates_per_lane,
        max_fused_candidates: 16,
        max_hydrated_results: 8,
        max_hydration_bytes: 65_536,
        deadline_micros: None,
    }
}

pub(crate) fn base_request(_query: &str, max_candidates_per_lane: u32) -> RetrievalRequest {
    RetrievalRequest {
        principal: id::<PrincipalId>("principal.fixture"),
        scope: RetrievalScope {
            privacy_domain: id("privacy.fixture"),
            root: SingleRootScopeV1 {
                repository: id("repository.fixture"),
                worktree: None,
                reference: None,
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: digest_id('f'),
            authorization_revision: id("authorization.v1"),
            captured_at: UtcMicros(7),
        },
        profile_id: id("profile.fixture.v1"),
        budget: budget(max_candidates_per_lane),
    }
}

pub(crate) fn query_view(query: &str) -> EphemeralSanitizedQueryViewV1 {
    EphemeralSanitizedQueryViewV1::sanitize(
        query,
        id::<SanitizerRevision>("query-sanitizer.v1"),
        id::<QueryNormalizationRevision>("query-normalization.v1"),
    )
    .expect("query sanitizes")
}

pub(crate) fn freshness(compatibility: FreshnessCompatibilityV1) -> SourceFreshness {
    SourceFreshness {
        source_namespace: id::<SourceNamespace>("ns.code.fixture"),
        source_instance: id::<SourceInstanceKey>("instance.fixture"),
        source_watermark: Some(7),
        projection_watermark: Some(7),
        observed_at: UtcMicros(7),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility,
        policy_revision: id("policy.fixture.v1"),
    }
}

pub(crate) fn projection_metadata(
    generation: &CodeGenerationId,
    compatibility: FreshnessCompatibilityV1,
) -> CodeLexicalProjectionMetadataV1 {
    let mut logical_paths = (0..=128)
        .map(|ordinal| {
            (
                id::<FileOccurrenceId>(&format!("file.{ordinal}")),
                format!("src/file-{ordinal}.rs"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    logical_paths.extend((0..=128).map(|ordinal| {
        (
            id::<FileOccurrenceId>(&format!("file.admitted.{ordinal}")),
            format!("src/admitted_{ordinal}.rs"),
        )
    }));
    CodeLexicalProjectionMetadataV1 {
        generation: generation.clone(),
        repository_id: Some(id::<RepositoryId>("repository.fixture")),
        logical_paths,
        freshness: freshness(compatibility),
        exact_retriever_revision: id::<ComponentRevision>("retriever.exact.v1"),
        lexical_retriever_revision: id::<ComponentRevision>("retriever.lexical.v1"),
        exact_score_domain: id::<ScoreDomainId>(QUERY_EXACT_SCORE_DOMAIN_V1),
        clone_route: Some(CodeLexicalCloneRouteV1 {
            project_id: id("project.fixture"),
            worktree_id: None,
            snapshot_digest: digest_id('d'),
        }),
    }
}

pub(crate) fn chunk(
    generation: &CodeGenerationId,
    ordinal: u32,
    grain: CodeSearchChunkGrainV1,
    text: &str,
    terms: &[(ExactTechnicalTermKindV1, &str)],
    subtokens: &[&str],
) -> CodeSearchChunkV1 {
    let symbol = matches!(
        grain,
        CodeSearchChunkGrainV1::SymbolSignature
            | CodeSearchChunkGrainV1::SymbolBody
            | CodeSearchChunkGrainV1::SymbolMember
    )
    .then(|| id::<SymbolOccurrenceId>(&format!("symbol.{ordinal}")));
    let mut exact_terms: Vec<ExactTechnicalTermV1> = terms
        .iter()
        .map(|(kind, term)| {
            let start = text
                .find(term)
                .unwrap_or_else(|| panic!("term {term:?} is present in {text:?}"));
            let span = SourceSpan {
                start_byte: start as u64,
                end_byte: (start + term.len()) as u64,
            };
            if *kind == ExactTechnicalTermKindV1::WholeSymbol {
                ExactTechnicalTermV1::untrusted_whole_symbol_candidate(
                    term.as_bytes().to_vec(),
                    span,
                    symbol.clone().expect("symbol grain"),
                )
            } else if matches!(
                kind,
                ExactTechnicalTermKindV1::CompilerErrorText
                    | ExactTechnicalTermKindV1::RuntimeErrorText
            ) {
                ExactTechnicalTermV1::untrusted_contextual_text_candidate(
                    *kind,
                    term.as_bytes().to_vec(),
                    span,
                )
            } else {
                ExactTechnicalTermV1::technical(*kind, term.as_bytes().to_vec(), span)
            }
            .expect("valid exact-term fixture")
        })
        .collect();
    exact_terms.sort_by(|left, right| {
        (
            left.span().start_byte,
            left.span().end_byte,
            left.kind(),
            left.canonical_bytes(),
            left.original_bytes(),
        )
            .cmp(&(
                right.span().start_byte,
                right.span().end_byte,
                right.kind(),
                right.canonical_bytes(),
                right.original_bytes(),
            ))
    });
    CodeSearchChunkV1 {
        id: id::<CodeSearchChunkId>(&format!("chunk.{ordinal}")),
        anchor: CodeSearchChunkAnchorV1 {
            generation_id: generation.clone(),
            file_occurrence_id: id::<FileOccurrenceId>(&format!("file.{ordinal}")),
            symbol_occurrence_id: symbol,
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: text.len() as u64,
            },
            grain,
            ordinal,
        },
        content_digest: digest_id::<ContentDigest>(
            char::from_digit((ordinal % 10) + 1, 16).expect("hex digit"),
        ),
        language_descriptor_revision: id::<LanguageDescriptorRevision>("language.rust.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        sanitizer_revision: id("sanitizer.v1"),
        sensitivity: SensitivityDecision {
            level: SensitivityLevelV1::Internal,
            policy_revision: id::<PolicyRevisionId>("policy.fixture.v1"),
        },
        exact_terms,
        subtokens: subtokens.iter().map(|value| (*value).to_owned()).collect(),
        sanitized_text: BoundedSanitizedText::new(text).expect("bounded fixture text"),
    }
}

fn admitted_rust_chunk(
    generation: &CodeGenerationId,
    ordinal: u32,
    source: &str,
    grain: CodeSearchChunkGrainV1,
    symbol_name: &str,
) -> ExtractionAdmittedCodeSearchChunkV1 {
    let registry = StaticLanguageRegistry::new();
    let descriptor = registry
        .descriptor(&id("rust"))
        .expect("rust descriptor")
        .clone();
    let sanitizer_revision = id::<SanitizerRevision>("sanitizer.v1");
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: id(&format!("file.admitted.{ordinal}")),
        logical_path: format!("src/admitted_{ordinal}.rs"),
        language: Some(id("rust")),
        content_digest: content_digest(source.as_bytes()),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let intake =
        SanitizedCodeIntake::new(registry, sanitizer_revision.clone(), UtcMicros(1_000_000));
    let snapshot = intake
        .admit(SanitizedCodeSnapshotV1 {
            repository: id("repo.fixture"),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: sanitizer_revision.clone(),
            sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.fixture")],
            content_identity: content_digest(source.as_bytes()),
            captured_at: UtcMicros(1_000_000),
            files: vec![file.clone()],
        })
        .expect("snapshot admission");
    let file = intake
        .bind_file(
            &snapshot,
            &id::<ProjectId>("project.fixture"),
            ValidatedCodeFileV1 {
                generation_id: generation.clone(),
                file,
                snapshot_digest: snapshot.snapshot().intake_digest.clone(),
                sanitized_bytes: source.as_bytes().to_vec(),
            },
        )
        .expect("file admission");
    let batch = TreeSitterExtractor::new()
        .extract(&file, &descriptor, &NeverCancelled)
        .expect("extract rust fixture");
    let chunker = DeterministicCodeChunker::new(
        generation.clone(),
        id("repo.fixture"),
        sanitizer_revision,
        id("policy.fixture.v1"),
        id("chunker.v1"),
    );
    let (artifacts, authority) = chunker
        .index_file_with_authority_from_extraction(
            &file,
            &batch,
            &descriptor,
            SensitivityLevelV1::Public,
            &NeverCancelled,
        )
        .expect("chunk with exact authority");
    let chunk = artifacts
        .chunks
        .chunks
        .into_iter()
        .find(|chunk| {
            chunk.anchor.grain == grain
                && chunk.exact_terms.iter().any(|term| {
                    term.kind() == ExactTechnicalTermKindV1::WholeSymbol
                        && term.original_bytes() == symbol_name.as_bytes()
                })
        })
        .expect("requested parser-minted symbol chunk");
    authority.admit(chunk).expect("exact extraction admission")
}

#[test]
fn retained_lexical_projection_preserves_progress_across_bounded_windows() {
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks = (0..3)
        .map(|ordinal| {
            let symbol = format!("retained_symbol_{ordinal}");
            admitted_rust_chunk(
                &generation,
                ordinal,
                &format!("pub fn {symbol}() -> usize {{ {ordinal} }}\n"),
                CodeSearchChunkGrainV1::SymbolSignature,
                &symbol,
            )
        })
        .collect::<Vec<_>>();
    let one_shot = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks.clone(),
        BTreeMap::new(),
    )
    .expect("one-shot retained lexical projection");
    let mut build = CodeLexicalProjectionBuildV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
        BTreeMap::new(),
    )
    .expect("start retained lexical projection");

    assert!(matches!(
        build.advance(1).expect("first bounded window"),
        CodeLexicalProjectionBuildStepV1::Pending {
            completed_documents: 1,
            total_documents: 3,
        }
    ));
    assert!(matches!(
        build.advance(1).expect("second bounded window"),
        CodeLexicalProjectionBuildStepV1::Pending {
            completed_documents: 2,
            total_documents: 3,
        }
    ));
    let projection = loop {
        match build.advance(1).expect("finish bounded projection") {
            CodeLexicalProjectionBuildStepV1::Pending { .. } => {}
            CodeLexicalProjectionBuildStepV1::Ready(projection) => break *projection,
        }
    };
    let request = lexical_request("retained_symbol_2", &["retained_symbol_2"], &[], &[], 0, 8);
    let outcome = LexicalLane::new(projection)
        .retrieve_lexical(&request)
        .expect("query completed retained projection");
    let one_shot_outcome = LexicalLane::new(one_shot)
        .retrieve_lexical(&request)
        .expect("query completed one-shot projection");
    assert_eq!(outcome, one_shot_outcome);
    let RetrieverOutcome::Complete(batch) = outcome else {
        panic!("completed retained projection must serve lexical query");
    };
    assert!(
        batch
            .candidates
            .iter()
            .any(|candidate| candidate.file_occurrence_id.as_ref() == Some(&id("file.admitted.2")))
    );
}

#[test]
fn disk_artifact_resume_reopen_and_lexical_results_match_one_shot_projection() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let generation = metadata.generation.clone();
    let chunks = pages
        .iter()
        .flat_map(|page| page.chunks().iter().cloned())
        .collect::<Vec<_>>();
    let one_shot = CodeLexicalProjectionAdapterV1::new_admitted(
        metadata.clone(),
        chunks.clone(),
        page_symbol_displays(&pages),
    )
    .expect("one-shot lexical projection");
    let import_evidence = pages
        .iter()
        .flat_map(|page| page.imports())
        .next()
        .expect("real source import evidence")
        .clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("lexical-artifact-v1.sqlite");
    let control = ArtifactControl { cancelled: false };
    {
        let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone())
            .expect("create artifact");
        let cancelled = ArtifactControl { cancelled: true };
        assert!(matches!(
            builder.append_page(&pages[0], &cancelled),
            Err(CodeLexicalArtifactErrorV1::Interrupted(_))
        ));
        assert_eq!(builder.progress().expect("progress").next_page_ordinal, 0);
        for page in &pages {
            builder.append_page(page, &control).expect("append page");
        }
    }
    let verified = {
        let mut resumed =
            CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
                &artifact_path,
                metadata.clone(),
                CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
                &control,
            )
            .expect("resume artifact");
        let replay = resumed
            .append_page(&pages[0], &control)
            .expect("replayed page is idempotent");
        assert_eq!(replay.next_page_ordinal, source_receipt.page_count());
        let mut final_source = fixture.open_source(128);
        resumed
            .rebuild_and_finalize(&mut final_source, &control)
            .expect("rebuild and finalize artifact from verified source")
    };
    let artifact_bytes = std::fs::read(&artifact_path).expect("read finalized artifact");
    let artifact_digest = ManifestDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(&artifact_bytes))
    ))
    .expect("artifact content digest");
    let reader = CodeLexicalArtifactReaderV1::open_content_addressed(
        &artifact_path,
        &artifact_digest,
        u64::try_from(artifact_bytes.len()).expect("artifact length fits u64"),
        &metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("verify and reopen content-addressed artifact");
    assert!(reader.retained_owned_bytes() <= CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1);
    let occurrence = reader
        .occurrence_by_chunk(&chunks.last().expect("real source chunk").chunk().id)
        .expect("artifact row lookup")
        .expect("artifact occurrence");
    assert_eq!(occurrence.logical_path, "src/artifact.ts");
    let symbol_chunk = chunks
        .iter()
        .find(|chunk| chunk.chunk().anchor.symbol_occurrence_id.is_some())
        .expect("parser-backed symbol chunk");
    let symbol_occurrence = reader
        .occurrence_by_chunk(&symbol_chunk.chunk().id)
        .expect("artifact symbol row lookup")
        .expect("artifact symbol occurrence");
    assert_eq!(symbol_occurrence.simple_name.as_deref(), Some("render"));
    assert_eq!(
        symbol_occurrence.qualified_name.as_deref(),
        Some("src/artifact.ts::render")
    );
    assert_eq!(symbol_occurrence.kind.as_deref(), Some("function"));
    let import_witness = reader
        .import_membership(&import_evidence)
        .expect("import membership")
        .expect("exact import witness");
    assert_eq!(import_witness.evidence, import_evidence);
    assert_eq!(
        &import_witness.import_dictionary_digest,
        verified.import_dictionary_digest()
    );

    let mut request = lexical_request(
        "rendre return value",
        &["rendre"],
        &[],
        &["return value"],
        2,
        8,
    );
    request.generation = generation;
    let artifact = LexicalLane::new(reader)
        .retrieve_lexical(&request)
        .expect("artifact lexical query");
    let expected = LexicalLane::new(one_shot)
        .retrieve_lexical(&request)
        .expect("one-shot lexical query");
    assert_eq!(artifact, expected);
}

#[test]
fn clone_payloads_are_content_addressed_and_postings_page() {
    let body = "one(); two(); three(); four(); five(); six(); seven(); eight(); nine(); ten();";
    let fixture = real_lexical_source_fixture_from_sources(vec![
        (
            "file.clone.alpha".to_owned(),
            "src/alpha.ts".to_owned(),
            format!("export function alpha() {{ {body} }}\n").into_bytes(),
        ),
        (
            "file.clone.beta".to_owned(),
            "src/beta.ts".to_owned(),
            format!("export function beta() {{ {body} }}\n").into_bytes(),
        ),
        (
            "file.clone.delta".to_owned(),
            "src/delta.ts".to_owned(),
            format!("export function delta() {{ {body} }}\n").into_bytes(),
        ),
        (
            "file.clone.gamma".to_owned(),
            "src/gamma.ts".to_owned(),
            b"export function gamma() { return 1; }\n".to_vec(),
        ),
    ]);
    let (pages, receipt) = drain_verified_pages(&fixture, 1);
    let clone_bodies = pages
        .iter()
        .flat_map(VerifiedSealedLexicalPageV1::clone_bodies)
        .collect::<Vec<_>>();
    assert_eq!(clone_bodies.len(), 4);
    let excluded = clone_bodies
        .iter()
        .find(|body| body.payload.token_count < 30)
        .expect("small body remains a payload occurrence");
    assert!(
        excluded
            .payload
            .exact_keys(excluded.occurrence.eligibility)
            .is_empty()
    );
    let key = clone_bodies[0]
        .payload
        .exact_keys(clone_bodies[0].occurrence.eligibility)
        .into_iter()
        .find(|key| key.class == CloneNormalizationClassV1::Conservative)
        .expect("conservative exact key");
    let authority = clone_bodies[0].occurrence.clone();

    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("lexical-artifact.sqlite");
    let control = ArtifactControl { cancelled: false };
    let verified = {
        let mut builder =
            CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata.clone())
                .expect("create artifact");
        for page in &pages {
            builder.append_page(page, &control).expect("append page");
        }
        finish_staged_artifact(&mut builder, &receipt, &control)
    };
    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect V16 artifact");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM clone_body_payloads", [], |row| row
                .get::<_, i64>(0))
            .expect("payload count"),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM clone_occurrences", [], |row| row
                .get::<_, i64>(0))
            .expect("occurrence count"),
        4
    );
    let (fingerprint_lists, counted_postings, untagged_payloads): (i64, i64, i64) = connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM clone_fingerprint_postings),
                (SELECT COALESCE(SUM(posting_count), 0) FROM clone_fingerprint_postings),
                (SELECT COUNT(*) FROM clone_body_payloads WHERE substr(payload, 1, 1) != x'02')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("fingerprint counts");
    assert!(fingerprint_lists > 0);
    assert!(
        counted_postings >= fingerprint_lists,
        "one sealed list per fingerprint holds every posting"
    );
    assert_eq!(untagged_payloads, 0, "clone payloads are stored deflated");
    drop(connection);

    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        &fixture.metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("open V16 artifact");
    let excluded_page = reader
        .clone_fingerprint_page(&excluded.occurrence, &excluded.payload, None, 10, &control)
        .expect("excluded fingerprint read");
    assert_eq!(
        excluded_page.source_eligibility,
        excluded.occurrence.eligibility
    );
    assert_eq!(
        excluded_page.coverage,
        RetrieverCoverage {
            examined: 1,
            excluded: 1,
            ..RetrieverCoverage::default()
        }
    );
    assert!(excluded_page.page.members.is_empty());
    let fingerprints = reader
        .clone_fingerprint_page(&authority, &clone_bodies[0].payload, None, 10, &control)
        .expect("fingerprint candidates");
    assert_eq!(
        fingerprints.coverage,
        RetrieverCoverage {
            examined: 1,
            eligible: 1,
            ..RetrieverCoverage::default()
        }
    );
    assert_eq!(fingerprints.page.members.len(), 1);
    assert_eq!(fingerprints.accounting.candidates_admitted, 1);
    assert_eq!(fingerprints.accounting.pairs_verified, 1);
    assert!(!fingerprints.page.members[0].ordered_anchors.is_empty());
    assert_eq!(fingerprints.page.members[0].occurrences.len(), 2);
    let cancelled = reader
        .clone_fingerprint_page(
            &authority,
            &clone_bodies[0].payload,
            None,
            10,
            &ArtifactControl { cancelled: true },
        )
        .expect("cancelled fingerprint read");
    assert_eq!(cancelled.coverage.unknown, 1);
    assert_eq!(
        cancelled.partial_reasons,
        vec![CloneFingerprintPartialReasonV1::Cancelled]
    );
    assert_eq!(
        cancelled.accounting.cancellation_point,
        Some(CloneFingerprintCancellationPointV1::FingerprintCountRead)
    );
    let mut unauthorized = authority.clone();
    unauthorized.repository_id = id::<RepositoryId>("repository.unauthorized");
    assert!(matches!(
        reader.clone_exact_page(&unauthorized, &key, None, 1, &control),
        Err(CodeLexicalArtifactErrorV1::Missing(_))
    ));
    assert!(matches!(
        reader.clone_exact_page(
            &authority,
            &key,
            None,
            MAX_CLONE_EXACT_PAGE_MEMBERS_V1 + 1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
    let first = reader
        .clone_exact_page(&authority, &key, None, 1, &control)
        .expect("first clone page");
    assert_eq!(first.members.len(), 1);
    assert!(matches!(
        reader.clone_fingerprint_page(
            &authority,
            &clone_bodies[0].payload,
            first.next_cursor.as_ref(),
            1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
    let rename_key = clone_bodies[0]
        .payload
        .exact_keys(clone_bodies[0].occurrence.eligibility)
        .into_iter()
        .find(|key| key.class == CloneNormalizationClassV1::Rename)
        .expect("rename exact key");
    assert!(matches!(
        reader.clone_exact_page(
            &authority,
            &rename_key,
            first.next_cursor.as_ref(),
            1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
    let mut altered_scope = authority.clone();
    altered_scope.project_id = id::<ProjectId>("project.other");
    assert!(matches!(
        reader.clone_exact_page(
            &altered_scope,
            &key,
            first.next_cursor.as_ref(),
            1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Missing(_))
    ));
    let second = reader
        .clone_exact_page(&authority, &key, first.next_cursor.as_ref(), 1, &control)
        .expect("second clone page");
    assert_eq!(second.members.len(), 1);
    assert!(second.next_cursor.is_none());
    assert_eq!(
        first.members[0].payload.payload_digest,
        second.members[0].payload.payload_digest
    );
    assert_ne!(
        first.members[0].occurrence.symbol_occurrence_id,
        second.members[0].occurrence.symbol_occurrence_id
    );

    let reduced = real_lexical_source_fixture_from_sources(vec![
        (
            "file.clone.alpha".to_owned(),
            "src/alpha.ts".to_owned(),
            format!("export function alpha() {{ {body} }}\n").into_bytes(),
        ),
        (
            "file.clone.gamma".to_owned(),
            "src/gamma.ts".to_owned(),
            b"export function gamma() { return 1; }\n".to_vec(),
        ),
    ]);
    let (reduced_pages, reduced_receipt) = drain_verified_pages(&reduced, 1);
    let reduced_payload = reduced_pages
        .iter()
        .flat_map(VerifiedSealedLexicalPageV1::clone_bodies)
        .find(|body| body.payload.token_count >= 30)
        .expect("unchanged eligible body");
    assert_eq!(
        reduced_payload.payload.payload_digest,
        first.members[0].payload.payload_digest
    );
    let reduced_path = directory.path().join("lexical-artifact-reduced-v15.sqlite");
    let reduced_verified = {
        let mut builder =
            CodeLexicalArtifactBuilderV1::create(&reduced_path, reduced.metadata.clone())
                .expect("create reduced V15 artifact");
        for page in &reduced_pages {
            builder
                .append_page(page, &control)
                .expect("append reduced page");
        }
        finish_staged_artifact(&mut builder, &reduced_receipt, &control)
    };
    let reduced_reader = CodeLexicalArtifactReaderV1::open_with_control(
        &reduced_path,
        &reduced_verified,
        &reduced.metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("open reduced V15 artifact");
    assert_eq!(
        reduced_reader
            .clone_exact_page(&reduced_payload.occurrence, &key, None, 10, &control)
            .expect("query after deletion")
            .members
            .len(),
        0,
        "deleted occurrences and postings must not remain active"
    );
    drop(reader);
    let connection = rusqlite::Connection::open(&artifact_path).expect("open clone tamper writer");
    connection
        .execute_batch(
            "DROP TRIGGER immutable_clone_body_payloads_update;
             UPDATE clone_body_payloads SET payload = zeroblob(length(payload));",
        )
        .expect("tamper clone payload bytes");
    drop(connection);
    assert!(matches!(
        CodeLexicalArtifactReaderV1::open_with_control(
            &artifact_path,
            &verified,
            &fixture.metadata,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Corrupt(_))
    ));
    let connection = rusqlite::Connection::open(&artifact_path).expect("open shape tamper writer");
    connection
        .execute_batch("DROP TABLE clone_exact_postings;")
        .expect("remove required clone table");
    drop(connection);
    assert!(matches!(
        CodeLexicalArtifactReaderV1::open_with_control(
            &artifact_path,
            &verified,
            &fixture.metadata,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Incompatible(_))
    ));
}

#[test]
fn fingerprint_candidates_reject_incompatible_bodies_and_page_byte_identically() {
    let shared = "shared01(); shared02(); shared03(); shared04(); shared05(); shared06(); shared07(); shared08(); shared09(); shared10(); shared11(); shared12(); shared13(); shared14();";
    let function = |name: &str, difference: &str| {
        format!(
            "export function {name}() {{ before(); before2(); {shared} {difference} after(); after2(); }}\n"
        )
    };
    let fixture = real_lexical_source_fixture_from_sources(vec![
        (
            "file.clone.page.alpha".to_owned(),
            "src/alpha.ts".to_owned(),
            function("alpha", "execute(\"original\");").into_bytes(),
        ),
        (
            "file.clone.page.beta".to_owned(),
            "src/beta.ts".to_owned(),
            function("beta", "execute(\"changed\");").into_bytes(),
        ),
        (
            "file.clone.page.delta".to_owned(),
            "src/delta.ts".to_owned(),
            function(
                "delta",
                "if (enabled()) { added_branch(); } execute(\"original\");",
            )
            .into_bytes(),
        ),
        (
            "file.clone.page.gamma".to_owned(),
            "src/gamma.ts".to_owned(),
            function(
                "gamma",
                "try { execute(\"original\"); } catch (error) { report(error); }",
            )
            .into_bytes(),
        ),
        (
            "file.clone.page.method".to_owned(),
            "src/method.ts".to_owned(),
            format!("export class Holder {{ candidate() {{ {shared} }} }}\n").into_bytes(),
        ),
    ]);
    let (_directory, pages, reader) = build_clone_artifact(&fixture);
    let source = pages
        .iter()
        .flat_map(VerifiedSealedLexicalPageV1::clone_bodies)
        .find(|body| body.occurrence.path == "src/alpha.ts")
        .expect("source clone body");
    let control = ArtifactControl { cancelled: false };
    let all = reader
        .clone_fingerprint_page(&source.occurrence, &source.payload, None, 256, &control)
        .expect("all fingerprint candidates");
    assert_eq!(all.coverage.capped, 0);
    assert_eq!(all.coverage.unknown, 0);
    assert_eq!(all.minimum_directional_coverage_millionths, 700_000);
    assert!(all.page.members.len() >= 3);
    let stream = all.stream.as_ref().expect("fingerprint stream");
    assert!(all.page.members.iter().all(|candidate| {
        let candidate_stream = candidate
            .payload
            .fingerprint_stream(candidate.occurrences[0].eligibility)
            .expect("candidate stream");
        candidate.payload.language == stream.language
            && candidate.payload.symbol_kind == source.payload.symbol_kind
            && candidate_stream.class == stream.class
            && candidate_stream.normalization_revision == stream.normalization_revision
            && !candidate.ordered_anchors.is_empty()
            && candidate.source == source.occurrence
            && candidate.extent == CloneNearMatchExtentV1::WholeBody
            && candidate.source_coverage_millionths >= 700_000
            && candidate.candidate_coverage_millionths >= 700_000
            && candidate.shared_ordered_token_count > 0
            && !candidate.differences.is_empty()
    }));
    for (path, expected_token) in [
        ("src/beta.ts", "changed"),
        ("src/delta.ts", "if"),
        ("src/gamma.ts", "try"),
    ] {
        let candidate = all
            .page
            .members
            .iter()
            .find(|candidate| candidate.occurrences[0].path == path)
            .expect("named near-clone candidate");
        assert!(
            candidate.differences.iter().any(|difference| {
                difference.right_tokens.iter().any(|token| match token {
                    tracedecay_code_extraction::ConservativeCloneTokenV1::StructureStart {
                        syntax_kind,
                    }
                    | tracedecay_code_extraction::ConservativeCloneTokenV1::StructureEnd {
                        syntax_kind,
                    } => syntax_kind.contains(expected_token),
                    tracedecay_code_extraction::ConservativeCloneTokenV1::Syntax {
                        syntax_kind,
                        text,
                    } => syntax_kind.contains(expected_token) || text.contains(expected_token),
                })
            }),
            "{path} differences did not expose {expected_token}: {candidate:#?}"
        );
    }

    let first = reader
        .clone_fingerprint_page(&source.occurrence, &source.payload, None, 1, &control)
        .expect("first fingerprint page");
    let cursor = first.page.next_cursor.as_ref().expect("fingerprint cursor");
    let exact_key = source
        .payload
        .exact_keys(source.occurrence.eligibility)
        .into_iter()
        .find(|key| key.class == CloneNormalizationClassV1::Conservative)
        .expect("exact conservative key");
    assert!(matches!(
        reader.clone_exact_page(&source.occurrence, &exact_key, Some(cursor), 1, &control),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
    let mut altered_scope = source.occurrence.clone();
    altered_scope.project_id = id::<ProjectId>("project.other");
    assert!(matches!(
        reader.clone_fingerprint_page(&altered_scope, &source.payload, Some(cursor), 1, &control,),
        Err(CodeLexicalArtifactErrorV1::Missing(_))
    ));
    let mut stale = source.occurrence.clone();
    stale.source_generation = id::<CodeGenerationId>("generation.stale");
    let stale_error = reader
        .clone_fingerprint_page(&stale, &source.payload, None, 1, &control)
        .expect_err("stale source generation");
    assert_eq!(
        stale_error.to_string(),
        format!(
            "lexical artifact authority is missing: clone lookup generation {} is stale; the artifact serves {}",
            stale.source_generation.as_str(),
            source.occurrence.source_generation.as_str()
        )
    );

    let mut paged = Vec::new();
    let mut cursor = None;
    loop {
        let page = reader
            .clone_fingerprint_page(
                &source.occurrence,
                &source.payload,
                cursor.as_ref(),
                1,
                &control,
            )
            .expect("paged fingerprint candidates");
        paged.extend(page.page.members);
        let Some(next) = page.page.next_cursor else {
            break;
        };
        cursor = Some(next);
    }
    assert_eq!(paged, all.page.members);

    let distinct_source_fingerprints = source
        .payload
        .fingerprint_positions(source.occurrence.eligibility)
        .expect("source fingerprints")
        .into_iter()
        .map(|position| position.fingerprint)
        .collect::<BTreeSet<_>>()
        .len();
    let cancelled = reader
        .clone_fingerprint_page(
            &source.occurrence,
            &source.payload,
            None,
            256,
            &CancelsAfterChecks {
                checks: AtomicUsize::new(0),
                cancel_at: distinct_source_fingerprints + 1,
            },
        )
        .expect("mid-read cancellation");
    assert_eq!(cancelled.coverage.unknown, 1);
    assert_eq!(
        cancelled.accounting.cancellation_point,
        Some(CloneFingerprintCancellationPointV1::PostingRead)
    );
    assert_eq!(cancelled.accounting.posting_rows_examined, 1);
}

#[test]
fn selected_block_finds_both_containment_directions_without_indexing_subtrees() {
    let inner = (0..14)
        .map(|ordinal| format!("inner_{ordinal}(); "))
        .collect::<String>();
    let selected = format!("selected_before(); {{ {inner} }} selected_after();");
    let containing_before = (0..80)
        .map(|ordinal| format!("before_edge_{ordinal}(); "))
        .collect::<String>();
    let containing_after = (0..80)
        .map(|ordinal| format!("after_edge_{ordinal}(); "))
        .collect::<String>();
    let fixture = real_lexical_source_fixture_from_sources(vec![
        (
            "file.clone.block.contained".to_owned(),
            "src/contained.ts".to_owned(),
            format!("export function contained() {{ {inner} }}").into_bytes(),
        ),
        (
            "file.clone.block.containing".to_owned(),
            "src/containing.ts".to_owned(),
            format!(
                "export function containing() {{ {containing_before} {{ {selected} }} {containing_after} }}"
            )
            .into_bytes(),
        ),
        (
            "file.clone.block.equal".to_owned(),
            "src/equal.ts".to_owned(),
            format!("export function equal() {{ {selected} }}").into_bytes(),
        ),
        (
            "file.clone.block.source".to_owned(),
            "src/source.ts".to_owned(),
            format!(
                "export function source() {{ source_before(); {{ {selected} }} source_after(); }}"
            )
            .into_bytes(),
        ),
    ]);
    let (directory, pages, reader) = build_clone_artifact(&fixture);
    let bodies = pages
        .iter()
        .flat_map(VerifiedSealedLexicalPageV1::clone_bodies)
        .collect::<Vec<_>>();
    let source = bodies
        .iter()
        .find(|body| body.occurrence.path == "src/source.ts")
        .expect("selected-block source");
    let equal = bodies
        .iter()
        .find(|body| body.occurrence.path == "src/equal.ts")
        .expect("equal selected block");
    let source_tokens = source
        .payload
        .fingerprint_stream(source.occurrence.eligibility)
        .expect("source fingerprint stream")
        .tokens;
    let equal_tokens = equal
        .payload
        .fingerprint_stream(equal.occurrence.eligibility)
        .expect("equal fingerprint stream")
        .tokens;
    let start = source_tokens
        .windows(equal_tokens.len())
        .position(|window| window == equal_tokens)
        .expect("selected statement block in source body");
    let selection = CloneSelectedBlockV1::from_payload(
        &source.payload,
        source.occurrence.eligibility,
        start..start + equal_tokens.len(),
    )
    .expect("selected block");
    let result = reader
        .clone_selected_block_page(
            &source.occurrence,
            &source.payload,
            &selection,
            None,
            10,
            &ArtifactControl { cancelled: false },
        )
        .expect("selected-block containment");
    let classes = result
        .page
        .members
        .iter()
        .flat_map(|candidate| {
            candidate
                .occurrences
                .iter()
                .map(|occurrence| (occurrence.path.as_str(), candidate.containment))
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        classes,
        BTreeSet::from([
            (
                "src/contained.ts",
                CloneSelectedBlockContainmentClassV1::SelectedBlockContainsCandidate,
            ),
            (
                "src/containing.ts",
                CloneSelectedBlockContainmentClassV1::CandidateContainsSelectedBlock,
            ),
            ("src/equal.ts", CloneSelectedBlockContainmentClassV1::Equal,),
        ])
    );
    assert_eq!(
        result.coverage,
        RetrieverCoverage {
            examined: 1,
            eligible: 1,
            ..RetrieverCoverage::default()
        }
    );
    assert!(
        u64::from(selection.tokens().len() as u32).saturating_mul(100)
            < u64::from(
                bodies
                    .iter()
                    .find(|body| body.occurrence.path == "src/containing.ts")
                    .expect("containing body")
                    .payload
                    .token_count
            )
            .saturating_mul(70),
        "containment must exercise the ratio that whole-body matching rejects"
    );
    let connection = rusqlite::Connection::open(directory.path().join("clone-artifact-v16.sqlite"))
        .expect("inspect clone artifact");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM clone_occurrences", [], |row| row
                .get::<_, i64>(0))
            .expect("clone occurrence count"),
        4,
        "the selected block must not add a subtree occurrence"
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM clone_body_payloads", [], |row| row
                .get::<_, i64>(0))
            .expect("clone payload count"),
        4,
        "the selected block must not add a subtree payload"
    );
}

#[test]
fn fingerprint_candidate_and_posting_budgets_report_partial_coverage() {
    let shared = (0..24)
        .map(|ordinal| format!("shared_{ordinal}(); "))
        .collect::<String>();
    let mut candidate_source = String::new();
    for ordinal in 0..258 {
        writeln!(
            candidate_source,
            "export function candidate_{ordinal}() {{ {shared} unique_{ordinal}(); }}"
        )
        .expect("write candidate source");
    }
    let candidate_fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.clone.candidate-budget".to_owned(),
        "src/candidate-budget.ts".to_owned(),
        candidate_source.into_bytes(),
    )]);
    let (_candidate_directory, candidate_pages, candidate_reader) =
        build_clone_artifact(&candidate_fixture);
    let candidate_source = candidate_pages
        .iter()
        .flat_map(VerifiedSealedLexicalPageV1::clone_bodies)
        .next()
        .expect("candidate-budget source body");
    let control = ArtifactControl { cancelled: false };
    let candidate_page = candidate_reader
        .clone_fingerprint_page(
            &candidate_source.occurrence,
            &candidate_source.payload,
            None,
            256,
            &control,
        )
        .expect("candidate-budget read");
    assert_eq!(candidate_page.coverage.capped, 1);
    assert!(
        candidate_page
            .partial_reasons
            .contains(&CloneFingerprintPartialReasonV1::CandidateBodyBudget)
    );
    assert!(
        candidate_page
            .partial_reasons
            .contains(&CloneFingerprintPartialReasonV1::VerificationBodyBudget)
    );
    assert_eq!(candidate_page.accounting.candidates_admitted, 256);
    assert_eq!(candidate_page.accounting.candidate_bodies_compared, 64);

    let long_body = (0..50)
        .map(|ordinal| format!("shared_long_{ordinal}(); "))
        .collect::<String>();
    let mut posting_source = String::new();
    for ordinal in 0..1_024 {
        writeln!(
            posting_source,
            "export function posting_{ordinal}() {{ {long_body} }}"
        )
        .expect("write posting source");
    }
    let posting_fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.clone.posting-budget".to_owned(),
        "src/posting-budget.ts".to_owned(),
        posting_source.into_bytes(),
    )]);
    let (_posting_directory, posting_pages, posting_reader) =
        build_clone_artifact(&posting_fixture);
    let posting_source = posting_pages
        .iter()
        .flat_map(VerifiedSealedLexicalPageV1::clone_bodies)
        .next()
        .expect("posting-budget source body");
    let posting_page = posting_reader
        .clone_fingerprint_page(
            &posting_source.occurrence,
            &posting_source.payload,
            None,
            256,
            &control,
        )
        .expect("posting-budget read");
    assert_eq!(posting_page.coverage.capped, 1);
    assert!(
        posting_page
            .partial_reasons
            .contains(&CloneFingerprintPartialReasonV1::PostingRowBudget)
    );
    assert_eq!(posting_page.accounting.posting_rows_examined, 16_384);
    assert_eq!(posting_page.accounting.hot_postings_skipped, 0);
}

#[test]
fn fingerprint_work_budget_cursor_stays_after_the_last_completed_candidate() {
    let shared = (0..1_400)
        .map(|ordinal| format!("{ordinal},"))
        .collect::<String>();
    let source_text = (0..12)
        .map(|ordinal| {
            format!(
                "export function cursor_{ordinal}() {{ const values = [{shared}]; return values[{ordinal}]; }}\n"
            )
        })
        .collect::<String>();
    let fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.clone.cursor".to_owned(),
        "src/cursor.ts".to_owned(),
        source_text.into_bytes(),
    )]);
    let (_directory, pages, reader) = build_clone_artifact(&fixture);
    let source = pages
        .iter()
        .flat_map(VerifiedSealedLexicalPageV1::clone_bodies)
        .min_by_key(|body| body.occurrence.body_span.start_byte)
        .expect("cursor source body");
    let read = reader
        .clone_fingerprint_page(
            &source.occurrence,
            &source.payload,
            None,
            256,
            &ArtifactControl { cancelled: false },
        )
        .expect("bounded fingerprint read");

    assert!(
        read.partial_reasons
            .contains(&CloneFingerprintPartialReasonV1::VerificationWorkBudget),
        "fixture must exhaust alignment work: {:?}",
        read.accounting
    );
    let last_completed = read
        .page
        .members
        .last()
        .expect("at least one completed candidate");
    let cursor = read.page.next_cursor.expect("partial read cursor");
    let mut ordered_candidates = pages
        .iter()
        .flat_map(VerifiedSealedLexicalPageV1::clone_bodies)
        .filter(|body| {
            body.occurrence.symbol_occurrence_id != source.occurrence.symbol_occurrence_id
        })
        .collect::<Vec<_>>();
    ordered_candidates.sort_by_key(|body| {
        (
            body.payload.body_digest.clone(),
            body.payload.payload_digest.clone(),
        )
    });
    let completed_index = ordered_candidates
        .iter()
        .position(|body| body.payload.payload_digest == last_completed.payload.payload_digest)
        .expect("completed candidate is in the ordered fixture");
    let expected_resumed = ordered_candidates
        .get(completed_index + 1)
        .expect("unfinished candidate remains after the completed candidate");
    let resumed = reader
        .clone_fingerprint_page(
            &source.occurrence,
            &source.payload,
            Some(&cursor),
            256,
            &ArtifactControl { cancelled: false },
        )
        .expect("resume bounded fingerprint read");
    let first_resumed = resumed
        .page
        .members
        .first()
        .expect("resume retries the unfinished candidate");
    assert_eq!(
        first_resumed.payload.payload_digest, expected_resumed.payload.payload_digest,
        "the unfinished candidate must remain behind the continuation cursor"
    );
}

#[test]
fn hot_only_fingerprints_are_partial_while_exact_digest_reads_still_work() {
    let body = (0..24)
        .map(|ordinal| format!("hot_shared_{ordinal}(); "))
        .collect::<String>();
    let mut source_text = String::new();
    for ordinal in 0..1_026 {
        writeln!(source_text, "export function hot_{ordinal}() {{ {body} }}")
            .expect("write hot source");
    }
    let fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.clone.hot".to_owned(),
        "src/hot.ts".to_owned(),
        source_text.into_bytes(),
    )]);
    let (_directory, pages, reader) = build_clone_artifact(&fixture);
    let source = pages
        .iter()
        .flat_map(VerifiedSealedLexicalPageV1::clone_bodies)
        .next()
        .expect("hot source body");
    let control = ArtifactControl { cancelled: false };
    let fingerprints = reader
        .clone_fingerprint_page(&source.occurrence, &source.payload, None, 256, &control)
        .expect("hot fingerprint read");
    assert_eq!(fingerprints.coverage.capped, 1);
    assert_eq!(
        fingerprints.partial_reasons,
        vec![CloneFingerprintPartialReasonV1::HotPostings]
    );
    assert!(fingerprints.page.members.is_empty());
    assert!(fingerprints.accounting.hot_postings_skipped > 0);
    assert!(fingerprints.accounting.hot_posting_rows_skipped > 1_024);
    assert_eq!(fingerprints.accounting.posting_rows_examined, 0);

    let exact_key = source
        .payload
        .exact_keys(source.occurrence.eligibility)
        .into_iter()
        .find(|key| key.class == CloneNormalizationClassV1::Conservative)
        .expect("exact conservative key");
    assert_eq!(
        reader
            .clone_exact_page(&source.occurrence, &exact_key, None, 1, &control)
            .expect("exact digest read")
            .members
            .len(),
        1
    );
}

/// The lexical row scan is cooperatively cancellable on both production
/// row sources. Over a real multi-file corpus whose every chunk matches the
/// query, a request cancelled after its `k`-th control consultation unwinds
/// with the typed cancellation error and stops consulting the control at that
/// checkpoint, far short of the candidate set, while the same request under
/// an active control completes, agrees byte-for-byte between the sealed
/// artifact and the in-memory projection, and is stable across runs.
#[test]
fn lexical_scan_cancellation_unwinds_artifact_and_in_memory_sources_before_completion() {
    let fixture = real_lexical_source_fixture_with_files(24);
    let (pages, source_receipt) = drain_verified_pages(&fixture, 128);
    let metadata = fixture.metadata.clone();
    let chunks = pages
        .iter()
        .flat_map(|page| page.chunks().iter().cloned())
        .collect::<Vec<_>>();
    let in_memory = LexicalLane::new(
        CodeLexicalProjectionAdapterV1::new_admitted(
            metadata.clone(),
            chunks,
            page_symbol_displays(&pages),
        )
        .expect("in-memory lexical projection"),
    );
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("cancellable-lexical.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    let artifact = LexicalLane::new(
        CodeLexicalArtifactReaderV1::open_with_control(
            &artifact_path,
            &verified,
            &metadata,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &control,
        )
        .expect("reopen sealed artifact"),
    );

    fn widget_request<'a>(
        generation: &CodeGenerationId,
        control: &'a dyn RetrievalExecutionControl,
    ) -> LexicalLaneRequest<'a> {
        let mut request = lexical_request("widget", &["widget"], &[], &[], 0, 64);
        request.generation = generation.clone();
        request.control = control;
        request
    }
    let generation = &metadata.generation;
    let request = widget_request(generation, &ACTIVE_CONTROL);

    let artifact_complete = artifact
        .retrieve_lexical(&request)
        .expect("uncancelled artifact scan completes");
    let in_memory_complete = in_memory
        .retrieve_lexical(&request)
        .expect("uncancelled in-memory scan completes");
    assert_eq!(artifact_complete, in_memory_complete);
    let candidates = complete(artifact_complete.clone()).candidates.len();
    assert!(
        candidates >= 24,
        "every fixture file must contribute a matching row, got {candidates}"
    );
    assert_eq!(
        artifact
            .retrieve_lexical(&request)
            .expect("repeated uncancelled artifact scan"),
        artifact_complete,
        "an active control leaves the ranked result deterministic across runs"
    );

    let cancel_at = 6;
    let lanes: [(&dyn LexicalLaneRetriever, &str); 2] =
        [(&artifact, "artifact"), (&in_memory, "in-memory")];
    for (lane, source) in lanes {
        let cancelled = CancelAtObservation::new(cancel_at);
        assert_eq!(
            lane.retrieve_lexical(&widget_request(generation, &cancelled)),
            Err(RetrievalPortError::Cancelled),
            "{source}: a cancelled scan unwinds with the typed cancellation error"
        );
        assert_eq!(
            cancelled.observations(),
            cancel_at,
            "{source}: the scan stops at the cancelling checkpoint instead of visiting the \
             remaining {candidates} candidates"
        );
    }
}

#[test]
fn extracted_qualified_names_match_in_memory_and_reopened_artifacts() {
    let fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.qualified".to_owned(),
        "src/qualified.rs".to_owned(),
        b"pub struct VectorWatermark;\nimpl VectorWatermark { pub fn merge_max(&self) {} }\npub struct UnrelatedContainer;\nimpl UnrelatedContainer { pub fn merge_max(&self) {} }\n".to_vec(),
    )]);
    let generation = Arc::clone(&fixture.generation);
    let memory = generation_backed_projection(fixture.metadata.clone(), &generation);
    let directory = tempfile::tempdir().expect("artifact directory");
    let path = directory.path().join("qualified.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&path, fixture.metadata.clone())
        .expect("create artifact");
    let verified = builder
        .rebuild_and_finalize(&mut fixture.open_source(128), &control)
        .expect("build from parser-attested pages");
    drop(builder);
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &path,
        &verified,
        &fixture.metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("reopen qualified-name postings");
    for (query, expected_name) in [
        (
            "VectorWatermark::merge_max",
            Some("VectorWatermark::merge_max"),
        ),
        (
            "UnrelatedContainer::merge_max",
            Some("UnrelatedContainer::merge_max"),
        ),
        ("WrongQualifier::merge_max", None),
    ] {
        let parts = tracedecay_query::retrieval::lexical::lexical_query_parts(query)
            .expect("canonical query grammar");
        assert_eq!(parts.whole_terms, vec![query]);
        assert!(parts.subtokens.is_empty());
        let mut request = lexical_request(query, &[], &[], &[], 8, 32);
        request.generation = fixture.metadata.generation.clone();
        request.whole_terms = Cow::Owned(parts.whole_terms);
        request.subtokens = Cow::Owned(parts.subtokens);
        request.phrases = Cow::Owned(parts.phrases);
        let disk = complete(
            reader
                .read_lexical_postings(&request)
                .expect("artifact query"),
        );
        let in_memory = complete(
            memory
                .read_lexical_postings(&request)
                .expect("memory query"),
        );
        assert_eq!(disk, in_memory, "{query} must use the same search fields");
        if let Some(expected_name) = expected_name {
            assert!(!disk.candidates.is_empty(), "missing {query}");
            for candidate in &disk.candidates {
                let evidence = &disk.evidence_by_occurrence[&candidate.source_occurrence_id];
                assert!(
                    !evidence
                        .binding
                        .matched_term_kinds
                        .contains(&ExactTechnicalTermKindV1::QualifiedName),
                    "derived lexical fields must not fabricate source-exact terms"
                );
                assert!(evidence.field_scores_micros.iter().any(|(field, score)| {
                    *field == LexicalFieldV1::QualifiedName && *score > 0
                }));
                let occurrence = reader
                    .occurrence_by_chunk(
                        evidence
                            .binding
                            .occurrence
                            .chunk
                            .as_ref()
                            .expect("chunk binding"),
                    )
                    .expect("read canonical occurrence")
                    .expect("matched occurrence");
                assert_eq!(
                    occurrence.qualified_name,
                    Some(format!("src/qualified.rs::{expected_name}")),
                    "a method name alone must not match the other containing type"
                );
            }
        } else {
            assert!(
                disk.candidates.is_empty(),
                "wrong qualifier matched loose method tokens"
            );
        }
    }
}

#[test]
fn vocabulary_fields_phrase_and_proximity_match_in_memory_and_reopened_artifacts() {
    let fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.vocabulary".to_owned(),
        "src/http-cache/client-store.rs".to_owned(),
        b"pub struct CachedResponse;\n\
          /// Loads the durable cache entry for the request owner.\n\
          pub fn loadCachedResponse(retry_budget: u32) -> CachedResponse {\n\
              let _ = retry_budget;\n\
              CachedResponse\n\
          }\n"
        .to_vec(),
    )]);
    let generation = Arc::clone(&fixture.generation);
    let memory = LexicalLane::new(generation_backed_projection(
        fixture.metadata.clone(),
        &generation,
    ));
    let directory = tempfile::tempdir().expect("artifact directory");
    let path = directory.path().join("vocabulary.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&path, fixture.metadata.clone())
        .expect("create artifact");
    let verified = builder
        .rebuild_and_finalize(&mut fixture.open_source(128), &control)
        .expect("build from parser-attested pages");
    drop(builder);
    let artifact = LexicalLane::new(
        CodeLexicalArtifactReaderV1::open_with_control(
            &path,
            &verified,
            &fixture.metadata,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &control,
        )
        .expect("reopen lexical fields"),
    );

    let run = |query: &str,
               whole_terms: &[&str],
               phrases: &[&str],
               field: LexicalFieldV1,
               proximities: Vec<LexicalProximityV1>| {
        let mut request = lexical_request(query, whole_terms, &[], phrases, 0, 32);
        request.generation = fixture.metadata.generation.clone();
        request.field_filters = Cow::Owned(vec![LexicalFieldFilterV1 {
            field,
            include: true,
        }]);
        request.proximities = Cow::Owned(proximities);
        let disk = artifact
            .retrieve_lexical(&request)
            .expect("artifact lexical query");
        let in_memory = memory
            .retrieve_lexical(&request)
            .expect("in-memory lexical query");
        assert_eq!(disk, in_memory, "{query} must agree across readers");
        complete(disk)
    };

    for (query, field) in [
        ("cached", LexicalFieldV1::SymbolName),
        ("client", LexicalFieldV1::Path),
        ("budget", LexicalFieldV1::Signature),
        ("response", LexicalFieldV1::QualifiedName),
    ] {
        assert!(
            !run(query, &[query], &[], field, Vec::new())
                .candidates
                .is_empty(),
            "{query} must recover from its field vocabulary"
        );
    }
    let mut typo = lexical_request("budgt", &["budgt"], &[], &[], 1, 32);
    typo.generation = fixture.metadata.generation.clone();
    typo.field_filters = Cow::Owned(vec![LexicalFieldFilterV1 {
        field: LexicalFieldV1::Signature,
        include: true,
    }]);
    let disk_typo = artifact
        .retrieve_lexical(&typo)
        .expect("artifact typo query");
    let memory_typo = memory
        .retrieve_lexical(&typo)
        .expect("in-memory typo query");
    assert_eq!(disk_typo, memory_typo);
    let disk_typo = complete(disk_typo);
    assert_eq!(
        disk_typo.evidence_by_occurrence[&disk_typo.candidates[0].source_occurrence_id]
            .spelling_variants,
        [LexicalSpellingVariantV1 {
            query: "budgt".to_owned(),
            alternative: "budget".to_owned(),
        }]
    );
    let phrase = run(
        "durable cache",
        &[],
        &["durable cache"],
        LexicalFieldV1::Documentation,
        Vec::new(),
    );
    assert_eq!(phrase.candidates.len(), 1);

    let proximity = LexicalProximityV1 {
        terms: vec!["durable".to_owned(), "owner".to_owned()],
        maximum_gap: 5,
    };
    assert_eq!(
        run(
            "durable owner",
            &[],
            &[],
            LexicalFieldV1::Documentation,
            vec![proximity],
        )
        .candidates
        .len(),
        1
    );
    let too_narrow = LexicalProximityV1 {
        terms: vec!["durable".to_owned(), "owner".to_owned()],
        maximum_gap: 4,
    };
    assert!(
        run(
            "durable owner",
            &[],
            &[],
            LexicalFieldV1::Documentation,
            vec![too_narrow],
        )
        .candidates
        .is_empty(),
        "the maximum gap is inclusive and cannot widen"
    );
}

#[test]
fn disk_artifact_seals_one_ngram_list_per_distinct_key_without_staging() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let generation = metadata.generation.clone();
    let chunks = pages
        .iter()
        .flat_map(|page| page.chunks().iter().cloned())
        .collect::<Vec<_>>();
    let one_shot = CodeLexicalProjectionAdapterV1::new_admitted(
        metadata.clone(),
        chunks,
        page_symbol_displays(&pages),
    )
    .expect("one-shot lexical projection");
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("ngram-bitmap-shards.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create artifact");
    builder
        .append_pages(&pages, &control)
        .expect("commit one durable source batch");

    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect staging");
    let staged_ngram_tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name LIKE 'ngram_posting%' AND name != 'ngram_postings'",
            [],
            |row| row.get(0),
        )
        .expect("inspect ngram staging");
    assert_eq!(
        staged_ngram_tables, 0,
        "n-gram lists are rebuilt from the stored rows, never staged per batch"
    );
    drop(connection);

    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect ngram lists");
    let (stored_rows, distinct_keys, postings): (i64, i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT printf('%d:%d', kind, ngram)), SUM(document_frequency) FROM ngram_postings",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("count sealed ngram keys");
    assert!(stored_rows > 0, "the fixture must produce ngram candidates");
    assert_eq!(
        stored_rows, distinct_keys,
        "one sealed list per distinct (kind, ngram), not one row per matching document"
    );
    assert!(postings > stored_rows, "lists hold several documents each");
    drop(connection);
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        &metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("open finalized bitmap artifact");
    let mut request = lexical_request(
        "rendre return value",
        &["rendre"],
        &[],
        &["return value"],
        2,
        8,
    );
    request.generation = generation;
    assert_eq!(
        LexicalLane::new(reader)
            .retrieve_lexical(&request)
            .expect("bitmap artifact lexical query"),
        LexicalLane::new(one_shot)
            .retrieve_lexical(&request)
            .expect("one-shot lexical query")
    );
}

#[test]
fn disk_artifact_base_receipts_are_independent_of_commit_batch_width() {
    let (fixture, pages, source_receipt) = real_verified_pages_with_maximum_page_chunks(1);
    assert!(pages.len() > 1, "fixture must span multiple source pages");
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let one_page_path = directory.path().join("one-page-receipts.sqlite");
    let batched_path = directory.path().join("batched-receipts.sqlite");
    let control = ArtifactControl { cancelled: false };

    let mut one_page =
        CodeLexicalArtifactBuilderV1::create(&one_page_path, fixture.metadata.clone())
            .expect("create one-page artifact");
    for page in &pages {
        one_page
            .append_page(page, &control)
            .expect("commit one source page");
    }

    let mut batched = CodeLexicalArtifactBuilderV1::create(&batched_path, fixture.metadata)
        .expect("create batched artifact");
    batched
        .append_pages(&pages, &control)
        .expect("commit one multi-page batch");

    let one_page_receipts = stored_base_section_receipts(&one_page_path);
    let batched_receipts = stored_base_section_receipts(&batched_path);
    assert_eq!(one_page_receipts.len(), pages.len());
    assert_eq!(one_page_receipts, batched_receipts);

    let one_page_verified = finish_staged_artifact(&mut one_page, &source_receipt, &control);
    let batched_verified = finish_staged_artifact(&mut batched, &source_receipt, &control);
    assert_eq!(one_page_verified, batched_verified);
}

#[test]
fn content_addressed_reader_rejects_atomic_same_size_replacement() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("content-addressed.sqlite");
    let replacement_path = directory.path().join("replacement.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata.clone())
        .expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    let original_bytes = std::fs::read(&artifact_path).expect("read verified artifact bytes");
    std::fs::copy(&artifact_path, &replacement_path).expect("copy replacement artifact");
    let replacement = rusqlite::Connection::open(&replacement_path)
        .expect("open replacement artifact for header-only mutation");
    replacement
        .pragma_update(None, "user_version", 42i64)
        .expect("change only replacement SQLite header");
    drop(replacement);
    let replacement_bytes =
        std::fs::read(&replacement_path).expect("read replacement artifact bytes");
    assert_eq!(replacement_bytes.len(), original_bytes.len());
    assert_ne!(
        replacement_bytes, original_bytes,
        "replacement must differ while retaining the durable file length"
    );
    let original_digest = ManifestDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(&original_bytes))
    ))
    .expect("original content address");
    let replacement_control =
        ReplaceArtifactAtObservation::new(artifact_path.clone(), replacement_path, 2);

    assert!(matches!(
        CodeLexicalArtifactReaderV1::open_content_addressed(
            &artifact_path,
            &original_digest,
            u64::try_from(original_bytes.len()).expect("artifact length fits u64"),
            &fixture.metadata,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &replacement_control,
        ),
        Err(CodeLexicalArtifactErrorV1::Corrupt(_))
    ));
    assert_eq!(
        verified.file_size_bytes(),
        u64::try_from(original_bytes.len()).expect("artifact length fits u64")
    );
}

/// Route identity is the opener's: two builds of one sealed content under
/// different generations, freshness, snapshots, and batch sizes seal
/// byte-identical files, either opener's reader serves its own route over
/// the same bytes, and a projection whose content differs is refused.
#[test]
fn artifacts_of_identical_content_are_byte_identical_across_routes() {
    let (fixture, pages, source_receipt) = real_verified_pages_with_maximum_page_chunks(1);
    assert!(pages.len() > 3, "the fixture spans several pages");
    let control = ArtifactControl { cancelled: false };
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let mut route_b = fixture.metadata.clone();
    let generation = fixture.metadata.generation.as_str();
    route_b.generation = id(&format!(
        "{}{}",
        &generation[..generation.len() - 1],
        if generation.ends_with('0') { '1' } else { '0' }
    ));
    route_b.freshness.source_instance = id("instance.route-b");
    route_b
        .clone_route
        .as_mut()
        .expect("fixture clone route")
        .snapshot_digest = digest_id('b');
    let build = |name: &str, metadata: &CodeLexicalProjectionMetadataV1, batch: usize| {
        let path = directory.path().join(name);
        let mut builder =
            CodeLexicalArtifactBuilderV1::create(&path, metadata.clone()).expect("create");
        for batch in pages.chunks(batch) {
            builder.append_pages(batch, &control).expect("append pages");
        }
        let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
        drop(builder);
        (path, verified)
    };
    let (path_a, verified_a) = build("route-a.sqlite", &fixture.metadata, 1);
    let (path_b, verified_b) = build("route-b.sqlite", &route_b, 3);
    assert_eq!(verified_a, verified_b, "the receipt binds content only");
    assert!(
        std::fs::read(&path_a).expect("read a") == std::fs::read(&path_b).expect("read b"),
        "identical content seals byte-identical files"
    );

    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &path_a,
        &verified_a,
        &route_b,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("route b opens route a's bytes");
    let body = pages
        .iter()
        .flat_map(VerifiedSealedLexicalPageV1::clone_bodies)
        .next()
        .expect("fixture clone body");
    let served = reader
        .clone_body(&body.occurrence.symbol_occurrence_id)
        .expect("clone lookup")
        .expect("stored clone body");
    assert_eq!(served.occurrence.source_generation, route_b.generation);
    assert_eq!(served.occurrence.snapshot_digest, digest_id('b'));
    assert_eq!(served.occurrence.project_id, body.occurrence.project_id);
    assert_eq!(served.occurrence.path, body.occurrence.path);
    assert_eq!(*served.payload, *body.payload);
    let mut from_route_a = body.occurrence.clone();
    from_route_a.source_generation = fixture.metadata.generation.clone();
    assert!(matches!(
        reader.clone_fingerprint_page(&from_route_a, &body.payload, None, 1, &control),
        Err(CodeLexicalArtifactErrorV1::Missing(_))
    ));

    let mut other_content = route_b.clone();
    other_content
        .logical_paths
        .insert(id("file.route-only"), "src/route_only.rs".to_owned());
    assert!(matches!(
        CodeLexicalArtifactReaderV1::open_with_control(
            &path_a,
            &verified_a,
            &other_content,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Incompatible(_))
    ));
}

#[test]
fn reader_rejects_unsupported_open_revisions_and_accepts_current() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("open-revision.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata.clone())
        .expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        &fixture.metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("the current revision must open");

    for revision in [25i64, 27] {
        let connection =
            rusqlite::Connection::open(&artifact_path).expect("open artifact mutation");
        connection
            .execute(
                "UPDATE artifact_state SET format_revision = ?1 WHERE singleton = 1",
                [revision],
            )
            .expect("write unsupported revision");
        drop(connection);
        assert!(
            matches!(
                CodeLexicalArtifactReaderV1::open_with_control(
                    &artifact_path,
                    &verified,
                    &fixture.metadata,
                    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
                    &control,
                ),
                Err(CodeLexicalArtifactErrorV1::Incompatible(_))
            ),
            "revision {revision} must fail closed"
        );
    }
}

#[test]
fn absent_and_common_terms_match_in_memory_and_reopened_artifacts() {
    let files = 128;
    let functions_per_file = MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1 / files + 1;
    let fixture = real_lexical_source_fixture_from_sources(
        (0..files)
            .map(|file| {
                let source = (0..functions_per_file)
                    .map(|function| {
                        format!("pub fn function_{function}() {{ shared_candidate(); }}\n")
                    })
                    .collect::<String>();
                (
                    format!("file.common.{file:03}"),
                    format!("src/common_{file:03}.rs"),
                    source.into_bytes(),
                )
            })
            .collect(),
    );
    let generation = Arc::clone(&fixture.generation);
    let memory = LexicalLane::new(generation_backed_projection(
        fixture.metadata.clone(),
        &generation,
    ));
    let mut common = lexical_request("shared_candidate", &["shared_candidate"], &[], &[], 0, 8);
    common.generation = fixture.metadata.generation.clone();
    let baseline = complete(memory.retrieve_lexical(&common).expect("common term query"));
    assert_eq!(baseline.candidates.len(), 8);
    assert!(
        baseline.coverage.eligible > MAX_LEXICAL_CANDIDATE_DOCUMENTS_V1 as u64,
        "fixture must exercise the first-source exception above the admission bound"
    );
    let mut mixed = lexical_request(
        "never_present_term shared_candidate",
        &["never_present_term", "shared_candidate"],
        &[],
        &[],
        0,
        8,
    );
    mixed.generation = fixture.metadata.generation.clone();
    let expected = complete(memory.retrieve_lexical(&mixed).expect("mixed term query"));
    assert_eq!(expected.candidates, baseline.candidates);
    assert_eq!(expected.coverage.eligible, baseline.coverage.eligible);

    let directory = tempfile::tempdir().expect("artifact directory");
    let control = ArtifactControl { cancelled: false };
    let path = directory.path().join("common.sqlite");
    let mut builder = CodeLexicalArtifactBuilderV1::create(&path, fixture.metadata.clone())
        .expect("create artifact");
    let verified = builder
        .rebuild_and_finalize(&mut fixture.open_source(128), &control)
        .expect("build canonical pages");
    drop(builder);
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &path,
        &verified,
        &fixture.metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("reopen artifact");
    let actual = complete(
        LexicalLane::new(reader)
            .retrieve_lexical(&mixed)
            .expect("mixed artifact query"),
    );
    assert_eq!(
        actual, expected,
        "the artifact must preserve canonical candidate parity"
    );
}

#[test]
fn case_sensitive_quoted_literals_match_in_memory_and_reopened_artifacts() {
    let sources = [
        "pub fn fooBarValue() -> u32 { 1 }\n",
        "pub fn foobarvalue() -> u32 { 2 }\n",
        "pub struct FooBar;\nimpl FooBar { pub fn value() -> u32 { 3 } }\n",
        "pub const Q: u32 = 4;\n",
    ];
    let fixture = real_lexical_source_fixture_from_sources(
        sources
            .iter()
            .enumerate()
            .map(|(file, source)| {
                (
                    format!("file.case.{file}"),
                    format!("src/case_{file}.rs"),
                    source.as_bytes().to_vec(),
                )
            })
            .collect(),
    );
    let generation = Arc::clone(&fixture.generation);
    let memory = generation_backed_projection(fixture.metadata.clone(), &generation);
    let directory = tempfile::tempdir().expect("artifact directory");
    let control = ArtifactControl { cancelled: false };
    let path = directory.path().join("case.sqlite");
    let mut builder = CodeLexicalArtifactBuilderV1::create(&path, fixture.metadata.clone())
        .expect("create artifact");
    let verified = builder
        .rebuild_and_finalize(&mut fixture.open_source(128), &control)
        .expect("build canonical pages");
    drop(builder);
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &path,
        &verified,
        &fixture.metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("reopen artifact");
    let authority =
        || CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>("exact-rules.v1"));
    for (query, expected_matches) in [
        (r#""fooBarValue""#, true),
        (r#""FooBar""#, true),
        (r#""impl FooBar {""#, true),
        (r#""Q""#, true),
        (r#""oBa""#, true),
        (r#""foobar""#, true),
        (r#""FOOBAR""#, false),
    ] {
        let base = base_request(query, 8);
        let view = query_view(query);
        let request = ExactLaneRequest {
            control: &ACTIVE_CONTROL,
            literals: authority().parse_literals(&view, &base),
            base,
            query_view: &view,
            generation: fixture.metadata.generation.clone(),
            budget: budget(8),
        };
        let expected = memory
            .exact_adapter(authority())
            .read_exact_postings(&request)
            .expect("in-memory exact query");
        let RetrieverOutcome::Complete(batch) = &expected else {
            panic!("in-memory exact query must complete");
        };
        assert_eq!(!batch.candidates.is_empty(), expected_matches, "{query}");
        assert_eq!(
            reader
                .exact_adapter(authority())
                .read_exact_postings(&request)
                .expect("artifact exact query"),
            expected,
            "{query}: the artifact must admit every case-sensitive raw match"
        );
    }
}

#[test]
fn annotation_uses_mint_no_lexical_documents_in_either_projection() {
    let fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.annotated.001".to_owned(),
        "src/annotated.rs".to_owned(),
        b"#[derive(Debug, Clone)]\npub struct AnnotatedProbe;\n\n#[inline]\n#[must_use]\npub fn annotated_probe() -> u32 {\n    1\n}\n"
            .to_vec(),
    )]);
    let (pages, _) = drain_verified_pages(&fixture, 128);
    let source_chunks: usize = pages.iter().map(|page| page.chunks().len()).sum();
    let annotation_chunks = pages
        .iter()
        .flat_map(|page| page.symbol_displays())
        .flatten()
        .filter(|display| display.kind() == "annotation_usage")
        .count();
    assert!(
        annotation_chunks > 0,
        "the fixture's attributes must reach the lexical source as annotation-use chunks"
    );

    let generation = Arc::clone(&fixture.generation);
    let memory = LexicalLane::new(generation_backed_projection(
        fixture.metadata.clone(),
        &generation,
    ));
    let directory = tempfile::tempdir().expect("artifact directory");
    let path = directory.path().join("annotated.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&path, fixture.metadata.clone())
        .expect("create artifact");
    let verified = builder
        .rebuild_and_finalize(&mut fixture.open_source(128), &control)
        .expect("build artifact");
    drop(builder);
    let stored_rows: i64 = rusqlite::Connection::open(&path)
        .expect("inspect artifact")
        .query_row("SELECT COUNT(*) FROM row_chunks", [], |row| row.get(0))
        .expect("count rows");
    assert_eq!(
        usize::try_from(stored_rows).expect("row count"),
        source_chunks - annotation_chunks,
        "every source chunk except annotation uses is one lexical document"
    );
    assert_eq!(verified.total_chunks() as usize, source_chunks);
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &path,
        &verified,
        &fixture.metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("open artifact");
    let artifact = LexicalLane::new(reader);
    for (query, terms) in [
        ("must_use inline", &["must_use", "inline"][..]),
        ("derive Debug", &["derive", "debug"][..]),
        ("annotated_probe", &["annotated_probe"][..]),
    ] {
        let mut request = lexical_request(query, terms, &[], &[], 0, 8);
        request.generation = fixture.metadata.generation.clone();
        let expected = complete(memory.retrieve_lexical(&request).expect("memory query"));
        let actual = complete(artifact.retrieve_lexical(&request).expect("artifact query"));
        assert_eq!(
            actual, expected,
            "{query}: both projections admit the same documents"
        );
        assert!(
            !expected.candidates.is_empty(),
            "{query}: attribute text stays searchable through the item it annotates"
        );
        assert_eq!(
            expected.coverage.examined,
            (source_chunks - annotation_chunks) as u64
        );
    }
}

/// Historical revision-10 artifact sealed by the pre-interning writer
/// (`tests/fixtures/lexical-artifact-v10.sqlite`). Readers refuse it as
/// incompatible, which withdraws the descriptor so the artifact is rebuilt.
#[test]
fn reader_refuses_historical_v10_writer_artifact_as_incompatible() {
    let checked_in =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lexical-artifact-v10.sqlite");
    let control = ArtifactControl { cancelled: false };
    let on_disk_revision: i64 = rusqlite::Connection::open(&checked_in)
        .expect("inspect v10 fixture")
        .query_row(
            "SELECT format_revision FROM artifact_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("read v10 format revision");
    assert_eq!(on_disk_revision, 10);

    let directory = tempfile::tempdir().expect("private v10 reopen dir");
    let artifact_path = directory.path().join("lexical-artifact-v10.sqlite");
    std::fs::copy(&checked_in, &artifact_path).expect("copy historical v10 fixture");
    drop(
        tracedecay_private_fs::make_private_file(&artifact_path)
            .expect("restore private-file protection for content-addressed open"),
    );
    let bytes = std::fs::read(&artifact_path).expect("read historical v10 fixture");
    let file_size_bytes = u64::try_from(bytes.len()).expect("v10 fixture length");
    let digest = ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
        .expect("v10 fixture digest");

    let Err(error) = CodeLexicalArtifactReaderV1::open_content_addressed(
        &artifact_path,
        &digest,
        file_size_bytes,
        &projection_metadata(&id("generation.v10"), FreshnessCompatibilityV1::Current),
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    ) else {
        panic!("a revision-10 artifact must not be served");
    };
    assert!(
        matches!(error, CodeLexicalArtifactErrorV1::Incompatible(_)),
        "unexpected error: {error:?}"
    );
}

#[test]
fn sealed_current_artifact_uses_compact_postings_and_reports_dbstat() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("current-plans.sqlite");
    let control = ArtifactControl { cancelled: false };
    let started = Instant::now();
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata.clone())
        .expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    let build_ms = started.elapsed().as_millis();
    let file_bytes = std::fs::metadata(&artifact_path)
        .expect("artifact metadata")
        .len();
    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect sealed artifact");
    let format_revision: i64 = connection
        .query_row(
            "SELECT format_revision FROM artifact_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("read current format revision");
    assert_eq!(format_revision, 26);
    let (ngram_lists, ngram_postings, untagged_ngram_lists): (i64, i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), SUM(document_frequency), SUM(substr(documents, 1, 1) NOT IN (x'00', x'01')) FROM ngram_postings",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("inspect sealed ngram lists");
    assert!(ngram_lists > 0 && ngram_postings >= ngram_lists);
    assert_eq!(
        untagged_ngram_lists, 0,
        "every sealed ngram list is a tagged delta-varint list or bitset"
    );
    let exact_columns = connection
        .prepare(
            "SELECT name, type FROM pragma_table_xinfo('exact_postings') WHERE hidden = 0 ORDER BY cid",
        )
        .expect("prepare exact columns")
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .expect("query exact columns")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect exact columns");
    assert_eq!(
        exact_columns,
        [
            ("term_id".to_owned(), "INTEGER".to_owned()),
            ("field".to_owned(), "INTEGER".to_owned()),
            ("documents".to_owned(), "BLOB".to_owned()),
        ]
    );
    let term_plan = connection
        .prepare("EXPLAIN QUERY PLAN SELECT lists FROM term_postings WHERE term = ?1")
        .expect("prepare term plan")
        .query_map(rusqlite::params!["widget"], |row| row.get::<_, String>(3))
        .expect("query term plan")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect term plan");
    assert!(
        term_plan
            .iter()
            .any(|detail| detail.contains("USING PRIMARY KEY")),
        "a term's lists must be one clustered-key seek, got {term_plan:?}"
    );
    let redundant_structures: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE (type = 'index' AND name NOT LIKE 'sqlite_autoindex_clone_%') \
             OR (type = 'table' AND name IN ('rows', 'vocabulary', 'term_stats', 'ngram_statistics', 'document_integrity', 'import_integrity', 'term_posting_runs', 'exact_posting_runs', 'ngram_posting_pages', 'row_chunk_pages'))",
            [],
            |row| row.get(0),
        )
        .expect("count redundant structures");
    assert_eq!(
        redundant_structures, 0,
        "the current revision keeps one physical order per family, no secondary index, and no derivable tables"
    );
    let freelist_pages: i64 = connection
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .expect("read freelist");
    assert_eq!(
        freelist_pages, 0,
        "finalization returns every dropped staging page to the filesystem"
    );
    let (blocks, documents, untagged_blocks): (i64, i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM row_blocks), (SELECT COUNT(*) FROM row_chunks), \
             (SELECT COUNT(*) FROM row_blocks WHERE substr(payload, 1, 1) != x'17')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("count row blocks");
    assert_eq!(
        untagged_blocks, 0,
        "every row block carries the current tag"
    );
    assert!(
        blocks < documents && blocks * 32 >= documents,
        "rows are grouped into blocks of at most 32: {blocks} blocks for {documents} rows"
    );
    let (interned_strings, staging_tables): (i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM row_dictionary), \
             (SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = 'row_dictionary_pages')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("inspect row string dictionary");
    assert!(
        interned_strings > 0,
        "rows must reference an interned dictionary"
    );
    assert_eq!(
        staging_tables, 0,
        "the staging dictionary is dropped at finalization"
    );
    {
        let dbstat = connection.prepare(
            "SELECT name, SUM(pgsize) FROM dbstat GROUP BY name ORDER BY SUM(pgsize) DESC",
        );
        if let Ok(mut statement) = dbstat {
            let sizes: BTreeMap<String, i64> = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("query dbstat")
                .collect::<Result<_, _>>()
                .expect("read dbstat");
            assert!(
                sizes.contains_key("term_postings"),
                "dbstat must account interned postings: {sizes:?}"
            );
            assert!(
                !sizes
                    .keys()
                    .any(|name| name.starts_with("term_postings_by") || name.ends_with("_runs")),
                "dbstat must not retain a secondary posting order or staging run: {sizes:?}"
            );
            assert!(
                sizes.contains_key("exact_vocabulary"),
                "dbstat must account the exact-term collision authority: {sizes:?}"
            );
            assert!(
                sizes.contains_key("row_dictionary") && !sizes.contains_key("row_dictionary_pages"),
                "dbstat must account the sealed row dictionary and no staging table: {sizes:?}"
            );
            eprintln!(
                "lexical v23 dbstat file_bytes={file_bytes} build_ms={build_ms} pages={} digest={} sizes={sizes:?}",
                verified.page_count(),
                verified.artifact_digest().as_str(),
            );
        } else {
            eprintln!(
                "lexical v23 size file_bytes={file_bytes} build_ms={build_ms} pages={} digest={} (dbstat unavailable)",
                verified.page_count(),
                verified.artifact_digest().as_str(),
            );
        }
    }
    drop(connection);

    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        &fixture.metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("open current artifact");
    let mut request = lexical_request(
        "rendre return value",
        &["rendre"],
        &[],
        &["return value"],
        2,
        8,
    );
    request.generation = reader.metadata().generation.clone();
    let lane = LexicalLane::new(reader);
    let mut latencies = Vec::new();
    for _ in 0..16 {
        let started = Instant::now();
        let _ = lane
            .retrieve_lexical(&request)
            .expect("current lexical query");
        latencies.push(started.elapsed().as_micros());
    }
    latencies.sort_unstable();
    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[(latencies.len() * 95) / 100];
    eprintln!("lexical v19 query_us p50={p50} p95={p95} samples={latencies:?}");
    assert!(p50 > 0 || file_bytes > 0);
}

#[test]
fn reader_rejects_current_artifact_missing_its_chunk_lookup_table() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("missing-chunk-lookup-index.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata.clone())
        .expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    let connection = rusqlite::Connection::open(&artifact_path).expect("open artifact mutation");
    connection
        .execute_batch("DROP TABLE row_chunks;")
        .expect("remove required chunk lookup table");
    drop(connection);

    assert!(matches!(
        CodeLexicalArtifactReaderV1::open_with_control(
            &artifact_path,
            &verified,
            &fixture.metadata,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Incompatible(_))
    ));
}

#[test]
fn disk_artifact_defers_statistics_and_serving_indexes_until_freeze() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("deferred-serving-state.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata)
        .expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }

    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect staging artifact");
    let staging_indexes: Vec<String> = connection
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'index' AND name NOT LIKE 'sqlite_autoindex_%' ORDER BY name",
        )
        .expect("prepare index inventory")
        .query_map([], |row| row.get(0))
        .expect("query index inventory")
        .collect::<Result<_, _>>()
        .expect("read index inventory");
    assert_eq!(staging_indexes, Vec::<String>::new());
    for table in [
        "field_stats",
        "term_postings",
        "exact_postings",
        "ngram_postings",
        "row_chunks",
    ] {
        let rows: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count deferred statistic rows");
        assert_eq!(rows, 0, "{table} must be derived after the base freeze");
    }
    for table in [
        "term_posting_runs",
        "exact_posting_runs",
        "row_chunk_pages",
        "row_blocks",
    ] {
        let rows: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count staged rows");
        assert!(rows > 0, "{table} is written during append");
    }
    let authority_rows: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM source_pages) + \
                    (SELECT COUNT(*) FROM row_chunk_pages) + \
                    (SELECT COUNT(*) FROM import_evidence)",
            [],
            |row| row.get(0),
        )
        .expect("count authenticated authority rows");
    let epoch: i64 = connection
        .query_row(
            "SELECT epoch FROM content_epoch WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("read authenticated authority epoch");
    assert_eq!(epoch, authority_rows);
    drop(connection);

    assert!(matches!(
        builder
            .advance_finalization(&source_receipt, 1, &control)
            .expect("persist base freeze"),
        CodeLexicalArtifactFinalizationStepV1::Pending { .. }
    ));
    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect frozen artifact");
    assert!(
        connection
            .execute(
                "UPDATE row_blocks SET payload = payload WHERE first_document = (SELECT MIN(first_document) FROM row_blocks)",
                [],
            )
            .is_err(),
        "the persisted freeze must deny base-row mutation"
    );
    drop(connection);

    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    assert_eq!(verified.total_chunks(), source_receipt.total_chunks());
    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect sealed artifact");
    let serving_indexes: Vec<String> = connection
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'index' AND name NOT LIKE 'sqlite_autoindex_%' ORDER BY name",
        )
        .expect("prepare final index inventory")
        .query_map([], |row| row.get(0))
        .expect("query final index inventory")
        .collect::<Result<_, _>>()
        .expect("read final index inventory");
    assert_eq!(serving_indexes, Vec::<String>::new());
    // `field_stats` is sealed from running totals the append phase carried,
    // each list's document frequency and each term's fuzzy flag from the
    // merge; all must agree exactly with a fresh decode of the postings
    // they summarize.
    let field_stats_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM field_stats", [], |row| row.get(0))
        .expect("count field statistics");
    assert!(
        field_stats_rows > 0,
        "the fixture must index at least one field"
    );
    let mut decoded_field_totals = BTreeMap::<i64, i64>::new();
    let mut term_stats_divergence = 0i64;
    let mut fuzzy_flag_divergence = 0i64;
    for (in_fuzzy, lists) in connection
        .prepare("SELECT in_fuzzy, lists FROM term_postings")
        .expect("prepare sealed term lists")
        .query_map([], |row| {
            Ok((row.get::<_, bool>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .expect("read sealed term lists")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect sealed term lists")
    {
        let lists = decode_term_lists_oracle(&lists);
        // Field code 7 is the subtoken field.
        fuzzy_flag_divergence +=
            i64::from(in_fuzzy != lists.iter().any(|(field, _, _)| *field != 7));
        for (field, document_frequency, postings) in lists {
            let decoded = decode_frequency_posting_list(&postings);
            term_stats_divergence += i64::from(decoded.len() as i64 != document_frequency);
            *decoded_field_totals.entry(field).or_default() += decoded
                .iter()
                .map(|(_, frequency)| i64::from(*frequency))
                .sum::<i64>();
        }
    }
    let sealed_field_totals = connection
        .prepare("SELECT field, total_length FROM field_stats ORDER BY field")
        .expect("prepare field statistics")
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))
        .expect("read field statistics")
        .collect::<Result<BTreeMap<_, _>, _>>()
        .expect("collect field statistics");
    let field_stats_divergence = i64::from(sealed_field_totals != decoded_field_totals);
    let staging_tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ('field_stats_staging', 'row_dictionary_pages', 'row_chunk_pages', 'term_posting_runs', 'exact_posting_runs', 'ngram_posting_pages')",
            [],
            |row| row.get(0),
        )
        .expect("count leftover staging tables");
    assert_eq!(field_stats_divergence, 0);
    assert_eq!(term_stats_divergence, 0);
    assert_eq!(fuzzy_flag_divergence, 0);
    assert_eq!(staging_tables, 0, "finalization drops every staging table");
}

#[test]
fn disk_artifact_production_wake_commits_one_restartable_setwise_step() {
    let fixture = real_lexical_source_fixture_with_files(64);
    let (pages, source_receipt) = drain_verified_pages(&fixture, 128);
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("restartable-setwise-steps.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }

    // Posting merges run before the statistics that read the sealed keys.
    assert!(matches!(
        builder
            .advance_finalization(&source_receipt, 4_096, &control)
            .expect("persist base freeze"),
        CodeLexicalArtifactFinalizationStepV1::Pending { .. }
    ));
    assert_eq!(
        persisted_finalization_position(&artifact_path),
        ("indexes".to_owned(), 0)
    );
    assert!(matches!(
        builder
            .advance_finalization(&source_receipt, 4_096, &control)
            .expect("build only the chunk lookup"),
        CodeLexicalArtifactFinalizationStepV1::Pending { .. }
    ));
    assert_eq!(
        persisted_finalization_position(&artifact_path),
        ("indexes".to_owned(), 1),
        "a production-sized wake commits exactly one corpus-wide step"
    );
    drop(builder);

    let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &artifact_path,
        metadata.clone(),
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("restart after committed chunk lookup");
    let cancellation = CancelOnBackgroundObservation::new();
    assert!(matches!(
        resumed.advance_finalization(&source_receipt, 4_096, &cancellation),
        Err(CodeLexicalArtifactErrorV1::Interrupted(
            CodeIndexInterruptionV1::Cancelled
        ))
    ));
    assert_eq!(
        persisted_finalization_position(&artifact_path),
        ("indexes".to_owned(), 1),
        "cancellation inside the next SQLite statement must not advance its durable state"
    );
    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect cancelled step");
    let (chunk_lookups, term_runs, sealed_terms): (i64, i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM row_chunks), (SELECT COUNT(*) FROM term_posting_runs), \
             (SELECT COUNT(*) FROM term_postings)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("inspect committed steps");
    assert!(
        chunk_lookups > 0 && term_runs > 0 && sealed_terms == 0,
        "the prior committed step survives cancellation and the interrupted step rolls back atomically"
    );
    drop(connection);
    drop(resumed);

    let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &artifact_path,
        metadata.clone(),
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("restart after cancelled term merge");
    resumed
        .advance_finalization(&source_receipt, 4_096, &control)
        .expect("retry only the term merge");
    assert_eq!(
        persisted_finalization_position(&artifact_path),
        ("indexes".to_owned(), 2),
        "retry resumes at the interrupted step instead of replaying the frozen prior step"
    );
    drop(resumed);

    // The remaining index steps (exact merge, n-gram rebuild from rows), then
    // the two statistics steps (field totals, releasing every dropped
    // staging page), each committed by exactly one restarted wake. No step
    // builds a secondary index.
    let expected_positions = [
        ("indexes", 3, 0),
        ("statistics", 0, 0),
        ("statistics", 1, 0),
        ("digest", 0, 0),
    ];
    for (phase, ordinal, expected_indexes) in expected_positions {
        let mut resumed =
            CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
                &artifact_path,
                metadata.clone(),
                CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
                &control,
            )
            .expect("restart between corpus-wide steps");
        resumed
            .advance_finalization(&source_receipt, 4_096, &control)
            .expect("advance one corpus-wide step");
        drop(resumed);
        assert_eq!(
            persisted_finalization_position(&artifact_path),
            (phase.to_owned(), ordinal)
        );
        let connection =
            rusqlite::Connection::open(&artifact_path).expect("inspect serving indexes");
        let indexes: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name NOT LIKE 'sqlite_autoindex_%'",
                [],
                |row| row.get(0),
            )
            .expect("count committed serving indexes");
        assert_eq!(
            indexes, expected_indexes,
            "each restarted production wake commits at most one serving index"
        );
        if (phase, ordinal) == ("digest", 0) {
            let (sealed_lists, staging_tables, freelist): (i64, i64, i64) = connection
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM term_postings) + (SELECT COUNT(*) FROM exact_postings) + (SELECT COUNT(*) FROM ngram_postings), \
                            (SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ('term_posting_runs', 'exact_posting_runs', 'ngram_posting_pages')), \
                            (SELECT freelist_count FROM pragma_freelist_count)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .expect("inspect merged postings");
            assert!(
                sealed_lists > 0,
                "the index phase merges committed staging into sealed lists"
            );
            assert_eq!(staging_tables, 0, "every merged staging table is dropped");
            assert_eq!(
                freelist, 0,
                "the final pre-digest wake releases their pages"
            );
        }
        if (phase, ordinal) == ("digest", 0) {
            let fuzzy_terms: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM term_postings WHERE in_fuzzy = 1",
                    [],
                    |row| row.get(0),
                )
                .expect("count derived fuzzy vocabulary");
            assert!(
                fuzzy_terms > 0,
                "the term merge derives the fuzzy vocabulary from the sealed term lists"
            );
        }
    }
}

/// Independent oracle for the sealed term-list format: LEB128 varints, each
/// document delta shifted left one bit whose low bit announces a following
/// frequency varint (frequency one otherwise).
fn decode_frequency_posting_list(mut encoded: &[u8]) -> Vec<(u32, u32)> {
    fn take(encoded: &mut &[u8]) -> u64 {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let (byte, rest) = encoded.split_first().expect("truncated posting varint");
            *encoded = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return value;
            }
            shift += 7;
        }
    }
    let mut postings = Vec::new();
    let mut previous: Option<u32> = None;
    while !encoded.is_empty() {
        let token = take(&mut encoded);
        let frequency = if token & 1 == 1 {
            u32::try_from(take(&mut encoded)).expect("frequency fits u32")
        } else {
            1
        };
        let delta = u32::try_from(token >> 1).expect("delta fits u32");
        let document = previous.map_or(delta, |previous| previous + delta);
        postings.push((document, frequency));
        previous = Some(document);
    }
    postings
}

/// Independent oracle for one sealed `term_postings.lists` value: per field,
/// LEB128 field code, document frequency, and length, then the list bytes.
fn decode_term_lists_oracle(mut encoded: &[u8]) -> Vec<(i64, i64, Vec<u8>)> {
    fn take(encoded: &mut &[u8]) -> u64 {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let (byte, rest) = encoded.split_first().expect("truncated term-list varint");
            *encoded = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return value;
            }
            shift += 7;
        }
    }
    let mut lists = Vec::new();
    while !encoded.is_empty() {
        let field = i64::try_from(take(&mut encoded)).expect("field code");
        let document_frequency = i64::try_from(take(&mut encoded)).expect("document frequency");
        let length = usize::try_from(take(&mut encoded)).expect("list length");
        let (list, rest) = encoded.split_at(length);
        encoded = rest;
        lists.push((field, document_frequency, list.to_vec()));
    }
    lists
}

/// Postings staged in one run table, counted by decoding every run with the
/// independent format oracle.
fn staged_posting_count(path: &Path, table: &str, frequencies: bool) -> usize {
    let connection = rusqlite::Connection::open(path).expect("open staged runs");
    let column = if frequencies { "postings" } else { "documents" };
    connection
        .prepare(&format!("SELECT {column} FROM {table}"))
        .expect("prepare staged runs")
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .expect("read staged runs")
        .map(|run| {
            let run = run.expect("staged run");
            if frequencies {
                decode_frequency_posting_list(&run).len()
            } else {
                decode_document_list(&run).len()
            }
        })
        .sum()
}

/// Independent oracle for sealed document sets without frequencies: LEB128
/// document deltas, the first absolute.
fn decode_document_list(mut encoded: &[u8]) -> Vec<u32> {
    let mut documents = Vec::new();
    let mut previous: Option<u32> = None;
    while !encoded.is_empty() {
        let mut delta = 0u64;
        let mut shift = 0;
        loop {
            let (byte, rest) = encoded.split_first().expect("truncated document varint");
            encoded = rest;
            delta |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let delta = u32::try_from(delta).expect("delta fits u32");
        let document = previous.map_or(delta, |previous| previous + delta);
        documents.push(document);
        previous = Some(document);
    }
    documents
}

fn persisted_finalization_position(path: &Path) -> (String, u64) {
    let connection = rusqlite::Connection::open(path).expect("open finalization state");
    let state: Vec<u8> = connection
        .query_row(
            "SELECT state FROM finalization_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("read finalization state");
    let state: serde_json::Value =
        serde_json::from_slice(&state).expect("decode finalization state");
    let phase = state["phase"]
        .as_str()
        .expect("finalization phase")
        .to_owned();
    let ordinal = state["section_ordinal"]
        .as_u64()
        .expect("finalization section ordinal");
    (phase, ordinal)
}

#[test]
fn disk_artifact_admission_keeps_real_pages_wide_until_the_actual_limit() {
    let (fixture, pages, _) = real_verified_pages_with_maximum_page_chunks(1);
    assert!(
        pages.len() >= 3,
        "parser-backed fixture must expose a three-page boundary"
    );
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let probe = CodeLexicalArtifactBuilderV1::create(
        directory.path().join("wide-prefix-probe.sqlite"),
        fixture.metadata.clone(),
    )
    .expect("create admission probe");
    let two_page_charge = probe
        .page_batch_ledger_charge_bytes(&pages[..2])
        .expect("measure two-page charge");
    let three_page_charge = probe
        .page_batch_ledger_charge_bytes(&pages[..3])
        .expect("measure three-page charge");
    let exact_budget = probe
        .fixed_ledger_charge_bytes()
        .checked_add(two_page_charge)
        .expect("exact two-page budget");
    assert!(
        probe.fixed_ledger_charge_bytes() + three_page_charge > exact_budget,
        "the third real page must be the actual memory authority boundary"
    );
    drop(probe);

    let artifact_path = directory.path().join("wide-prefix.sqlite");
    let mut builder = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        &artifact_path,
        fixture.metadata,
        exact_budget,
    )
    .expect("create exactly bounded builder");
    let selected = builder
        .largest_admissible_page_prefix(&pages)
        .expect("select real parser-backed prefix");
    assert_eq!(
        selected, 2,
        "all-limit preflight must preserve a two-page batch and stop at its real third-page bound"
    );
    let progress = builder
        .append_pages(&pages[..selected], &ArtifactControl { cancelled: false })
        .expect("the selected multi-page prefix must pass exact post-preparation admission");
    assert_eq!(progress.next_page_ordinal, 2);
}

#[test]
fn disk_artifact_term_insert_execution_is_monotone_by_primary_key() {
    let (fixture, pages, _) = real_verified_pages();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("term-insert-order.sqlite");
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata)
        .expect("create artifact");
    let trace = rusqlite::Connection::open(&artifact_path).expect("open term insert observer");
    trace
        .execute_batch(
            "CREATE TABLE term_insert_trace (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                page_ordinal INTEGER NOT NULL,
                term TEXT NOT NULL,
                field INTEGER NOT NULL
            );
            CREATE TRIGGER trace_term_insert AFTER INSERT ON term_posting_runs BEGIN
                INSERT INTO term_insert_trace(page_ordinal, term, field)
                VALUES (NEW.page_ordinal, NEW.term, NEW.field);
            END;",
        )
        .expect("install term insert observer");
    drop(trace);

    builder
        .append_pages(&pages, &ArtifactControl { cancelled: false })
        .expect("append observed term postings");
    let trace = rusqlite::Connection::open(&artifact_path).expect("read term insert observer");
    // Batches stage `term_posting_runs` keyed `(page_ordinal, term, field)`,
    // so that is the order a monotone insert stream must follow.
    let keys = trace
        .prepare("SELECT page_ordinal, term, field FROM term_insert_trace ORDER BY sequence")
        .expect("prepare term insert trace")
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .expect("query term insert trace")
        .collect::<Result<Vec<_>, _>>()
        .expect("read term insert trace");
    assert!(
        keys.len() > 1,
        "fixture must emit multiple term posting runs"
    );
    let resets = keys.windows(2).filter(|pair| pair[1] < pair[0]).count();
    assert_eq!(
        resets, 0,
        "term INSERT execution must follow the WITHOUT ROWID primary key"
    );
}

#[test]
fn disk_artifact_posting_insert_plans_obey_exact_memory_boundary_before_mutation() {
    const TERM_INSERT_PLAN_BYTES_PER_REF: usize = 5 * std::mem::size_of::<usize>();
    const TERM_INSERT_SORT_RUN_ROWS: usize = 4_096;
    const EXACT_INSERT_PLAN_BYTES_PER_REF: usize = 8 * std::mem::size_of::<usize>();
    const EXACT_INSERT_SORT_RUN_ROWS: usize = TERM_INSERT_SORT_RUN_ROWS;

    let (fixture, pages, _) = real_verified_pages();
    let pages = &pages[..1];
    let metadata = fixture.metadata;
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let probe_path = directory.path().join("posting-plans-probe.sqlite");
    let mut probe = CodeLexicalArtifactBuilderV1::create(&probe_path, metadata.clone())
        .expect("create posting plans probe");
    let control = ArtifactControl { cancelled: false };
    let prepared = probe
        .prepare_pages(pages, &control)
        .expect("prepare posting plans fixture");
    let prepared_ledger = prepared[0]
        .ledger_charge_bytes()
        .expect("prepared page ledger charge");
    let fixed_ledger = probe.fixed_ledger_charge_bytes();
    probe
        .append_prepared_pages(&prepared, &control)
        .expect("append posting plans probe");
    let term_rows = staged_posting_count(&probe_path, "term_posting_runs", true);
    assert!(term_rows > 0, "fixture must emit term postings");
    let exact_rows = staged_posting_count(&probe_path, "exact_posting_runs", false);
    assert!(exact_rows > 0, "fixture must emit exact postings");
    let entry_ledger = term_rows
        .checked_mul(TERM_INSERT_PLAN_BYTES_PER_REF)
        .expect("term plan ledger charge");
    let merge_heap_ledger = term_rows
        .div_ceil(TERM_INSERT_SORT_RUN_ROWS)
        .checked_mul(std::mem::size_of::<(&str, i64, i64, usize, usize, usize)>())
        .expect("term merge heap ledger charge");
    let exact_entry_ledger = exact_rows
        .checked_mul(EXACT_INSERT_PLAN_BYTES_PER_REF)
        .expect("exact plan ledger charge");
    let exact_merge_heap_ledger = exact_rows
        .div_ceil(EXACT_INSERT_SORT_RUN_ROWS)
        .checked_mul(std::mem::size_of::<(i64, i64, i64, usize, usize)>())
        .expect("exact merge heap ledger charge");
    let plan_ledger = entry_ledger
        .checked_add(merge_heap_ledger)
        .and_then(|bytes| bytes.checked_add(exact_entry_ledger))
        .and_then(|bytes| bytes.checked_add(exact_merge_heap_ledger))
        .expect("complete posting plan ledger charge");
    let exact_budget = fixed_ledger
        .checked_add(prepared_ledger)
        .and_then(|bytes| bytes.checked_add(plan_ledger))
        .expect("exact posting plans budget");
    drop(probe);

    let refused_path = directory.path().join("posting-plans-refused.sqlite");
    let mut refused = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        &refused_path,
        metadata.clone(),
        exact_budget - 1,
    )
    .expect("create one-byte-under posting plans builder");
    assert_eq!(refused.fixed_ledger_charge_bytes(), fixed_ledger);
    assert!(matches!(
        refused.append_prepared_pages(&prepared, &control),
        Err(CodeLexicalArtifactErrorV1::BatchTooLarge {
            limit: CodeLexicalArtifactBatchLimitV1::Memory,
            required,
            maximum,
        }) if required == exact_budget && maximum == exact_budget - 1
    ));
    assert_eq!(
        refused
            .progress()
            .expect("progress after posting plans refusal")
            .next_page_ordinal,
        0
    );
    assert_eq!(staged_row_cardinality(&refused_path), (0, 0));
    assert_eq!(
        staged_posting_count(&refused_path, "term_posting_runs", true),
        0
    );
    let refused_exact_rows = staged_posting_count(&refused_path, "exact_posting_runs", false);
    assert_eq!(
        refused_exact_rows, 0,
        "memory refusal must not write exact postings"
    );
    drop(refused);

    let interrupted_path = directory.path().join("posting-plans-interrupted.sqlite");
    let mut interrupted = CodeLexicalArtifactBuilderV1::create(&interrupted_path, metadata.clone())
        .expect("create interrupted posting plans builder");
    let documents = usize::try_from(prepared[0].chunk_count()).expect("prepared document count");
    // Append entry + plan entry + both page/document passes + checkpoints
    // before and after the single bounded run + the post-run checkpoint.
    let post_sort_observation = documents
        .checked_mul(2)
        .and_then(|observations| observations.checked_add(7))
        .expect("post-sort observation");
    let cancellation = CancelAtObservation::new(post_sort_observation);
    assert!(matches!(
        interrupted.append_prepared_pages(&prepared, &cancellation),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));
    assert_eq!(
        interrupted
            .progress()
            .expect("progress after posting plans interruption")
            .next_page_ordinal,
        0,
        "post-sort cancellation must precede transaction entry"
    );
    assert_eq!(staged_row_cardinality(&interrupted_path), (0, 0));
    drop(interrupted);
    let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &interrupted_path,
        metadata.clone(),
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("resume after posting plans interruption");
    assert_eq!(
        resumed
            .append_prepared_pages(&prepared, &control)
            .expect("resume exact prepared batch")
            .next_page_ordinal,
        1
    );
    drop(resumed);

    let exact_path = directory.path().join("posting-plans-exact.sqlite");
    let mut exact = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        &exact_path,
        metadata,
        exact_budget,
    )
    .expect("create exact posting plans builder");
    let progress = exact
        .append_prepared_pages(&prepared, &control)
        .expect("accept exact posting plans boundary");
    assert_eq!(progress.next_page_ordinal, 1);
}

#[test]
fn disk_artifact_term_run_sort_observes_cancellation_before_transaction_entry() {
    const TERM_SORT_RUN_ROWS: usize = 4_096;

    let mut source = String::with_capacity(192 * 1024);
    for ordinal in 0..768 {
        source.push_str(&format!(
            "export function ordered_symbol_{ordinal:04}(input_value: string) {{ const local_value_{ordinal:04} = input_value + 'term_{ordinal:04}'; return local_value_{ordinal:04}; }}\n"
        ));
    }
    let fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.artifact.term-runs".to_owned(),
        "src/term-runs.ts".to_owned(),
        source.into_bytes(),
    )]);
    let (pages, _) = drain_verified_pages(&fixture, 128);
    assert!(
        !pages.is_empty(),
        "term-run fixture must emit at least one lexical page"
    );
    assert!(
        pages.iter().all(|page| page.imports().is_empty()),
        "term-run fixture must reach document writes without import checkpoints"
    );
    let metadata = fixture.metadata;
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let probe_path = directory.path().join("term-run-probe.sqlite");
    let mut probe = CodeLexicalArtifactBuilderV1::create(&probe_path, metadata.clone())
        .expect("create term-run probe");
    let control = ArtifactControl { cancelled: false };
    let prepared = probe
        .prepare_pages(&pages, &control)
        .expect("prepare multi-run term batch");
    probe
        .append_prepared_pages(&prepared, &control)
        .expect("append term-run probe");
    let term_rows = staged_posting_count(&probe_path, "term_posting_runs", true);
    assert!(
        term_rows > TERM_SORT_RUN_ROWS,
        "fixture must require at least two bounded sort runs: {term_rows}"
    );
    drop(probe);

    let interrupted_path = directory.path().join("term-run-interrupted.sqlite");
    let mut interrupted = CodeLexicalArtifactBuilderV1::create(&interrupted_path, metadata.clone())
        .expect("create interrupted term-run builder");
    let page_count = prepared.len();
    let document_count = prepared.iter().try_fold(0usize, |documents, page| {
        usize::try_from(page.chunk_count())
            .ok()
            .and_then(|page_documents| documents.checked_add(page_documents))
    });
    let document_count = document_count.expect("prepared document count");
    // Entry checkpoints plus both page/document passes consume
    // 2 + 2*pages + 2*documents observations. The third later observation is
    // the checkpoint before the second bounded sort run. With one monolithic
    // sort it instead occurs after the first document row has opened SQLite's
    // DELETE-mode rollback journal, making this regression non-vacuous.
    let cancellation_observation = page_count
        .checked_mul(2)
        .and_then(|observations| {
            document_count
                .checked_mul(2)
                .and_then(|documents| observations.checked_add(documents))
        })
        .and_then(|observations| observations.checked_add(5))
        .expect("second term sort run observation");
    let cancellation =
        CancelAtObservationWithJournalProbe::new(&interrupted_path, cancellation_observation);
    assert!(matches!(
        interrupted.append_prepared_pages(&prepared, &cancellation),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));
    assert!(
        !cancellation.journal_seen(),
        "sort-scale cancellation must be observed before SQLite opens its rollback journal"
    );
    assert_eq!(
        interrupted
            .progress()
            .expect("progress after run-sort interruption")
            .next_page_ordinal,
        0
    );
    assert_eq!(staged_row_cardinality(&interrupted_path), (0, 0));
    drop(interrupted);

    let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &interrupted_path,
        metadata,
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("reopen after run-sort interruption");
    assert_eq!(
        resumed
            .append_prepared_pages(&prepared, &control)
            .expect("retry interrupted term runs")
            .next_page_ordinal,
        u64::try_from(prepared.len()).expect("prepared page count")
    );
}

#[test]
fn disk_artifact_widened_reservation_commits_high_ngram_window_atomically() {
    const PRIOR_BUILD_BUDGET_BYTES: usize = 768 * 1024 * 1024;
    const WIDENED_BUILD_BUDGET_BYTES: usize = 1536 * 1024 * 1024;
    const SOURCE_WINDOW_BYTES: usize = 64 * 1024 * 1024;
    const MAXIMUM_PREPARED_BATCH_ROWS: usize = 2_000_000;
    const MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES: usize = 256 * 1024 * 1024;

    let sources = (0..32)
        .map(|file_ordinal| {
            let mut source = String::with_capacity(128 * 1024);
            for symbol_ordinal in 0..128 {
                let mut state = u64::try_from(file_ordinal * 128 + symbol_ordinal + 1)
                    .expect("fixture seed");
                let token = (0..240)
                    .map(|_| {
                        state = state
                            .wrapping_mul(6_364_136_223_846_793_005)
                            .wrapping_add(1_442_695_040_888_963_407);
                        let alphabet_ordinal =
                            usize::try_from((state >> 32) % 26).expect("alphabet ordinal");
                        char::from(b'a' + u8::try_from(alphabet_ordinal).expect("ASCII letter"))
                    })
                    .collect::<String>();
                source.push_str(&format!(
                    "export function symbol_{file_ordinal:02}_{symbol_ordinal:03}(value: string) {{ return value + '{token}'; }}\n"
                ));
            }
            (
                format!("file.artifact.high-ngram.{file_ordinal:02}"),
                format!("src/high-ngram-{file_ordinal:02}.ts"),
                source.into_bytes(),
            )
        })
        .collect();
    let fixture = real_lexical_source_fixture_from_sources(sources);
    let (pages, _) = drain_verified_pages(&fixture, 128);
    assert!(
        pages.len() >= 32,
        "the parser-backed high-ngram corpus must expose a full 32-page source window"
    );
    let pages = &pages[..32];
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("thirty-two-page-batch.sqlite");

    let prior_path = directory.path().join("prior-reservation.sqlite");
    let mut prior = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        &prior_path,
        fixture.metadata.clone(),
        PRIOR_BUILD_BUDGET_BYTES,
    )
    .expect("create builder with the prior reservation");
    let batch_charge = prior
        .fixed_ledger_charge_bytes()
        .checked_add(
            prior
                .page_batch_ledger_charge_bytes(pages)
                .expect("measure high-ngram window"),
        )
        .expect("high-ngram window ledger charge");
    let staging_window_bytes = fixture.open_source(128).staging_window_bytes();
    let production_builder_budget = WIDENED_BUILD_BUDGET_BYTES
        .checked_sub(staging_window_bytes)
        .expect("production builder budget after source reservation");
    assert!(
        batch_charge > PRIOR_BUILD_BUDGET_BYTES,
        "the production-shaped window must reproduce the measured 768 MiB memory limit: {batch_charge}"
    );
    assert!(
        batch_charge <= production_builder_budget,
        "the same bounded window must fit after the production source reservation: batch={batch_charge}, staging={staging_window_bytes}, builder={production_builder_budget}"
    );
    assert!(
        prior
            .largest_admissible_page_prefix(pages)
            .expect("select prior reservation prefix")
            < pages.len(),
        "the prior reservation must stop before the complete high-ngram window"
    );
    assert!(matches!(
        prior.append_pages(pages, &ArtifactControl { cancelled: false }),
        Err(CodeLexicalArtifactErrorV1::BatchTooLarge {
            limit: CodeLexicalArtifactBatchLimitV1::Memory,
            required,
            maximum: PRIOR_BUILD_BUDGET_BYTES,
        }) if required == batch_charge
    ));
    assert_eq!(
        prior
            .progress()
            .expect("progress after typed reservation denial")
            .next_page_ordinal,
        0,
        "the memory denial must precede the atomic staging transaction"
    );
    drop(prior);

    assert_eq!(
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1, WIDENED_BUILD_BUDGET_BYTES,
        "the canonical reservation must cover the measured production window"
    );
    let mut builder = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        &artifact_path,
        fixture.metadata,
        production_builder_budget,
    )
    .expect("create builder after the production source reservation");
    assert_eq!(
        builder
            .largest_admissible_page_prefix(pages)
            .expect("select widened reservation prefix"),
        pages.len(),
        "the widened memory authority must admit the complete window"
    );
    let source_retained_bytes = pages
        .iter()
        .map(VerifiedSealedLexicalPageV1::retained_owned_bytes)
        .sum::<usize>();
    assert!(
        source_retained_bytes <= SOURCE_WINDOW_BYTES,
        "the fixture must remain inside the 64 MiB source window: {source_retained_bytes}"
    );
    assert!(
        pages.iter().all(|page| {
            page.retained_owned_bytes() <= CODE_LEXICAL_ARTIFACT_MAXIMUM_PAGE_RETAINED_BYTES_V1
        }),
        "every source page must remain inside its unchanged retained-byte bound"
    );
    let control = ArtifactControl { cancelled: false };
    let prepared = builder
        .prepare_pages(pages, &control)
        .expect("prepare one full production source window");
    let estimated_rows = prepared
        .iter()
        .map(|page| page.estimated_write_rows())
        .sum::<usize>();
    let estimated_write_bytes = prepared
        .iter()
        .map(|page| page.estimated_write_bytes())
        .sum::<usize>();
    assert!(
        estimated_rows <= MAXIMUM_PREPARED_BATCH_ROWS,
        "the high-ngram window must remain inside the unchanged row bound: {estimated_rows}"
    );
    assert!(
        estimated_write_bytes <= MAXIMUM_ESTIMATED_BATCH_WRITE_BYTES,
        "the high-ngram window must remain inside the unchanged write bound: {estimated_write_bytes}"
    );
    let progress = builder
        .append_prepared_pages(&prepared, &control)
        .expect("commit the complete real prefix atomically");
    assert_eq!(progress.next_page_ordinal, 32);
    assert_eq!(
        staged_row_cardinality(&artifact_path).0,
        pages
            .iter()
            .map(VerifiedSealedLexicalPageV1::chunk_count)
            .sum::<u64>(),
        "one transaction must make every page row visible together"
    );
}

#[test]
fn disk_artifact_subdivides_refused_suffix_and_resumes_exact_cursor() {
    let sources = (0..24)
        .map(|ordinal| {
            let body = if ordinal < 16 {
                "return 1;".to_owned()
            } else {
                format!(
                    "return \"{}\";",
                    (0..200)
                        .map(|n| format!("token{n:03} "))
                        .collect::<String>()
                )
            };
            (
                format!("file.subdivision.{ordinal:02}"),
                format!("src/subdivision_{ordinal:02}.ts"),
                format!("import {{ helper }} from \"dependency\";\nexport function function_{ordinal:02}() {{ {body} }}\n").into_bytes(),
            )
        })
        .collect();
    let fixture = real_lexical_source_fixture_from_sources(sources);
    let (single_pages, expected_receipt) = drain_verified_pages(&fixture, 1);
    let control = ArtifactControl { cancelled: false };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("subdivision.sqlite");
    let probe = CodeLexicalArtifactBuilderV1::create(
        directory.path().join("probe.sqlite"),
        fixture.metadata.clone(),
    )
    .unwrap();
    let budget = probe.fixed_ledger_charge_bytes()
        + single_pages
            .iter()
            .map(|page| {
                probe
                    .page_batch_ledger_charge_bytes(std::slice::from_ref(page))
                    .unwrap()
            })
            .max()
            .unwrap();
    drop(probe);
    let mut builder = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        &path,
        fixture.metadata.clone(),
        budget,
    )
    .unwrap();
    let mut source = fixture.open_source(4);
    let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(1, 64 * 1024 * 1024).unwrap();
    let mut refusals = 0;
    let mut reopened = false;
    let receipt = loop {
        let before = source.cursor().clone();
        let progress_before = builder.progress().unwrap();
        let result = source
            .next_page_batch_if(&control, bounds, |pages| {
                let prepared = builder.prepare_admissible_page_prefix(pages, &control)?;
                let accepted = prepared.accepted_prefix();
                builder.append_prepared_pages(prepared.prepared_pages(), &control)?;
                Ok(accepted)
            })
            .unwrap();
        match result {
            Err(CodeLexicalArtifactErrorV1::BatchTooLarge { .. }) => {
                assert!(
                    before.emitted_chunks() > 0,
                    "real builder must accept a prefix before the larger suffix refuses"
                );
                assert_eq!(source.cursor(), &before);
                assert_eq!(builder.progress().unwrap(), progress_before);
                refusals += 1;
                assert!(refusals <= 2, "four chunks need at most two subdivisions");
                assert!(source.tighten_page_record_bound().is_some());
                // Cancellation cannot consume the newly subdivided suffix.
                assert!(matches!(
                    source.next_page_batch_if(
                        &ArtifactControl { cancelled: true },
                        bounds,
                        |_| -> Result<NonZeroUsize, CodeLexicalArtifactErrorV1> {
                            panic!("cancelled source must not call builder")
                        }
                    ),
                    Err(CodeIndexProductionErrorV1::Interrupted(_))
                ));
                assert_eq!(source.cursor(), &before);
            }
            Err(error) => panic!("unexpected builder refusal: {error}"),
            Ok(VerifiedSealedLexicalPageBatchReadV1::Pages(pages)) => {
                assert_eq!(pages[0].page_ordinal(), before.next_page_ordinal());
                assert_eq!(
                    builder.progress().unwrap().next_cursor.as_ref(),
                    Some(source.cursor())
                );
                if refusals > 0 && !reopened {
                    let cursor = builder.progress().unwrap().next_cursor.unwrap();
                    let persisted = cursor.persisted_bytes().unwrap();
                    drop(builder);
                    builder = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
                        &path, fixture.metadata.clone(), budget, &control).unwrap();
                    source = fixture.open_source(1);
                    source
                        .restore_cursor(
                            &VerifiedSealedLexicalCursorV1::restore_persisted(&persisted).unwrap(),
                            &control,
                        )
                        .unwrap();
                    assert_eq!(
                        builder.progress().unwrap().next_cursor.as_ref(),
                        Some(source.cursor())
                    );
                    reopened = true;
                }
            }
            Ok(VerifiedSealedLexicalPageBatchReadV1::Complete(receipt)) => break receipt,
        }
    };
    assert!(
        refusals > 0 && reopened,
        "must exercise actual refusal and persisted recovery"
    );
    assert_eq!(receipt.total_chunks(), expected_receipt.total_chunks());
    let expected_cursor = single_pages.last().unwrap().next_cursor();
    assert!(
        expected_cursor.emitted_imports() > 0,
        "fixture must authenticate a nonempty import dictionary"
    );
    assert_eq!(
        source.cursor().cumulative_digest(),
        expected_cursor.cumulative_digest()
    );
    assert_eq!(
        source.cursor().import_dictionary_digest(),
        expected_cursor.import_dictionary_digest()
    );
    let (rows, distinct) = staged_row_cardinality(&path);
    assert_eq!(
        (rows, distinct),
        (receipt.total_chunks(), receipt.total_chunks())
    );
    finish_staged_artifact(&mut builder, &receipt, &control);
}

#[test]
fn disk_artifact_subdivides_import_only_suffix_without_replaying_chunks() {
    let mut text = (0..12)
        .map(|n| format!("import {{ helper{n} }} from \"dependency{n}\";\n"))
        .collect::<String>();
    text.push_str("export const imported = helper0;\n");
    let fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.imports".to_owned(),
        "src/imports.ts".to_owned(),
        text.into_bytes(),
    )]);
    let (single_pages, expected_receipt) = drain_verified_pages(&fixture, 1);
    let (wide_pages, _) = drain_verified_pages(&fixture, 4);
    let prefix_len = wide_pages
        .iter()
        .position(|page| page.chunk_count() == 0 && page.import_count() > 1)
        .expect("divisible import-only suffix");
    assert!(prefix_len > 0);
    let control = ArtifactControl { cancelled: false };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("import-subdivision.sqlite");
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&path, fixture.metadata.clone()).unwrap();
    for page in &wide_pages[..prefix_len] {
        builder.append_page(page, &control).unwrap();
    }
    let budget = builder.fixed_ledger_charge_bytes()
        + single_pages
            .iter()
            .filter(|page| page.chunk_count() == 0)
            .map(|page| {
                assert_eq!(page.import_count(), 1);
                builder
                    .page_batch_ledger_charge_bytes(std::slice::from_ref(page))
                    .unwrap()
            })
            .max()
            .expect("one-import page charges");
    let cursor = builder.progress().unwrap().next_cursor.unwrap();
    drop(builder);
    let mut builder = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &path,
        fixture.metadata.clone(),
        budget,
        &control,
    )
    .unwrap();
    let mut source = fixture.open_source(4);
    source.restore_cursor(&cursor, &control).unwrap();
    let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(1, 64 * 1024 * 1024).unwrap();
    let mut refusals = 0;
    let receipt = loop {
        let before = source.cursor().clone();
        let progress = builder.progress().unwrap();
        let result = source
            .next_page_batch_if(&control, bounds, |pages| {
                assert!(pages.iter().all(|page| page.chunk_count() == 0));
                let prepared = builder.prepare_admissible_page_prefix(pages, &control)?;
                let accepted = prepared.accepted_prefix();
                builder.append_prepared_pages(prepared.prepared_pages(), &control)?;
                Ok(accepted)
            })
            .unwrap();
        match result {
            Err(CodeLexicalArtifactErrorV1::BatchTooLarge { .. }) => {
                refusals += 1;
                assert_eq!(source.cursor(), &before);
                assert_eq!(builder.progress().unwrap(), progress);
                assert!(source.tighten_page_record_bound().is_some());
            }
            Err(error) => panic!("unexpected import refusal: {error}"),
            Ok(VerifiedSealedLexicalPageBatchReadV1::Pages(_)) => {
                assert_eq!(source.cursor().emitted_chunks(), cursor.emitted_chunks());
            }
            Ok(VerifiedSealedLexicalPageBatchReadV1::Complete(receipt)) => break receipt,
        }
    };
    assert!(refusals > 0);
    assert_eq!(receipt.total_chunks(), expected_receipt.total_chunks());
    let expected = single_pages.last().unwrap().next_cursor();
    assert_eq!(
        source.cursor().cumulative_digest(),
        expected.cumulative_digest()
    );
    assert_eq!(
        source.cursor().import_dictionary_digest(),
        expected.import_dictionary_digest()
    );
    assert_eq!(
        source.cursor().emitted_imports(),
        expected.emitted_imports()
    );
    finish_staged_artifact(&mut builder, &receipt, &control);
}

#[test]
fn disk_artifact_indivisible_refusal_keeps_source_and_builder_progress() {
    let fixture = real_lexical_source_fixture();
    let control = ArtifactControl { cancelled: false };
    let directory = tempfile::tempdir().unwrap();
    let probe = CodeLexicalArtifactBuilderV1::create(
        directory.path().join("probe.sqlite"),
        fixture.metadata.clone(),
    )
    .unwrap();
    let budget = probe.fixed_ledger_charge_bytes() + 1;
    drop(probe);
    let builder = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        directory.path().join("indivisible.sqlite"),
        fixture.metadata.clone(),
        budget,
    )
    .unwrap();
    let mut source = fixture.open_source(1);
    let before = source.cursor().clone();
    let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(1, 64 * 1024 * 1024).unwrap();
    let refusal = source
        .next_page_batch_if(&control, bounds, |pages| {
            builder
                .prepare_admissible_page_prefix(pages, &control)
                .map(|prepared| prepared.accepted_prefix())
        })
        .unwrap();
    assert!(matches!(
        refusal,
        Err(CodeLexicalArtifactErrorV1::BatchTooLarge { .. })
    ));
    assert_eq!(source.tighten_page_record_bound(), None);
    assert_eq!(source.cursor(), &before);
    assert_eq!(builder.progress().unwrap().next_page_ordinal, 0);
}

#[test]
fn disk_artifact_repetitive_multi_chunk_page_makes_exact_prefix_progress() {
    let mut source = String::with_capacity(700_000);
    source.push_str("// ");
    source.push_str(&"a".repeat(650_000));
    source.push_str(
        "\nexport function first() { return 1; }\nexport function second() { return 2; }\n",
    );
    let fixture = real_lexical_source_fixture_from_sources(vec![(
        "file.artifact.repetitive".to_owned(),
        "src/repetitive.ts".to_owned(),
        source.into_bytes(),
    )]);
    let (pages, _) = drain_verified_pages(&fixture, 128);
    let page = pages.first().expect("repetitive source page");
    assert!(
        page.chunks().len() > 1,
        "the real parser-backed page must cover multiple chunks"
    );
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("repetitive-prefix.sqlite");
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata)
        .expect("create artifact");
    let prepared = builder
        .prepare_admissible_page_prefix(
            std::slice::from_ref(page),
            &ArtifactControl { cancelled: false },
        )
        .expect("select repetitive page prefix");
    assert_eq!(
        prepared.accepted_prefix().get(),
        1,
        "a conservative pre-dedup estimate must not turn one valid page into a permanent zero prefix"
    );
    let progress = builder
        .append_prepared_pages(
            prepared.prepared_pages(),
            &ArtifactControl { cancelled: false },
        )
        .expect("exactly prepared repetitive page must commit inside every canonical limit");
    assert_eq!(progress.next_page_ordinal, 1);
}

#[test]
fn disk_artifact_finalization_resumes_after_restart_without_source_replay() {
    let (mut fixture, pages, source_receipt) = real_verified_pages_with_maximum_page_chunks(1);
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("resumable-finalization.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create");
    for page in &pages {
        builder
            .append_page(page, &control)
            .expect("stage source page");
    }
    let staged = builder.progress().expect("staged source progress");

    assert!(matches!(
        builder
            .advance_finalization(&source_receipt, 1, &control)
            .expect("first bounded finalization step"),
        CodeLexicalArtifactFinalizationStepV1::Pending { .. }
    ));
    drop(builder);

    // The restart must continue from SQLite state. Clearing this fixture's
    // only raw copy makes a source replay impossible in the finalization path.
    fixture.manifest.clear();
    fixture.segments = Arc::default();
    let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &artifact_path,
        metadata,
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("resume staged artifact");
    let interrupted = CancelAtObservation::new(3);
    assert!(matches!(
        resumed.advance_finalization(&source_receipt, 2, &interrupted),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));
    assert_eq!(
        resumed
            .progress()
            .expect("source progress after interruption"),
        staged,
        "finalization never replays or mutates staged source pages"
    );
    drop(resumed);

    let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &artifact_path,
        fixture.metadata,
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("resume interrupted finalization");
    let verified = loop {
        match resumed
            .advance_finalization(&source_receipt, 2, &control)
            .expect("bounded finalization resumes")
        {
            CodeLexicalArtifactFinalizationStepV1::Pending { .. } => {}
            CodeLexicalArtifactFinalizationStepV1::Ready(receipt) => break receipt,
        }
    };
    assert_eq!(verified.total_chunks(), source_receipt.total_chunks());
    assert_eq!(
        staged_row_cardinality(&artifact_path).0,
        source_receipt.total_chunks()
    );
}

#[test]
fn disk_artifact_controlled_reopen_cancels_receipt_scan_and_resumes() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("controlled-reopen.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create");
    for page in &pages {
        builder
            .append_page(page, &control)
            .expect("stage source page");
    }
    drop(builder);

    // The ninth checkpoint is while the fixed 16KiB all-zero receipt
    // reservation is scanned. Reopen must yield rather than treating the
    // staged artifact as available after the scheduler's epoch expires.
    let interrupted = CancelAtObservation::new(9);
    assert!(matches!(
        CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
            &artifact_path,
            metadata.clone(),
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
            &interrupted,
        ),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));

    let resumed = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &artifact_path,
        metadata,
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("resume after controlled reopen cancellation");
    let progress = resumed.progress().expect("resumed source progress");
    assert_eq!(progress.next_page_ordinal, source_receipt.page_count());
    assert_eq!(progress.completed_chunks, source_receipt.total_chunks());
    assert_eq!(
        progress.completed_payload_bytes,
        source_receipt.total_payload_bytes()
    );
    assert_eq!(progress.completed_imports, source_receipt.total_imports());
    assert_eq!(
        progress.completed_import_payload_bytes,
        source_receipt.import_payload_bytes()
    );
    assert_eq!(
        progress.import_dictionary_digest,
        Some(source_receipt.import_dictionary_digest().clone())
    );
    assert_eq!(
        progress.cumulative_source_digest,
        Some(source_receipt.cumulative_digest().clone())
    );
    assert_eq!(
        progress.next_cursor,
        pages.last().map(|page| page.next_cursor().clone()),
        "cancellable reopen must leave durable source staging untouched"
    );
}

#[test]
fn disk_artifact_revision_four_is_incompatible_before_new_index_queries() {
    let (fixture, pages, _) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("legacy-staging-schema.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create");
    builder
        .append_page(&pages[0], &control)
        .expect("stage current-format source page");
    drop(builder);

    // Revision four predates the term-leading statistics index. The declared
    // revision must reject it before resume or query code can require that
    // index by name.
    let connection = rusqlite::Connection::open(&artifact_path).expect("open legacy mutation");
    connection
        .execute(
            "UPDATE artifact_state SET format_revision = 4 WHERE singleton = 1",
            [],
        )
        .expect("write revision-four artifact state");
    drop(connection);

    assert!(matches!(
        CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
            &artifact_path,
            metadata,
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Incompatible(_))
    ));
}

#[test]
fn disk_artifact_resume_rejects_current_revision_with_wrong_chunk_lookup_shape() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("wrong-chunk-index-shape.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create");
    for page in &pages {
        builder
            .append_page(page, &control)
            .expect("stage current-format source page");
    }
    assert!(matches!(
        builder
            .advance_finalization(&source_receipt, 1, &control)
            .expect("freeze current artifact"),
        CodeLexicalArtifactFinalizationStepV1::Pending { .. }
    ));
    for _ in 0..6 {
        assert!(matches!(
            builder
                .advance_finalization(&source_receipt, 4_096, &control)
                .expect("advance one bounded pre-digest step"),
            CodeLexicalArtifactFinalizationStepV1::Pending { .. }
        ));
    }
    drop(builder);

    let connection = rusqlite::Connection::open(&artifact_path).expect("open lookup mutation");
    connection
        .execute_batch(
            "DROP TABLE row_chunks;
             CREATE TABLE row_chunks (
                document_id INTEGER NOT NULL,
                chunk_id BLOB NOT NULL,
                PRIMARY KEY(document_id, chunk_id)
             ) WITHOUT ROWID;",
        )
        .expect("replace the chunk lookup table with the wrong key order");
    drop(connection);

    assert!(matches!(
        CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
            &artifact_path,
            metadata,
            CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Incompatible(_))
    ));
}

#[test]
fn disk_artifact_finalization_refuses_inter_wake_mutation() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("inter-wake-mutation.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create");
    for page in &pages {
        builder
            .append_page(page, &control)
            .expect("stage source page");
    }
    let staged = builder.progress().expect("staged source progress");

    assert!(matches!(
        builder
            .advance_finalization(&source_receipt, 1, &control)
            .expect("start bounded finalization"),
        CodeLexicalArtifactFinalizationStepV1::Pending { .. }
    ));
    let connection = rusqlite::Connection::open(&artifact_path).expect("open artifact mutation");
    assert!(
        connection
            .execute(
                "UPDATE row_blocks SET payload = payload WHERE first_document = (SELECT MIN(first_document) FROM row_blocks)",
                [],
            )
            .is_err(),
        "the persisted freeze must reject inter-wake mutation"
    );
    drop(connection);

    let sealed = finish_staged_artifact(&mut builder, &source_receipt, &control);
    assert_eq!(
        staged.next_page_ordinal,
        sealed.page_count(),
        "a changed artifact must not self-attest through later bounded wakes"
    );
    assert_eq!(staged.completed_chunks, sealed.total_chunks());
}

#[test]
fn disk_artifact_rejects_noncanonical_receipt_reservation_tail() {
    let (fixture, pages, source_receipt) = real_verified_pages_with_maximum_page_chunks(1);
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("noncanonical-receipt.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata.clone())
        .expect("create artifact");
    for page in &pages {
        builder
            .append_page(page, &control)
            .expect("stage source page");
    }
    let verified = loop {
        match builder
            .advance_finalization(&source_receipt, 128, &control)
            .expect("finalize staged artifact")
        {
            CodeLexicalArtifactFinalizationStepV1::Pending { .. } => {}
            CodeLexicalArtifactFinalizationStepV1::Ready(receipt) => break receipt,
        }
    };
    let connection = rusqlite::Connection::open(&artifact_path).expect("open sealed artifact");
    let mut receipt: Vec<u8> = connection
        .query_row(
            "SELECT receipt FROM artifact_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("read receipt reservation");
    let tail = receipt.len().checked_sub(1).expect("reserved receipt byte");
    receipt[tail] = 1;
    connection
        .execute(
            "UPDATE artifact_state SET receipt = ?1 WHERE singleton = 1",
            [receipt],
        )
        .expect("write noncanonical receipt tail");
    drop(connection);

    assert!(matches!(
        CodeLexicalArtifactReaderV1::open_with_control(
            &artifact_path,
            &verified,
            &fixture.metadata,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Corrupt(_))
    ));
}

#[test]
fn disk_artifact_mandatory_verifier_rejects_tampered_real_source_chain() {
    let (fixture, pages, _) = real_verified_pages_with_maximum_page_chunks(1);
    let metadata = fixture.metadata.clone();
    assert!(pages.len() > 1, "fixture must emit a page transition");
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("tampered-page.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create artifact");
    assert!(matches!(
        builder.append_page(&pages[1], &control),
        Err(CodeLexicalArtifactErrorV1::Corrupt(_))
    ));
    builder
        .append_page(&pages[0], &control)
        .expect("append canonical first page");
    let connection = rusqlite::Connection::open(&artifact_path).expect("open artifact mutation");
    assert!(
        connection
            .execute(
                "UPDATE source_pages SET page_digest = ?1, cumulative_digest = ?2, import_dictionary_digest = ?3 WHERE page_ordinal = 0",
                [
                    digest_id::<ManifestDigest>('1').as_str(),
                    digest_id::<ManifestDigest>('2').as_str(),
                    digest_id::<ManifestDigest>('3').as_str(),
                ],
            )
            .is_err(),
        "source-page authority is immutable from admission"
    );
    drop(connection);
    builder
        .append_page(&pages[1], &control)
        .expect("append canonical successor after denied tamper");
}

#[test]
fn disk_artifact_seal_is_terminal_and_refuses_page_replay() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("terminal-seal.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    assert!(
        matches!(
            builder.progress(),
            Err(CodeLexicalArtifactErrorV1::Contract(_))
        ),
        "a sealed artifact keeps no source cursor to resume"
    );
    assert!(matches!(
        builder.append_page(&pages[0], &control),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
    assert_eq!(
        builder.sealed_receipt().expect("sealed receipt after rejected replay"),
        Some(verified.clone()),
        "a sealed artifact must reject an append without changing its seal"
    );
    assert_eq!(
        builder
            .finalize(&source_receipt, &control)
            .expect("sealed receipt remains intact"),
        verified,
        "a rejected append must not mutate the sealed receipt"
    );
}

#[test]
fn disk_artifact_preseal_gate_denies_external_derived_mutation() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("preseal-derived-mutation.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }

    let connection = rusqlite::Connection::open(&artifact_path).expect("open artifact mutation");
    let original_row: Vec<u8> = connection
        .query_row(
            "SELECT payload FROM row_blocks ORDER BY first_document LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("artifact row block");
    let original_term_postings = i64::try_from(staged_posting_count(
        &artifact_path,
        "term_posting_runs",
        true,
    ))
    .expect("term posting count");
    let original_imports: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_evidence", [], |row| row.get(0))
        .expect("import evidence count");
    assert!(original_term_postings > 0);
    assert!(original_imports > 0);
    let mut mutated_row = original_row.clone();
    mutated_row.push(b' ');
    let row_mutation = connection.execute(
        "UPDATE row_blocks SET payload = ?1 WHERE first_document = (SELECT MIN(first_document) FROM row_blocks)",
        [mutated_row],
    );
    let posting_mutation = connection.execute("DELETE FROM term_posting_runs", []);
    let row_insertion = connection.execute(
        "INSERT INTO row_chunk_pages(document_id, chunk_id) VALUES (?1, 'external-conflict')",
        [i64::MAX],
    );
    assert!(
        row_mutation.is_err(),
        "schema-time mutation authority must deny external row updates before finalization"
    );
    assert!(
        posting_mutation.is_err(),
        "schema-time mutation authority must deny external posting deletes before finalization"
    );
    assert!(
        row_insertion.is_err(),
        "schema-time mutation authority must deny external row inserts before finalization"
    );
    let integrity: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .expect("SQLite integrity check");
    assert_eq!(integrity, "ok");
    drop(connection);

    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        &metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("denied mutation preserves a readable finalized artifact");
    let connection =
        rusqlite::Connection::open(&artifact_path).expect("inspect finalized artifact");
    let rebuilt_row: Vec<u8> = connection
        .query_row(
            "SELECT payload FROM row_blocks ORDER BY first_document LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("finalized artifact row block");
    let rebuilt_term_postings: i64 = connection
        .prepare("SELECT lists FROM term_postings")
        .expect("prepare finalized term lists")
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .expect("read finalized term lists")
        .map(|lists| {
            decode_term_lists_oracle(&lists.expect("finalized term lists"))
                .iter()
                .map(|(_, document_frequency, _)| document_frequency)
                .sum::<i64>()
        })
        .sum();
    let rebuilt_imports: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_evidence", [], |row| row.get(0))
        .expect("finalized import evidence count");
    assert_eq!(rebuilt_row, original_row);
    assert_eq!(rebuilt_term_postings, original_term_postings);
    assert_eq!(rebuilt_imports, original_imports);
}

#[test]
fn disk_artifact_finalization_rejects_mutated_artifact_state() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory
        .path()
        .join("preseal-artifact-state-mutation.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone())
        .expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }

    let mut forged_metadata = metadata;
    forged_metadata.logical_paths.insert(
        id::<FileOccurrenceId>("file.artifact"),
        "src/forged.ts".to_owned(),
    );
    let forged_metadata = serde_json::to_vec(&forged_metadata).expect("canonical forged metadata");
    let forged_digest = digest_id::<ManifestDigest>('9');
    let connection = rusqlite::Connection::open(&artifact_path).expect("open artifact mutation");
    connection
        .execute(
            "UPDATE artifact_state SET metadata = ?1, metadata_digest = ?2 WHERE singleton = 1",
            rusqlite::params![forged_metadata, forged_digest.as_str()],
        )
        .expect("mutate structurally valid pre-seal artifact state");
    let integrity: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .expect("SQLite integrity check");
    assert_eq!(integrity, "ok");
    drop(connection);

    assert!(matches!(
        builder.advance_finalization(&source_receipt, 4_096, &control),
        Err(CodeLexicalArtifactErrorV1::Corrupt(_))
    ));
}

#[test]
fn disk_artifact_metadata_rejects_noncanonical_logical_paths() {
    let generation = id::<CodeGenerationId>("generation.paths");
    for (ordinal, path) in ["/src/lib.rs", "src\\lib.rs", "src/../lib.rs"]
        .into_iter()
        .enumerate()
    {
        let mut metadata = projection_metadata(&generation, FreshnessCompatibilityV1::Current);
        metadata
            .logical_paths
            .insert(id::<FileOccurrenceId>("file.0"), path.to_owned());
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let Err(error) = CodeLexicalArtifactBuilderV1::create(
            directory
                .path()
                .join(format!("invalid-path-{ordinal}.sqlite")),
            metadata,
        ) else {
            panic!("noncanonical logical path must be refused");
        };
        assert!(matches!(error, CodeLexicalArtifactErrorV1::Contract(_)));
    }
}

#[test]
fn disk_artifact_progress_persists_exact_source_cursor_and_replay() {
    let (fixture, pages, _) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let expected_cursor = pages[0].next_cursor().clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("exact-progress.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone())
        .expect("create artifact");
    let appended = builder
        .append_page(&pages[0], &control)
        .expect("append exact page");
    assert_eq!(appended.next_cursor.as_ref(), Some(&expected_cursor));
    drop(builder);

    let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &artifact_path,
        metadata,
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("resume artifact");
    assert_eq!(
        resumed
            .progress()
            .expect("persisted progress")
            .next_cursor
            .as_ref(),
        Some(&expected_cursor)
    );
    let replayed = resumed
        .append_page(&pages[0], &control)
        .expect("replay exact page");
    assert_eq!(replayed.next_cursor.as_ref(), Some(&expected_cursor));
}

#[test]
fn disk_artifact_cancellation_rolls_back_import_append_and_reopen_verification() {
    let (fixture, pages, _) = real_verified_pages_with_maximum_page_chunks(1);
    let metadata = fixture.metadata.clone();
    let import_page = pages
        .iter()
        .find(|page| page.chunks().is_empty() && !page.imports().is_empty())
        .expect("real source emits an import-only page");
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("cancelled-verification.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create artifact");
    for page in pages
        .iter()
        .take_while(|page| page.page_ordinal() < import_page.page_ordinal())
    {
        builder
            .append_page(page, &control)
            .expect("append prefix page");
    }
    let progress_before = builder.progress().expect("progress before import page");
    let cancellation = CancelAtObservation::new(2);
    assert!(matches!(
        builder.append_page(import_page, &cancellation),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));
    assert_eq!(
        builder.progress().expect("rolled back progress"),
        progress_before
    );
    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect staging artifact");
    let imports: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_evidence", [], |row| row.get(0))
        .expect("count staged imports");
    let imports = u64::try_from(imports).expect("staged import count must be nonnegative");
    assert_eq!(
        imports, 0,
        "cancelled import page must roll back atomically"
    );
    drop(connection);

    builder
        .append_page(import_page, &control)
        .expect("resume exact import page");
    let staged_before_seal_replay = builder.progress().expect("staged progress before replay");
    let mut cancelled_source = fixture.open_source(1);
    let replay_cancellation = CancelAtObservation::new(3);
    assert!(matches!(
        builder.rebuild_and_finalize(&mut cancelled_source, &replay_cancellation),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));
    assert_eq!(
        builder.progress().expect("replay rollback progress"),
        staged_before_seal_replay,
        "cancelled source replay must roll back its derived rebuild atomically"
    );
    let mut final_source = fixture.open_source(1);
    let verified = builder
        .rebuild_and_finalize(&mut final_source, &control)
        .expect("rebuild and finalize artifact");
    let reopen_cancellation = CancelAtObservation::new(3);
    assert!(matches!(
        CodeLexicalArtifactReaderV1::open_with_control(
            &artifact_path,
            &verified,
            &metadata,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &reopen_cancellation,
        ),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));
    CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        &metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("cancelled verification must not alter the sealed artifact");
}

/// Count the staged lexical rows and their distinct chunk identities so a
/// double-advanced row is visible as a cardinality mismatch. The guarded
/// production failure is quoted verbatim so a regression reproduces the
/// exact activation error it protects against.
fn staged_row_cardinality(artifact_path: &Path) -> (u64, u64) {
    let connection = rusqlite::Connection::open(artifact_path).expect("inspect staging artifact");
    // Appends stage chunk ids in `row_chunk_pages`; sealing moves them into
    // `row_chunks`.
    let staging: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'row_chunk_pages')",
            [],
            |row| row.get(0),
        )
        .expect("probe chunk staging");
    let table = if staging {
        "row_chunk_pages"
    } else {
        "row_chunks"
    };
    let (rows, distinct): (i64, i64) = connection
        .query_row(
            &format!("SELECT COUNT(*), COUNT(DISTINCT chunk_id) FROM {table}"),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("staged row cardinality");
    assert_eq!(
        rows, distinct,
        "guarded production activation failure: 'code-index retained generation did not \
         activate because lexical projection row was advanced more than once'"
    );
    (
        u64::try_from(rows).expect("staged row count"),
        u64::try_from(distinct).expect("distinct staged row count"),
    )
}

#[test]
fn disk_artifact_receipt_failure_rolls_back_prior_page_rows_and_receipts() {
    let (fixture, pages, _) = real_verified_pages_with_maximum_page_chunks(1);
    assert!(pages.len() >= 2, "fixture must emit a multi-page batch");
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("receipt-failure-batch.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata)
        .expect("create artifact");
    let connection = rusqlite::Connection::open(&artifact_path).expect("install receipt failpoint");
    connection
        .execute_batch(
            "CREATE TRIGGER fail_second_page_receipt
             BEFORE INSERT ON source_pages
             WHEN NEW.page_ordinal = 1
             BEGIN SELECT RAISE(ABORT, 'forced receipt failure'); END;",
        )
        .expect("create receipt failpoint");
    drop(connection);

    assert!(builder.append_pages(&pages[..2], &control).is_err());
    assert_eq!(
        builder
            .progress()
            .expect("progress after receipt failure")
            .next_page_ordinal,
        0
    );
    assert_eq!(
        staged_row_cardinality(&artifact_path).0,
        0,
        "receipt failure must roll back all prior relational writes"
    );
}

#[test]
fn disk_artifact_budget_refusal_precedes_progress_and_accepts_boundary() {
    let (fixture, pages, source_receipt) = real_verified_pages_with_maximum_page_chunks(1);
    let metadata = fixture.metadata.clone();
    let control = ArtifactControl { cancelled: false };
    let directory = tempfile::tempdir().expect("artifact tempdir");

    // Measure the real deterministic ledger charges with a default builder.
    let probe = CodeLexicalArtifactBuilderV1::create(
        directory.path().join("charge-probe.sqlite"),
        metadata.clone(),
    )
    .expect("create charge probe");
    let fixed = probe.fixed_ledger_charge_bytes();
    let first_page_charge = probe
        .page_ledger_charge_bytes(&pages[0])
        .expect("first page ledger charge");
    let max_page_charge = pages
        .iter()
        .map(|page| {
            probe
                .page_ledger_charge_bytes(page)
                .expect("page ledger charge")
        })
        .max()
        .expect("fixture pages");
    assert!(first_page_charge > 0, "a real page must carry ledger cost");

    // A budget exactly one byte under the first page's charge refuses the
    // append before any progress mutation.
    let refused_path = directory.path().join("refused.sqlite");
    let mut refused = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        &refused_path,
        metadata.clone(),
        fixed + first_page_charge - 1,
    )
    .expect("create one-byte-under builder");
    let bytes_before = std::fs::metadata(&refused_path)
        .expect("fresh staging artifact metadata")
        .len();
    assert!(matches!(
        refused.append_page(&pages[0], &control),
        Err(CodeLexicalArtifactErrorV1::BatchTooLarge { .. })
    ));
    assert_eq!(
        refused
            .progress()
            .expect("refused progress")
            .next_page_ordinal,
        0,
        "a ledger refusal must precede every progress mutation"
    );
    assert_eq!(
        std::fs::metadata(&refused_path)
            .expect("refused staging artifact metadata")
            .len(),
        bytes_before,
        "preflight refusal must not allocate or persist projection rows"
    );
    let connection = rusqlite::Connection::open(&refused_path).expect("inspect refusal state");
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM row_chunk_pages", [], |row| row.get(0))
        .expect("row count after refusal");
    assert_eq!(rows, 0, "preflight refusal must precede row staging");
    drop(connection);

    // At exactly the measured charge the same page is admitted.
    let mut boundary = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        directory.path().join("boundary.sqlite"),
        metadata.clone(),
        fixed + first_page_charge,
    )
    .expect("create boundary-budget builder");
    boundary
        .append_page(&pages[0], &control)
        .expect("the boundary budget admits the measured page exactly");

    // Once the caller has held a verified page within its own source
    // reservation, the builder needs only its fixed charge and this page's
    // transient bound. Every staged row then advances exactly once.
    let sealed_path = directory.path().join("sealed.sqlite");
    let mut sealed = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        &sealed_path,
        metadata,
        fixed + max_page_charge,
    )
    .expect("create admitting builder");
    for page in &pages {
        sealed
            .append_page(page, &control)
            .expect("page fits the independently reserved source window");
    }
    let verified = finish_staged_artifact(&mut sealed, &source_receipt, &control);
    assert_eq!(verified.total_chunks(), source_receipt.total_chunks());
    let (rows, distinct) = staged_row_cardinality(&sealed_path);
    assert_eq!(
        rows,
        source_receipt.total_chunks(),
        "every lexical row advances exactly once"
    );
    assert_eq!(rows, distinct, "no lexical row may advance twice");
}

#[test]
fn disk_artifact_rows_advance_once_across_retry_replay_and_cancellation() {
    let (fixture, pages, source_receipt) = real_verified_pages_with_maximum_page_chunks(1);
    let metadata = fixture.metadata.clone();
    assert!(pages.len() > 1, "fixture must emit several pages");
    let control = ArtifactControl { cancelled: false };
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("once-advance.sqlite");
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create artifact");

    let mut appended_chunks = 0u64;
    for page in &pages {
        // Cancellation mid-append rolls back without advancing any row.
        let cancellation = CancelAtObservation::new(2);
        assert!(matches!(
            builder.append_page(page, &cancellation),
            Err(CodeLexicalArtifactErrorV1::Interrupted(_))
        ));
        assert_eq!(
            staged_row_cardinality(&artifact_path).0,
            appended_chunks,
            "a cancelled append must not advance any row"
        );
        // The retried append advances each of the page's rows exactly once.
        builder
            .append_page(page, &control)
            .expect("append page after cancellation");
        appended_chunks += page.chunk_count();
        let (rows, distinct) = staged_row_cardinality(&artifact_path);
        assert_eq!(rows, appended_chunks, "a retried append advances once");
        assert_eq!(rows, distinct, "no retried row may advance twice");
        // An idempotent replay of the same page advances nothing.
        builder
            .append_page(page, &control)
            .expect("replayed page is idempotent");
        assert_eq!(
            staged_row_cardinality(&artifact_path).0,
            appended_chunks,
            "a replayed page must not advance any row"
        );
    }
    assert_eq!(appended_chunks, source_receipt.total_chunks());

    // A cancelled seal replay rolls back its derived rebuild atomically.
    let mut cancelled_source = fixture.open_source(1);
    let replay_cancellation = CancelAtObservation::new(3);
    assert!(matches!(
        builder.rebuild_and_finalize(&mut cancelled_source, &replay_cancellation),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));
    assert_eq!(
        staged_row_cardinality(&artifact_path).0,
        appended_chunks,
        "a cancelled seal replay must not advance any row"
    );

    // The retried replay still lands every row exactly once and seals.
    let mut final_source = fixture.open_source(1);
    let verified = builder
        .rebuild_and_finalize(&mut final_source, &control)
        .expect("rebuild and finalize artifact");
    let (rows, distinct) = staged_row_cardinality(&artifact_path);
    assert_eq!(rows, source_receipt.total_chunks());
    assert_eq!(rows, distinct);
    CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        &metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("once-advanced artifact reopens with full verification");
}

#[test]
fn disk_artifact_bounded_work_budget_exhaustion_resumes_activation() {
    // Stage0a on `5ddd16271`: repeated activation failures (~00:12:14,
    // 00:13:14, 00:15:14, 00:19:14 UTC) and no generation sealed after ~11
    // minutes. The earlier activation failed with "the read port exceeded
    // its bounded work budget" and later retries with "code-index retained
    // generation did not activate because lexical projection row was
    // advanced more than once". A fresh beta.33 dogfood reproduced the same
    // lane: daemon PID 32033 still `warming` after ~56 minutes,
    // `latest_generation_id: null`, graph `exact_scope_generation_not_ready`,
    // pre-embedding (no model.onnx/generation/graph-replay FD), ~135% CPU
    // across 115 threads, and VmRSS 6.88GB, past every advertised memory
    // ceiling. This regression drives the same retry shape through the real
    // sealed source: every exhausted window must stay a typed, resumable
    // interruption that never advances a row twice, the retry storm must
    // not grow the staged artifact or the enforced ledger claim, and the
    // retried activation must seal instead of burning unbounded work.
    let (fixture, pages, source_receipt) = real_verified_pages_with_maximum_page_chunks(1);
    let metadata = fixture.metadata.clone();
    let control = ArtifactControl { cancelled: false };
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("bounded-budget-resume.sqlite");
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone())
        .expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let staged = builder.progress().expect("staged progress");
    // The enforced ledger claim must hold for every real page while the
    // retry storm runs: unbounded RSS growth under warming is exactly what
    // the beta.33 evidence shows an unenforced claim permits.
    let source_window = fixture.open_source(1).staging_window_bytes();
    let max_page_charge = pages
        .iter()
        .map(|page| {
            builder
                .page_ledger_charge_bytes(page)
                .expect("page ledger charge")
        })
        .max()
        .expect("fixture pages");
    assert!(
        builder.fixed_ledger_charge_bytes() + source_window + max_page_charge
            <= CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        "the whole warming replay must fit the enforced build memory claim"
    );

    // Repeated bounded finalization steps can interrupt without replaying
    // source or duplicating rows. Their SQLite cursor is deliberately
    // durable, so retries are not required to preserve the staging file's
    // byte size.
    assert!(matches!(
        builder
            .advance_finalization(&source_receipt, 1, &control)
            .expect("persist immutable finalization freeze"),
        CodeLexicalArtifactFinalizationStepV1::Pending { .. }
    ));
    for round in 0..4 {
        // Every retry starts from the already durable freeze and may yield
        // without replaying source or weakening immutable base authority.
        let exhausted = BudgetExhaustedAtObservation::new(5);
        let outcome = builder.advance_finalization(&source_receipt, usize::MAX, &exhausted);
        assert!(
            matches!(
                outcome,
                Err(CodeLexicalArtifactErrorV1::Interrupted(
                    CodeIndexInterruptionV1::DeadlineExceeded
                ))
            ),
            "round {round}: an exhausted bounded work budget must stay a resumable typed \
             interruption, never a terminal activation failure"
        );
        assert_eq!(
            builder.progress().expect("progress after exhausted round"),
            staged,
            "round {round}: an exhausted bounded work budget must not mutate staged progress"
        );
        staged_row_cardinality(&artifact_path);
        assert!(
            matches!(
                builder.append_page(&pages[0], &control),
                Err(CodeLexicalArtifactErrorV1::Contract(_))
            ),
            "round {round}: finalization makes staged source pages immutable"
        );
    }

    // The retried activation RESUMES and seals.
    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    let (rows, _) = staged_row_cardinality(&artifact_path);
    assert_eq!(rows, source_receipt.total_chunks());

    // The published read port resumes the same way: an exhausted
    // verification is a typed interruption and the retried open serves
    // lexical reads.
    let exhausted_open = BudgetExhaustedAtObservation::new(3);
    assert!(matches!(
        CodeLexicalArtifactReaderV1::open_with_control(
            &artifact_path,
            &verified,
            &metadata,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &exhausted_open,
        ),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        &metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect(
        "the reopened read port must resume instead of terminally failing its bounded work budget",
    );
    let mut request = lexical_request("render", &["render"], &[], &[], 0, 8);
    request.generation = metadata.generation.clone();
    let RetrieverOutcome::Complete(batch) = LexicalLane::new(reader)
        .retrieve_lexical(&request)
        .expect("the resumed read port serves lexical reads")
    else {
        panic!("the resumed lexical read must complete");
    };
    assert!(!batch.candidates.is_empty());
}

#[test]
fn disk_artifact_same_source_instance_resumes_after_accepted_page_failure() {
    // A cancellation that lands AFTER at least one page was accepted leaves
    // the source cursor mid-stream. The retried seal must replay the very
    // same source instance (not a fresh one) and advance every row exactly
    // once; a page-zero precondition that terminally blocks the retry is
    // the production activation stall.
    let (fixture, _, source_receipt) = real_verified_pages_with_maximum_page_chunks(1);
    let metadata = fixture.metadata.clone();
    let control = ArtifactControl { cancelled: false };
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("same-source.sqlite");
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create artifact");
    let mut source = fixture.open_source(1);
    let cancellation = CancelAfterAcceptedPage::default();

    let first_page = source
        .next_page_if(&cancellation, |page| {
            builder.append_page(page, &cancellation)?;
            cancellation.mark_page_accepted();
            Ok::<(), CodeLexicalArtifactErrorV1>(())
        })
        .expect("stage the first verified page")
        .expect("admit the first verified page");
    assert!(matches!(
        first_page,
        VerifiedSealedLexicalPageReadV1::Page(_)
    ));
    assert_eq!(source.cursor().next_page_ordinal(), 1);

    assert!(matches!(
        builder.rebuild_and_finalize(&mut source, &cancellation),
        Err(CodeLexicalArtifactErrorV1::Interrupted(
            CodeIndexInterruptionV1::Cancelled
        ))
    ));
    assert_eq!(source.cursor().next_page_ordinal(), 1);

    // The failure landed after page acceptance: the SAME instance must
    // resume and seal.
    let verified = builder.rebuild_and_finalize(&mut source, &control).expect(
        "the same source instance must resume after an accepted-page failure \
         instead of terminally blocking on the page-zero precondition",
    );
    assert_eq!(verified.total_chunks(), source_receipt.total_chunks());
    let (rows, _) = staged_row_cardinality(&artifact_path);
    assert_eq!(rows, source_receipt.total_chunks());
}

/// A test authority that denies a configured set of literal byte strings and
/// delegates everything else to the central authority.
#[derive(Clone)]
struct DenyingExactAuthority {
    central: CentralExactAdmissionAuthorityV1,
    denied: BTreeSet<Vec<u8>>,
}

impl ExactAdmissionValidator for DenyingExactAuthority {
    fn admit(
        &self,
        field: ExactFieldV1,
        candidate_bytes: &[u8],
        request: &RetrievalRequest,
    ) -> Result<Option<ExactAdmissionProof>, RetrievalError> {
        if self.denied.contains(candidate_bytes) {
            return Ok(None);
        }
        self.central.admit(field, candidate_bytes, request)
    }
}

impl ExactAdmissionAuthority for DenyingExactAuthority {
    fn parse_literals(
        &self,
        query_view: &EphemeralSanitizedQueryViewV1,
        request: &RetrievalRequest,
    ) -> Vec<ExactLiteralV1> {
        self.central.parse_literals(query_view, request)
    }
}

#[test]
fn artifact_exact_reader_prefers_admitted_matches_over_denied_best() {
    // Admission must precede heap eligibility: with cap=1, a raw-best
    // document whose matched literals are all denied must be excluded so
    // the next admitted document is returned, never a Contract failure.
    let fixture = real_lexical_source_fixture_with_files(3);
    let metadata = fixture.metadata.clone();
    let generation = metadata.generation.clone();
    let control = ArtifactControl { cancelled: false };
    let (pages, _) = drain_verified_pages(&fixture, 128);
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("denied-best.sqlite");
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let mut final_source = fixture.open_source(128);
    let verified = builder
        .rebuild_and_finalize(&mut final_source, &control)
        .expect("rebuild and finalize artifact");
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        &metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("verify and reopen artifact");

    // `render_01`'s document matches four phrases (all denied); `render_02`'s
    // matches three, one of which stays admitted.
    let exact_query = r#""return value" "widget-kit" "render_01" "function render_01" "render_02""#;
    let authority = DenyingExactAuthority {
        central: CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>(
            "exact-rules.v1",
        )),
        denied: BTreeSet::from([
            b"return value".to_vec(),
            b"widget-kit".to_vec(),
            b"render_01".to_vec(),
            b"function render_01".to_vec(),
        ]),
    };
    let base = base_request(exact_query, 1);
    let exact_query_view = query_view(exact_query);
    let exact_request = ExactLaneRequest {
        control: &ACTIVE_CONTROL,
        literals: authority.parse_literals(&exact_query_view, &base),
        base,
        query_view: &exact_query_view,
        generation,
        budget: budget(1),
    };
    let RetrieverOutcome::Complete(batch) = reader
        .exact_adapter(authority)
        .read_exact_postings(&exact_request)
        .expect("a denied raw-best match must not fail the exact batch")
    else {
        panic!("the exact artifact port must complete");
    };
    assert_eq!(
        batch.candidates.len(),
        1,
        "cap=1 must return exactly the admitted document"
    );
    let evidence = &batch.evidence_by_occurrence[&batch.candidates[0].source_occurrence_id];
    assert_eq!(evidence.admission_proof.original_bytes, b"render_02");
    assert!(
        evidence
            .matched_literals
            .iter()
            .any(|literal| literal.original_bytes == b"render_02"),
        "the returned document carries the admitted literal"
    );
}

#[test]
fn exact_candidate_scan_stops_before_the_next_batch_after_cancellation() {
    struct CancelDuringScan {
        checks: AtomicUsize,
    }
    impl RetrievalExecutionControl for CancelDuringScan {
        fn is_cancelled(&self) -> bool {
            self.checks.fetch_add(1, Ordering::SeqCst) >= 2
        }
        fn elapsed_micros(&self) -> u64 {
            0
        }
    }

    let fixture = real_lexical_source_fixture_with_files(256);
    let metadata = fixture.metadata.clone();
    let generation = metadata.generation.clone();
    let build_control = ArtifactControl { cancelled: false };
    let (pages, _) = drain_verified_pages(&fixture, 128);
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let path = directory.path().join("cancel-exact.sqlite");
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&path, metadata.clone()).expect("create real artifact");
    for page in &pages {
        builder
            .append_page(page, &build_control)
            .expect("append page");
    }
    let mut source = fixture.open_source(128);
    let verified = builder
        .rebuild_and_finalize(&mut source, &build_control)
        .expect("finalize artifact");
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &path,
        &verified,
        &metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &build_control,
    )
    .expect("open artifact");
    let authority = CentralExactAdmissionAuthorityV1::new(id("exact-rules.v1"));
    let query = r#""return value""#;
    let base = base_request(query, 8);
    let view = query_view(query);
    let control = CancelDuringScan {
        checks: AtomicUsize::new(0),
    };
    let mut request = ExactLaneRequest {
        literals: authority.parse_literals(&view, &base),
        base,
        query_view: &view,
        generation,
        budget: budget(8),
        control: &control,
    };
    let exact = reader.exact_adapter(authority);
    assert_eq!(
        exact.read_exact_postings(&request),
        Err(RetrievalPortError::Cancelled),
        "the exact artifact scan must consult the carried request control between candidate batches",
    );
    request.control = &ACTIVE_CONTROL;
    let first = complete(exact.read_exact_postings(&request).expect("active read"));
    assert!(
        first.coverage.eligible > 128,
        "fixture must span candidate batches"
    );
    assert_eq!(first.candidates.len(), 8);
    assert_eq!(
        first,
        complete(exact.read_exact_postings(&request).expect("repeat read"))
    );
}

#[test]
fn in_memory_rebuilds_observe_cancellation_at_phase_and_batch_boundaries() {
    let fixture = real_lexical_source_fixture_with_files(256);
    let (pages, _) = drain_verified_pages(&fixture, 128);
    let projection = CodeLexicalProjectionAdapterV1::new_admitted(
        fixture.metadata.clone(),
        pages
            .iter()
            .flat_map(|page| page.chunks().iter().cloned())
            .collect::<Vec<_>>(),
        page_symbol_displays(&pages),
    )
    .expect("real admitted in-memory projection");

    // Empty rebuilds must still consult the phase boundary. For the wide
    // fixture, cancellation occurs after enough observations to enter a later
    // rebuild batch; entry and final checks alone cannot trigger it.
    for (term, cancel_at, has_matches) in [("absentzzxyz", 2, false), ("widget", 10, true)] {
        let control = CancelAtObservation::new(cancel_at);
        let mut request = lexical_request(term, &[term], &[], &[], 0, 1024);
        request.generation = fixture.metadata.generation.clone();
        let baseline = complete(
            projection
                .read_lexical_postings(&request)
                .expect("active lexical read"),
        );
        assert_eq!(baseline.candidates.len() > 128, has_matches);
        request.control = &control;
        assert_eq!(
            projection.read_lexical_postings(&request),
            Err(RetrievalPortError::Cancelled)
        );
        assert_eq!(control.observations(), cancel_at);
    }

    let authority = CentralExactAdmissionAuthorityV1::new(id("exact-rules.v1"));
    let exact = projection.exact_adapter(authority.clone());
    for (query, cancel_at, has_matches) in [
        (r#""absentzzxyz""#, 3, false),
        (r#""return value""#, 9, true),
    ] {
        let control = CancelAtObservation::new(cancel_at);
        let view = query_view(query);
        let base = base_request(query, 1024);
        let mut request = ExactLaneRequest {
            literals: authority.parse_literals(&view, &base),
            base,
            query_view: &view,
            generation: fixture.metadata.generation.clone(),
            budget: budget(1024),
            control: &ACTIVE_CONTROL,
        };
        let baseline = complete(
            exact
                .read_exact_postings(&request)
                .expect("active exact read"),
        );
        assert_eq!(baseline.candidates.len() > 128, has_matches);
        request.control = &control;
        assert_eq!(
            exact.read_exact_postings(&request),
            Err(RetrievalPortError::Cancelled)
        );
        assert_eq!(control.observations(), cancel_at);
    }
}

#[test]
fn disk_artifact_ledger_charges_stay_page_local_across_corpus_scaling() {
    let control = ArtifactControl { cancelled: false };
    let mut max_charges = Vec::new();
    let mut chunk_totals = Vec::new();
    for (index, file_count) in [2usize, 20usize].into_iter().enumerate() {
        let fixture = real_lexical_source_fixture_with_files(file_count);
        let metadata = fixture.metadata.clone();
        let (pages, receipt) = drain_verified_pages(&fixture, 1);
        let directory = tempfile::tempdir().expect("artifact tempdir");
        let artifact_path = directory.path().join(format!("scaling-{index}.sqlite"));
        let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata)
            .expect("create artifact");
        let max_charge = pages
            .iter()
            .map(|page| {
                builder
                    .page_ledger_charge_bytes(page)
                    .expect("page ledger charge")
            })
            .max()
            .expect("corpus pages");
        assert!(
            builder.fixed_ledger_charge_bytes() + max_charge
                <= CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
            "the real corpus must fit the enforced build memory budget"
        );
        for page in &pages {
            builder.append_page(page, &control).expect("append page");
        }
        let mut final_source = fixture.open_source(1);
        let verified = builder
            .rebuild_and_finalize(&mut final_source, &control)
            .expect("rebuild and finalize artifact");
        assert_eq!(verified.total_chunks(), receipt.total_chunks());
        let (rows, distinct) = staged_row_cardinality(&artifact_path);
        assert_eq!(rows, receipt.total_chunks());
        assert_eq!(rows, distinct);
        max_charges.push(max_charge);
        chunk_totals.push(receipt.total_chunks());
    }
    assert_eq!(
        chunk_totals[1],
        chunk_totals[0] * 10,
        "the large corpus must really be ten times the small corpus"
    );
    assert!(
        max_charges[1] <= max_charges[0].saturating_mul(2),
        "the per-page ledger charge must track page content, not corpus size: {} vs {} bytes",
        max_charges[1],
        max_charges[0]
    );
}

#[test]
fn disk_artifact_reader_selects_bounded_top_k_with_lane_tie_order_and_coverage() {
    let fixture = real_lexical_source_fixture_with_files(9);
    let metadata = fixture.metadata.clone();
    let generation = metadata.generation.clone();
    let control = ArtifactControl { cancelled: false };
    let (pages, _) = drain_verified_pages(&fixture, 128);
    let chunks = pages
        .iter()
        .flat_map(|page| page.chunks().iter().cloned())
        .collect::<Vec<_>>();
    let one_shot = CodeLexicalProjectionAdapterV1::new_admitted(
        metadata.clone(),
        chunks,
        page_symbol_displays(&pages),
    )
    .expect("one-shot lexical projection");
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("top-k.sqlite");
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let mut final_source = fixture.open_source(128);
    let verified = builder
        .rebuild_and_finalize(&mut final_source, &control)
        .expect("rebuild and finalize artifact");
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        &metadata,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("verify and reopen artifact");

    // Every file scores identical token counts for "widget", so the K=7 cut
    // runs straight through a score tie and must fall back to the lane's
    // stable occurrence order.
    let mut request = lexical_request("widget", &["widget"], &[], &[], 0, 7);
    request.generation = generation.clone();
    let RetrieverOutcome::Complete(lexical_port) = reader
        .read_lexical_postings(&request)
        .expect("artifact lexical port read")
    else {
        panic!("artifact lexical port must complete");
    };
    assert!(
        lexical_port.coverage.eligible > 7,
        "fixture must overflow the K=7 cap"
    );
    assert_eq!(
        lexical_port.candidates.len(),
        7,
        "the artifact port hydrates at most K candidates"
    );
    assert_eq!(
        lexical_port.coverage.capped,
        lexical_port.coverage.eligible - 7,
        "the surplus above K is reported as capped coverage"
    );

    let artifact_lane = complete(
        LexicalLane::new(reader.clone())
            .retrieve_lexical(&request)
            .expect("artifact lexical lane"),
    );
    let memory_lane = complete(
        LexicalLane::new(one_shot.clone())
            .retrieve_lexical(&request)
            .expect("one-shot lexical lane"),
    );
    assert_eq!(
        artifact_lane, memory_lane,
        "the K=7 lexical lane batch (candidates, evidence, coverage, continuation) must \
         match the one-shot projection exactly; a pre-capped port must not surface as \
         eligible=K/capped=0/exhausted"
    );
    assert_eq!(
        artifact_lane.coverage.capped,
        artifact_lane.coverage.eligible - 7,
        "lane coverage must preserve the port's truncation surplus"
    );
    assert!(
        !artifact_lane
            .continuation
            .as_ref()
            .expect("lexical lane continuation")
            .exhausted,
        "a capped lexical search must not be reported exhausted"
    );
    assert_eq!(
        lexical_port
            .candidates
            .iter()
            .map(|candidate| &candidate.source_occurrence_id)
            .collect::<Vec<_>>(),
        artifact_lane
            .candidates
            .iter()
            .map(|candidate| &candidate.source_occurrence_id)
            .collect::<Vec<_>>(),
        "the port's bounded selection already uses the lane's canonical tie order"
    );

    // Exact-lane parity under the same K=7: every document matches the
    // quoted literal once, so the cut again runs through a tie.
    let authority =
        CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>("exact-rules.v1"));
    let exact_query = r#""return value""#;
    let base = base_request(exact_query, 7);
    let exact_query_view = query_view(exact_query);
    let exact_request = ExactLaneRequest {
        control: &ACTIVE_CONTROL,
        literals: authority.parse_literals(&exact_query_view, &base),
        base,
        query_view: &exact_query_view,
        generation,
        budget: budget(7),
    };
    let RetrieverOutcome::Complete(exact_port) = reader
        .exact_adapter(authority.clone())
        .read_exact_postings(&exact_request)
        .expect("artifact exact port read")
    else {
        panic!("artifact exact port must complete");
    };
    assert!(
        exact_port.coverage.eligible > 7,
        "fixture must overflow the exact K=7 cap"
    );
    assert_eq!(exact_port.candidates.len(), 7);
    assert_eq!(exact_port.coverage.capped, exact_port.coverage.eligible - 7);

    let artifact_exact = complete(
        ExactLane::new(authority.clone(), reader.exact_adapter(authority.clone()))
            .retrieve_exact(&exact_request)
            .expect("artifact exact lane"),
    );
    let memory_exact = complete(
        ExactLane::new(authority.clone(), one_shot.exact_adapter(authority))
            .retrieve_exact(&exact_request)
            .expect("one-shot exact lane"),
    );
    assert_eq!(
        artifact_exact, memory_exact,
        "the K=7 exact lane batch (candidates, evidence, coverage, continuation) must \
         match the one-shot projection exactly; a pre-capped port must not surface as \
         eligible=K/capped=0/exhausted"
    );
    assert_eq!(
        artifact_exact.coverage.capped,
        artifact_exact.coverage.eligible - 7,
        "exact lane coverage must preserve the port's truncation surplus"
    );
    assert!(
        !artifact_exact
            .continuation
            .as_ref()
            .expect("exact lane continuation")
            .exhausted,
        "a capped exact search must not be reported exhausted"
    );
}

#[test]
fn retained_lexical_projection_bounds_marginal_owned_byte_growth_for_repeated_tokens() {
    let generation = id::<CodeGenerationId>("generation.1");
    let small_repeated = "retained_token ".repeat(1_000);
    let large_repeated = "retained_token ".repeat(3_000);
    let small_source = format!(
        "pub fn retained_symbol() -> usize {{ let retained_token = 1; {small_repeated} retained_token }}\n"
    );
    let large_source = format!(
        "pub fn retained_symbol() -> usize {{ let retained_token = 1; {large_repeated} retained_token }}\n"
    );
    let small_projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        vec![admitted_rust_chunk(
            &generation,
            0,
            &small_source,
            CodeSearchChunkGrainV1::SymbolBody,
            "retained_symbol",
        )],
        BTreeMap::new(),
    )
    .expect("build small repeated-token projection");
    let large_projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        vec![admitted_rust_chunk(
            &generation,
            0,
            &large_source,
            CodeSearchChunkGrainV1::SymbolBody,
            "retained_symbol",
        )],
        BTreeMap::new(),
    )
    .expect("build large repeated-token projection");

    let small_retained = small_projection.retained_owned_bytes();
    let large_retained = large_projection.retained_owned_bytes();
    let marginal_retained = large_retained
        .checked_sub(small_retained)
        .expect("large projection must not retain fewer owned bytes than small projection");
    let marginal_source = large_source
        .len()
        .checked_sub(small_source.len())
        .expect("large source must not be smaller than small source");
    assert!(
        marginal_retained <= marginal_source * 2,
        "projection retained {marginal_retained} marginal owned bytes for {marginal_source} marginal source bytes"
    );

    let request = lexical_request("retained_token", &["retained_token"], &[], &[], 0, 8);
    let RetrieverOutcome::Complete(batch) = LexicalLane::new(large_projection)
        .retrieve_lexical(&request)
        .expect("query repeated-token projection")
    else {
        panic!("repeated-token projection must be current");
    };
    assert_eq!(batch.candidates.len(), 1);
    assert!(batch.candidates[0].raw_score.micros() > 0);
}

pub(crate) fn lexical_request(
    query: &str,
    whole_terms: &[&str],
    subtokens: &[&str],
    phrases: &[&str],
    fuzzy_budget: u32,
    max_candidates: u32,
) -> LexicalLaneRequest<'static> {
    let query_view = Box::leak(Box::new(query_view(query)));
    LexicalLaneRequest {
        base: base_request(query, max_candidates),
        query_view,
        generation: id("generation.1"),
        whole_terms: Cow::Owned(whole_terms.iter().map(|term| (*term).to_owned()).collect()),
        subtokens: Cow::Owned(subtokens.iter().map(|term| (*term).to_owned()).collect()),
        phrases: Cow::Owned(phrases.iter().map(|term| (*term).to_owned()).collect()),
        proximities: Cow::Owned(Vec::new()),
        field_filters: Cow::Owned(Vec::<LexicalFieldFilterV1>::new()),
        fuzzy_budget,
        lexical_profile_revision: id("lexical-profile.v1"),
        score_domain: id(QUERY_LEXICAL_SCORE_DOMAIN_V1),
        budget: budget(max_candidates),
        control: &ACTIVE_CONTROL,
    }
}

pub(crate) fn complete<T: fmt::Debug>(outcome: RetrieverOutcome<T>) -> T {
    match outcome {
        RetrieverOutcome::Complete(value) => value,
        other => panic!("expected complete retrieval, got {other:?}"),
    }
}

#[test]
fn matching_symbol_occurrence_does_not_admit_raw_or_json_exact_terms() {
    let generation = id::<CodeGenerationId>("generation.1");
    let raw = chunk(
        &generation,
        1,
        CodeSearchChunkGrainV1::SymbolSignature,
        "fn forged_symbol",
        &[(ExactTechnicalTermKindV1::WholeSymbol, "forged_symbol")],
        &["forged", "symbol"],
    );
    assert_eq!(
        raw.exact_terms[0].symbol_occurrence_id(),
        raw.anchor.symbol_occurrence_id.as_ref()
    );
    let metadata = projection_metadata(&generation, FreshnessCompatibilityV1::Current);
    assert!(
        CodeLexicalProjectionAdapterV1::new(metadata.clone(), vec![raw.clone()]).is_err(),
        "public raw-parts construction cannot admit WholeSymbol evidence"
    );

    let decoded: CodeSearchChunkV1 =
        serde_json::from_slice(&serde_json::to_vec(&raw).unwrap()).unwrap();
    assert_eq!(
        decoded.exact_terms[0].symbol_occurrence_id(),
        decoded.anchor.symbol_occurrence_id.as_ref()
    );
    assert!(
        CodeLexicalProjectionAdapterV1::new(metadata, vec![decoded]).is_err(),
        "JSON chunks remain untrusted even when occurrence ids match"
    );
}

#[test]
fn central_exact_authority_classifies_every_protected_term() {
    let authority =
        CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>("exact-rules.v1"));
    let query = r#"reserve_stock std::collections::HashMap src/main.rs "connection refused" error:"socket closed" E0308 --release cargo tracedecay.data.dir deadbee"#;
    let request = base_request(query, 16);
    let query_view = query_view(query);

    let literals = authority.parse_literals(&query_view, &request);
    let fields: BTreeSet<ExactFieldV1> = literals.iter().map(|literal| literal.field).collect();

    assert_eq!(
        fields,
        BTreeSet::from([
            ExactFieldV1::Identifier,
            ExactFieldV1::QualifiedName,
            ExactFieldV1::Path,
            ExactFieldV1::QuotedPhrase,
            ExactFieldV1::DiagnosticCode,
            ExactFieldV1::CompilerOrRuntimeError,
            ExactFieldV1::CliFlag,
            ExactFieldV1::ToolName,
            ExactFieldV1::ConfigurationKey,
            ExactFieldV1::CommitIdentifier,
        ])
    );
    for literal in literals {
        let proof = authority
            .admit(literal.field, &literal.original_bytes, &request)
            .expect("admission is evaluated")
            .expect("parsed protected literal is admitted");
        assert_eq!(proof.canonical_bytes, literal.canonical_bytes);
        proof
            .validate_for_request(&request)
            .expect("proof binds the request");
    }
    assert!(
        authority
            .admit(ExactFieldV1::Path, b"not a path", &request)
            .expect("invalid path is evaluated")
            .is_none()
    );
}

#[test]
fn exact_projection_emits_only_authority_minted_proofs() {
    let generation = id::<CodeGenerationId>("generation.1");
    let text = "std::collections::HashMap src/main.rs E0308 --release cargo tracedecay.data.dir commit:deadbee";
    let source = chunk(
        &generation,
        1,
        CodeSearchChunkGrainV1::SymbolBody,
        text,
        &[
            (
                ExactTechnicalTermKindV1::QualifiedName,
                "std::collections::HashMap",
            ),
            (ExactTechnicalTermKindV1::Path, "src/main.rs"),
            (ExactTechnicalTermKindV1::CompilerErrorCode, "E0308"),
            (ExactTechnicalTermKindV1::CliFlag, "--release"),
            (ExactTechnicalTermKindV1::ToolName, "cargo"),
            (
                ExactTechnicalTermKindV1::ConfigurationKey,
                "tracedecay.data.dir",
            ),
            (ExactTechnicalTermKindV1::CommitIdentifier, "commit:deadbee"),
        ],
        &["reserve", "stock"],
    );
    let authority =
        CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>("exact-rules.v1"));
    let projection = CodeLexicalProjectionAdapterV1::new(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        vec![source],
    )
    .expect("projection builds");
    let query = r#"std::collections::HashMap src/main.rs E0308 --release cargo tracedecay.data.dir commit:deadbee"#;
    let base = base_request(query, 16);
    let query_view = query_view(query);
    let request = ExactLaneRequest {
        control: &ACTIVE_CONTROL,
        literals: authority.parse_literals(&query_view, &base),
        base,
        query_view: &query_view,
        generation,
        budget: budget(16),
    };
    let lane = ExactLane::new(authority.clone(), projection.exact_adapter(authority));

    let batch = complete(
        lane.retrieve_exact(&request)
            .expect("exact projection query succeeds"),
    );

    assert_eq!(batch.candidates.len(), 1);
    assert_eq!(batch.coverage.examined, 1);
    assert_eq!(batch.coverage.eligible, 1);
    assert_eq!(batch.coverage.excluded, 0);
    let candidate = &batch.candidates[0];
    let proof = candidate
        .exact_admission_proof
        .as_ref()
        .expect("exact candidate carries an authority proof");
    proof
        .validate_for_request(&request.base)
        .expect("proof remains request-bound");
    let evidence = &batch.evidence_by_occurrence[&candidate.source_occurrence_id];
    assert_eq!(evidence.matched_literals.len(), 7);
}

#[test]
fn fielded_bm25_keeps_whole_identifiers_and_subtokens_distinct() {
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks = vec![
        admitted_rust_chunk(
            &generation,
            1,
            "pub fn reserve_stock() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "reserve_stock",
        ),
        admitted_rust_chunk(
            &generation,
            2,
            "pub fn reserve() { let stock_inventory = 1; }\n",
            CodeSearchChunkGrainV1::SymbolBody,
            "reserve",
        ),
    ];
    let projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
        BTreeMap::new(),
    )
    .expect("projection builds");
    let whole_request = lexical_request("reserve_stock", &["reserve_stock"], &[], &[], 0, 8);

    let whole = complete(
        LexicalLane::new(projection.clone())
            .retrieve_lexical(&whole_request)
            .expect("whole-term retrieval succeeds"),
    );

    assert_eq!(whole.candidates.len(), 1);
    let evidence = &whole.evidence_by_occurrence[&whole.candidates[0].source_occurrence_id];
    assert!(
        evidence
            .matched_whole_terms
            .contains(&"reserve_stock".to_owned())
    );
    assert!(evidence.matched_subtokens.is_empty());
    assert!(
        evidence
            .field_scores_micros
            .iter()
            .any(|(field, _)| *field == LexicalFieldV1::SymbolName)
    );

    let whole_subtoken_text = lexical_request("reserve", &["reserve"], &[], &[], 0, 8);
    let whole_only = complete(
        LexicalLane::new(projection.clone())
            .retrieve_lexical(&whole_subtoken_text)
            .expect("whole-term/subtoken boundary retrieval succeeds"),
    );
    assert_eq!(
        whole_only.candidates.len(),
        1,
        "a whole-term query must not consume the distinct subtoken field"
    );

    let subtoken_request = lexical_request("reserve", &[], &["reserve"], &[], 0, 8);
    let subtokens = complete(
        LexicalLane::new(projection)
            .retrieve_lexical(&subtoken_request)
            .expect("subtoken retrieval succeeds"),
    );
    assert_eq!(subtokens.candidates.len(), 2);
    assert!(subtokens.evidence_by_occurrence.values().all(|evidence| {
        evidence.matched_whole_terms.is_empty()
            && evidence.matched_subtokens == vec!["reserve".to_owned()]
    }));
}

#[test]
fn lexical_phrase_and_bounded_fuzzy_recovery_are_deterministic() {
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks = vec![
        admitted_rust_chunk(
            &generation,
            1,
            "pub fn reserve() { // reserve stock inventory\n}\n",
            CodeSearchChunkGrainV1::SymbolBody,
            "reserve",
        ),
        admitted_rust_chunk(
            &generation,
            2,
            "pub fn reserve_stock() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "reserve_stock",
        ),
    ];
    let projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
        BTreeMap::new(),
    )
    .expect("projection builds");
    let phrase_request = lexical_request(r#""reserve stock""#, &[], &[], &["reserve stock"], 0, 8);
    let phrase = complete(
        LexicalLane::new(projection.clone())
            .retrieve_lexical(&phrase_request)
            .expect("phrase retrieval succeeds"),
    );
    assert_eq!(phrase.candidates.len(), 1);
    assert_eq!(
        phrase.evidence_by_occurrence[&phrase.candidates[0].source_occurrence_id].matched_phrases,
        vec!["reserve stock".to_owned()]
    );

    let disabled = lexical_request("resreve_stock", &["resreve_stock"], &[], &[], 0, 8);
    assert!(
        complete(
            LexicalLane::new(projection.clone())
                .retrieve_lexical(&disabled)
                .expect("disabled fuzzy retrieval succeeds"),
        )
        .candidates
        .is_empty()
    );

    let fuzzy = lexical_request("resreve_stock", &["resreve_stock"], &[], &[], 1, 8);
    let first = complete(
        LexicalLane::new(projection.clone())
            .retrieve_lexical(&fuzzy)
            .expect("fuzzy retrieval succeeds"),
    );
    let second = complete(
        LexicalLane::new(projection)
            .retrieve_lexical(&fuzzy)
            .expect("fuzzy replay succeeds"),
    );
    assert_eq!(first, second);
    assert_eq!(first.candidates.len(), 1);
    assert!(
        first.evidence_by_occurrence[&first.candidates[0].source_occurrence_id]
            .typo_recovery_applied
    );
    assert_eq!(
        first.evidence_by_occurrence[&first.candidates[0].source_occurrence_id].spelling_variants,
        [LexicalSpellingVariantV1 {
            query: "resreve_stock".to_owned(),
            alternative: "reserve_stock".to_owned(),
        }]
    );

    let over_budget = lexical_request(
        "resreve_stock",
        &["resreve_stock"],
        &[],
        &[],
        MAX_FUZZY_TERM_EXPANSIONS_V1 + 1,
        8,
    );
    assert!(over_budget.validate().is_err());

    let oversized_term = "x".repeat(MAX_LEXICAL_QUERY_TERM_BYTES_V1 + 1);
    let oversized = lexical_request(&oversized_term, &[oversized_term.as_str()], &[], &[], 1, 8);
    assert!(oversized.validate().is_err());
}

#[test]
fn lexical_phrase_candidate_set_and_frequency_are_reused_without_drift() {
    // Equivalence guard for finding 14: the per-phrase n-gram candidate set is
    // now intersected once and reused for both the document-frequency tally and
    // the lexical document set. Two documents contain the phrase and one does
    // not; the reused candidate set must still return exactly the two
    // phrase-bearing documents, deterministically.
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks = vec![
        admitted_rust_chunk(
            &generation,
            1,
            "pub fn reserve() {\n    // reserve stock inventory ledger\n}\n",
            CodeSearchChunkGrainV1::SymbolBody,
            "reserve",
        ),
        admitted_rust_chunk(
            &generation,
            2,
            "pub fn hold() {\n    // reserve stock inventory ledger\n}\n",
            CodeSearchChunkGrainV1::SymbolBody,
            "hold",
        ),
        admitted_rust_chunk(
            &generation,
            3,
            "pub fn unrelated() {\n    // nothing relevant lives here\n}\n",
            CodeSearchChunkGrainV1::SymbolBody,
            "unrelated",
        ),
    ];
    let projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
        BTreeMap::new(),
    )
    .expect("projection builds");

    let phrase_request = lexical_request(r#""reserve stock""#, &[], &[], &["reserve stock"], 0, 8);
    let first = complete(
        LexicalLane::new(projection.clone())
            .retrieve_lexical(&phrase_request)
            .expect("phrase retrieval succeeds"),
    );
    let second = complete(
        LexicalLane::new(projection)
            .retrieve_lexical(&phrase_request)
            .expect("phrase retrieval replays"),
    );

    // Reusing the shared candidate set is deterministic and drift-free.
    assert_eq!(first, second);
    // Exactly the two phrase-bearing documents are returned; the unrelated
    // document is excluded.
    assert_eq!(first.candidates.len(), 2);
    for candidate in &first.candidates {
        assert_eq!(
            first.evidence_by_occurrence[&candidate.source_occurrence_id].matched_phrases,
            vec!["reserve stock".to_owned()]
        );
    }
}

#[test]
fn duplicate_whole_terms_do_not_consume_the_global_fuzzy_budget() {
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks = vec![
        admitted_rust_chunk(
            &generation,
            1,
            "pub fn reserve() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "reserve",
        ),
        admitted_rust_chunk(
            &generation,
            2,
            "pub fn reserved() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "reserved",
        ),
        admitted_rust_chunk(
            &generation,
            3,
            "pub fn other() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "other",
        ),
    ];
    let projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
        BTreeMap::new(),
    )
    .expect("projection builds");
    let request = lexical_request(
        "reservd reservd otherr",
        &["reservd", "reservd", "otherr"],
        &[],
        &[],
        3,
        8,
    );

    let batch = complete(
        LexicalLane::new(projection)
            .retrieve_lexical(&request)
            .expect("fuzzy retrieval succeeds"),
    );

    assert_eq!(batch.candidates.len(), 3);
    assert!(
        batch
            .evidence_by_occurrence
            .values()
            .any(|evidence| { evidence.matched_whole_terms.contains(&"otherr".to_owned()) })
    );
}

#[test]
fn lexical_projection_reports_freshness_coverage_and_page_cutoff() {
    let generation = id::<CodeGenerationId>("generation.1");
    let chunks: Vec<ExtractionAdmittedCodeSearchChunkV1> = (1..=3)
        .map(|ordinal| {
            admitted_rust_chunk(
                &generation,
                ordinal,
                "pub fn target() {}\n",
                CodeSearchChunkGrainV1::SymbolSignature,
                "target",
            )
        })
        .collect();
    let current = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks.clone(),
        BTreeMap::new(),
    )
    .expect("current projection builds");
    let request = lexical_request("target", &["target"], &[], &[], 0, 2);

    let page = complete(
        LexicalLane::new(current)
            .retrieve_lexical(&request)
            .expect("page retrieval succeeds"),
    );

    assert_eq!(page.candidates.len(), 2);
    assert_eq!(page.coverage.examined, 3);
    assert_eq!(page.coverage.eligible, 3);
    assert_eq!(page.coverage.capped, 1);
    assert!(!page.continuation.expect("continuation").exhausted);

    let stale = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Stale),
        chunks,
        BTreeMap::new(),
    )
    .expect("stale projection remains inspectable");
    let outcome = LexicalLane::new(stale)
        .retrieve_lexical(&request)
        .expect("staleness is a typed outcome");
    assert!(matches!(outcome, RetrieverOutcome::Stale(_)));
}

#[test]
fn lexical_source_occurrence_identity_is_generation_exact() {
    let first_generation = id::<CodeGenerationId>("generation.1");
    let second_generation = id::<CodeGenerationId>("generation.2");
    let first_projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&first_generation, FreshnessCompatibilityV1::Current),
        vec![admitted_rust_chunk(
            &first_generation,
            1,
            "pub fn target() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "target",
        )],
        BTreeMap::new(),
    )
    .expect("first projection builds");
    let second_projection = CodeLexicalProjectionAdapterV1::new_admitted(
        projection_metadata(&second_generation, FreshnessCompatibilityV1::Current),
        vec![admitted_rust_chunk(
            &second_generation,
            1,
            "pub fn target() {}\n",
            CodeSearchChunkGrainV1::SymbolSignature,
            "target",
        )],
        BTreeMap::new(),
    )
    .expect("second projection builds");
    let first_request = lexical_request("target", &["target"], &[], &[], 0, 8);
    let mut second_request = lexical_request("target", &["target"], &[], &[], 0, 8);
    second_request.generation = second_generation;

    let first = complete(
        LexicalLane::new(first_projection)
            .retrieve_lexical(&first_request)
            .expect("first retrieval succeeds"),
    );
    let second = complete(
        LexicalLane::new(second_projection)
            .retrieve_lexical(&second_request)
            .expect("second retrieval succeeds"),
    );

    // Symbol occurrence identity binds the file occurrence and the logical
    // symbol identity, not the generation, so an unchanged file keeps one
    // shareable anchor across generations. Generation exactness lives
    // in the source occurrence instead.
    assert_eq!(
        first.candidates[0].anchor_id, second.candidates[0].anchor_id,
        "an unchanged symbol occurrence keeps one anchor across generations"
    );
    assert_ne!(
        first.candidates[0].source_occurrence_id, second.candidates[0].source_occurrence_id,
        "the logical chunk is stable but each generation has a distinct occurrence"
    );
    assert!(
        first.candidates[0]
            .source_occurrence_id
            .as_str()
            .contains(first_request.generation.as_str()),
        "the source occurrence names the generation it was retrieved from"
    );
}
