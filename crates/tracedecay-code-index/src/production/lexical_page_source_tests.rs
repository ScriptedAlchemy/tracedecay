use std::{
    collections::BTreeSet,
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use tracedecay_domain::{
    ChunkerRevision, FileOccurrenceId, LanguageId, ManifestDigest, PolicyRevisionId,
    PrivacyDomainId, ProjectId, ProjectionKeyV1, ProjectionKindV1, RepositoryDirtyStateV1,
    RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SensitivityLevelV1, SnapshotFileDispositionV1, UtcMicros,
};

use super::*;
use crate::{
    chunks::content_digest,
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
        CodeIndexGenerationScopeV1, CodeIndexInterruptionV1, CodeIndexProductionConfigV1,
        CodeIndexProductionErrorV1, CodeIndexProductionOwnerV1, CodeIndexPublicationStoreErrorV1,
        CodeIndexPublishedGenerationV1, CodeIndexRepositoryParseIdentityV1,
    },
    projection::{
        ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
        ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
    },
};

const BATCH_FIXTURE_SOURCE: &str = concat!(
    "pub fn first_batch_page() -> usize { 1 }\n",
    "pub fn retained_batch_page() -> &'static str { ",
    "\"retained-batch-page-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
    "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\" }\n",
    "pub fn final_batch_page() -> usize { 3 }\n",
);

#[derive(Default)]
struct TestPublicationStore;

impl CodeIndexAtomicPublicationPort for TestPublicationStore {
    fn load_active(
        &self,
        _scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        Ok(None)
    }

    fn publish_atomically(
        &mut self,
        _scope: &CodeIndexGenerationScopeV1,
        _expected_active_generation: Option<&tracedecay_domain::CodeGenerationId>,
        _generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        Ok(())
    }
}

#[derive(Default)]
struct ApplyingProjectionSink;

impl CodeChunkProjectionSink for ApplyingProjectionSink {
    fn project_changed_chunks(
        &mut self,
        request: &tracedecay_domain::ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let mut decisions: Vec<ChunkProjectionDecisionV1> = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: if change.prior_digest.is_some() {
                    tracedecay_domain::ProjectionOperationV1::Updated
                } else {
                    tracedecay_domain::ProjectionOperationV1::Added
                },
                outcome: tracedecay_domain::ProjectionOutcomeV1::Applied,
                output_digest: Some(
                    change
                        .current_digest
                        .clone()
                        .expect("added or changed chunks have a digest"),
                ),
            })
            .collect();
        decisions.extend(
            request
                .changes
                .deleted
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: None,
                    operation: tracedecay_domain::ProjectionOperationV1::Deleted,
                    outcome: tracedecay_domain::ProjectionOutcomeV1::Applied,
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
                    operation: tracedecay_domain::ProjectionOperationV1::Reused,
                    outcome: tracedecay_domain::ProjectionOutcomeV1::Reused,
                    output_digest: None,
                }),
        );
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

struct CancelDuringStaging {
    checks: AtomicUsize,
}

impl CancelDuringStaging {
    fn new() -> Self {
        Self {
            checks: AtomicUsize::new(0),
        }
    }
}

impl CodeIndexExecutionControlV1 for CancelDuringStaging {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, Ordering::AcqRel) >= 3
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
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

struct SealedSourceFixture {
    sealed: Vec<u8>,
    state_digest: ManifestDigest,
    generation: Arc<CodeIndexPublishedGenerationV1>,
}

impl SealedSourceFixture {
    fn open(&self) -> VerifiedSealedLexicalPageSourceV1<Cursor<Vec<u8>>> {
        self.open_with_page_chunks(1)
    }

    fn open_with_page_chunks(
        &self,
        maximum_page_chunks: usize,
    ) -> VerifiedSealedLexicalPageSourceV1<Cursor<Vec<u8>>> {
        VerifiedSealedLexicalPageSourceV1::open(
            Cursor::new(self.sealed.clone()),
            u64::try_from(self.sealed.len()).expect("sealed fixture length fits u64"),
            self.state_digest.clone(),
            maximum_page_chunks,
            1024 * 1024,
            &ActiveControl,
        )
        .expect("real sealed fixture source opens")
    }
}

#[derive(Debug, PartialEq, Eq)]
struct OnePageExpectation {
    page_ordinal: u64,
    chunk_count: u64,
    payload_bytes: u64,
    import_count: u64,
    import_payload_bytes: u64,
    page_digest: String,
    next_cursor: Vec<u8>,
    retained_owned_bytes: usize,
}

fn fixture() -> SealedSourceFixture {
    fixture_for_source(BATCH_FIXTURE_SOURCE)
}

#[test]
fn content_addressed_open_reports_authenticated_scan_progress_and_text_metadata() {
    let fixture = fixture();
    let file_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&fixture.sealed))
        .expect("fixture file digest is canonical");
    let mut progress = Vec::new();
    let source = VerifiedSealedLexicalPageSourceV1::open_content_addressed_with_progress(
        Cursor::new(fixture.sealed.clone()),
        u64::try_from(fixture.sealed.len()).expect("fixture length fits u64"),
        file_digest,
        1,
        1024 * 1024,
        &ActiveControl,
        |scanned, total| progress.push((scanned, total)),
    )
    .expect("authenticated source opens with progress");

    assert_eq!(progress.first(), Some(&(0, fixture.sealed.len() as u64)));
    assert_eq!(
        progress.last(),
        Some(&(fixture.sealed.len() as u64, fixture.sealed.len() as u64))
    );
    assert_eq!(
        source.metadata().snapshot().repository.as_str(),
        "repository.lexical-page-batch"
    );
    assert_eq!(
        source.metadata().snapshot().files[0].logical_path,
        "src/batch_fixture.rs"
    );
    assert_eq!(
        source.metadata().manifest().project_id.as_str(),
        "project.lexical-page-batch"
    );
    assert_eq!(
        source.metadata().manifest().privacy_domain.as_str(),
        "privacy.lexical-page-batch"
    );
}

