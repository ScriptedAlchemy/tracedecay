use std::sync::Arc;

use tracedecay_code_index::diagnostics::{
    DiagnosticEvidenceWatermarkV1, DiagnosticJoinInputCoverageV1,
};
use tracedecay_code_index::generations::GenerationPlanner;
use tracedecay_code_index::git_join::{
    GenerationGitContextProvidersV1, GenerationGitEvidenceScopeV1, GenerationGitReadWatermarkV1,
    GenerationGitWatermarkV1, GitFileContentIdentityV1, GitSymbolLineBindingV1,
};
use tracedecay_code_index::impact_join::{GenerationImpactJoinV1, GenerationOccurrenceBindingV1};
use tracedecay_code_index::intake::{CodeIndexIntake, SanitizedCodeIntake};
use tracedecay_code_index::production_joins::{
    GenerationDiagnosticEvidenceAuthorityV1, GenerationDiagnosticEvidenceV1,
    GenerationGitBlameEvidenceV1, GenerationGitContextAuthorityV1, GenerationGitDiffEvidenceV1,
    GenerationGitEvidenceAuthorityV1, GenerationGitHistoryEvidenceV1,
    GenerationJoinCodeAuthorityV1, GenerationTestAttributionEvidenceAuthorityV1,
    GenerationTestAttributionEvidenceV1, ProductionGenerationDiagnosticJoinReaderV1,
    ProductionGenerationGitJoinReaderV1, ProductionGenerationTestAttributionJoinReaderV1,
};
use tracedecay_code_index::provider::{
    GenerationDiagnosticJoinReadPort, GenerationGitJoinReadPort, GenerationProviderCoverageV1,
    GenerationProviderReadV1, GenerationTestAttributionJoinReadPort,
};
use tracedecay_code_index::test_attribution::{
    TestAttributionJoinInputCoverageV1, TestAttributionOccurrenceV1, TestAttributionWatermarkV1,
};
use tracedecay_application::retrieval::{AffectedTestsResult, GraphImpactResult};
use tracedecay_domain::{
    CodeGenerationManifestV1, ContentDigest, DiagnosticEvidenceClassV1, DiagnosticProducerKindV1,
    DiagnosticProvenanceV1, DiagnosticRecordStateV1, DiagnosticSeverityV1, FileOccurrenceId,
    GenerationDiagnosticV1, GenerationTestAttributionV1, GitBlameAvailabilityV1, GitBlameV1,
    GitChangeKindV1, GitCoverageV1, GitDiffScopeV1, GitDiffV1, GitFileDiffV1, GitFileModeV1,
    GitHunkV1, GitOidV1, ManifestDigest, ProviderEvaluationStateV1, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SnapshotFileDispositionV1, SourceSpan, TestAttributionEvidenceClassV1,
    UtcMicros, ValidatedCodeSnapshotV1,
};

use super::support::{id, registry};

fn content(byte: char) -> ContentDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn digest(byte: char) -> ManifestDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("oid")
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
                logical_path: "tests/lib.rs".to_owned(),
                language: Some(id("rust")),
                content_digest: content('b'),
                disposition: SnapshotFileDispositionV1::Present,
            },
        ],
    };
    let validated = SanitizedCodeIntake::new(registry(), id("sanitizer.v1"), UtcMicros(20))
        .validate(snapshot)
        .expect("snapshot");
    let manifest = GenerationPlanner::new(
        id("repository.fixture"),
        registry(),
        id("chunker.v1"),
        id("privacy.fixture"),
        1,
    )
    .plan_generation(&validated, None, UtcMicros(30))
    .expect("generation");
    (validated, manifest)
}

fn complete<T>(evidence: T) -> GenerationProviderReadV1<T> {
    GenerationProviderReadV1::new(
        ProviderEvaluationStateV1::SupportedCompletedComplete,
        GenerationProviderCoverageV1::Complete {
            examined: 1,
            eligible: 1,
            excluded: 0,
        },
        Some(evidence),
    )
    .expect("complete provider read")
}

