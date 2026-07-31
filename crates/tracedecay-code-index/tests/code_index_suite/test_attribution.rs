use tracedecay_code_index::generations::GenerationPlanner;
use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_code_index::test_attribution::{
    GenerationTestJoinCoverageV1, GenerationTestJoinDispositionV1, GenerationTestJoinErrorV1,
    GenerationTestJoinV1, TestAttributionJoinInputCoverageV1, TestAttributionOccurrenceV1,
    TestAttributionWatermarkV1,
};
use tracedecay_domain::{
    CodeGenerationManifestV1, ContentDigest, GenerationTestAttributionV1, ManifestDigest,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SnapshotFileDispositionV1,
    TestAttributionEvidenceClassV1, UtcMicros, ValidatedCodeSnapshotV1,
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
        files: vec![
            SanitizedCodeFileV1 {
                file_occurrence_id: id("file.source"),
                logical_path: "src/lib.rs".to_owned(),
                language: Some(id("rust")),
                content_digest: content('a'),
                disposition: SnapshotFileDispositionV1::Present,
            },
            SanitizedCodeFileV1 {
                file_occurrence_id: id("file.test"),
                logical_path: "tests/lib_test.rs".to_owned(),
                language: Some(id("rust")),
                content_digest: content('b'),
                disposition: SnapshotFileDispositionV1::Present,
            },
        ],
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
    coverage: TestAttributionJoinInputCoverageV1,
    attributions: &[GenerationTestAttributionV1],
    occurrences: &[TestAttributionOccurrenceV1],
) -> TestAttributionWatermarkV1 {
    let mut watermark = TestAttributionWatermarkV1 {
        generation_id: manifest.generation_id.clone(),
        snapshot_digest: manifest.snapshot_digest.clone(),
        content_identity: snapshot.snapshot.content_identity.clone(),
        source_revision: snapshot.snapshot.source_revision.clone(),
        attribution_revision: id("test-map.v1"),
        evidence_digest: manifest_digest('9'),
        coverage,
    };
    watermark.evidence_digest = watermark
        .recompute_evidence_digest(attributions, occurrences)
        .expect("canonical attribution evidence digest");
    watermark
}

fn occurrences() -> Vec<TestAttributionOccurrenceV1> {
    vec![
        TestAttributionOccurrenceV1 {
            occurrence_id: id("symbol.source"),
            file_occurrence_id: id("file.source"),
            content_digest: content('a'),
        },
        TestAttributionOccurrenceV1 {
            occurrence_id: id("symbol.test"),
            file_occurrence_id: id("file.test"),
            content_digest: content('b'),
        },
    ]
}

fn attribution(
    generation: &CodeGenerationManifestV1,
    evidence_class: TestAttributionEvidenceClassV1,
) -> GenerationTestAttributionV1 {
    GenerationTestAttributionV1 {
        generation_id: generation.generation_id.clone(),
        source_revision: Some(id("commit.fixture")),
        test_occurrence: id("symbol.test"),
        covered_occurrences: vec![id("symbol.source")],
        evidence_class,
        attribution_revision: id("test-map.v1"),
    }
}

#[test]
fn exact_occurrence_content_binds_current_attribution() {
    let (snapshot, manifest) = generation();
    let attributions = vec![attribution(
        &manifest,
        TestAttributionEvidenceClassV1::ObservedCoverageCandidates,
    )];
    let occurrence_evidence = occurrences();
    let joined = GenerationTestJoinV1::join(
        &manifest,
        &snapshot,
        &attributions,
        &occurrence_evidence,
        &watermark(
            &snapshot,
            &manifest,
            TestAttributionJoinInputCoverageV1::Complete,
            &attributions,
            &occurrence_evidence,
        ),
    )
    .expect("exact test attribution join");

    assert_eq!(joined.coverage, GenerationTestJoinCoverageV1::Complete);
    assert_eq!(joined.records.len(), 1);
    assert!(matches!(
        joined.records[0].disposition,
        GenerationTestJoinDispositionV1::Current {
            evidence_class: TestAttributionEvidenceClassV1::ObservedCoverageCandidates
        }
    ));
    assert_eq!(
        joined.records[0]
            .test_occurrence
            .as_ref()
            .expect("test occurrence resolved")
            .content_digest,
        content('b')
    );
    assert_eq!(joined.records[0].covered_occurrences.len(), 1);
}

