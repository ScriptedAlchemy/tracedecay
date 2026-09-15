//! Deterministic, operator-data-free comparison of lexical artifact ingestion
//! transaction shapes. This target intentionally owns its fixture because
//! query's production fixture helpers are test-private.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tracedecay_code_index::chunks::content_digest;
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
    ChunkerRevision, CodeGenerationId, ComponentRevision, FileOccurrenceId,
    FreshnessCompatibilityV1, ManifestDigest, PolicyRevisionId, PrivacyDomainId, ProjectId,
    ProjectionBatchRequestV1, ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1,
    ProjectionOutcomeV1, RepositoryDirtyStateV1, RepositoryId, SanitizationReceiptId,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision, ScoreDomainId,
    SensitivityLevelV1, SnapshotFileDispositionV1, SourceFreshness, SourceInstanceKey,
    SourceNamespace, UtcMicros,
};
use tracedecay_query::retrieval::lexical::{
    CodeLexicalArtifactBuilderV1, CodeLexicalArtifactFinalizationStepV1,
    CodeLexicalProjectionMetadataV1, VerifiedCodeLexicalArtifactV1,
};

const FIXTURE_FILE_COUNT: usize = 48;
const BATCH_PAGE_LIMIT: usize = 16;

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
struct PublicationStore {
    active: Arc<Mutex<BTreeMap<CodeIndexGenerationScopeV1, Arc<CodeIndexPublishedGenerationV1>>>>,
}

impl CodeIndexAtomicPublicationPort for PublicationStore {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .expect("benchmark publication lock")
            .get(scope)
            .map(Arc::clone))
    }

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self.active.lock().expect("benchmark publication lock");
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

struct ProjectionSink;

impl CodeChunkProjectionSink for ProjectionSink {
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

struct Fixture {
    metadata: CodeLexicalProjectionMetadataV1,
    pages: Vec<VerifiedSealedLexicalPageV1>,
    source_receipt: VerifiedSealedLexicalSourceReceiptV1,
}

#[derive(Clone, Copy)]
enum IngestionMode {
    OnePage,
    BoundedBatch,
}

impl IngestionMode {
    const fn name(self) -> &'static str {
        match self {
            Self::OnePage => "one_page",
            Self::BoundedBatch => "bounded_batch",
        }
    }
}

#[derive(Serialize)]
struct RunReport {
    mode: &'static str,
    ingest_wall_ns: u64,
    end_to_end_wall_ns: u64,
    committed_pages: u64,
    committed_chunks: u64,
    committed_payload_bytes: u64,
    sqlite_ingestion_commits: u64,
    artifact_bytes: u64,
    artifact_digest: String,
    source_cumulative_digest: String,
}

struct RunResult {
    report: RunReport,
    receipt: VerifiedCodeLexicalArtifactV1,
}

#[derive(Serialize)]
struct ComparisonReport {
    fixture_pages: usize,
    batch_page_limit: usize,
    one_page: RunReport,
    bounded_batch: RunReport,
    final_receipt_equal: bool,
    artifact_digest_equal: bool,
    source_cumulative_digest_equal: bool,
}

fn main() {
    configure_hotpath();
    // Dropped when `main` returns, so a requested profile observes both
    // compared ingestion paths.
    let _hotpath = hotpath::HotpathGuardBuilder::new("code-lexical-catchup-bench").build();
    let fixture = build_fixture();
    let one_page = run(&fixture, IngestionMode::OnePage);
    let bounded_batch = run(&fixture, IngestionMode::BoundedBatch);

    let final_receipt_equal = one_page.receipt == bounded_batch.receipt;
    let artifact_digest_equal =
        one_page.receipt.artifact_digest() == bounded_batch.receipt.artifact_digest();
    let source_cumulative_digest_equal = one_page.receipt.source_cumulative_digest()
        == bounded_batch.receipt.source_cumulative_digest();
    let report = ComparisonReport {
        fixture_pages: fixture.pages.len(),
        batch_page_limit: BATCH_PAGE_LIMIT,
        one_page: one_page.report,
        bounded_batch: bounded_batch.report,
        final_receipt_equal,
        artifact_digest_equal,
        source_cumulative_digest_equal,
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("serialize benchmark report")
    );
    assert!(
        final_receipt_equal && artifact_digest_equal && source_cumulative_digest_equal,
        "the compared ingestion paths must produce the exact same final receipt and digests"
    );
}