fn unavailable<T>() -> GenerationProviderReadV1<T> {
    GenerationProviderReadV1::new(
        ProviderEvaluationStateV1::Unavailable,
        GenerationProviderCoverageV1::Unavailable,
        None,
    )
    .expect("unavailable provider read")
}

fn diagnostic_evidence(
    snapshot: &ValidatedCodeSnapshotV1,
    manifest: &CodeGenerationManifestV1,
) -> GenerationDiagnosticEvidenceV1 {
    let mut record = GenerationDiagnosticV1 {
        diagnostic_anchor: id("anchor.diagnostic"),
        generation_id: manifest.generation_id.clone(),
        repository: snapshot.snapshot.repository.clone(),
        worktree: snapshot.snapshot.worktree.clone(),
        reference: snapshot.snapshot.reference.clone(),
        source_revision: snapshot.snapshot.source_revision.clone(),
        file_occurrence_id: id("file.source"),
        content_digest: content('a'),
        symbol_occurrence_id: Some(id("symbol.source")),
        span: SourceSpan {
            start_byte: 1,
            end_byte: 4,
        },
        code: "E1".to_owned(),
        severity: DiagnosticSeverityV1::Error,
        message: "fixture".to_owned(),
        message_digest: digest('0'),
        provenance: DiagnosticProvenanceV1 {
            producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
            producer: id("producer.fixture"),
            analyzer_revision: id("analyzer.v1"),
            configuration_revision: id("config.v1"),
            sanitization_receipt: Some(id("receipt.fixture")),
        },
        evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
        collected_at: UtcMicros(40),
        state: DiagnosticRecordStateV1::Current,
    };
    record.message_digest = record.compute_message_digest().expect("message digest");
    GenerationDiagnosticEvidenceV1 {
        records: vec![record],
        watermark: DiagnosticEvidenceWatermarkV1 {
            generation_id: manifest.generation_id.clone(),
            snapshot_digest: manifest.snapshot_digest.clone(),
            content_identity: snapshot.snapshot.content_identity.clone(),
            observed_through: UtcMicros(50),
            coverage: DiagnosticJoinInputCoverageV1::Complete,
        },
    }
}

fn attribution_evidence(
    snapshot: &ValidatedCodeSnapshotV1,
    manifest: &CodeGenerationManifestV1,
) -> GenerationTestAttributionEvidenceV1 {
    let attributions = vec![GenerationTestAttributionV1 {
        generation_id: manifest.generation_id.clone(),
        source_revision: snapshot.snapshot.source_revision.clone(),
        test_occurrence: id("symbol.test"),
        covered_occurrences: vec![id("symbol.source")],
        evidence_class: TestAttributionEvidenceClassV1::ObservedCoverageCandidates,
        attribution_revision: id("test-map.v1"),
    }];
    let occurrences = vec![
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
    ];
    let mut watermark = TestAttributionWatermarkV1 {
        generation_id: manifest.generation_id.clone(),
        snapshot_digest: manifest.snapshot_digest.clone(),
        content_identity: snapshot.snapshot.content_identity.clone(),
        source_revision: snapshot.snapshot.source_revision.clone(),
        attribution_revision: id("test-map.v1"),
        evidence_digest: digest('9'),
        coverage: TestAttributionJoinInputCoverageV1::Complete,
    };
    watermark.evidence_digest = watermark
        .recompute_evidence_digest(&attributions, &occurrences)
        .expect("attribution digest");
    GenerationTestAttributionEvidenceV1 {
        attributions,
        occurrences,
        watermark,
    }
}

struct DiagnosticAuthority(GenerationDiagnosticEvidenceV1);