#[test]
fn published_memory_files_admit_the_same_pages_as_sealed_decode() {
    let fixture = fixture();
    let disk = one_page_expectations(&fixture);
    let mut source = fixture.open();
    source
        .attach_published_files(&fixture.generation)
        .expect("published files attach onto the scanned layout");
    let mut memory = Vec::new();
    loop {
        match source
            .next_page(&ActiveControl)
            .expect("memory-admitted page")
        {
            VerifiedSealedLexicalPageReadV1::Page(page) => {
                memory.push(expectation(&page));
            }
            VerifiedSealedLexicalPageReadV1::Complete(receipt) => {
                receipt
                    .verify_completion(Some(source.cursor()))
                    .expect("memory-admitted receipt verifies");
                break;
            }
        }
    }
    assert_eq!(disk, memory);
}

#[test]
fn partitioned_reopen_reports_encoded_byte_progress_and_bounds_prefetch() {
    let fixture =
        fixture_for_source_files(BATCH_FIXTURE_SOURCE, "src/batch_fixture.rs", "rust", 25);
    let mut segments = BTreeMap::new();
    let manifest = fixture
        .generation
        .encode_partitioned_sealed(|request| {
            if let super::super::SealedGenerationSegmentPublicationV1::File { digest, bytes } =
                request
            {
                segments.insert(digest.clone(), bytes.to_vec());
            }
            Ok(())
        })
        .expect("partitioned generation encodes");
    let manifest_value: serde_json::Value =
        serde_json::from_slice(&manifest).expect("partitioned manifest envelope");
    let segment_sizes = manifest_value["generation"]["file_segments"]
        .as_array()
        .expect("partitioned file descriptors")
        .iter()
        .map(|descriptor| {
            descriptor["segment_size_bytes"]
                .as_u64()
                .expect("partitioned segment size")
        })
        .collect::<Vec<_>>();
    let segments = Arc::new(segments);
    let read_segments = Arc::clone(&segments);
    let reads = Arc::new(AtomicUsize::new(0));
    let read_count = Arc::clone(&reads);
    let mut source = VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
        Cursor::new(Vec::<u8>::new()),
        &manifest,
        fixture.state_digest.clone(),
        move |digest, _, buffer| {
            read_count.fetch_add(1, Ordering::SeqCst);
            buffer.clear();
            buffer.extend_from_slice(read_segments.get(digest).expect("sealed segment exists"));
            Ok(())
        },
        1,
        1 << 20,
    )
    .expect("partitioned source opens")
    .expect("partitioned format");
    let total_segment_bytes = segment_sizes.iter().sum::<u64>();
    assert_eq!(source.total_lexical_units(), total_segment_bytes);
    assert!(source.total_lexical_units() > source.total_files());
    assert_eq!(source.completed_lexical_units().expect("initial bytes"), 0);
    assert_eq!(
        reads.load(Ordering::SeqCst),
        0,
        "opening a page source must not decode every file before the first page"
    );
    assert!(
        source.retained_layout_bytes() < fixture.sealed.len() / 8,
        "compact file identities must remain below an eighth of the decoded corpus encoding"
    );
    source.next_page(&ActiveControl).expect("first page admits");
    assert!(reads.load(Ordering::SeqCst) <= crate::parallelism::indexing_workers().max(1));
    source.rewind().expect("rewind lazy source");
    let mut observed_encoded_byte_progress = false;
    loop {
        match source.next_page(&ActiveControl).expect("lazy source page") {
            VerifiedSealedLexicalPageReadV1::Page(_) => {
                let completed =
                    usize::try_from(source.completed_files()).expect("completed file count");
                let completed_bytes = source.completed_lexical_units().expect("completed bytes");
                assert_eq!(
                    completed_bytes,
                    segment_sizes[..completed].iter().sum::<u64>()
                );
                observed_encoded_byte_progress |= completed_bytes
                    > u64::try_from(completed).expect("completed file count fits u64");
            }
            VerifiedSealedLexicalPageReadV1::Complete(receipt) => {
                receipt
                    .verify_completion(Some(source.cursor()))
                    .expect("verified completion");
                assert_eq!(
                    source.completed_lexical_units().expect("completed bytes"),
                    total_segment_bytes
                );
                assert!(observed_encoded_byte_progress);
                break;
            }
        }
    }
    source.rewind().expect("rewind before cancellation");
    let cursor = source.cursor().clone();
    assert!(matches!(
        source.next_page(&CancelDuringStaging::new()),
        Err(CodeIndexProductionErrorV1::Interrupted(
            CodeIndexInterruptionV1::Cancelled
        ))
    ));
    assert_eq!(
        &cursor,
        source.cursor(),
        "cancelled reads preserve accepted progress"
    );
    let mut corrupt = VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
        Cursor::new(Vec::<u8>::new()),
        &manifest,
        fixture.state_digest.clone(),
        move |digest, _, buffer| {
            buffer.clear();
            buffer.extend_from_slice(segments.get(digest).expect("sealed segment exists"));
            buffer[0] ^= 1;
            Ok(())
        },
        1,
        1 << 20,
    )
    .expect("lazy source authenticates manifest")
    .expect("partitioned format");
    let initial = corrupt.cursor().clone();
    assert!(matches!(
        corrupt.next_page(&ActiveControl),
        Err(CodeIndexProductionErrorV1::Contract(_))
    ));
    assert_eq!(
        corrupt.cursor(),
        &initial,
        "tampered file must not advance the cursor"
    );
}

