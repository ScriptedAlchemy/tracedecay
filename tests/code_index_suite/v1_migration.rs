use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tracedecay::code_index::chunks::content_digest;
use tracedecay::code_index::extract::{ExtractionCancellation, NeverCancelled};
use tracedecay::code_index::generations::GenerationPlanner;
use tracedecay::code_index::intake::INTAKE_DIGEST_SEPARATOR;
use tracedecay::code_index::v1_import::{
    V1CodeBatchConsumer, V1CodeBatchCountsV1, V1CodeImportErrorV1, V1GenerationRebuilder,
    V1MigrationProvenanceV1, V1SanitizedCodeBatchV1, V1SanitizedCodeRowPayloadV1,
    V1SanitizedCodeRowV1, VerifiedV1SanitizedCodeBatchV1, expected_v1_batch_digest,
};
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationManifestV1, CommitId, ComponentVersion, ContentDigest,
    FileOccurrenceId, LanguageId, ManifestDigest, PrivacyDomainId, RefId, RepositoryId,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    SnapshotFileDispositionV1, UtcMicros, WorktreeId, canonical_sha256,
};

use crate::support::{id, registry};

#[derive(Default)]
struct RebuildState {
    calls: usize,
    provenance: Option<V1MigrationProvenanceV1>,
    rows: Vec<V1SanitizedCodeRowV1>,
}

#[derive(Clone, Default)]
struct RecordingRebuilder {
    state: Rc<RefCell<RebuildState>>,
}

impl V1GenerationRebuilder for RecordingRebuilder {
    fn rebuild_generation(
        &self,
        batch: &VerifiedV1SanitizedCodeBatchV1,
    ) -> Result<CodeGenerationManifestV1, V1CodeImportErrorV1> {
        let mut state = self.state.borrow_mut();
        state.calls += 1;
        state.provenance = Some(batch.provenance().clone());
        state.rows = batch.rows().to_vec();
        drop(state);

        GenerationPlanner::new(
            batch.snapshot().snapshot.repository.clone(),
            registry(),
            id::<ChunkerRevision>("chunker.v1"),
            id::<PrivacyDomainId>("privacy.fixture"),
            7,
        )
        .plan_generation(batch.snapshot(), None, UtcMicros(3_000))
        .map_err(|error| V1CodeImportErrorV1::RebuildFailed(error.to_string()))
    }
}

struct TripsAfter {
    checks: Cell<usize>,
    allowed_checks: usize,
}

impl TripsAfter {
    fn new(allowed_checks: usize) -> Self {
        Self {
            checks: Cell::new(0),
            allowed_checks,
        }
    }
}

impl ExtractionCancellation for TripsAfter {
    fn is_cancelled(&self) -> bool {
        let checks = self.checks.get() + 1;
        self.checks.set(checks);
        checks > self.allowed_checks
    }
}

fn manifest_digest(byte: char) -> ManifestDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn source_file(
    occurrence: &str,
    path: &str,
    bytes: &[u8],
    disposition: SnapshotFileDispositionV1,
) -> SanitizedCodeFileV1 {
    SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>(occurrence),
        logical_path: path.to_owned(),
        language: (disposition == SnapshotFileDispositionV1::Present)
            .then(|| id::<LanguageId>("rust")),
        content_digest: content_digest(bytes),
        disposition,
    }
}

fn batch() -> V1SanitizedCodeBatchV1 {
    let source = b"pub fn migrated() {}\n".to_vec();
    let unsupported = source_file(
        "file.unsupported",
        "assets/legacy.unknown",
        b"unsupported-metadata",
        SnapshotFileDispositionV1::UnsupportedLanguage,
    );
    let supported = source_file(
        "file.supported",
        "src/lib.rs",
        &source,
        SnapshotFileDispositionV1::Present,
    );
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repository.fixture"),
        worktree: Some(id::<WorktreeId>("worktree.fixture")),
        reference: Some(id::<RefId>("refs/heads/main")),
        source_revision: Some(id::<CommitId>("commit.fixture")),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.fixture")],
        content_identity: id::<ContentDigest>(&format!("sha256:{}", "c".repeat(64))),
        captured_at: UtcMicros(1_000),
        files: vec![unsupported, supported],
    };
    let mut batch = V1SanitizedCodeBatchV1 {
        provenance: V1MigrationProvenanceV1 {
            source_generation: "v1-generation-42".to_owned(),
            source_schema_revision: ComponentVersion::new("v1-code-schema.v3").unwrap(),
            importer_revision: ComponentVersion::new("v1-logical-importer.v1").unwrap(),
        },
        snapshot,
        rows: vec![
            V1SanitizedCodeRowV1 {
                source_row_id: "v1-row-supported".to_owned(),
                file_occurrence_id: id("file.supported"),
                payload: V1SanitizedCodeRowPayloadV1::Supported {
                    sanitized_bytes: source,
                },
            },
            V1SanitizedCodeRowV1 {
                source_row_id: "v1-row-unsupported".to_owned(),
                file_occurrence_id: id("file.unsupported"),
                payload: V1SanitizedCodeRowPayloadV1::Unsupported {
                    reason: "legacy language has no registered descriptor".to_owned(),
                },
            },
        ],
        expected_counts: V1CodeBatchCountsV1 {
            total_rows: 2,
            supported_rows: 1,
            unsupported_rows: 1,
            sanitized_bytes: 20,
        },
        expected_digest: manifest_digest('0'),
    };
    batch.expected_counts.sanitized_bytes = match &batch.rows[0].payload {
        V1SanitizedCodeRowPayloadV1::Supported { sanitized_bytes } => sanitized_bytes.len() as u64,
        V1SanitizedCodeRowPayloadV1::Unsupported { .. } => unreachable!(),
    };
    batch.expected_digest = expected_v1_batch_digest(&batch).expect("batch digest");
    batch
}

