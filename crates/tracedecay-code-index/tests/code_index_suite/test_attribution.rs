use std::borrow::Borrow;

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
    TestAttributionEvidenceClassV1, UtcMicros,
};

use super::support::{id, registry};
use tracedecay_code_index::intake::ValidatedCodeSnapshotV1;

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
        omitted_sources: Vec::new(),
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
    let mut owned_reference = attributions.clone();
    owned_reference.sort_by_key(|attribution| attribution.evidence_class);
    let legacy_digest = tracedecay_domain::canonical_sha256(&serde_json::json!({
        "domain": "tracedecay.test-attribution-evidence.v1",
        "generation_id": watermark.generation_id,
        "snapshot_digest": watermark.snapshot_digest,
        "content_identity": watermark.content_identity,
        "source_revision": watermark.source_revision,
        "attribution_revision": watermark.attribution_revision,
        "coverage": watermark.coverage,
        "attributions": owned_reference,
        "occurrences": occurrences,
    }))
    .unwrap();
    assert_eq!(watermark.evidence_digest, legacy_digest);
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
    let joined = GenerationTestJoinV1::join(
        &manifest,
        &snapshot,
        &attributions,
        &occurrences,
        &watermark,
    )
    .unwrap();
    let reversed = GenerationTestJoinV1::join(
        &manifest,
        &snapshot,
        &reversed_attributions,
        &reversed_occurrences,
        &watermark,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_vec(&joined).unwrap(),
        serde_json::to_vec(&reversed).unwrap()
    );
    assert_eq!(joined.retained_bytes(), reversed.retained_bytes());
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

#[test]
fn records_covering_one_occurrence_share_a_single_resident_copy() {
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
    .expect("joined attribution");

    let covered = |record: usize| -> &TestAttributionOccurrenceV1 {
        joined.records[record].covered_occurrences[0].borrow()
    };
    assert_eq!(covered(0).occurrence_id, covered(1).occurrence_id);
    assert!(
        std::ptr::eq(covered(0), covered(1)),
        "every test covering an occurrence must share it; a transitive closure \
         copied per test makes the join quadratic in resident memory"
    );
}

#[test]
fn join_preserves_first_duplicate_and_first_stale_covered_occurrence() {
    let (snapshot, manifest) = generation();
    let mut attribution = attribution(
        &manifest,
        TestAttributionEvidenceClassV1::ObservedCoverageCandidates,
    );
    attribution.covered_occurrences.push(id("symbol.z-missing"));
    let attributions = vec![attribution];
    let mut evidence = occurrences();
    evidence[0].content_digest = content('c');
    let watermark = watermark(
        &snapshot,
        &manifest,
        TestAttributionJoinInputCoverageV1::Complete,
        &attributions,
        &evidence,
    );
    evidence.reverse();
    let joined =
        GenerationTestJoinV1::join(&manifest, &snapshot, &attributions, &evidence, &watermark)
            .unwrap();
    assert!(matches!(
        &joined.records[0].disposition,
        GenerationTestJoinDispositionV1::StaleContent { occurrence_id, .. }
            if occurrence_id.as_str() == "symbol.source"
    ));
    evidence.extend(evidence.clone());
    assert_eq!(
        GenerationTestJoinV1::join(&manifest, &snapshot, &attributions, &evidence, &watermark),
        Err(GenerationTestJoinErrorV1::DuplicateOccurrence(id(
            "symbol.test"
        ))),
    );
}

#[test]
fn join_checks_missing_test_and_covered_occurrences_without_content_drift() {
    let (snapshot, manifest) = generation();
    let attributions = vec![attribution(
        &manifest,
        TestAttributionEvidenceClassV1::ObservedCoverageCandidates,
    )];
    for missing in ["symbol.source", "symbol.test"] {
        let mut evidence = occurrences();
        evidence.retain(|occurrence| occurrence.occurrence_id.as_str() != missing);
        let watermark = watermark(
            &snapshot,
            &manifest,
            TestAttributionJoinInputCoverageV1::Complete,
            &attributions,
            &evidence,
        );
        let joined =
            GenerationTestJoinV1::join(&manifest, &snapshot, &attributions, &evidence, &watermark)
                .unwrap();
        assert_eq!(
            joined.records[0].disposition,
            GenerationTestJoinDispositionV1::MissingOccurrence {
                occurrence_id: id(missing),
            },
        );
    }
}