#[test]
fn incompatible_cursor_restore_drops_the_stale_prefetch_window() {
    let fixture = fixture();
    let mut source = fixture.open();
    let mut incompatible = source.cursor().clone();
    incompatible.next_chunk_ordinal = u64::MAX;

    assert!(matches!(
        source.restore_cursor_classified(&incompatible, &ActiveControl),
        Err(VerifiedSealedLexicalCursorRestoreErrorV1::IncompatiblePosition)
    ));
    assert!(
        source.admitted_window.is_empty(),
        "a rejected stale cursor must not retain its prefetched decode window"
    );
}

#[test]
fn foreign_memory_files_cannot_mint_an_import_cursor_for_a_sealed_source() {
    let imports = (0..128)
        .map(|ordinal| format!("import type {{ Type{ordinal} }} from \"module-{ordinal}\";\n"))
        .collect::<String>();
    let target_source = format!(
        "{imports}{}",
        (0..64)
            .map(|ordinal| {
                format!("export function targetItem{ordinal}(): number {{ return {ordinal}; }}\n")
            })
            .collect::<String>()
    );
    let foreign_source =
        format!("{imports}export function foreignItem(): number {{ return 1; }}\n");
    let target = fixture_for_typescript_source(&target_source);
    let foreign = fixture_for_typescript_source(&foreign_source);
    assert!(
        target
            .generation
            .admitted_chunks()
            .expect("target generation exposes chunks")
            .len()
            > foreign
                .generation
                .admitted_chunks()
                .expect("foreign generation exposes chunks")
                .len(),
        "the authenticated target must have more chunks than the foreign memory source"
    );
    assert!(
        foreign.generation.imports().len() > 1,
        "the foreign source must reach a partial import position"
    );
    let maximum_page_bytes = [&target.generation, &foreign.generation]
        .into_iter()
        .map(|generation| {
            let admitted =
                admit_file_generation_artifacts(generation.files[0].as_ref(), 1, &ActiveControl)
                    .expect("fixture file admits");
            admitted
                .serialized_chunks
                .iter()
                .zip(&admitted.serialized_displays)
                .map(|(chunk, display)| {
                    chunk
                        .len()
                        .saturating_add(display.as_ref().map_or(0, Vec::len))
                })
                .chain(admitted.serialized_imports.iter().map(Vec::len))
                .max()
                .expect("fixture exposes lexical records")
        })
        .max()
        .expect("fixtures expose lexical records")
        .saturating_add(1);
    let foreign_import_bytes = foreign.generation.files[0]
        .artifacts
        .imports
        .iter()
        .map(|evidence| {
            serde_json::to_vec(evidence)
                .expect("import serializes")
                .len()
        })
        .sum::<usize>();
    assert!(
        foreign_import_bytes > maximum_page_bytes,
        "imports must span more than one bounded page"
    );

    let mut source = VerifiedSealedLexicalPageSourceV1::open(
        Cursor::new(target.sealed.clone()),
        u64::try_from(target.sealed.len()).expect("target sealed length fits u64"),
        target.state_digest.clone(),
        usize::MAX,
        maximum_page_bytes,
        &ActiveControl,
    )
    .expect("authenticated target source opens");
    let foreign_was_rejected = source.attach_published_files(&foreign.generation).is_err();

    let boundary_cursor = loop {
        let previous_cursor = source.cursor().clone();
        let read = source
            .next_page_if(&ActiveControl, |page| {
                page.verify_transition(Some(&previous_cursor))
                    .expect("source-minted page verifies before acceptance");
                Ok::<(), std::convert::Infallible>(())
            })
            .expect("source stages the next boundary page");
        let page = match read {
            Ok(VerifiedSealedLexicalPageReadV1::Page(page)) => page,
            Ok(VerifiedSealedLexicalPageReadV1::Complete(_)) => {
                panic!("fixture must expose a partial import cursor")
            }
            Err(never) => match never {},
        };
        if page.next_cursor().next_file_ordinal() == 0
            && page.next_cursor().next_import_ordinal() > 0
        {
            break page.next_cursor().clone();
        }
    };
    let persisted = boundary_cursor
        .persisted_bytes()
        .expect("accepted import cursor persists");
    let cursor_before_cancellation = source.cursor().clone();
    let error = source
        .next_page(&CancelDuringStaging::new())
        .expect_err("cancellation interrupts the next import page");
    assert!(matches!(
        error,
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
    ));
    assert_eq!(
        source.cursor(),
        &cursor_before_cancellation,
        "cancelled staging must preserve the accepted import cursor"
    );

    let restored = VerifiedSealedLexicalCursorV1::restore_persisted(&persisted)
        .expect("accepted import cursor restores");
    let mut resumed = VerifiedSealedLexicalPageSourceV1::open(
        Cursor::new(target.sealed.clone()),
        u64::try_from(target.sealed.len()).expect("target sealed length fits u64"),
        target.state_digest.clone(),
        usize::MAX,
        maximum_page_bytes,
        &ActiveControl,
    )
    .expect("fresh authenticated target source opens");
    resumed
        .restore_cursor(&restored, &ActiveControl)
        .expect("an accepted cursor must resume its authenticated source");
    let resumed_page = resumed
        .next_page_if(&ActiveControl, |page| {
            page.verify_transition(Some(&restored))
                .expect("resumed import page continues the accepted cursor");
            Ok::<(), std::convert::Infallible>(())
        })
        .expect("resumed source stages an import page")
        .expect("resumed page acceptance is infallible");
    let VerifiedSealedLexicalPageReadV1::Page(resumed_page) = resumed_page else {
        panic!("imports must remain after the accepted boundary")
    };
    assert!(
        resumed_page.next_cursor().next_import_ordinal() > restored.next_import_ordinal(),
        "resumed acceptance must advance the import position"
    );
    assert!(
        foreign_was_rejected,
        "decoded files from another generation must not replace sealed source authority"
    );
}

