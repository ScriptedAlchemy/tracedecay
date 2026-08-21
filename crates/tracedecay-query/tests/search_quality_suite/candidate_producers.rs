use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tracedecay_code_index::chunks::{
    DeterministicCodeChunker, ExtractionAdmittedCodeSearchChunkV1, content_digest,
};
use tracedecay_code_index::extract::{LanguageExtractor, NeverCancelled, TreeSitterExtractor};
use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_code_index::production::{
    CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
    CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexProductionConfigV1,
    CodeIndexProductionOwnerV1, CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
    CodeIndexRepositoryParseIdentityV1, VerifiedSealedLexicalPageReadV1,
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
    EphemeralSanitizedQueryViewV1, ExactAdmissionRuleRevision, ExactAdmissionValidator,
    ExactFieldV1, ExactTechnicalTermKindV1, ExactTechnicalTermV1, FileOccurrenceId,
    FreshnessCompatibilityV1, LanguageDescriptorRevision, ManifestDigest, PolicyRevisionId,
    PrincipalId, PrivacyDomainId, ProjectId, ProjectionBatchRequestV1, ProjectionKeyV1,
    ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1, QueryNormalizationRevision,
    RepositoryDirtyStateV1, RepositoryId, RetrievalBudget, RetrievalRequest, RetrievalScope,
    RetrievalSnapshot, RetrieverOutcome, SanitizationReceiptId, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SanitizerRevision, ScoreDomainId, SensitivityDecision,
    SensitivityLevelV1, SingleRootScopeV1, SnapshotFileDispositionV1, SourceFreshness,
    SourceInstanceKey, SourceNamespace, SourceSpan, SymbolOccurrenceId, TemporalModeV1, UtcMicros,
    ValidatedCodeFileV1, VectorWatermark,
};
use tracedecay_query::retrieval::exact::{
    CentralExactAdmissionAuthorityV1, ExactAdmissionAuthority, ExactLane, ExactLaneRequest,
    ExactLaneRetriever,
};
use tracedecay_query::retrieval::lexical::{
    CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1, CodeLexicalArtifactBuilderV1,
    CodeLexicalArtifactErrorV1, CodeLexicalArtifactReaderV1, CodeLexicalProjectionAdapterV1,
    CodeLexicalProjectionBuildStepV1, CodeLexicalProjectionBuildV1,
    CodeLexicalProjectionMetadataV1, LexicalFieldFilterV1, LexicalFieldV1, LexicalLane,
    LexicalLaneRequest, LexicalLaneRetriever, MAX_FUZZY_TERM_EXPANSIONS_V1,
    MAX_LEXICAL_QUERY_TERM_BYTES_V1,
};

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
    let repository = id::<RepositoryId>("repository.artifact");
    let sanitizer_revision = id::<SanitizerRevision>("sanitizer.v1");
    let source = b"import type { Widget } from \"widget-kit\";\nexport function render(value: Widget) { return value; }\n";
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>("file.artifact"),
        logical_path: "src/artifact.ts".to_owned(),
        language: Some(id("typescript")),
        content_digest: content_digest(source),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: repository.clone(),
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: sanitizer_revision.clone(),
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.artifact")],
        content_identity: content_digest(source),
        captured_at: UtcMicros(1_000_000),
        files: vec![file.clone()],
    };
    let request = CodeIndexBuildRequestV1 {
        snapshot,
        captured_files: vec![CodeIndexCapturedFileV1 {
            file_occurrence_id: file.file_occurrence_id.clone(),
            sanitized_bytes: source.to_vec(),
            sensitivity_level: SensitivityLevelV1::Public,
        }],
        changed_files: BTreeSet::from([file.logical_path.clone()]),
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
    let control = ArtifactControl { cancelled: false };
    let mut source = fixture.open_source(maximum_page_chunks);
    let mut pages = Vec::new();
    let receipt = loop {
        match source.next_page(&control).expect("verified lexical page") {
            VerifiedSealedLexicalPageReadV1::Page(page) => pages.push(page),
            VerifiedSealedLexicalPageReadV1::Complete(receipt) => break receipt,
        }
    };
    assert!(!pages.is_empty(), "production source emits lexical pages");
    assert!(
        pages.iter().any(|page| !page.imports().is_empty()),
        "production source emits parser-validated import evidence"
    );
    (fixture, pages, receipt)
}

