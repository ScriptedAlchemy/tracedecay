use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::{
    CodeIndexImportEvidenceV1, DeterministicCodeChunker, ExtractionAdmittedCodeSearchChunkV1,
    content_digest,
};
use tracedecay_code_index::extract::{LanguageExtractor, NeverCancelled, TreeSitterExtractor};
use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
    CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexInterruptionV1,
    CodeIndexProductionConfigV1, CodeIndexProductionErrorV1, CodeIndexProductionOwnerV1,
    CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    CodeIndexRepositoryParseIdentityV1, VerifiedSealedLexicalPageBatchBoundsV1,
    VerifiedSealedLexicalPageBatchReadV1, VerifiedSealedLexicalPageReadV1,
    VerifiedSealedLexicalPageSourceV1, VerifiedSealedLexicalPageV1,
    VerifiedSealedLexicalSourceReceiptV1,
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
    RetrievalError, RetrievalRequest, RetrievalScope, RetrievalSnapshot, RetrieverOutcome,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    ScoreDomainId, SensitivityDecision, SensitivityLevelV1, SingleRootScopeV1,
    SnapshotFileDispositionV1, SourceFreshness, SourceInstanceKey, SourceNamespace, SourceSpan,
    SymbolOccurrenceId, TemporalModeV1, UtcMicros, ValidatedCodeFileV1, VectorWatermark,
};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
    ExactLaneRetriever, ExactLiteralV1,
};
use tracedecay_query::retrieval::lexical::{
    CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactBuilderV1,
    CodeLexicalArtifactErrorV1, CodeLexicalArtifactFinalizationStepV1, CodeLexicalArtifactReaderV1,
    CodeLexicalProjectionAdapterV1, CodeLexicalProjectionBuildStepV1, CodeLexicalProjectionBuildV1,
    CodeLexicalProjectionMetadataV1, LexicalFieldFilterV1, LexicalFieldV1, LexicalLane,
    LexicalLaneRequest, LexicalLaneRetriever, MAX_FUZZY_TERM_EXPANSIONS_V1,
    MAX_LEXICAL_QUERY_TERM_BYTES_V1, VerifiedCodeLexicalArtifactV1,
};
use tracedecay_query::retrieval::ports::{ExactTermPostingReadPort, LexicalPostingReadPort};

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

#[derive(Default)]
struct ArtifactPublicationStore {
    active: Arc<Mutex<BTreeMap<CodeIndexGenerationScopeV1, Arc<CodeIndexPublishedGenerationV1>>>>,
}

impl CodeIndexAtomicPublicationPort for ArtifactPublicationStore {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .expect("artifact publication lock")
            .get(scope)
            .map(|generation| generation.as_ref().clone()))
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