fn fixture_for_source(source: &str) -> SealedSourceFixture {
    fixture_for_source_parts(source, "src/batch_fixture.rs", "rust")
}

fn fixture_for_typescript_source(source: &str) -> SealedSourceFixture {
    fixture_for_source_parts(source, "src/batch_fixture.ts", "typescript")
}

fn fixture_for_source_parts(
    source: &str,
    logical_path: &str,
    language: &str,
) -> SealedSourceFixture {
    fixture_for_source_files(source, logical_path, language, 1)
}

fn fixture_for_source_files(
    source: &str,
    logical_path: &str,
    language: &str,
    file_count: usize,
) -> SealedSourceFixture {
    let source = source.as_bytes();
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: FileOccurrenceId::new("file.lexical-page-batch")
            .expect("fixture file occurrence ID"),
        logical_path: logical_path.to_owned(),
        language: Some(LanguageId::new(language).expect("fixture language ID")),
        content_digest: content_digest(source),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let mut files = (0..file_count)
        .map(|ordinal| {
            let mut file = file.clone();
            if ordinal > 0 {
                file.file_occurrence_id =
                    FileOccurrenceId::new(format!("file.lexical-page-batch-{ordinal}"))
                        .expect("fixture file identity");
                file.logical_path = format!("{ordinal}/{logical_path}");
            }
            file
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| {
        (&left.logical_path, &left.file_occurrence_id)
            .cmp(&(&right.logical_path, &right.file_occurrence_id))
    });
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: RepositoryId::new("repository.lexical-page-batch")
            .expect("fixture repository ID"),
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: SanitizerRevision::new("sanitizer.lexical-page-batch")
            .expect("fixture sanitizer revision"),
        sanitization_receipts: vec![
            SanitizationReceiptId::new("receipt.lexical-page-batch")
                .expect("fixture sanitization receipt"),
        ],
        content_identity: content_digest(source),
        captured_at: UtcMicros(1_000_000),
        files: files.clone(),
    };
    let request = CodeIndexBuildRequestV1 {
        snapshot,
        captured_files: files
            .into_iter()
            .map(|file| CodeIndexCapturedFileV1 {
                file_occurrence_id: file.file_occurrence_id,
                sanitized_bytes: Arc::from(source),
                sensitivity_level: SensitivityLevelV1::Public,
            })
            .collect(),
        changed_files: BTreeSet::new(),
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
            profile_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
                .expect("fixture projection profile digest"),
        },
    };
    let mut owner = CodeIndexProductionOwnerV1::new(
        CodeIndexProductionConfigV1 {
            project_id: ProjectId::new("project.lexical-page-batch").expect("fixture project ID"),
            repository: RepositoryId::new("repository.lexical-page-batch")
                .expect("fixture repository ID"),
            sanitizer_revision: SanitizerRevision::new("sanitizer.lexical-page-batch")
                .expect("fixture sanitizer revision"),
            policy_revision: PolicyRevisionId::new("policy.lexical-page-batch")
                .expect("fixture policy revision"),
            chunker_revision: ChunkerRevision::new("chunker.lexical-page-batch")
                .expect("fixture chunker revision"),
            privacy_domain: PrivacyDomainId::new("privacy.lexical-page-batch")
                .expect("fixture privacy domain"),
            privacy_key_epoch: 7,
            max_snapshot_age_micros: None,
        },
        TestPublicationStore,
        ApplyingProjectionSink,
    )
    .expect("fixture production owner opens");
    let generation = owner
        .build_and_publish(request, &ActiveControl)
        .expect("fixture generation publishes");
    let sealed = generation
        .encode_sealed()
        .expect("fixture generation seals");
    let envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("fixture sealed envelope decodes");
    let state_digest = ManifestDigest::new(
        envelope["state_digest"]
            .as_str()
            .expect("fixture sealed state digest"),
    )
    .expect("fixture state digest is canonical");
    SealedSourceFixture {
        sealed,
        state_digest,
        generation,
    }
}

fn one_page_expectations(fixture: &SealedSourceFixture) -> Vec<OnePageExpectation> {
    let mut source = fixture.open();
    let mut expectations = Vec::new();
    loop {
        match source
            .next_page(&ActiveControl)
            .expect("fixture one-page read")
        {
            VerifiedSealedLexicalPageReadV1::Page(page) => {
                expectations.push(expectation(&page));
            }
            VerifiedSealedLexicalPageReadV1::Complete(receipt) => {
                receipt
                    .verify_completion(Some(source.cursor()))
                    .expect("fixture one-page receipt verifies");
                return expectations;
            }
        }
    }
}

fn expectation(page: &VerifiedSealedLexicalPageV1) -> OnePageExpectation {
    OnePageExpectation {
        page_ordinal: page.page_ordinal(),
        chunk_count: page.chunk_count(),
        payload_bytes: page.payload_bytes(),
        import_count: page.import_count(),
        import_payload_bytes: page.import_payload_bytes(),
        page_digest: page.page_digest().as_str().to_owned(),
        next_cursor: page
            .next_cursor()
            .persisted_bytes()
            .expect("one-page cursor persists"),
        retained_owned_bytes: page.retained_owned_bytes(),
    }
}

fn assert_page_matches(page: &VerifiedSealedLexicalPageV1, expected: &OnePageExpectation) {
    assert_eq!(page.page_ordinal(), expected.page_ordinal);
    assert_eq!(page.chunk_count(), expected.chunk_count);
    assert_eq!(page.payload_bytes(), expected.payload_bytes);
    assert_eq!(page.import_count(), expected.import_count);
    assert_eq!(page.import_payload_bytes(), expected.import_payload_bytes);
    assert_eq!(page.page_digest().as_str(), expected.page_digest.as_str());
    assert_eq!(
        page.next_cursor()
            .persisted_bytes()
            .expect("batch page cursor persists"),
        expected.next_cursor,
    );
}