fn real_verified_pages() -> (
    RealLexicalSourceFixture,
    Vec<VerifiedSealedLexicalPageV1>,
    VerifiedSealedLexicalSourceReceiptV1,
) {
    real_verified_pages_with_maximum_page_chunks(128)
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
            CodeLexicalProjectionBuildStepV1::Ready(projection) => break projection,
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
        let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume(&artifact_path, metadata)
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
    let reader = CodeLexicalArtifactReaderV1::open(
        &artifact_path,
        &verified,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
    )
    .expect("verify and reopen artifact");
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

    let mut request = lexical_request("render", &["render"], &[], &[], 1, 8);
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
    connection
        .execute(
            "UPDATE source_pages SET page_digest = ?1, cumulative_digest = ?2, import_dictionary_digest = ?3 WHERE page_ordinal = 0",
            [
                digest_id::<ManifestDigest>('1').as_str(),
                digest_id::<ManifestDigest>('2').as_str(),
                digest_id::<ManifestDigest>('3').as_str(),
            ],
        )
        .expect("tamper persisted page chain");
    drop(connection);
    assert!(matches!(
        builder.append_page(&pages[1], &control),
        Err(CodeLexicalArtifactErrorV1::Corrupt(_))
    ));
}

#[test]
fn disk_artifact_seal_is_terminal_and_refuses_page_replay() {
    let (fixture, pages, _) = real_verified_pages();
    let metadata = fixture.metadata.clone();
    let directory = tempfile::tempdir().expect("artifact tempdir");
    let artifact_path = directory.path().join("terminal-seal.sqlite");
    let control = ArtifactControl { cancelled: false };
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, metadata).expect("create artifact");
    for page in &pages {
        builder.append_page(page, &control).expect("append page");
    }
    let mut final_source = fixture.open_source(128);
    builder
        .rebuild_and_finalize(&mut final_source, &control)
        .expect("rebuild and finalize artifact");
    assert!(matches!(
        builder.append_page(&pages[0], &control),
        Err(CodeLexicalArtifactErrorV1::Contract(_))
    ));
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
    connection
        .execute("DELETE FROM import_evidence", [])
        .expect("remove derived import evidence before first seal");
    let integrity: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .expect("SQLite integrity check");
    assert_eq!(integrity, "ok");
    drop(connection);

    assert!(matches!(
        builder.finalize(&source_receipt, &control),
        Err(CodeLexicalArtifactErrorV1::Corrupt(_))
    ));
    let mut final_source = fixture.open_source(128);
    builder
        .rebuild_and_finalize(&mut final_source, &control)
        .expect("verified replay rebuilds mutated derived state before sealing");
    let connection = rusqlite::Connection::open(&artifact_path).expect("inspect rebuilt artifact");
    let rebuilt_row: Vec<u8> = connection
        .query_row(
            "SELECT row FROM rows ORDER BY document_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("rebuilt artifact row");
    let rebuilt_term_postings: i64 = connection
        .query_row("SELECT COUNT(*) FROM term_postings", [], |row| row.get(0))
        .expect("rebuilt term posting count");
    let rebuilt_imports: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_evidence", [], |row| row.get(0))
        .expect("rebuilt import evidence count");
    assert_eq!(rebuilt_row, original_row);
    assert_eq!(rebuilt_term_postings, original_term_postings);
    assert_eq!(rebuilt_imports, original_imports);
}

#[test]
fn disk_artifact_rebuild_restores_canonical_artifact_state_before_return() {
    let (fixture, pages, _) = real_verified_pages();
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
            "UPDATE artifact_state SET format_revision = ?1, metadata = ?2, metadata_digest = ?3 WHERE singleton = 1",
            rusqlite::params![2u32, forged_metadata, forged_digest.as_str()],
        )
        .expect("mutate structurally valid pre-seal artifact state");
    let integrity: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .expect("SQLite integrity check");
    assert_eq!(integrity, "ok");
    drop(connection);

    let mut final_source = fixture.open_source(128);
    let verified = builder
        .rebuild_and_finalize(&mut final_source, &control)
        .expect("authoritative replay restores the singleton state");
    let reader = CodeLexicalArtifactReaderV1::open_with_control(
        &artifact_path,
        &verified,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
        &control,
    )
    .expect("returned receipt must reopen immediately");
    assert_eq!(reader.metadata(), &fixture.metadata);
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
    connection
        .execute(
            "UPDATE rows SET row = ?1 WHERE document_id = (SELECT MIN(document_id) FROM rows)",
            [row],
        )
        .expect("mutate artifact row without damaging SQLite structure");
    let integrity: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .expect("SQLite integrity check");
    assert_eq!(integrity, "ok");
    drop(connection);

    for _ in 0..2 {
        assert!(matches!(
            builder.finalize(&source_receipt, &control),
            Err(CodeLexicalArtifactErrorV1::Corrupt(_))
        ));
        assert!(matches!(
            CodeLexicalArtifactReaderV1::open(
                &artifact_path,
                &verified,
                CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
            ),
            Err(CodeLexicalArtifactErrorV1::Corrupt(_))
        ));
    }
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

    let mut resumed = CodeLexicalArtifactBuilderV1::open_or_resume(&artifact_path, metadata)
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
    CodeLexicalArtifactReaderV1::open(
        &artifact_path,
        &verified,
        CODE_LEXICAL_ARTIFACT_QUERY_CACHE_BUDGET_BYTES_V1,
    )
    .expect("cancelled verification must not alter the sealed artifact");
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
