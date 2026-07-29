use tracedecay_code_index::chunks::content_digest as source_digest;
use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, FileOccurrenceId, IntakeRejectionV1, LanguageId,
    RefId, RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SnapshotFileDispositionV1, UtcMicros, ValidatedCodeFileV1, WorktreeId,
};

use crate::support::{id, registry};

fn content_digest(byte: char) -> ContentDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn file(
    occurrence: &str,
    path: &str,
    language: Option<&str>,
    disposition: SnapshotFileDispositionV1,
) -> SanitizedCodeFileV1 {
    SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>(occurrence),
        logical_path: path.to_owned(),
        language: language.map(id::<LanguageId>),
        content_digest: content_digest('a'),
        disposition,
    }
}

fn snapshot(mut files: Vec<SanitizedCodeFileV1>) -> SanitizedCodeSnapshotV1 {
    files.sort_by(|left, right| {
        (&left.logical_path, &left.file_occurrence_id)
            .cmp(&(&right.logical_path, &right.file_occurrence_id))
    });
    SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repo.fixture"),
        worktree: Some(id::<WorktreeId>("worktree.fixture")),
        reference: Some(id::<RefId>("refs/heads/main")),
        source_revision: Some(id::<CommitId>("commit.fixture")),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.fixture")],
        content_identity: content_digest('b'),
        captured_at: UtcMicros(1_000_000),
        files,
    }
}

fn intake() -> SanitizedCodeIntake<tracedecay_code_index::languages::StaticLanguageRegistry> {
    SanitizedCodeIntake::new(
        registry(),
        id::<SanitizerRevision>("sanitizer.v1"),
        UtcMicros(2_000_000),
    )
}

#[test]
fn intake_is_receipt_bound_registry_backed_and_deterministic() {
    let source = file(
        "file.source",
        "src/lib.rs",
        Some("rust"),
        SnapshotFileDispositionV1::Present,
    );
    let binary = file(
        "file.binary",
        "assets/logo.bin",
        None,
        SnapshotFileDispositionV1::Binary,
    );
    let admitted = snapshot(vec![source, binary]);

    let first = intake()
        .validate(admitted.clone())
        .expect("snapshot admitted");
    let second = intake().validate(admitted).expect("snapshot admitted");
    assert_eq!(first.intake_digest, second.intake_digest);
    assert_eq!(first.validated_at, UtcMicros(2_000_000));
}

#[test]
fn intake_capability_binds_sanitized_bytes_digest_and_receipts() {
    let bytes = b"pub fn admitted() {}\n";
    let mut source = file(
        "file.source",
        "src/lib.rs",
        Some("rust"),
        SnapshotFileDispositionV1::Present,
    );
    source.content_digest = source_digest(bytes);

    let admission = intake();
    let capability = admission
        .admit(snapshot(vec![source.clone()]))
        .expect("receipt-bound snapshot capability");
    let candidate = ValidatedCodeFileV1 {
        generation_id: id::<CodeGenerationId>("generation.fixture"),
        file: source.clone(),
        snapshot_digest: capability.snapshot().intake_digest.clone(),
        sanitized_bytes: bytes.to_vec(),
    };
    let bound = admission
        .bind_file(&capability, candidate.clone())
        .expect("matching file becomes receipt-bound");
    assert_eq!(bound.sanitized_bytes(), bytes);

    let mut forged_bytes = candidate.clone();
    forged_bytes.sanitized_bytes = b"pub fn forged() {}\n".to_vec();
    assert!(matches!(
        admission.bind_file(&capability, forged_bytes),
        Err(IntakeRejectionV1::UnsanitizedInput)
    ));

    let mut forged_utf8 = candidate.clone();
    forged_utf8.sanitized_bytes = vec![b'f', b'n', b' ', 0xff];
    forged_utf8.file.content_digest = source_digest(&forged_utf8.sanitized_bytes);
    assert!(matches!(
        admission.bind_file(&capability, forged_utf8),
        Err(IntakeRejectionV1::UnsanitizedInput)
    ));

    let mut forged_snapshot = candidate.clone();
    forged_snapshot.snapshot_digest = id(&format!("sha256:{}", "f".repeat(64)));
    assert!(matches!(
        admission.bind_file(&capability, forged_snapshot),
        Err(IntakeRejectionV1::UnsanitizedInput)
    ));

    let mut alternate_snapshot = snapshot(vec![source]);
    alternate_snapshot.sanitization_receipts =
        vec![id::<SanitizationReceiptId>("receipt.alternate")];
    let alternate_capability = admission
        .admit(alternate_snapshot)
        .expect("independently receipt-bound snapshot");
    let mut forged_receipt = candidate;
    forged_receipt.snapshot_digest = alternate_capability.snapshot().intake_digest.clone();
    assert!(matches!(
        admission.bind_file(&capability, forged_receipt),
        Err(IntakeRejectionV1::UnsanitizedInput)
    ));
}

#[test]
fn intake_rejects_missing_stale_mixed_and_unsanitized_snapshots() {
    let source = file(
        "file.source",
        "src/lib.rs",
        Some("rust"),
        SnapshotFileDispositionV1::Present,
    );

    let mut missing_receipt = snapshot(vec![source.clone()]);
    missing_receipt.sanitization_receipts.clear();
    assert_eq!(
        intake().validate(missing_receipt),
        Err(IntakeRejectionV1::MissingReceipt)
    );

    assert_eq!(
        intake()
            .with_max_snapshot_age_micros(500_000)
            .validate(snapshot(vec![source.clone()])),
        Err(IntakeRejectionV1::StaleSnapshot)
    );

    let mixed = snapshot(vec![
        source.clone(),
        file(
            "file.source",
            "src/other.rs",
            Some("rust"),
            SnapshotFileDispositionV1::Present,
        ),
    ]);
    assert_eq!(
        intake().validate(mixed),
        Err(IntakeRejectionV1::MixedSnapshot)
    );

    let unknown = file(
        "file.source",
        "src/lib.unknown",
        Some("unknown-language"),
        SnapshotFileDispositionV1::Present,
    );
    assert_eq!(
        intake().validate(snapshot(vec![unknown])),
        Err(IntakeRejectionV1::UnsanitizedInput)
    );
}

#[test]
fn stale_snapshot_check_handles_extreme_timestamps_without_overflow() {
    let source = file(
        "file.source",
        "src/lib.rs",
        Some("rust"),
        SnapshotFileDispositionV1::Present,
    );
    let mut stale = snapshot(vec![source]);
    stale.captured_at = UtcMicros(i64::MIN);

    assert_eq!(
        intake().with_max_snapshot_age_micros(1).validate(stale),
        Err(IntakeRejectionV1::StaleSnapshot)
    );
}