fn bounds_for(expected: &[OnePageExpectation]) -> VerifiedSealedLexicalPageBatchBoundsV1 {
    let page_slots = std::mem::size_of::<VerifiedSealedLexicalPageV1>()
        .checked_mul(expected.len())
        .expect("fixture page-slot bytes do not overflow");
    let retained_bytes = expected.iter().fold(page_slots, |bytes, page| {
        bytes
            .checked_add(page.retained_owned_bytes)
            .expect("fixture retained bytes do not overflow")
    });
    VerifiedSealedLexicalPageBatchBoundsV1::new(expected.len(), retained_bytes)
        .expect("fixture batch bounds are retainable")
}

fn pages(read: VerifiedSealedLexicalPageBatchReadV1) -> Vec<VerifiedSealedLexicalPageV1> {
    match read {
        VerifiedSealedLexicalPageBatchReadV1::Pages(pages) => pages,
        VerifiedSealedLexicalPageBatchReadV1::Complete(_) => {
            panic!("fixture must stage lexical pages")
        }
    }
}

#[test]
fn batch_bounds_refuse_limits_that_cannot_retain_a_bounded_page_batch() {
    let page_slot_bytes = std::mem::size_of::<VerifiedSealedLexicalPageV1>();
    for (maximum_pages, maximum_retained_bytes) in
        [(0, 1), (1, 0), (1, page_slot_bytes.saturating_sub(1))]
    {
        let error =
            VerifiedSealedLexicalPageBatchBoundsV1::new(maximum_pages, maximum_retained_bytes)
                .expect_err("an unbounded or unretainable batch must be refused");
        assert!(matches!(error, CodeIndexProductionErrorV1::Contract(_)));
    }
}

#[test]
fn rejected_batch_keeps_the_exact_cursor_and_retries_the_first_one_page_value() {
    let fixture = fixture();
    let expected = one_page_expectations(&fixture);
    assert!(
        expected.len() >= 2,
        "fixture must provide a multi-page source"
    );
    let mut source = fixture.open();
    let cursor_before = source
        .cursor()
        .persisted_bytes()
        .expect("initial cursor persists");
    let rejected = source
        .next_page_batch_if(&ActiveControl, bounds_for(&expected[..2]), |pages| {
            assert_eq!(pages.len(), 2, "fixture stages a full two-page batch");
            Err::<NonZeroUsize, _>("builder rejects the complete batch")
        })
        .expect("source stages the rejected batch");
    assert_eq!(
        rejected.expect_err("callback refusal must be surfaced"),
        "builder rejects the complete batch"
    );
    assert_eq!(
        source
            .cursor()
            .persisted_bytes()
            .expect("rejected cursor persists"),
        cursor_before,
    );

    let retried = match source.next_page(&ActiveControl).expect("one-page retry") {
        VerifiedSealedLexicalPageReadV1::Page(page) => page,
        VerifiedSealedLexicalPageReadV1::Complete(_) => panic!("fixture must retain pages"),
    };
    assert_page_matches(&retried, &expected[0]);
}

#[test]
fn rejected_page_can_tighten_future_chunk_bound_without_advancing_cursor() {
    let fixture = fixture();
    let mut source = fixture.open_with_page_chunks(4);
    let cursor_before = source
        .cursor()
        .persisted_bytes()
        .expect("initial cursor persists");
    let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(1, 128 * 1024 * 1024)
        .expect("one-page fixture bound is valid");
    let rejected = source
        .next_page_batch_if(&ActiveControl, bounds, |pages| {
            assert_eq!(pages.len(), 1);
            assert_eq!(pages[0].chunk_count(), 4);
            Err::<NonZeroUsize, _>("builder rejects the four-chunk page")
        })
        .expect("source stages the rejected page");
    assert_eq!(
        rejected.expect_err("callback refusal must be surfaced"),
        "builder rejects the four-chunk page"
    );
    assert_eq!(
        source
            .cursor()
            .persisted_bytes()
            .expect("rejected cursor persists"),
        cursor_before,
    );

    assert_eq!(source.tighten_page_record_bound(), Some((4, 2)));
    let retried = match source.next_page(&ActiveControl).expect("tightened retry") {
        VerifiedSealedLexicalPageReadV1::Page(page) => page,
        VerifiedSealedLexicalPageReadV1::Complete(_) => panic!("fixture must retain pages"),
    };
    assert_eq!(retried.chunk_count(), 2);
    assert_eq!(retried.page_ordinal(), 0);
}

