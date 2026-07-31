use tracedecay_code_index::diagnostics::{
    DiagnosticEvidenceWatermarkV1, DiagnosticJoinInputCoverageV1,
    GenerationDiagnosticDispositionV1, GenerationDiagnosticJoinCoverageV1,
    GenerationDiagnosticJoinV1,
};
use tracedecay_code_index::generations::GenerationPlanner;
use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_domain::{
    CodeGenerationManifestV1, ContentDigest, DiagnosticEvidenceClassV1, DiagnosticProducerKindV1,
    DiagnosticProvenanceV1, DiagnosticRecordStateV1, DiagnosticSeverityV1, GenerationDiagnosticV1,
    ManifestDigest, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SnapshotFileDispositionV1,
    SourceSpan, UtcMicros, ValidatedCodeSnapshotV1,
};

use super::support::{id, registry};

fn content(byte: char) -> ContentDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn manifest_digest(byte: char) -> ManifestDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn generation() -> (ValidatedCodeSnapshotV1, CodeGenerationManifestV1) {
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id("repository.fixture"),
        worktree: Some(id("worktree.fixture")),
        reference: Some(id("ref.main")),
        source_revision: Some(id("commit.fixture")),
        sanitizer_revision: id("sanitizer.v1"),
        sanitization_receipts: vec![id("receipt.fixture")],
        content_identity: content('f'),
        captured_at: UtcMicros(10),
        files: vec![SanitizedCodeFileV1 {
            file_occurrence_id: id("file.fixture"),
            logical_path: "src/lib.rs".to_owned(),
            language: Some(id("rust")),
            content_digest: content('a'),
            disposition: SnapshotFileDispositionV1::Present,
        }],
    };
    let intake = SanitizedCodeIntake::new(registry(), id("sanitizer.v1"), UtcMicros(20));
    let validated = intake
        .validate(snapshot)
        .expect("validated fixture snapshot");
    let manifest = GenerationPlanner::new(
        id("project.fixture"),
        id("repository.fixture"),
        registry(),
        id("chunker.v1"),
        id("privacy.fixture"),
        7,
    )
    .plan_generation(&validated, None, UtcMicros(30))
    .expect("sealed fixture generation");
    (validated, manifest)
}

fn watermark(
    snapshot: &ValidatedCodeSnapshotV1,
    manifest: &CodeGenerationManifestV1,
    coverage: DiagnosticJoinInputCoverageV1,
) -> DiagnosticEvidenceWatermarkV1 {
    DiagnosticEvidenceWatermarkV1 {
        generation_id: manifest.generation_id.clone(),
        snapshot_digest: manifest.snapshot_digest.clone(),
        content_identity: snapshot.snapshot.content_identity.clone(),
        observed_through: UtcMicros(100),
        coverage,
    }
}

fn diagnostic(
    generation: &str,
    anchor: &str,
    digest: char,
    state: DiagnosticRecordStateV1,
) -> GenerationDiagnosticV1 {
    let mut record = GenerationDiagnosticV1 {
        diagnostic_anchor: id(anchor),
        generation_id: id(generation),
        repository: id("repository.fixture"),
        worktree: Some(id("worktree.fixture")),
        reference: Some(id("ref.main")),
        source_revision: Some(id("commit.fixture")),
        file_occurrence_id: id("file.fixture"),
        content_digest: content(digest),
        span: SourceSpan {
            start_byte: 1,
            end_byte: 4,
        },
        symbol_occurrence_id: Some(id("symbol.fixture")),
        code: "E0308".to_owned(),
        severity: DiagnosticSeverityV1::Error,
        message: "mismatched types".to_owned(),
        message_digest: manifest_digest('0'),
        provenance: DiagnosticProvenanceV1 {
            producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
            producer: id("producer.rustc"),
            analyzer_revision: id("analyzer.v1"),
            configuration_revision: id("config.v1"),
            sanitization_receipt: Some(id("receipt.fixture")),
        },
        evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
        collected_at: UtcMicros(50),
        state,
    };
    record.message_digest = record.compute_message_digest().expect("message digest");
    record
}