impl GenerationDiagnosticEvidenceAuthorityV1 for DiagnosticAuthority {
    fn read_diagnostics(
        &self,
        _: &tracedecay_domain::CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationDiagnosticEvidenceV1> {
        complete(self.0.clone())
    }
}

struct DiagnosticReadAuthority(GenerationProviderReadV1<GenerationDiagnosticEvidenceV1>);

impl GenerationDiagnosticEvidenceAuthorityV1 for DiagnosticReadAuthority {
    fn read_diagnostics(
        &self,
        _: &tracedecay_domain::CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationDiagnosticEvidenceV1> {
        self.0.clone()
    }
}

struct AttributionAuthority(GenerationTestAttributionEvidenceV1);

impl GenerationTestAttributionEvidenceAuthorityV1 for AttributionAuthority {
    fn read_attribution(
        &self,
        _: &tracedecay_domain::CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationTestAttributionEvidenceV1> {
        complete(self.0.clone())
    }
}

struct GitAuthority(GenerationGitDiffEvidenceV1);

impl GenerationGitEvidenceAuthorityV1 for GitAuthority {
    fn read_diff(
        &self,
        _: &tracedecay_domain::CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationGitDiffEvidenceV1> {
        complete(self.0.clone())
    }

    fn read_history(
        &self,
        _: &tracedecay_domain::CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationGitHistoryEvidenceV1> {
        unavailable::<GenerationGitHistoryEvidenceV1>()
    }

    fn read_blame(
        &self,
        _: &tracedecay_domain::CodeGenerationId,
        _: &FileOccurrenceId,
    ) -> GenerationProviderReadV1<GenerationGitBlameEvidenceV1> {
        unavailable::<GenerationGitBlameEvidenceV1>()
    }
}

struct BlameAuthority(GenerationGitBlameEvidenceV1);

impl GenerationGitEvidenceAuthorityV1 for BlameAuthority {
    fn read_diff(
        &self,
        _: &tracedecay_domain::CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationGitDiffEvidenceV1> {
        unavailable()
    }

    fn read_history(
        &self,
        _: &tracedecay_domain::CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationGitHistoryEvidenceV1> {
        unavailable()
    }

    fn read_blame(
        &self,
        _: &tracedecay_domain::CodeGenerationId,
        _: &FileOccurrenceId,
    ) -> GenerationProviderReadV1<GenerationGitBlameEvidenceV1> {
        complete(self.0.clone())
    }
}

struct ContextAuthority(GenerationGitContextProvidersV1);

impl GenerationGitContextAuthorityV1 for ContextAuthority {
    fn read_context(
        &self,
        _: &tracedecay_domain::CodeGenerationId,
    ) -> GenerationGitContextProvidersV1 {
        self.0.clone()
    }
}

#[test]
fn production_readers_join_exact_authorities_and_reject_foreign_generation() {
    let (snapshot, manifest) = generation();
    let code = GenerationJoinCodeAuthorityV1 {
        manifest: manifest.clone(),
        snapshot: snapshot.clone(),
    };
    let diagnostic_reader = ProductionGenerationDiagnosticJoinReaderV1::new(
        code.clone(),
        Arc::new(DiagnosticAuthority(diagnostic_evidence(
            &snapshot, &manifest,
        ))),
    );
    let diagnostics = diagnostic_reader.read_generation_diagnostics(&manifest.generation_id);
    assert_eq!(
        diagnostics.provider_state,
        ProviderEvaluationStateV1::SupportedCompletedComplete
    );

    let attribution_reader = ProductionGenerationTestAttributionJoinReaderV1::new(
        code.clone(),
        Arc::new(AttributionAuthority(attribution_evidence(
            &snapshot, &manifest,
        ))),
    );
    let attribution = attribution_reader.read_test_attribution(&manifest.generation_id);
    assert_eq!(
        attribution.provider_state,
        ProviderEvaluationStateV1::SupportedCompletedComplete
    );

    let graph = complete(GraphImpactResult {
        affected_files: vec![id("file.source")],
        affected_callers: vec![id("symbol.caller")],
        evidence_anchors: vec![id("anchor.hazard")],
    });
    let tests = complete(AffectedTestsResult {
        tests: vec![id("symbol.test")],
        attributions: Vec::new(),
    });
    let occurrences = vec![
        GenerationOccurrenceBindingV1 {
            generation_id: manifest.generation_id.clone(),
            symbol_occurrence_id: id("symbol.source"),
            file_occurrence_id: id("file.source"),
            content_digest: content('a'),
        },
        GenerationOccurrenceBindingV1 {
            generation_id: manifest.generation_id.clone(),
            symbol_occurrence_id: id("symbol.caller"),
            file_occurrence_id: id("file.source"),
            content_digest: content('a'),
        },
        GenerationOccurrenceBindingV1 {
            generation_id: manifest.generation_id.clone(),
            symbol_occurrence_id: id("symbol.test"),
            file_occurrence_id: id("file.test"),
            content_digest: content('b'),
        },
    ];
    let impact = GenerationImpactJoinV1::join(&manifest, &snapshot, graph, tests, &occurrences)
        .expect("impact");

    let diff = GitDiffV1 {
        repository: snapshot.snapshot.repository.clone(),
        scope: GitDiffScopeV1::WorkingTree,
        files: vec![GitFileDiffV1 {
            path: "src/lib.rs".to_owned(),
            original_path: None,
            change: GitChangeKindV1::Modified,
            old_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).expect("mode")),
            new_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).expect("mode")),
            old_blob: Some(oid('1')),
            new_blob: Some(oid('2')),
            binary: false,
            submodule: false,
            insertions: Some(2),
            deletions: Some(1),
            hunks: vec![GitHunkV1 {
                old_start: 3,
                old_lines: 1,
                new_start: 3,
                new_lines: 2,
                section: None,
                patch_digest: digest('3'),
            }],
        }],
        coverage: GitCoverageV1::complete(),
    };
    let mut watermark = GenerationGitWatermarkV1 {
        repository: snapshot.snapshot.repository.clone(),
        source_revision: snapshot.snapshot.source_revision.clone(),
        snapshot_content_identity: snapshot.snapshot.content_identity.clone(),
        scope: GenerationGitEvidenceScopeV1 {
            worktree: snapshot.snapshot.worktree.clone(),
            index_tree: Some(oid('4')),
            tree: Some(oid('5')),
            reference: snapshot.snapshot.reference.clone(),
            options_digest: digest('6'),
        },
        diff_scope: diff.scope.clone(),
        git_snapshot_digest: digest('7'),
        captured_at: UtcMicros(60),
    };
    watermark.git_snapshot_digest = watermark
        .recompute_evidence_digest(&diff)
        .expect("git digest");
    let git_reader = ProductionGenerationGitJoinReaderV1::new(
        code,
        Arc::new(GitAuthority(GenerationGitDiffEvidenceV1 {
            diff,
            watermark,
            file_contents: vec![GitFileContentIdentityV1 {
                path: "src/lib.rs".to_owned(),
                content_digest: content('a'),
            }],
        })),
        Arc::new(ContextAuthority(GenerationGitContextProvidersV1 {
            symbol_bindings: vec![GitSymbolLineBindingV1 {
                generation_id: manifest.generation_id.clone(),
                file_occurrence_id: id("file.source"),
                symbol_occurrence_id: id("symbol.source"),
                content_digest: content('a'),
                start_line: 2,
                end_line: 5,
            }],
            impacts: vec![(id("symbol.source"), complete(impact))],
            diagnostics: diagnostics.clone(),
            test_attribution: attribution.clone(),
        })),
    );