#[test]
fn rejected_import_page_subdivides_without_advancing_cursor() {
    let imports = (0..32)
        .map(|ordinal| format!("import type {{ Type{ordinal} }} from \"module-{ordinal}\";\n"))
        .collect::<String>();
    let fixture = fixture_for_typescript_source(&format!(
        "{imports}export function item(): number {{ return 1; }}\n"
    ));
    let mut source = fixture.open_with_page_chunks(4);
    let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(1, 128 * 1024 * 1024)
        .expect("one-page fixture bound is valid");
    let cursor_before = loop {
        let cursor = source.cursor().clone();
        let read = source.next_page(&ActiveControl).expect("fixture page");
        let VerifiedSealedLexicalPageReadV1::Page(page) = read else {
            panic!("fixture must expose an import-only page");
        };
        if page.chunk_count() == 0 && page.imports().len() == 4 {
            source
                .restore_cursor_classified(&cursor, &ActiveControl)
                .expect("restore uncommitted import page");
            break cursor;
        }
    };
    let rejected = source
        .next_page_batch_if(&ActiveControl, bounds, |pages| {
            assert_eq!(pages.len(), 1);
            assert_eq!(pages[0].chunk_count(), 0);
            assert_eq!(pages[0].imports().len(), 4);
            Err::<NonZeroUsize, _>("builder rejects the four-import page")
        })
        .expect("source stages import page");
    assert_eq!(
        rejected.expect_err("callback refusal must be surfaced"),
        "builder rejects the four-import page"
    );
    assert_eq!(source.cursor(), &cursor_before);
    assert_eq!(source.tighten_page_record_bound(), Some((4, 2)));
    assert_eq!(source.cursor(), &cursor_before);
    let VerifiedSealedLexicalPageReadV1::Page(page) = source
        .next_page(&ActiveControl)
        .expect("subdivided import page")
    else {
        panic!("fixture must retain import records");
    };
    assert_eq!(page.chunk_count(), 0);
    assert_eq!(page.imports().len(), 2);
}

#[test]
fn out_of_range_accepted_prefix_keeps_the_exact_cursor_and_retries_the_first_page() {
    let fixture = fixture();
    let expected = one_page_expectations(&fixture);
    assert!(
        expected.len() >= 2,
        "fixture must provide a multi-page source"
    );
    let mut source = fixture.open();
    let cursor_before = source
        .cursor()
        .persisted_bytes()
        .expect("initial cursor persists");
    let error = source
        .next_page_batch_if(&ActiveControl, bounds_for(&expected[..2]), |pages| {
            assert_eq!(pages.len(), 2, "fixture stages a full two-page batch");
            Ok::<_, ()>(
                NonZeroUsize::new(pages.len() + 1)
                    .expect("out-of-range accepted prefix remains non-zero"),
            )
        })
        .expect_err("out-of-range accepted prefix must be refused");
    assert!(matches!(error, CodeIndexProductionErrorV1::Contract(_)));
    assert_eq!(
        source
            .cursor()
            .persisted_bytes()
            .expect("rejected cursor persists"),
        cursor_before,
    );

    let retried = match source.next_page(&ActiveControl).expect("one-page retry") {
        VerifiedSealedLexicalPageReadV1::Page(page) => page,
        VerifiedSealedLexicalPageReadV1::Complete(_) => panic!("fixture must retain pages"),
    };
    assert_page_matches(&retried, &expected[0]);
}

#[test]
fn count_bound_returns_the_first_two_one_page_values_in_order() {
    let fixture = fixture();
    let expected = one_page_expectations(&fixture);
    assert!(
        expected.len() >= 2,
        "fixture must provide a multi-page source"
    );
    let mut source = fixture.open();
    let batch = source
        .next_page_batch_if(&ActiveControl, bounds_for(&expected[..2]), |pages| {
            assert_eq!(pages.len(), 2);
            Ok::<_, ()>(NonZeroUsize::new(pages.len()).expect("staged batch is non-empty"))
        })
        .expect("source stages a count-bounded batch")
        .expect("callback accepts the count-bounded batch");
    let batch = pages(batch);
    assert_eq!(batch.len(), 2);
    assert_page_matches(&batch[0], &expected[0]);
    assert_page_matches(&batch[1], &expected[1]);
    assert_eq!(
        source
            .cursor()
            .persisted_bytes()
            .expect("batch cursor persists"),
        expected[1].next_cursor,
    );
}

#[test]
fn accepts_only_fifteen_of_sixteen_staged_parser_backed_pages() {
    let source_text = (0..16)
        .map(|index| format!("pub fn batch_prefix_page_{index}() -> usize {{ {index} }}\n"))
        .collect::<String>();
    let fixture = fixture_for_source(&source_text);
    let expected = one_page_expectations(&fixture);
    assert!(
        expected.len() >= 16,
        "parser-backed fixture must expose sixteen one-page values"
    );
    let mut source = fixture.open();
    let accepted = source
        .next_page_batch_if(&ActiveControl, bounds_for(&expected[..16]), |pages| {
            assert_eq!(
                pages.len(),
                16,
                "fixture stages sixteen parser-backed pages"
            );
            Ok::<_, ()>(NonZeroUsize::new(15).expect("fifteen is non-zero"))
        })
        .expect("source stages the parser-backed batch")
        .expect("callback accepts a fifteen-page prefix");
    let accepted = pages(accepted);
    assert_eq!(accepted.len(), 15);
    for (page, expected) in accepted.iter().zip(&expected[..15]) {
        assert_page_matches(page, expected);
    }
    assert_eq!(
        source
            .cursor()
            .persisted_bytes()
            .expect("accepted-prefix cursor persists"),
        expected[14].next_cursor,
    );

    let next = match source
        .next_page(&ActiveControl)
        .expect("read the first unaccepted page")
    {
        VerifiedSealedLexicalPageReadV1::Page(page) => page,
        VerifiedSealedLexicalPageReadV1::Complete(_) => {
            panic!("the sixteenth staged page must remain available")
        }
    };
    assert_page_matches(&next, &expected[15]);
}