/// Mirrors `tracedecay-index-bench`'s guard defaults: stdout here carries the
/// machine-read comparison JSON, so the hotpath report goes to
/// `HOTPATH_OUTPUT_PATH` when one is named and nowhere otherwise, and the
/// localhost metrics server stays off. This runs as the first statement of
/// `main`, before any other thread exists, which makes `set_var` sound.
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

fn build_fixture() -> Fixture {
    let repository = id::<RepositoryId>("repository.catchup-benchmark");
    let sanitizer_revision = id::<SanitizerRevision>("sanitizer.catchup-benchmark.v1");
    let sources = (0..FIXTURE_FILE_COUNT)
        .map(|ordinal| {
            let file_id = format!("file.catchup.{ordinal:03}");
            let logical_path = format!("src/catchup_{ordinal:03}.ts");
            let source = format!(
                "import type {{ Widget }} from \"widget-kit\";\nexport function render_{ordinal:03}(value: Widget) {{ return value; }}\n"
            )
            .into_bytes();
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
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: repository.clone(),
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: sanitizer_revision.clone(),
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.catchup-benchmark")],
        content_identity: content_digest(&sources[0].1),
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
        project_id: id::<ProjectId>("project.catchup-benchmark"),
        repository: repository.clone(),
        sanitizer_revision,
        policy_revision: id::<PolicyRevisionId>("policy.catchup-benchmark.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.catchup-benchmark.v1"),
        privacy_domain: id::<PrivacyDomainId>("privacy.catchup-benchmark"),
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    };
    let control = ActiveControl;
    let mut owner =
        CodeIndexProductionOwnerV1::new(config, PublicationStore::default(), ProjectionSink)
            .expect("build deterministic production fixture owner");
    let generation = owner
        .build_and_publish(request, &control)
        .expect("build deterministic sealed generation");
    let sealed = generation
        .encode_sealed()
        .expect("encode deterministic sealed generation");
    let sealed_len = u64::try_from(sealed.len()).expect("sealed generation length");
    let envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("decode sealed generation envelope");
    let state_digest = id::<ManifestDigest>(
        envelope["state_digest"]
            .as_str()
            .expect("sealed generation state digest"),
    );
    let metadata = CodeLexicalProjectionMetadataV1 {
        generation: generation.manifest().generation_id.clone(),
        repository_id: Some(repository),
        logical_paths: generation
            .snapshot()
            .files
            .iter()
            .map(|file| (file.file_occurrence_id.clone(), file.logical_path.clone()))
            .collect(),
        freshness: freshness(),
        exact_retriever_revision: id::<ComponentRevision>("retriever.exact.catchup-benchmark.v1"),
        lexical_retriever_revision: id::<ComponentRevision>(
            "retriever.lexical.catchup-benchmark.v1",
        ),
        exact_score_domain: id::<ScoreDomainId>("score.exact.catchup-benchmark.v1"),
    };
    let mut source = VerifiedSealedLexicalPageSourceV1::open(
        Cursor::new(sealed),
        sealed_len,
        state_digest,
        1,
        1024 * 1024,
        &control,
    )
    .expect("open verified lexical page source");
    let mut pages = Vec::new();
    let source_receipt = loop {
        match source
            .next_page(&control)
            .expect("read verified lexical page")
        {
            VerifiedSealedLexicalPageReadV1::Page(page) => pages.push(page),
            VerifiedSealedLexicalPageReadV1::Complete(receipt) => break receipt,
        }
    };
    assert!(
        pages.len() > BATCH_PAGE_LIMIT,
        "fixture must provide more pages than one bounded batch"
    );
    Fixture {
        metadata,
        pages,
        source_receipt,
    }
}

fn run(fixture: &Fixture, mode: IngestionMode) -> RunResult {
    let directory = tempfile::tempdir().expect("create benchmark artifact directory");
    let artifact_path = directory.path().join(format!("{}.sqlite", mode.name()));
    let control = ActiveControl;
    let mut builder =
        CodeLexicalArtifactBuilderV1::create(&artifact_path, fixture.metadata.clone())
            .expect("create isolated benchmark artifact");
    let started = Instant::now();
    let (progress, sqlite_ingestion_commits) = match mode {
        IngestionMode::OnePage => {
            let mut progress = builder.progress().expect("read initial artifact progress");
            for page in &fixture.pages {
                progress = builder
                    .append_page(page, &control)
                    .expect("append deterministic benchmark page");
            }
            (
                progress,
                u64::try_from(fixture.pages.len()).expect("page count fits u64"),
            )
        }
        IngestionMode::BoundedBatch => {
            let mut progress = builder.progress().expect("read initial artifact progress");
            let mut commits = 0_u64;
            for pages in fixture.pages.chunks(BATCH_PAGE_LIMIT) {
                progress = builder
                    .append_pages(pages, &control)
                    .expect("append deterministic benchmark page batch");
                commits = commits.checked_add(1).expect("commit count fits u64");
            }
            (progress, commits)
        }
    };
    let ingest_wall_ns = elapsed_ns(started.elapsed());
    let receipt = finalize(&mut builder, &fixture.source_receipt, &control);
    let end_to_end_wall_ns = elapsed_ns(started.elapsed());
    assert_eq!(
        progress.next_page_ordinal,
        u64::try_from(fixture.pages.len()).expect("page count fits u64"),
        "ingestion must durably commit every verified fixture page"
    );
    RunResult {
        report: RunReport {
            mode: mode.name(),
            ingest_wall_ns,
            end_to_end_wall_ns,
            committed_pages: progress.next_page_ordinal,
            committed_chunks: progress.completed_chunks,
            committed_payload_bytes: progress.completed_payload_bytes,
            sqlite_ingestion_commits,
            artifact_bytes: receipt.file_size_bytes(),
            artifact_digest: receipt.artifact_digest().as_str().to_owned(),
            source_cumulative_digest: receipt.source_cumulative_digest().as_str().to_owned(),
        },
        receipt,
    }
}

fn finalize(
    builder: &mut CodeLexicalArtifactBuilderV1,
    source_receipt: &VerifiedSealedLexicalSourceReceiptV1,
    control: &dyn CodeIndexExecutionControlV1,
) -> VerifiedCodeLexicalArtifactV1 {
    loop {
        match builder
            .advance_finalization(source_receipt, 4_096, control)
            .expect("finalize deterministic benchmark artifact")
        {
            CodeLexicalArtifactFinalizationStepV1::Pending { .. } => {}
            CodeLexicalArtifactFinalizationStepV1::Ready(receipt) => return *receipt,
        }
    }
}

fn freshness() -> SourceFreshness {
    SourceFreshness {
        source_namespace: id::<SourceNamespace>("ns.code.catchup-benchmark"),
        source_instance: id::<SourceInstanceKey>("instance.catchup-benchmark"),
        source_watermark: Some(7),
        projection_watermark: Some(7),
        observed_at: UtcMicros(7),
        source_generation: Some(1),
        generation_lag: Some(0),
        compatibility: FreshnessCompatibilityV1::Current,
        policy_revision: id("policy.catchup-benchmark.v1"),
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid deterministic benchmark identity")
}

fn digest_id<T>(byte: char) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn elapsed_ns(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).expect("benchmark wall time fits u64")
}
