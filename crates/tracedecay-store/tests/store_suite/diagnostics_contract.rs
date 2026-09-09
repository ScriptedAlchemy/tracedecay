use tracedecay_domain::{
    CodeGenerationId, ComponentVersion, ContentDigest, DiagnosticEvidenceClassV1,
    DiagnosticProducerKindV1, DiagnosticProvenanceV1, DiagnosticRecordStateV1,
    DiagnosticSeverityV1, FileOccurrenceId, GenerationDiagnosticV1, ManifestDigest, ProviderId,
    RepositoryId, RetrievalAnchorId, SanitizationReceiptId, SourceSpan, UtcMicros,
};
use tracedecay_store::{DiagnosticStoreError, SanitizedCleanDiagnosticSnapshotV1};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn fixture_record(generation: &str, anchor: &str) -> GenerationDiagnosticV1 {
    let mut record = GenerationDiagnosticV1 {
        diagnostic_anchor: id::<RetrievalAnchorId>(anchor),
        generation_id: id::<CodeGenerationId>(generation),
        repository: id::<RepositoryId>("repository.fixture"),
        worktree: None,
        reference: None,
        source_revision: None,
        file_occurrence_id: id::<FileOccurrenceId>("file.occurrence.1"),
        content_digest: id::<ContentDigest>(&digest('a')),
        span: SourceSpan {
            start_byte: 10,
            end_byte: 42,
        },
        symbol_occurrence_id: None,
        code: "E0308".to_owned(),
        severity: DiagnosticSeverityV1::Error,
        message: "mismatched types".to_owned(),
        message_digest: id::<ManifestDigest>(&digest('b')),
        provenance: DiagnosticProvenanceV1 {
            producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
            producer: id::<ProviderId>("producer.rustc"),
            analyzer_revision: id::<ComponentVersion>("analyzer.v1"),
            configuration_revision: id::<ComponentVersion>("config.v1"),
            sanitization_receipt: Some(id::<SanitizationReceiptId>("receipt.sanitization.1")),
        },
        evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
        collected_at: UtcMicros(1_700_000_000_000_000),
        state: DiagnosticRecordStateV1::Current,
    };
    record.message_digest = record.compute_message_digest().unwrap();
    record
}

#[test]
fn clean_snapshot_preserves_one_sanitized_generation_identity() {
    let generation = id::<CodeGenerationId>("generation.clean.1");
    let snapshot = SanitizedCleanDiagnosticSnapshotV1::new(
        generation.clone(),
        vec![
            fixture_record(generation.as_str(), "anchor.diagnostic.2"),
            fixture_record(generation.as_str(), "anchor.diagnostic.1"),
        ],
    )
    .unwrap();

    assert_eq!(snapshot.generation_id(), &generation);
    assert_eq!(
        snapshot
            .records()
            .iter()
            .map(|record| record.diagnostic_anchor.as_str())
            .collect::<Vec<_>>(),
        vec!["anchor.diagnostic.1", "anchor.diagnostic.2"]
    );
}

#[test]
fn clean_snapshot_rejects_cross_snapshot_or_stale_records() {
    let generation = id::<CodeGenerationId>("generation.clean.1");
    let mixed = SanitizedCleanDiagnosticSnapshotV1::new(
        generation.clone(),
        vec![fixture_record(
            "generation.clean.2",
            "anchor.diagnostic.mixed",
        )],
    );
    assert!(matches!(
        mixed,
        Err(DiagnosticStoreError::GenerationMismatch { .. })
    ));

    let stale = fixture_record(generation.as_str(), "anchor.diagnostic.stale")
        .supersede(id("generation.clean.2"))
        .unwrap();
    assert!(matches!(
        SanitizedCleanDiagnosticSnapshotV1::new(generation.clone(), vec![stale]),
        Err(DiagnosticStoreError::NonCurrentRecord { .. })
    ));

    let duplicate = fixture_record(generation.as_str(), "anchor.diagnostic.duplicate");
    assert!(matches!(
        SanitizedCleanDiagnosticSnapshotV1::new(generation, vec![duplicate.clone(), duplicate]),
        Err(DiagnosticStoreError::DuplicateAnchor { .. })
    ));
}