#[test]
fn retained_byte_bound_stops_before_the_next_larger_one_page_value() {
    let fixture = fixture();
    let expected = one_page_expectations(&fixture);
    let (start, first, second) = expected
        .windows(2)
        .enumerate()
        .find_map(|(index, pair)| {
            (pair[0].retained_owned_bytes < pair[1].retained_owned_bytes)
                .then_some((index, &pair[0], &pair[1]))
        })
        .expect("fixture has an increasing one-page retained-byte boundary");
    let mut source = fixture.open();
    for _ in 0..start {
        let _ = source
            .next_page(&ActiveControl)
            .expect("advance to retained boundary");
    }
    let page_slots = std::mem::size_of::<VerifiedSealedLexicalPageV1>()
        .checked_mul(2)
        .expect("fixture page-slot bytes do not overflow");
    let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(
        2,
        page_slots
            .checked_add(first.retained_owned_bytes)
            .expect("fixture retained bound does not overflow"),
    )
    .expect("first page fits the retained-byte bound");
    let batch = source
        .next_page_batch_if(&ActiveControl, bounds, |pages| {
            assert_eq!(pages.len(), 1, "larger next page must stay unstaged");
            Ok::<_, ()>(NonZeroUsize::new(pages.len()).expect("staged batch is non-empty"))
        })
        .expect("source stages the retained-byte-bounded batch")
        .expect("callback accepts the retained-byte-bounded batch");
    let batch = pages(batch);
    assert_eq!(batch.len(), 1);
    assert_page_matches(&batch[0], first);
    assert_eq!(
        source
            .cursor()
            .persisted_bytes()
            .expect("retained-byte cursor persists"),
        first.next_cursor,
    );

    let next = match source
        .next_page(&ActiveControl)
        .expect("read byte-stopped page")
    {
        VerifiedSealedLexicalPageReadV1::Page(page) => page,
        VerifiedSealedLexicalPageReadV1::Complete(_) => panic!("fixture must retain next page"),
    };
    assert_page_matches(&next, second);
}

#[test]
fn completion_follows_the_last_accepted_batch_without_an_empty_callback() {
    let fixture = fixture();
    let expected = one_page_expectations(&fixture);
    assert!(!expected.is_empty(), "fixture must provide lexical pages");
    let mut source = fixture.open();
    let accepted = source
        .next_page_batch_if(&ActiveControl, bounds_for(&expected), |pages| {
            assert_eq!(pages.len(), expected.len());
            Ok::<_, ()>(NonZeroUsize::new(pages.len()).expect("staged batch is non-empty"))
        })
        .expect("source stages the final batch")
        .expect("callback accepts the final batch");
    let accepted = pages(accepted);
    assert_eq!(accepted.len(), expected.len());
    for (page, expected) in accepted.iter().zip(&expected) {
        assert_page_matches(page, expected);
    }

    let mut callback_called = false;
    let complete = source
        .next_page_batch_if(&ActiveControl, bounds_for(&expected), |_| {
            callback_called = true;
            Ok::<_, ()>(NonZeroUsize::MIN)
        })
        .expect("completed source stays readable")
        .expect("completion has no callback error");
    let VerifiedSealedLexicalPageBatchReadV1::Complete(receipt) = complete else {
        panic!("completion follows the last accepted batch")
    };
    assert!(
        !callback_called,
        "completion must not invoke an empty callback"
    );
    receipt
        .verify_completion(Some(source.cursor()))
        .expect("completed receipt matches accepted cursor");
}

#[test]
fn cancellation_during_staging_keeps_the_exact_pre_batch_cursor() {
    let fixture = fixture();
    let expected = one_page_expectations(&fixture);
    assert!(
        expected.len() >= 2,
        "fixture must provide a multi-page source"
    );
    let mut source = fixture.open();
    let cursor_before = source
        .cursor()
        .persisted_bytes()
        .expect("initial cursor persists");
    let control = CancelDuringStaging::new();
    let mut callback_called = false;
    let error = source
        .next_page_batch_if(&control, bounds_for(&expected[..2]), |_| {
            callback_called = true;
            Ok::<_, ()>(NonZeroUsize::MIN)
        })
        .expect_err("cancellation must interrupt batch staging");
    assert!(matches!(
        error,
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
    ));
    assert!(
        control.checks.load(Ordering::Acquire) > 1,
        "cancellation must be checked during source staging"
    );
    assert!(
        !callback_called,
        "cancelled staging must not invoke the callback"
    );
    assert_eq!(
        source
            .cursor()
            .persisted_bytes()
            .expect("cancelled cursor persists"),
        cursor_before,
    );
}

