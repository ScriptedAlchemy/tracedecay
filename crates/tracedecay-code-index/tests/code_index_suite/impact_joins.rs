use tracedecay_code_index::generations::GenerationPlanner;
use tracedecay_code_index::impact_join::{
    GenerationImpactJoinCoverageV1, GenerationImpactJoinErrorV1, GenerationImpactJoinV1,
    GenerationOccurrenceBindingV1,
};
use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_code_index::provider::{
    CodeIndexAffectedTestsEvidenceV1 as AffectedTestsResult,
    CodeIndexGraphImpactEvidenceV1 as GraphImpactResult,
};
use tracedecay_code_index::provider::{
    GenerationProviderContractErrorV1, GenerationProviderCoverageV1, GenerationProviderReadV1,
};
use tracedecay_domain::{
    CodeGenerationManifestV1, ContentDigest, ProviderEvaluationStateV1, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SnapshotFileDispositionV1, UtcMicros, ValidatedCodeSnapshotV1,
};

use super::support::{id, registry};

fn content(byte: char) -> ContentDigest {
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

fn occurrences(generation: &CodeGenerationManifestV1) -> Vec<GenerationOccurrenceBindingV1> {
    vec![
        GenerationOccurrenceBindingV1 {
            generation_id: generation.generation_id.clone(),
            symbol_occurrence_id: id("symbol.source"),
            file_occurrence_id: id("file.source"),
            content_digest: content('a'),
        },
        GenerationOccurrenceBindingV1 {
            generation_id: generation.generation_id.clone(),
            symbol_occurrence_id: id("symbol.caller"),
            file_occurrence_id: id("file.source"),
            content_digest: content('a'),
        },
        GenerationOccurrenceBindingV1 {
            generation_id: generation.generation_id.clone(),
            symbol_occurrence_id: id("symbol.test"),
            file_occurrence_id: id("file.test"),
            content_digest: content('b'),
        },
    ]
}

#[test]
fn graph_impact_and_affected_tests_bind_exact_occurrences() {
    let (snapshot, manifest) = generation();
    let graph = GenerationProviderReadV1::new(
        ProviderEvaluationStateV1::SupportedCompletedComplete,
        GenerationProviderCoverageV1::Complete {
            examined: 2,
            eligible: 2,
            excluded: 0,
        },
        Some(GraphImpactResult {
            affected_files: vec![id("file.source")],
            affected_callers: vec![id("symbol.caller")],
            evidence_anchors: vec![id("anchor.graph")],
        }),
    )
    .expect("complete graph provider result");
    let tests = GenerationProviderReadV1::new(
        ProviderEvaluationStateV1::SupportedCompletedComplete,
        GenerationProviderCoverageV1::Complete {
            examined: 1,
            eligible: 1,
            excluded: 0,
        },
        Some(AffectedTestsResult {
            tests: vec![id("symbol.test")],
            attributions: Vec::new(),
        }),
    )
    .expect("complete test provider result");

    let joined =
        GenerationImpactJoinV1::join(&manifest, &snapshot, graph, tests, &occurrences(&manifest))
            .expect("generation-exact impact join");

    assert_eq!(joined.generation_id, manifest.generation_id);
    assert_eq!(joined.coverage, GenerationImpactJoinCoverageV1::Complete);
    assert_eq!(
        joined.affected_callers[0].symbol_occurrence_id.as_str(),
        "symbol.caller"
    );
    assert_eq!(
        joined.affected_tests[0].symbol_occurrence_id.as_str(),
        "symbol.test"
    );
}

#[test]
fn provider_unavailability_stays_typed_without_fabricated_test_evidence() {
    let (snapshot, manifest) = generation();
    let graph = GenerationProviderReadV1::new(
        ProviderEvaluationStateV1::Partial,
        GenerationProviderCoverageV1::Partial {
            examined: 2,
            eligible: 1,
            excluded: 0,
            unknown: 1,
            capped: false,
        },
        Some(GraphImpactResult {
            affected_files: vec![id("file.source")],
            affected_callers: vec![id("symbol.caller")],
            evidence_anchors: vec![id("anchor.graph")],
        }),
    )
    .expect("partial graph provider result");
    let tests = GenerationProviderReadV1::new(
        ProviderEvaluationStateV1::Unavailable,
        GenerationProviderCoverageV1::Unavailable,
        None,
    )
    .expect("unavailable test provider result");

    let joined =
        GenerationImpactJoinV1::join(&manifest, &snapshot, graph, tests, &occurrences(&manifest))
            .expect("partial evidence remains joinable");

    assert!(matches!(
        joined.coverage,
        GenerationImpactJoinCoverageV1::Partial {
            graph: ProviderEvaluationStateV1::Partial,
            affected_tests: ProviderEvaluationStateV1::Unavailable,
        }
    ));
    assert!(joined.affected_tests.is_empty());
    assert!(joined.test_provider.evidence.is_none());
}

#[test]
fn stale_occurrence_identity_cannot_bind_graph_or_test_results() {
    let (snapshot, manifest) = generation();
    let graph = GenerationProviderReadV1::new(
        ProviderEvaluationStateV1::SupportedCompletedComplete,
        GenerationProviderCoverageV1::Complete {
            examined: 1,
            eligible: 1,
            excluded: 0,
        },
        Some(GraphImpactResult {
            affected_files: vec![id("file.source")],
            affected_callers: vec![id("symbol.caller")],
            evidence_anchors: Vec::new(),
        }),
    )
    .expect("complete graph provider result");
    let tests = GenerationProviderReadV1::new(
        ProviderEvaluationStateV1::Absent,
        GenerationProviderCoverageV1::Unavailable,
        None,
    )
    .expect("absent test provider result");
    let mut stale = occurrences(&manifest);
    stale[1].content_digest = content('c');

    assert_eq!(
        GenerationImpactJoinV1::join(&manifest, &snapshot, graph, tests, &stale),
        Err(GenerationImpactJoinErrorV1::StaleOccurrenceContent(id(
            "symbol.caller"
        )))
    );
}

#[test]
fn provider_state_cannot_overclaim_coverage_or_missing_evidence() {
    assert_eq!(
        GenerationProviderReadV1::<GraphImpactResult>::new(
            ProviderEvaluationStateV1::SupportedCompletedComplete,
            GenerationProviderCoverageV1::Complete {
                examined: 1,
                eligible: 1,
                excluded: 0,
            },
            None,
        ),
        Err(GenerationProviderContractErrorV1::StateCoverageMismatch)
    );
    assert_eq!(
        GenerationProviderReadV1::new(
            ProviderEvaluationStateV1::Partial,
            GenerationProviderCoverageV1::Complete {
                examined: 1,
                eligible: 1,
                excluded: 0,
            },
            Some(GraphImpactResult {
                affected_files: Vec::new(),
                affected_callers: Vec::new(),
                evidence_anchors: Vec::new(),
            }),
        ),
        Err(GenerationProviderContractErrorV1::StateCoverageMismatch)
    );
}