#[test]
fn exact_current_diagnostic_attaches_to_the_clean_generation() {
    let (snapshot, manifest) = generation();
    let record = diagnostic(
        manifest.generation_id.as_str(),
        "anchor.diagnostic.current",
        'a',
        DiagnosticRecordStateV1::Current,
    );

    let joined = GenerationDiagnosticJoinV1::join(
        &manifest,
        &snapshot,
        &[record],
        &watermark(
            &snapshot,
            &manifest,
            DiagnosticJoinInputCoverageV1::Complete,
        ),
    )
    .expect("exact diagnostic join");

    assert_eq!(
        joined.coverage,
        GenerationDiagnosticJoinCoverageV1::Complete
    );
    assert_eq!(joined.records.len(), 1);
    match &joined.records[0].disposition {
        GenerationDiagnosticDispositionV1::Current { attachment } => {
            assert_eq!(attachment.generation_id, manifest.generation_id);
            assert_eq!(attachment.file_occurrence_id.as_str(), "file.fixture");
            assert_eq!(attachment.content_digest, content('a'));
        }
        other => panic!("expected current attachment, got {other:?}"),
    }
}

#[test]
fn superseded_and_cleared_records_remain_typed_historical_evidence() {
    let (snapshot, manifest) = generation();
    let superseded = diagnostic(
        "generation.prior.1",
        "anchor.diagnostic.superseded",
        'a',
        DiagnosticRecordStateV1::Superseded {
            successor_generation: manifest.generation_id.clone(),
        },
    );
    let cleared = diagnostic(
        "generation.prior.2",
        "anchor.diagnostic.cleared",
        'a',
        DiagnosticRecordStateV1::Cleared {
            cleared_in_generation: manifest.generation_id.clone(),
        },
    );

    let joined = GenerationDiagnosticJoinV1::join(
        &manifest,
        &snapshot,
        &[superseded, cleared],
        &watermark(
            &snapshot,
            &manifest,
            DiagnosticJoinInputCoverageV1::Complete,
        ),
    )
    .expect("historical evidence remains inspectable");

    assert!(matches!(
        joined.records[0].disposition,
        GenerationDiagnosticDispositionV1::Cleared { .. }
            | GenerationDiagnosticDispositionV1::Superseded { .. }
    ));
    assert!(matches!(
        joined.records[1].disposition,
        GenerationDiagnosticDispositionV1::Cleared { .. }
            | GenerationDiagnosticDispositionV1::Superseded { .. }
    ));
    assert!(joined.records.iter().all(|record| !matches!(
        record.disposition,
        GenerationDiagnosticDispositionV1::Current { .. }
    )));
}

#[test]
fn out_of_scope_historical_record_is_not_classified_as_lifecycle_history() {
    let (snapshot, manifest) = generation();
    let mut record = diagnostic(
        "generation.prior",
        "anchor.diagnostic.out-of-scope",
        'a',
        DiagnosticRecordStateV1::Superseded {
            successor_generation: manifest.generation_id.clone(),
        },
    );
    record.reference = Some(id("ref.other"));

    let joined = GenerationDiagnosticJoinV1::join(
        &manifest,
        &snapshot,
        &[record],
        &watermark(
            &snapshot,
            &manifest,
            DiagnosticJoinInputCoverageV1::Complete,
        ),
    )
    .expect("out-of-scope evidence remains inspectable");

    assert!(matches!(
        joined.records[0].disposition,
        GenerationDiagnosticDispositionV1::StaleScope
    ));
}

#[test]
fn stale_content_and_partial_capture_never_report_a_clean_current_set() {
    let (snapshot, manifest) = generation();
    let stale_generation = diagnostic(
        "generation.other",
        "anchor.diagnostic.generation-stale",
        'a',
        DiagnosticRecordStateV1::Current,
    );
    let stale_content = diagnostic(
        manifest.generation_id.as_str(),
        "anchor.diagnostic.content-stale",
        'b',
        DiagnosticRecordStateV1::Current,
    );

    let joined = GenerationDiagnosticJoinV1::join(
        &manifest,
        &snapshot,
        &[stale_generation, stale_content],
        &watermark(
            &snapshot,
            &manifest,
            DiagnosticJoinInputCoverageV1::Partial {
                reason: "producer truncated output".to_owned(),
            },
        ),
    )
    .expect("stale evidence is typed rather than rejected or upgraded");

    assert!(matches!(
        joined.coverage,
        GenerationDiagnosticJoinCoverageV1::Partial { .. }
    ));
    assert!(joined.records.iter().any(|record| matches!(
        record.disposition,
        GenerationDiagnosticDispositionV1::StaleGeneration { .. }
    )));
    assert!(joined.records.iter().any(|record| matches!(
        record.disposition,
        GenerationDiagnosticDispositionV1::StaleContent { .. }
    )));
    assert!(joined.records.iter().all(|record| !matches!(
        record.disposition,
        GenerationDiagnosticDispositionV1::Current { .. }
    )));
}
