use super::{FeedbackDiagnosticProjectionSkipV1, classify_feedback_diagnostic_admission};
use crate::diagnostics_publication::{
    CleanGenerationDiagnosticScopeV1, CleanGenerationDiagnosticSnapshotBuilderV1,
    DiagnosticContributionV1, DiagnosticPillarV1,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, DiagnosticRecordStateV1, DiagnosticSeverityV1,
    FileOccurrenceId, GenerationDiagnosticV1, SourceSpan, UtcMicros,
};

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

/// Builds a record through the real production publication builder so the
/// admission rules are exercised against records shaped exactly like the
/// ones a pillar publishes.
fn record(pillar: DiagnosticPillarV1) -> GenerationDiagnosticV1 {
    let mut builder =
        CleanGenerationDiagnosticSnapshotBuilderV1::new(CleanGenerationDiagnosticScopeV1 {
            generation_id: id("generation.admission.1"),
            repository: id("repository.fixture"),
            worktree: Some(id("worktree.fixture")),
            reference: Some(id("ref.main")),
            source_revision: Some(id("commit.head")),
            analyzer_revision: id("analyzer.v1"),
            configuration_revision: id("config.v1"),
            collected_at: UtcMicros(1_700_000_000_000_000),
        });
    builder
        .contribute(
            pillar,
            DiagnosticContributionV1 {
                anchor: id("anchor.admission.1"),
                file_occurrence_id: id("src/lib.rs"),
                content_digest: id(&digest('a')),
                span: SourceSpan {
                    start_byte: 0,
                    end_byte: 4,
                },
                symbol_occurrence_id: None,
                code: "E0308".to_owned(),
                severity: DiagnosticSeverityV1::Error,
                message: "mismatched types".to_owned(),
            },
        )
        .expect("contribution accepted");
    builder.records().pop().expect("one record")
}

fn admit(
    record: &GenerationDiagnosticV1,
    target: Option<&FileOccurrenceId>,
) -> Result<(), FeedbackDiagnosticProjectionSkipV1> {
    classify_feedback_diagnostic_admission(
        record,
        target,
        &id::<CodeGenerationId>("generation.admission.1"),
        &id::<ContentDigest>(&digest('a')),
        &id::<CommitId>("commit.head"),
    )
}

#[test]
fn every_pillar_record_is_admitted_when_identity_matches() {
    for pillar in [
        DiagnosticPillarV1::Compiler,
        DiagnosticPillarV1::GitHubReview,
        DiagnosticPillarV1::CiLocalization,
        DiagnosticPillarV1::Proximity,
    ] {
        let record = record(pillar);
        let target: FileOccurrenceId = id("src/lib.rs");
        assert_eq!(
            admit(&record, Some(&target)),
            Ok(()),
            "{pillar:?} record was refused despite exact identity"
        );
    }
}

/// The formerly silent case: the record attaches to a different file than
/// the cycle's impact target. It must now be a named refusal.
#[test]
fn impact_target_file_mismatch_is_named_not_silent() {
    let record = record(DiagnosticPillarV1::Compiler);
    let other: FileOccurrenceId = id("src/other.rs");
    assert_eq!(
        admit(&record, Some(&other)),
        Err(FeedbackDiagnosticProjectionSkipV1::ImpactTargetFileMismatch)
    );
}

#[test]
fn absent_impact_target_is_named_separately_from_mismatch() {
    let record = record(DiagnosticPillarV1::Proximity);
    assert_eq!(
        admit(&record, None),
        Err(FeedbackDiagnosticProjectionSkipV1::ImpactTargetAbsent)
    );
}

#[test]
fn generation_content_state_and_revision_drift_each_have_a_reason() {
    let target: FileOccurrenceId = id("src/lib.rs");

    let mut wrong_generation = record(DiagnosticPillarV1::GitHubReview);
    wrong_generation.generation_id = id("generation.admission.2");
    assert_eq!(
        admit(&wrong_generation, Some(&target)),
        Err(FeedbackDiagnosticProjectionSkipV1::GenerationMismatch)
    );

    let mut wrong_content = record(DiagnosticPillarV1::CiLocalization);
    wrong_content.content_digest = id(&digest('b'));
    assert_eq!(
        admit(&wrong_content, Some(&target)),
        Err(FeedbackDiagnosticProjectionSkipV1::ContentDigestMismatch)
    );

    let mut cleared = record(DiagnosticPillarV1::Compiler);
    cleared.state = DiagnosticRecordStateV1::Cleared {
        cleared_in_generation: id("generation.admission.2"),
    };
    assert_eq!(
        admit(&cleared, Some(&target)),
        Err(FeedbackDiagnosticProjectionSkipV1::RecordNotCurrent)
    );

    let mut drifted = record(DiagnosticPillarV1::Compiler);
    drifted.source_revision = Some(id("commit.other"));
    assert_eq!(
        admit(&drifted, Some(&target)),
        Err(FeedbackDiagnosticProjectionSkipV1::SourceRevisionDrift)
    );
}