struct CancelAtObservation {
    cancellation_observation: usize,
    observations: AtomicUsize,
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

#[derive(Clone)]
struct RealLexicalSourceFixture {
    sealed: Vec<u8>,
    state_digest: ManifestDigest,
    metadata: CodeLexicalProjectionMetadataV1,
}

impl RealLexicalSourceFixture {
    fn open_source(
        &self,
        maximum_page_chunks: usize,
    ) -> VerifiedSealedLexicalPageSourceV1<Cursor<Vec<u8>>> {
        VerifiedSealedLexicalPageSourceV1::open(
            Cursor::new(self.sealed.clone()),
            u64::try_from(self.sealed.len()).expect("sealed length"),
            self.state_digest.clone(),
            maximum_page_chunks,
            1024 * 1024,
            &ArtifactControl { cancelled: false },
        )
        .expect("verified sealed lexical source")
    }
}

fn real_lexical_source_fixture() -> RealLexicalSourceFixture {
    real_lexical_source_fixture_with_files(1)
}

/// One real production corpus with `file_count` TypeScript files. The first
/// file keeps the original single-file identity; the rest share its token
/// shape (identical per-field token counts) under distinct symbols so BM25
/// scores tie across files without content-identical chunks.
fn real_lexical_source_fixture_with_files(file_count: usize) -> RealLexicalSourceFixture {
    assert!(file_count >= 1, "fixture needs at least one file");
    let identity_source = b"import type { Widget } from \"widget-kit\";\nexport function render(value: Widget) { return value; }\n";
    let sources = (0..file_count)
        .map(|ordinal| {
            if ordinal == 0 {
                (
                    "file.artifact".to_owned(),
                    "src/artifact.ts".to_owned(),
                    identity_source.to_vec(),
                )
            } else {
                // Zero-padded ordinals keep lexicographic file order equal to
                // generation order, which snapshot intake requires.
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
        .collect();
    real_lexical_source_fixture_from_sources(sources)
}

fn real_lexical_source_fixture_from_sources(
    source_inputs: Vec<(String, String, Vec<u8>)>,
) -> RealLexicalSourceFixture {
    assert!(!source_inputs.is_empty(), "fixture needs at least one file");
    let repository = id::<RepositoryId>("repository.artifact");
    let sanitizer_revision = id::<SanitizerRevision>("sanitizer.v1");
    let sources = source_inputs
        .into_iter()
        .map(|(file_id, logical_path, source)| {
            let file = SanitizedCodeFileV1 {
                file_occurrence_id: id::<FileOccurrenceId>(&file_id),
                logical_path,
                language: Some(id("typescript")),
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
                sanitized_bytes: source.clone(),
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
    let sealed = generation
        .encode_sealed()
        .expect("sealed production generation");
    let envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("sealed generation envelope");
    let state_digest = id::<ManifestDigest>(
        envelope["state_digest"]
            .as_str()
            .expect("sealed state digest"),
    );
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
        exact_score_domain: id::<ScoreDomainId>("score.exact.v1"),
    };
    RealLexicalSourceFixture {
        sealed,
        state_digest,
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

/// Fixture-only source driver retained for legacy regression setup. Production
/// finalization receives the source receipt and never owns a source reader.
trait TestArtifactSourceStaging {
    fn rebuild_and_finalize<R: std::io::Read + std::io::Seek>(
        &mut self,
        source: &mut VerifiedSealedLexicalPageSourceV1<R>,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<VerifiedCodeLexicalArtifactV1, CodeLexicalArtifactErrorV1>;
}

impl TestArtifactSourceStaging for CodeLexicalArtifactBuilderV1 {
    fn rebuild_and_finalize<R: std::io::Read + std::io::Seek>(
        &mut self,
        source: &mut VerifiedSealedLexicalPageSourceV1<R>,
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

pub(crate) fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

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
        exact_score_domain: id::<ScoreDomainId>("score.exact.v1"),
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
        tracedecay_code_extraction::LanguageRegistry::new(),
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
    )
    .expect("one-shot retained lexical projection");
    let mut build = CodeLexicalProjectionBuildV1::new_admitted(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        chunks,
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
    let one_shot = CodeLexicalProjectionAdapterV1::new_admitted(metadata.clone(), chunks.clone())
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
                metadata,
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
fn content_addressed_reader_rejects_atomic_same_size_replacement() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("content-addressed.sqlite");
    let replacement_path = directory.path().join("replacement.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata)
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

#[test]
fn reader_rejects_revision_four_artifact_before_indexed_queries() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("incompatible-state.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata)
        .expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    let connection = rusqlite::Connection::open(&artifact_path).expect("open artifact mutation");
    connection
        .execute(
            "UPDATE artifact_state SET format_revision = 4 WHERE singleton = 1",
            [],
        )
        .expect("write revision-four artifact state");
    drop(connection);

    assert!(matches!(
        CodeLexicalArtifactReaderV1::open_with_control(
            &artifact_path,
            &verified,
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &control,
        ),
        Err(CodeLexicalArtifactErrorV1::Incompatible(_))
    ));
}

#[test]
fn reader_rejects_current_artifact_missing_required_term_statistics_index() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory
        .path()
        .join("missing-term-statistics-index.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata)
        .expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let verified = finish_staged_artifact(&mut builder, &source_receipt, &control);
    let connection = rusqlite::Connection::open(&artifact_path).expect("open artifact mutation");
    connection
        .execute_batch("DROP INDEX term_stats_by_term;")
        .expect("remove required term-statistics index");
    drop(connection);

    assert!(matches!(
        CodeLexicalArtifactReaderV1::open_with_control(
            &artifact_path,
            &verified,
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
    for table in ["field_stats", "term_stats", "vocabulary"] {
        let rows: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count deferred statistic rows");
        assert_eq!(rows, 0, "{table} must be derived after the base freeze");
    }
    let authority_rows: i64 = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM source_pages) + \
                    (SELECT COUNT(*) FROM document_integrity) + \
                    (SELECT COUNT(*) FROM import_integrity) + \
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
                "UPDATE rows SET row = row WHERE document_id = (SELECT MIN(document_id) FROM rows)",
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
    assert_eq!(
        serving_indexes,
        [
            "exact_postings_by_document",
            "ngram_postings_by_document",
            "rows_by_chunk",
            "term_postings_by_document",
            "term_postings_by_document_term",
            "term_postings_by_term",
            "term_stats_by_term",
        ]
    );
    let incorrect_field_stats: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM field_stats AS actual LEFT JOIN (SELECT field, SUM(frequency) AS total_length FROM term_postings GROUP BY field) AS expected USING(field) WHERE actual.total_length != expected.total_length",
            [],
            |row| row.get(0),
        )
        .expect("compare field statistics");
    let incorrect_term_stats: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM term_stats AS actual LEFT JOIN (SELECT field, term, COUNT(*) AS document_frequency FROM term_postings GROUP BY field, term) AS expected USING(field, term) WHERE actual.document_frequency != expected.document_frequency",
            [],
            |row| row.get(0),
        )
        .expect("compare term statistics");
    assert_eq!(incorrect_field_stats, 0);
    assert_eq!(incorrect_term_stats, 0);
}

#[test]
fn disk_artifact_production_wake_commits_one_restartable_setwise_step() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("restartable-setwise-steps.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone()).expect("create");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }

    // Give the aggregate and index statements a production-shaped VM loop.
    // These rows use an authenticated document id, so the freeze accepts the
    // base relation while the later digest would still reject the deliberate
    // test-only document mismatch.
    let connection = rusqlite::Connection::open(&artifact_path).expect("open wide staging file");
    connection
        .execute_batch(
            "WITH digits(value) AS (VALUES(0),(1),(2),(3),(4),(5),(6),(7),(8),(9)),
                  numbers(value) AS (
                      SELECT ones.value + 10 * tens.value + 100 * hundreds.value +
                             1000 * thousands.value + 10000 * ten_thousands.value
                      FROM digits AS ones
                      CROSS JOIN digits AS tens
                      CROSS JOIN digits AS hundreds
                      CROSS JOIN digits AS thousands
                      CROSS JOIN digits AS ten_thousands
                  ),
                  authority AS (
                      SELECT field, document_id FROM term_postings ORDER BY document_id LIMIT 1
                  )
             INSERT INTO term_postings(field, term, document_id, frequency)
             SELECT authority.field, printf('restart-wide-%05d', numbers.value),
                    authority.document_id, 1
             FROM authority CROSS JOIN numbers;",
        )
        .expect("seed production-width posting relation");
    drop(connection);

    assert!(matches!(
        builder
            .advance_finalization(&source_receipt, 4_096, &control)
            .expect("persist base freeze"),
        CodeLexicalArtifactFinalizationStepV1::Pending { .. }
    ));
    assert_eq!(
        persisted_finalization_position(&artifact_path),
        ("statistics".to_owned(), 0)
    );
    assert!(matches!(
        builder
            .advance_finalization(&source_receipt, 4_096, &control)
            .expect("derive only field statistics"),
        CodeLexicalArtifactFinalizationStepV1::Pending { .. }
    ));
    assert_eq!(
        persisted_finalization_position(&artifact_path),
        ("statistics".to_owned(), 1),
        "a production-sized wake commits exactly one corpus-wide step"
    );
    drop(builder);

    let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &artifact_path,
        metadata.clone(),
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("restart after committed field statistics");
    let cancellation = CancelOnBackgroundObservation::new();
    assert!(matches!(
        resumed.advance_finalization(&source_receipt, 4_096, &cancellation),
        Err(CodeLexicalArtifactErrorV1::Interrupted(
            CodeIndexInterruptionV1::Cancelled
        ))
    ));
    assert_eq!(
        persisted_finalization_position(&artifact_path),
        ("statistics".to_owned(), 1),
        "cancellation inside the next SQLite statement must not advance its durable state"
    );
    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect cancelled step");
    let field_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM field_stats", [], |row| row.get(0))
        .expect("count committed field statistics");
    let term_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM term_stats", [], |row| row.get(0))
        .expect("count rolled-back term statistics");
    assert!(
        field_rows > 0,
        "the prior committed step survives cancellation"
    );
    assert_eq!(term_rows, 0, "the interrupted step rolls back atomically");
    drop(connection);
    drop(resumed);

    let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &artifact_path,
        metadata.clone(),
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("restart after cancelled term statistics");
    resumed
        .advance_finalization(&source_receipt, 4_096, &control)
        .expect("retry only term statistics");
    assert_eq!(
        persisted_finalization_position(&artifact_path),
        ("statistics".to_owned(), 2),
        "retry resumes at the interrupted step instead of replaying the frozen prior step"
    );
    drop(resumed);

    let mut expected_indexes = 0i64;
    for expected_position in 0..=7u64 {
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

        if expected_position == 0 {
            assert_eq!(
                persisted_finalization_position(&artifact_path),
                ("indexes".to_owned(), 0),
                "the vocabulary step alone transitions to index construction"
            );
        } else {
            expected_indexes += 1;
            let expected_state = if expected_position == 7 {
                ("digest".to_owned(), 0)
            } else {
                ("indexes".to_owned(), expected_position)
            };
            assert_eq!(
                persisted_finalization_position(&artifact_path),
                expected_state
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
                "each restarted production wake commits exactly one serving index"
            );
        }
    }
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
fn disk_artifact_admission_selects_the_exact_largest_contiguous_prefix() {
    let (fixture, pages, _) = real_verified_pages_with_maximum_page_chunks(1);
    assert!(
        pages.len() >= 2,
        "fixture must expose a real prefix boundary"
    );
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let probe_path = directory.path().join("prefix-probe.sqlite");
    let probe = CodeLexicalArtifactBuilderV1::create(&probe_path, fixture.metadata.clone())
        .expect("create admission probe");
    let first_page_charge = probe
        .page_batch_ledger_charge_bytes(&pages[..1])
        .expect("measure first page charge");
    let exact_budget = probe
        .fixed_ledger_charge_bytes()
        .checked_add(first_page_charge)
        .expect("exact first-page budget");
    drop(probe);

    let artifact_path = directory.path().join("prefix-bound.sqlite");
    let builder = CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        &artifact_path,
        fixture.metadata,
        exact_budget,
    )
    .expect("create exactly bounded builder");
    assert_eq!(
        builder
            .largest_admissible_page_prefix(&pages)
            .expect("select admissible prefix"),
        1,
        "the selector must accept the equality boundary and stop before the first over-budget page"
    );
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
    fixture.sealed.clear();
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
fn disk_artifact_resume_rejects_current_revision_with_wrong_term_index_shape() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("wrong-term-index-shape.sqlite");
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
    for _ in 0..10 {
        assert!(matches!(
            builder
                .advance_finalization(&source_receipt, 4_096, &control)
                .expect("advance one statistics or serving-index step"),
            CodeLexicalArtifactFinalizationStepV1::Pending { .. }
        ));
    }
    drop(builder);

    let connection = rusqlite::Connection::open(&artifact_path).expect("open index mutation");
    connection
        .execute_batch(
            "DROP INDEX term_stats_by_term;
             CREATE INDEX term_stats_by_term ON term_stats(field, term);",
        )
        .expect("replace term-statistics index with wrong column order");
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
                "UPDATE rows SET row = row WHERE document_id = (SELECT MIN(document_id) FROM rows)",
                [],
            )
            .is_err(),
        "the persisted freeze must reject inter-wake mutation"
    );
    drop(connection);

    finish_staged_artifact(&mut builder, &source_receipt, &control);
    assert_eq!(
        builder.progress().expect("source progress after refusal"),
        staged,
        "a changed artifact must not self-attest through later bounded wakes"
    );
}

#[test]
fn disk_artifact_rejects_noncanonical_receipt_reservation_tail() {
    let (fixture, pages, source_receipt) = real_verified_pages_with_maximum_page_chunks(1);
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("noncanonical-receipt.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata)
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
    let progress_before = builder.progress().expect("sealed progress");
    assert!(matches!(
        builder.append_page(&pages[0], &control),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
    assert_eq!(
        builder.progress().expect("progress after rejected replay"),
        progress_before,
        "a sealed artifact must reject an append without changing source progress"
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
fn disk_artifact_first_finalize_rejects_self_attesting_derived_mutation() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("preseal-derived-mutation.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }

    let connection = rusqlite::Connection::open(&artifact_path).expect("open artifact mutation");
    let original_row: Vec<u8> = connection
        .query_row(
            "SELECT row FROM rows ORDER BY document_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("artifact row");
    let original_term_postings: i64 = connection
        .query_row("SELECT COUNT(*) FROM term_postings", [], |row| row.get(0))
        .expect("term posting count");
    let original_imports: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_evidence", [], |row| row.get(0))
        .expect("import evidence count");
    assert!(original_term_postings > 0);
    assert!(original_imports > 0);
    let mut row = original_row.clone();
    row.push(b' ');
    connection
        .execute(
            "UPDATE rows SET row = ?1 WHERE document_id = (SELECT MIN(document_id) FROM rows)",
            [row],
        )
        .expect("mutate derived row before first seal");
    connection
        .execute("DELETE FROM term_postings", [])
        .expect("remove derived term postings before first seal");
    let integrity: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .expect("SQLite integrity check");
    assert_eq!(integrity, "ok");
    drop(connection);

    assert!(matches!(
        builder
            .advance_finalization(&source_receipt, 1, &control)
            .expect("persist freeze before authenticated scan"),
        CodeLexicalArtifactFinalizationStepV1::Pending { .. }
    ));
    let corrupted = loop {
        match builder.advance_finalization(&source_receipt, 4_096, &control) {
            Ok(CodeLexicalArtifactFinalizationStepV1::Pending { .. }) => {}
            result => break result,
        }
    };
    assert!(
        matches!(corrupted, Err(CodeLexicalArtifactErrorV1::Corrupt(_))),
        "mutated derived content must fail authenticated finalization: {corrupted:?}"
    );

    // A corrupted staging file must fail closed. Recovery explicitly restages
    // trusted pages into a fresh artifact; bounded finalization never replays
    // the source just to overwrite mutable derived rows.
    let recovered_path = directory.path().join("recovered-preseal-artifact.sqlite");
    let mut recovered = CodeLexicalArtifactBuilderV1::create(&recovered_path, fixture.metadata)
        .expect("create fresh recovery artifact");
    for page in &pages {
        recovered
            .append_page(page, &control)
            .expect("restage trusted page");
    }
    let verified = finish_staged_artifact(&mut recovered, &source_receipt, &control);
    CodeLexicalArtifactReaderV1::open_with_control(
        &recovered_path,
        &verified,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("freshly staged artifact verifies");
    let connection = rusqlite::Connection::open(&recovered_path).expect("inspect recovery");
    let rebuilt_row: Vec<u8> = connection
        .query_row(
            "SELECT row FROM rows ORDER BY document_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("recovered artifact row");
    let rebuilt_term_postings: i64 = connection
        .query_row("SELECT COUNT(*) FROM term_postings", [], |row| row.get(0))
        .expect("recovered term posting count");
    let rebuilt_imports: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_evidence", [], |row| row.get(0))
        .expect("recovered import evidence count");
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
fn disk_artifact_corruption_is_sticky_across_finalize_and_reopen_retries() {
    let (fixture, pages, source_receipt) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("sticky-corruption.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let mut final_source = fixture.open_source(128);
    let verified = builder
        .rebuild_and_finalize(&mut final_source, &control)
        .expect("rebuild and finalize artifact");

    let connection = rusqlite::Connection::open(&artifact_path).expect("open artifact mutation");
    let mut row: Vec<u8> = connection
        .query_row(
            "SELECT row FROM rows ORDER BY document_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("artifact row");
    row.push(b' ');
    assert!(
        connection
            .execute(
                "UPDATE rows SET row = ?1 WHERE document_id = (SELECT MIN(document_id) FROM rows)",
                [row],
            )
            .is_err(),
        "sealed artifacts deny base-row mutation"
    );
    let integrity: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .expect("SQLite integrity check");
    assert_eq!(integrity, "ok");
    drop(connection);

    assert_eq!(
        builder
            .finalize(&source_receipt, &control)
            .expect("denied mutation preserves sealed receipt"),
        verified
    );
    CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("denied mutation preserves readable artifact");
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
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create artifact");
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
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &reopen_cancellation,
        ),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));
    CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
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
    let (rows, distinct): (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT chunk_id) FROM rows",
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
fn disk_artifact_multi_page_append_rolls_back_the_whole_fresh_suffix() {
    let (fixture, pages, _) = real_verified_pages_with_maximum_page_chunks(1);
    assert!(pages.len() >= 2, "fixture must emit a multi-page batch");
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("atomic-page-batch.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata)
        .expect("create artifact");

    // Force the second page's document insert to fail after the first page
    // has already performed relational writes in the same transaction.
    let second_document = i64::try_from(pages[0].chunk_count()).expect("first page document count");
    let connection = rusqlite::Connection::open(&artifact_path).expect("open failure fixture");
    connection
        .execute(
            "INSERT INTO rows(document_id, chunk_id, row) VALUES (?1, 'forced-conflict', X'7b7d')",
            [second_document],
        )
        .expect("install second-page conflict");
    drop(connection);

    assert!(matches!(
        builder.append_pages(&pages[..2], &control),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
    assert_eq!(
        builder
            .progress()
            .expect("progress after rollback")
            .next_page_ordinal,
        0,
        "a failed later page must roll back every earlier page in the batch"
    );
    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect rolled back batch");
    let receipts: i64 = connection
        .query_row("SELECT COUNT(*) FROM source_pages", [], |row| row.get(0))
        .expect("source receipt count");
    assert_eq!(receipts, 0, "no page receipt may escape the failed batch");
    connection
        .execute("DELETE FROM rows WHERE document_id = ?1", [second_document])
        .expect("remove forced conflict");
    drop(connection);

    let progress = builder
        .append_pages(&pages[..2], &control)
        .expect("retry exact ordered batch");
    assert_eq!(progress.next_page_ordinal, 2);
    assert_eq!(
        progress.completed_chunks,
        pages[0].chunk_count() + pages[1].chunk_count()
    );
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
fn disk_artifact_committed_batch_replays_after_restart_without_duplicate_rows() {
    let (fixture, pages, _) = real_verified_pages_with_maximum_page_chunks(1);
    assert!(pages.len() >= 2, "fixture must emit a multi-page batch");
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("commit-ack-gap.sqlite");
    let metadata = fixture.metadata;
    let mut builder = CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata.clone())
        .expect("create artifact");
    let control = ArtifactControl { cancelled: false };
    builder
        .append_pages(&pages[..2], &control)
        .expect("commit exact ordered batch");
    assert_eq!(
        builder
            .progress()
            .expect("durable progress after commit")
            .next_page_ordinal,
        2,
        "the whole batch must be durable before source acknowledgement"
    );
    drop(builder);
    let mut builder = CodeLexicalArtifactBuilderV1::open_or_resume_with_memory_budget_and_control(
        &artifact_path,
        metadata,
        CODE_LEXICAL_ARTIFACT_BUILD_MEMORY_BUDGET_BYTES_V1,
        &control,
    )
    .expect("resume after a process boundary");
    let replay = builder
        .append_pages(&pages[..2], &control)
        .expect("replay batch after source cursor was not acknowledged");
    assert_eq!(replay.next_page_ordinal, 2);
    assert_eq!(
        staged_row_cardinality(&artifact_path).0,
        pages[0].chunk_count() + pages[1].chunk_count(),
        "restart replay must not duplicate relational rows"
    );
}

#[test]
fn disk_artifact_batch_ledger_charges_every_parallel_preparation_upper_bound() {
    let (fixture, pages, _) = real_verified_pages_with_maximum_page_chunks(1);
    assert!(pages.len() >= 2, "fixture must emit a multi-page batch");
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let builder = CodeLexicalArtifactBuilderV1::create(
        directory.path().join("batch-ledger.sqlite"),
        fixture.metadata,
    )
    .expect("create artifact");
    let control = ArtifactControl { cancelled: false };
    let pages = pages.as_slice();
    assert!(
        pages.iter().any(|page| !page.imports().is_empty()),
        "ledger fixture must retain import evidence"
    );
    assert!(
        pages.iter().flat_map(|page| page.chunks()).any(|chunk| {
            chunk.chunk().sanitized_text.as_str().len() >= 3
                && (!chunk.chunk().subtokens.is_empty() || !chunk.chunk().exact_terms.is_empty())
        }),
        "ledger fixture must exercise term and n-gram preparation"
    );
    let source_retained = pages
        .iter()
        .try_fold(0usize, |total, page| {
            total.checked_add(page.retained_owned_bytes())
        })
        .expect("retained page sum");
    let conservative_charge = builder
        .page_batch_ledger_charge_bytes(pages)
        .expect("batch ledger charge");
    let prepared = builder
        .prepare_pages(pages, &control)
        .expect("prepare deterministic ledger probe");
    let prepared_retained = prepared
        .iter()
        .map(|page| page.retained_owned_bytes())
        .sum::<usize>();
    let effective_workers =
        tracedecay_code_index::parallelism::indexing_workers().min(prepared.len());
    let mut scratch = prepared
        .iter()
        .map(|page| page.preparation_scratch_bytes())
        .collect::<Vec<_>>();
    scratch.sort_unstable_by(|left, right| right.cmp(left));
    let active_scratch = scratch.into_iter().take(effective_workers).sum::<usize>();
    let exact_prepared_charge = source_retained + prepared_retained + active_scratch;
    assert!(
        conservative_charge >= exact_prepared_charge,
        "pre-preparation admission undercounted live batch components: conservative={conservative_charge}, source={source_retained}, prepared={prepared_retained}, active_scratch={active_scratch}, workers={effective_workers}, exact={exact_prepared_charge}"
    );
    for (source, prepared) in pages.iter().zip(&prepared) {
        let conservative = builder
            .page_ledger_charge_bytes(source)
            .expect("one-page ledger charge");
        let exact = prepared
            .ledger_charge_bytes()
            .expect("prepared page charge");
        assert!(
            conservative >= exact,
            "page {} preflight undercounted exact components: conservative={conservative}, source={}, prepared={}, scratch={}, exact={exact}",
            source.page_ordinal(),
            prepared.source_retained_bytes(),
            prepared.retained_owned_bytes(),
            prepared.preparation_scratch_bytes(),
        );
    }
}

#[test]
fn disk_artifact_one_page_wrapper_matches_the_batch_path() {
    let (fixture, pages, _) = real_verified_pages_with_maximum_page_chunks(1);
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let control = ArtifactControl { cancelled: false };
    let mut wrapper = CodeLexicalArtifactBuilderV1::create(
        directory.path().join("one-page-wrapper.sqlite"),
        fixture.metadata.clone(),
    )
    .expect("create wrapper artifact");
    let mut batch = CodeLexicalArtifactBuilderV1::create(
        directory.path().join("one-page-batch.sqlite"),
        fixture.metadata,
    )
    .expect("create batch artifact");

    assert_eq!(
        wrapper
            .append_page(&pages[0], &control)
            .expect("append through wrapper"),
        batch
            .append_pages(&pages[..1], &control)
            .expect("append through batch path")
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
        .query_row("SELECT COUNT(*) FROM rows", [], |row| row.get(0))
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
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create artifact");

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
    // across 115 threads, and VmRSS 6.88GB — past every advertised memory
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
            CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            &exhausted_open,
        ),
        Err(CodeLexicalArtifactErrorV1::Interrupted(_))
    ));
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
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
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create artifact");
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
fn disk_artifact_fixed_ledger_charge_covers_simultaneous_metadata_copies() {
    // Create, open, and rebuild hold two metadata structures plus one
    // serialized JSON copy at once; the fixed charge must grow at least
    // that fast when the metadata grows.
    let fixture = real_lexical_source_fixture_with_files(1);
    let small = fixture.metadata.clone();
    let mut large = small.clone();
    let mut payload_delta = 0usize;
    for ordinal in 0..512usize {
        let file = format!("file.metadata-heavy.{ordinal:04}");
        let path = format!("src/metadata_heavy/module_{ordinal:04}.rs");
        payload_delta += file.len() + path.len();
        large
            .logical_paths
            .insert(id::<FileOccurrenceId>(&file), path);
    }
    let serialized_small = serde_json::to_vec(&small).expect("serialize small metadata");
    let serialized_large = serde_json::to_vec(&large).expect("serialize large metadata");
    let serialized_delta = serialized_large.len() - serialized_small.len();

    let directory = tempfile::tempdir().expect("artifact tempdir");
    let small_fixed =
        CodeLexicalArtifactBuilderV1::create(directory.path().join("metadata-small.sqlite"), small)
            .expect("create small-metadata builder")
            .fixed_ledger_charge_bytes();
    let large_builder = CodeLexicalArtifactBuilderV1::create(
        directory.path().join("metadata-large.sqlite"),
        large.clone(),
    )
    .expect("create large-metadata builder");
    let large_fixed = large_builder.fixed_ledger_charge_bytes();
    drop(large_builder);
    let fixed_delta = large_fixed - small_fixed;
    assert!(
        fixed_delta >= payload_delta * 2 + serialized_delta,
        "the fixed ledger charge must cover both retained metadata structures and the \
         serialized copy: grew {fixed_delta} bytes for {payload_delta} payload bytes and \
         {serialized_delta} serialized bytes"
    );

    // Boundary: a budget the metadata itself exhausts refuses at creation.
    assert!(matches!(
        CodeLexicalArtifactBuilderV1::create_with_memory_budget(
            directory.path().join("metadata-exhausted.sqlite"),
            large.clone(),
            large_fixed,
        ),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
    // Clones shrink string capacities to their lengths, so the boundary is
    // probed with the same clone shape the charge was measured on.
    CodeLexicalArtifactBuilderV1::create_with_memory_budget(
        directory.path().join("metadata-boundary.sqlite"),
        large.clone(),
        large_fixed + 1,
    )
    .expect("one byte above the fixed charge must admit the builder");
}

#[test]
fn sealed_page_retained_bytes_include_digest_identities() {
    // The page carries six heap-owned digest strings (page, cumulative, and
    // two digests on each of the next and previous cursors). The retained
    // accounting must equal the payload recomputation PLUS those digests;
    // an accounting that only counts chunk and import payloads undercounts
    // every ledger charge derived from it.
    let (_, pages, _) = real_verified_pages_with_maximum_page_chunks(1);
    let sha256_digest_len = "sha256:".len() + 64;
    for page in &pages {
        let chunk_bytes = page.chunks().iter().fold(
            page.chunk_capacity()
                .saturating_mul(std::mem::size_of::<ExtractionAdmittedCodeSearchChunkV1>()),
            |bytes, admitted| {
                let chunk = admitted.chunk();
                let exact_term_bytes = chunk.exact_terms.iter().fold(
                    chunk
                        .exact_terms
                        .capacity()
                        .saturating_mul(std::mem::size_of::<ExactTechnicalTermV1>()),
                    |bytes, term| {
                        bytes
                            + term.original_bytes().len()
                            + term.canonical_bytes().len()
                            + term
                                .symbol_occurrence_id()
                                .map_or(0, |occurrence| occurrence.as_str().len())
                    },
                );
                let subtoken_bytes = chunk.subtokens.iter().fold(
                    chunk
                        .subtokens
                        .capacity()
                        .saturating_mul(std::mem::size_of::<String>()),
                    |bytes, subtoken| bytes + subtoken.capacity(),
                );
                bytes
                    + chunk.id.as_str().len()
                    + chunk.anchor.generation_id.as_str().len()
                    + chunk.anchor.file_occurrence_id.as_str().len()
                    + chunk
                        .anchor
                        .symbol_occurrence_id
                        .as_ref()
                        .map_or(0, |occurrence| occurrence.as_str().len())
                    + chunk
                        .anchor
                        .parent_chunk_id
                        .as_ref()
                        .map_or(0, |parent| parent.as_str().len())
                    + chunk.content_digest.as_str().len()
                    + chunk.language_descriptor_revision.as_str().len()
                    + chunk.chunker_revision.as_str().len()
                    + chunk.sanitizer_revision.as_str().len()
                    + chunk.sensitivity.policy_revision.as_str().len()
                    + exact_term_bytes
                    + subtoken_bytes
                    + chunk.sanitized_text.as_str().len()
            },
        );
        let payload_bytes = page.imports().iter().fold(
            chunk_bytes
                + page
                    .import_capacity()
                    .saturating_mul(std::mem::size_of::<CodeIndexImportEvidenceV1>()),
            |bytes, evidence| {
                bytes
                    + evidence.logical_path.capacity()
                    + evidence.file_occurrence_id.as_str().len()
                    + evidence.module_specifier.capacity()
                    + evidence.imported_name.as_ref().map_or(0, String::capacity)
                    + evidence.local_name.as_ref().map_or(0, String::capacity)
            },
        );
        assert_eq!(
            page.retained_owned_bytes(),
            payload_bytes + 8 * sha256_digest_len,
            "page {} retained bytes must include its eight digest identity strings",
            page.page_ordinal()
        );
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
    let one_shot = CodeLexicalProjectionAdapterV1::new_admitted(metadata.clone(), chunks)
        .expect("one-shot lexical projection");
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("top-k.sqlite");
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create artifact");
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
        whole_terms: whole_terms.iter().map(|term| (*term).to_owned()).collect(),
        subtokens: subtokens.iter().map(|term| (*term).to_owned()).collect(),
        phrases: phrases.iter().map(|term| (*term).to_owned()).collect(),
        field_filters: Vec::<LexicalFieldFilterV1>::new(),
        fuzzy_budget,
        lexical_profile_revision: id("lexical-profile.v1"),
        score_domain: id("score.lexical.v1"),
        budget: budget(max_candidates),
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
fn exact_projection_matches_typescript_and_csharp_diagnostic_codes() {
    let generation = id::<CodeGenerationId>("generation.1");
    let source = chunk(
        &generation,
        1,
        CodeSearchChunkGrainV1::SymbolBody,
        "TS1234 CS5678",
        &[
            (ExactTechnicalTermKindV1::CompilerErrorCode, "TS1234"),
            (ExactTechnicalTermKindV1::CompilerErrorCode, "CS5678"),
        ],
        &[],
    );
    let authority =
        CentralExactAdmissionAuthorityV1::new(id::<ExactAdmissionRuleRevision>("exact-rules.v1"));
    let projection = CodeLexicalProjectionAdapterV1::new(
        projection_metadata(&generation, FreshnessCompatibilityV1::Current),
        vec![source],
    )
    .expect("projection builds");
    let query = "ts1234 cs5678";
    let base = base_request(query, 8);
    let query_view = query_view(query);
    let request = ExactLaneRequest {
        literals: authority.parse_literals(&query_view, &base),
        base,
        query_view: &query_view,
        generation,
        budget: budget(8),
    };

    let batch = complete(
        ExactLane::new(authority.clone(), projection.exact_adapter(authority))
            .retrieve_exact(&request)
            .expect("diagnostic retrieval succeeds"),
    );

    assert_eq!(batch.candidates.len(), 1);
    assert_eq!(
        batch.evidence_by_occurrence[&batch.candidates[0].source_occurrence_id]
            .matched_literals
            .len(),
        2
    );
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

    assert_ne!(
        first.candidates[0].anchor_id, second.candidates[0].anchor_id,
        "symbol occurrence anchors are generation-bound"
    );
    assert_ne!(
        first.candidates[0].source_occurrence_id, second.candidates[0].source_occurrence_id,
        "the logical chunk is stable but each generation has a distinct occurrence"
    );
}