    let joined = git_reader.read_git_diff(&manifest.generation_id);
    let hunk = &joined.evidence.expect("joined diff").files[0].hunk_contexts[0];
    assert_eq!(hunk.symbol_occurrence_ids, vec![id("symbol.source")]);
    assert_eq!(hunk.diagnostic_anchors, vec![id("anchor.diagnostic")]);
    assert_eq!(hunk.hazard_anchors, vec![id("anchor.hazard")]);
    assert_eq!(hunk.affected_tests, vec![id("symbol.test")]);

    let foreign = git_reader.read_git_diff(&id("generation.foreign"));
    assert_eq!(
        foreign.provider_state,
        ProviderEvaluationStateV1::Unavailable
    );
    assert!(foreign.evidence.is_none());
}

#[test]
fn production_readers_abstain_as_stale_on_join_identity_drift() {
    let (snapshot, manifest) = generation();
    let mut evidence = diagnostic_evidence(&snapshot, &manifest);
    evidence.watermark.content_identity = content('e');
    let reader = ProductionGenerationDiagnosticJoinReaderV1::new(
        GenerationJoinCodeAuthorityV1 {
            manifest: manifest.clone(),
            snapshot,
        },
        Arc::new(DiagnosticAuthority(evidence)),
    );

    let result = reader.read_generation_diagnostics(&manifest.generation_id);
    assert_eq!(result.provider_state, ProviderEvaluationStateV1::Stale);
    assert_eq!(result.coverage, GenerationProviderCoverageV1::Unavailable);
    assert!(result.evidence.is_none());
}

#[test]
fn production_readers_preserve_cancelled_provider_state_with_partial_evidence() {
    let (snapshot, manifest) = generation();
    let read = GenerationProviderReadV1::new(
        ProviderEvaluationStateV1::Cancelled,
        GenerationProviderCoverageV1::Partial {
            examined: 2,
            eligible: 1,
            excluded: 0,
            unknown: 1,
            capped: false,
        },
        Some(diagnostic_evidence(&snapshot, &manifest)),
    )
    .expect("cancelled read may retain bounded partial evidence");
    let reader = ProductionGenerationDiagnosticJoinReaderV1::new(
        GenerationJoinCodeAuthorityV1 {
            manifest: manifest.clone(),
            snapshot,
        },
        Arc::new(DiagnosticReadAuthority(read)),
    );

    let result = reader.read_generation_diagnostics(&manifest.generation_id);
    assert_eq!(result.provider_state, ProviderEvaluationStateV1::Cancelled);
    assert!(matches!(
        result.coverage,
        GenerationProviderCoverageV1::Partial { unknown: 1, .. }
    ));
    assert!(result.evidence.is_some());
}

#[test]
fn production_blame_reader_rejects_another_requested_file() {
    let (snapshot, manifest) = generation();
    let blame = GitBlameV1 {
        repository: snapshot.snapshot.repository.clone(),
        path: "src/lib.rs".to_owned(),
        lines: Vec::new(),
        availability: GitBlameAvailabilityV1::Available,
        coverage: GitCoverageV1::complete(),
    };
    let mut watermark = GenerationGitReadWatermarkV1 {
        repository: snapshot.snapshot.repository.clone(),
        source_revision: snapshot.snapshot.source_revision.clone(),
        snapshot_content_identity: snapshot.snapshot.content_identity.clone(),
        scope: GenerationGitEvidenceScopeV1 {
            worktree: snapshot.snapshot.worktree.clone(),
            index_tree: Some(oid('4')),
            tree: Some(oid('5')),
            reference: snapshot.snapshot.reference.clone(),
            options_digest: digest('6'),
        },
        evidence_digest: digest('7'),
        captured_at: UtcMicros(60),
    };
    watermark.evidence_digest = watermark
        .recompute_blame_digest(&blame)
        .expect("blame digest");
    let reader = ProductionGenerationGitJoinReaderV1::new(
        GenerationJoinCodeAuthorityV1 {
            manifest: manifest.clone(),
            snapshot,
        },
        Arc::new(BlameAuthority(GenerationGitBlameEvidenceV1 {
            blame,
            watermark,
            file_content: GitFileContentIdentityV1 {
                path: "src/lib.rs".to_owned(),
                content_digest: content('a'),
            },
            symbol_bindings: Vec::new(),
        })),
        Arc::new(ContextAuthority(GenerationGitContextProvidersV1 {
            symbol_bindings: Vec::new(),
            impacts: Vec::new(),
            diagnostics: unavailable(),
            test_attribution: unavailable(),
        })),
    );

    let result = reader.read_git_blame(&manifest.generation_id, &id("file.test"));
    assert_eq!(result.provider_state, ProviderEvaluationStateV1::Stale);
    assert_eq!(result.coverage, GenerationProviderCoverageV1::Unavailable);
    assert!(result.evidence.is_none());
}