#[test]
fn large_string_layout_scan_skips_non_structural_bytes() {
    const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
    let file = format!(r#"{{"payload":"{}"}}"#, "x".repeat(PAYLOAD_BYTES));
    let generation = format!(r#"{{"format_revision":6,"files":[{file}]}}"#);
    let state_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(generation.as_bytes()))
        .expect("synthetic generation digest is canonical");
    let sealed = format!(
        r#"{{"state_digest":"{}","generation":{generation}}}"#,
        state_digest.as_str()
    )
    .into_bytes();
    let first_file_offset = sealed
        .windows(b"{\"payload\"".len())
        .position(|window| window == b"{\"payload\"")
        .expect("synthetic file object is present");
    let files_end_offset = first_file_offset
        .checked_add(file.len())
        .expect("synthetic files end fits usize");

    let layout = scan_layout(
        &mut Cursor::new(&sealed),
        u64::try_from(sealed.len()).expect("synthetic seal length fits u64"),
        None,
        &ActiveControl,
    )
    .expect("synthetic seal has a valid lexical layout");

    assert_eq!(layout.state_digest, state_digest);
    assert_eq!(layout.format_revision, 6);
    assert_eq!(layout.file_count, 1);
    assert_eq!(
        layout.first_file_offset,
        u64::try_from(first_file_offset).expect("synthetic file offset fits u64")
    );
    assert_eq!(
        layout.files_end_offset,
        u64::try_from(files_end_offset).expect("synthetic files end fits u64")
    );
    assert_eq!(
        layout.maximum_file_bytes,
        u64::try_from(file.len()).expect("synthetic file length fits u64")
    );
    assert_eq!(
        layout.file_ranges,
        [(
            layout.first_file_offset,
            layout.first_file_offset
                + u64::try_from(file.len()).expect("synthetic file length fits u64")
        )]
    );
    assert!(
        layout.structural_byte_visits < 1024,
        "an 8 MiB JSON string should require bounded structural visits, observed {}",
        layout.structural_byte_visits
    );
}

#[test]
fn layout_scan_preserves_digest_and_file_boundaries_across_escaped_syntax() {
    let first_file = r#"{"payload":"escaped \\\" quote and { [ ] } syntax"}"#;
    let second_file = format!(r#"{{"payload":"{}"}}"#, "y".repeat(96 * 1024));
    let generation =
        format!(r#"{{"format_revision":6,"files":[{first_file},{second_file}],"tail":"done"}}"#);
    let state_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(generation.as_bytes()))
        .expect("synthetic generation digest is canonical");
    let sealed = format!(
        r#"{{"state_digest":"{}","generation":{generation}}}"#,
        state_digest.as_str()
    )
    .into_bytes();
    let file_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&sealed))
        .expect("synthetic file digest is canonical");
    let first_file_offset = sealed
        .windows(first_file.len())
        .position(|window| window == first_file.as_bytes())
        .expect("first synthetic file is present");
    let files_end_offset = first_file_offset + first_file.len() + 1 + second_file.len();

    let layout = scan_layout(
        &mut Cursor::new(&sealed),
        u64::try_from(sealed.len()).expect("synthetic seal length fits u64"),
        Some(&file_digest),
        &ActiveControl,
    )
    .expect("escaped syntax does not alter the authenticated layout");

    assert_eq!(layout.state_digest, state_digest);
    assert_eq!(layout.file_count, 2);
    assert_eq!(layout.first_file_offset, first_file_offset as u64);
    assert_eq!(layout.files_end_offset, files_end_offset as u64);
    assert_eq!(layout.maximum_file_bytes, second_file.len() as u64);
    assert_eq!(
        layout.file_ranges,
        [
            (
                first_file_offset as u64,
                (first_file_offset + first_file.len()) as u64
            ),
            (
                (first_file_offset + first_file.len() + 1) as u64,
                files_end_offset as u64
            )
        ]
    );
}

#[test]
fn layout_scan_rejects_cancelled_and_corrupted_sources() {
    let file = format!(r#"{{"payload":"{}"}}"#, "z".repeat(512 * 1024));
    let generation = format!(r#"{{"format_revision":6,"files":[{file}]}}"#);
    let state_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(generation.as_bytes()))
        .expect("synthetic generation digest is canonical");
    let sealed = format!(
        r#"{{"state_digest":"{}","generation":{generation}}}"#,
        state_digest.as_str()
    )
    .into_bytes();

    let cancellation = CancelDuringStaging::new();
    let cancelled = match scan_layout(
        &mut Cursor::new(&sealed),
        sealed.len() as u64,
        None,
        &cancellation,
    ) {
        Ok(_) => panic!("layout opening must honor bounded read checkpoints"),
        Err(error) => error,
    };
    assert!(matches!(
        cancelled,
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
    ));

    let mut corrupted = sealed;
    let payload = corrupted
        .windows(b"zzzz".len())
        .position(|window| window == b"zzzz")
        .expect("synthetic payload is present");
    corrupted[payload] = b'x';
    let error = match scan_layout(
        &mut Cursor::new(&corrupted),
        corrupted.len() as u64,
        None,
        &ActiveControl,
    ) {
        Ok(_) => panic!("payload corruption must fail the exact generation digest"),
        Err(error) => error,
    };
    assert!(matches!(error, CodeIndexProductionErrorV1::Contract(_)));
}

#[test]
fn layout_scanner_retains_a_constant_string_window() {
    let file = format!(r#"{{"payload":"{}"}}"#, "w".repeat(8 * 1024 * 1024));
    let generation = format!(r#"{{"format_revision":6,"files":[{file}]}}"#);
    let state_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(generation.as_bytes()))
        .expect("synthetic generation digest is canonical");
    let sealed = format!(
        r#"{{"state_digest":"{}","generation":{generation}}}"#,
        state_digest.as_str()
    )
    .into_bytes();
    let mut scanner = LayoutScanner::default();
    for (chunk_ordinal, chunk) in sealed.chunks(64 * 1024).enumerate() {
        scanner
            .observe_slice(chunk, (chunk_ordinal * 64 * 1024) as u64)
            .expect("bounded chunk scan succeeds");
        assert!(scanner.string_len <= scanner.string.len());
        assert!(std::mem::size_of::<LayoutScanner>() < 1024);
    }
    let layout = scanner.finish().expect("bounded scanner layout verifies");
    assert_eq!(layout.file_count, 1);
    assert_eq!(layout.state_digest, state_digest);
}

#[test]
fn layout_scan_does_not_allocate_for_unrelated_short_strings() {
    let values = (0..50_000)
        .map(|index| format!(r#""term-{index}""#))
        .collect::<Vec<_>>()
        .join(",");
    let file = format!(r#"{{"payload":[{values}]}}"#);
    let generation = format!(r#"{{"format_revision":6,"files":[{file}]}}"#);
    let state_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(generation.as_bytes()))
        .expect("synthetic generation digest is canonical");
    let sealed = format!(
        r#"{{"state_digest":"{}","generation":{generation}}}"#,
        state_digest.as_str()
    )
    .into_bytes();

    let layout = scan_layout(
        &mut Cursor::new(&sealed),
        sealed.len() as u64,
        None,
        &ActiveControl,
    )
    .expect("short-string-heavy layout verifies");

    assert!(
        layout.temporary_string_allocations <= 1,
        "only the authenticated state digest may require a temporary string, observed {} allocations",
        layout.temporary_string_allocations
    );
}