#[test]
fn every_declared_attribution_evidence_class_stays_typed() {
    let (snapshot, manifest) = generation();
    let classes = [
        TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
        TestAttributionEvidenceClassV1::ObservedCoverageCandidates,
        TestAttributionEvidenceClassV1::PredictiveRankedCandidates,
        TestAttributionEvidenceClassV1::StaleEvidence,
        TestAttributionEvidenceClassV1::UnknownUnsupported,
    ];
    let attributions: Vec<_> = classes
        .iter()
        .copied()
        .map(|class| attribution(&manifest, class))
        .collect();
    let occurrence_evidence = occurrences();

    let joined = GenerationTestJoinV1::join(
        &manifest,
        &snapshot,
        &attributions,
        &occurrence_evidence,
        &watermark(
            &snapshot,
            &manifest,
            TestAttributionJoinInputCoverageV1::Complete,
            &attributions,
            &occurrence_evidence,
        ),
    )
    .expect("all evidence classes remain representable");

    assert_eq!(joined.records.len(), classes.len());
    assert_eq!(
        joined
            .records
            .iter()
            .filter(|record| matches!(
                record.disposition,
                GenerationTestJoinDispositionV1::Current { .. }
            ))
            .count(),
        3
    );
    assert!(joined.records.iter().any(|record| matches!(
        record.disposition,
        GenerationTestJoinDispositionV1::StaleEvidence
    )));
    assert!(joined.records.iter().any(|record| matches!(
        record.disposition,
        GenerationTestJoinDispositionV1::UnknownUnsupported
    )));
    assert!(matches!(
        joined.coverage,
        GenerationTestJoinCoverageV1::Partial { .. }
    ));
}

#[test]
fn tampered_evidence_digest_never_binds_attribution_as_current() {
    let (snapshot, manifest) = generation();
    let attributions = vec![attribution(
        &manifest,
        TestAttributionEvidenceClassV1::ObservedCoverageCandidates,
    )];
    let occurrence_evidence = occurrences();
    let mut evidence = watermark(
        &snapshot,
        &manifest,
        TestAttributionJoinInputCoverageV1::Complete,
        &attributions,
        &occurrence_evidence,
    );
    evidence.evidence_digest = manifest_digest('8');

    assert_eq!(
        GenerationTestJoinV1::join(
            &manifest,
            &snapshot,
            &attributions,
            &occurrence_evidence,
            &evidence,
        ),
        Err(GenerationTestJoinErrorV1::StaleAttributionWatermark)
    );
}

#[test]
fn attribution_evidence_digest_is_canonical_across_input_order() {
    let (snapshot, manifest) = generation();
    let attributions = vec![
        attribution(
            &manifest,
            TestAttributionEvidenceClassV1::ObservedCoverageCandidates,
        ),
        attribution(
            &manifest,
            TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
        ),
    ];
    let occurrences = occurrences();
    let watermark = watermark(
        &snapshot,
        &manifest,
        TestAttributionJoinInputCoverageV1::Complete,
        &attributions,
        &occurrences,
    );
    let mut reversed_attributions = attributions.clone();
    reversed_attributions.reverse();
    let mut reversed_occurrences = occurrences.clone();
    reversed_occurrences.reverse();

    assert_eq!(
        watermark
            .recompute_evidence_digest(&attributions, &occurrences)
            .expect("canonical digest"),
        watermark
            .recompute_evidence_digest(&reversed_attributions, &reversed_occurrences)
            .expect("canonical digest")
    );
}

#[test]
fn generation_source_and_content_drift_are_typed_partial_not_current() {
    let (snapshot, manifest) = generation();
    let mut stale_generation = attribution(
        &manifest,
        TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
    );
    stale_generation.generation_id = id("generation.other");
    let mut stale_source = attribution(
        &manifest,
        TestAttributionEvidenceClassV1::ObservedCoverageCandidates,
    );
    stale_source.source_revision = Some(id("commit.other"));
    let current = attribution(
        &manifest,
        TestAttributionEvidenceClassV1::PredictiveRankedCandidates,
    );
    let mut occurrence_evidence = occurrences();
    occurrence_evidence[0].content_digest = content('c');
    let attributions = vec![stale_generation, stale_source, current];

    let joined = GenerationTestJoinV1::join(
        &manifest,
        &snapshot,
        &attributions,
        &occurrence_evidence,
        &watermark(
            &snapshot,
            &manifest,
            TestAttributionJoinInputCoverageV1::Partial {
                reason: "coverage collector truncated".to_owned(),
            },
            &attributions,
            &occurrence_evidence,
        ),
    )
    .expect("drift remains typed evidence");

    assert!(matches!(
        joined.coverage,
        GenerationTestJoinCoverageV1::Partial { .. }
    ));
    assert!(joined.records.iter().any(|record| matches!(
        record.disposition,
        GenerationTestJoinDispositionV1::StaleGeneration { .. }
    )));
    assert!(joined.records.iter().any(|record| matches!(
        record.disposition,
        GenerationTestJoinDispositionV1::StaleSourceRevision { .. }
    )));
    assert!(joined.records.iter().any(|record| matches!(
        record.disposition,
        GenerationTestJoinDispositionV1::StaleContent { .. }
    )));
    assert!(joined.records.iter().all(|record| !matches!(
        record.disposition,
        GenerationTestJoinDispositionV1::Current { .. }
    )));
}