#[test]
fn logical_batches_rebuild_deterministically_and_preserve_provenance() {
    let rebuilder = RecordingRebuilder::default();
    let state = Rc::clone(&rebuilder.state);
    let consumer = V1CodeBatchConsumer::new(rebuilder, NeverCancelled);
    let input = batch();

    let first = consumer.rebuild(input.clone()).expect("first rebuild");
    let second = consumer.rebuild(input.clone()).expect("replay rebuild");

    assert_eq!(first, second);
    assert_eq!(first.snapshot_digest, consumer_digest(&input));
    let state = state.borrow();
    assert_eq!(state.calls, 2);
    assert_eq!(state.provenance.as_ref(), Some(&input.provenance));
    assert_eq!(state.rows.len(), 2);
    assert!(matches!(
        &state.rows[1].payload,
        V1SanitizedCodeRowPayloadV1::Unsupported { .. }
    ));
}

#[test]
fn declared_counts_and_batch_digest_must_match_logical_rows() {
    let rebuilder = RecordingRebuilder::default();
    let state = Rc::clone(&rebuilder.state);
    let consumer = V1CodeBatchConsumer::new(rebuilder, NeverCancelled);

    let mut wrong_counts = batch();
    wrong_counts.expected_counts.supported_rows = 2;
    assert!(matches!(
        consumer.rebuild(wrong_counts),
        Err(V1CodeImportErrorV1::CountMismatch { .. })
    ));

    let mut wrong_digest = batch();
    wrong_digest.expected_digest = manifest_digest('f');
    assert_eq!(
        consumer.rebuild(wrong_digest),
        Err(V1CodeImportErrorV1::DigestMismatch)
    );
    assert_eq!(state.borrow().calls, 0);
}

#[test]
fn duplicate_source_rows_and_file_occurrences_are_rejected() {
    let consumer = V1CodeBatchConsumer::new(RecordingRebuilder::default(), NeverCancelled);

    let mut duplicate_row = batch();
    duplicate_row.rows[1].source_row_id = duplicate_row.rows[0].source_row_id.clone();
    duplicate_row.expected_digest =
        expected_v1_batch_digest(&duplicate_row).expect("duplicate digest");
    assert_eq!(
        consumer.rebuild(duplicate_row),
        Err(V1CodeImportErrorV1::DuplicateSourceRow(
            "v1-row-supported".to_owned()
        ))
    );

    let mut duplicate_file = batch();
    duplicate_file.rows[1].file_occurrence_id = id("file.supported");
    duplicate_file.expected_digest =
        expected_v1_batch_digest(&duplicate_file).expect("duplicate digest");
    assert_eq!(
        consumer.rebuild(duplicate_file),
        Err(V1CodeImportErrorV1::DuplicateFileOccurrence(id(
            "file.supported"
        )))
    );
}

#[test]
fn unsupported_rows_are_explicit_and_cannot_masquerade_as_supported() {
    let consumer = V1CodeBatchConsumer::new(RecordingRebuilder::default(), NeverCancelled);
    let mut input = batch();
    input.rows[1].payload = V1SanitizedCodeRowPayloadV1::Supported {
        sanitized_bytes: b"pretend source".to_vec(),
    };
    input.expected_counts = V1CodeBatchCountsV1 {
        total_rows: 2,
        supported_rows: 2,
        unsupported_rows: 0,
        sanitized_bytes: input.expected_counts.sanitized_bytes + 14,
    };
    input.expected_digest = expected_v1_batch_digest(&input).expect("batch digest");

    assert!(matches!(
        consumer.rebuild(input),
        Err(V1CodeImportErrorV1::InvalidRow { source_row_id, .. })
            if source_row_id == "v1-row-unsupported"
    ));
}

#[test]
fn cancellation_mid_batch_prevents_generation_rebuild() {
    let rebuilder = RecordingRebuilder::default();
    let state = Rc::clone(&rebuilder.state);
    let consumer = V1CodeBatchConsumer::new(rebuilder, TripsAfter::new(2));

    assert_eq!(
        consumer.rebuild(batch()),
        Err(V1CodeImportErrorV1::Cancelled)
    );
    assert_eq!(state.borrow().calls, 0);
}

#[test]
fn v1_import_boundary_has_no_database_or_filesystem_open_surface() {
    let source = include_str!("../../src/code_index/v1_import.rs");
    for forbidden in [
        "libsql",
        "rusqlite",
        "std::fs",
        "tokio::fs",
        "std::path",
        "PathBuf",
        "Connection",
        "Builder::new_local",
        "File::open",
    ] {
        assert!(
            !source.contains(forbidden),
            "V1 import boundary must not contain {forbidden}"
        );
    }
}

fn consumer_digest(batch: &V1SanitizedCodeBatchV1) -> ManifestDigest {
    canonical_sha256(&(INTAKE_DIGEST_SEPARATOR, &batch.snapshot)).expect("snapshot digest")
}
