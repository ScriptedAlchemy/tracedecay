use tracedecay::code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_domain::{
    CommitId, ContentDigest, FileOccurrenceId, IntakeRejectionV1, LanguageId, RefId, RepositoryId,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    SnapshotFileDispositionV1, UtcMicros, WorktreeId,
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

fn intake() -> SanitizedCodeIntake<tracedecay::code_index::languages::StaticLanguageRegistry> {
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
