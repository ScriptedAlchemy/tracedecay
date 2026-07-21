//! One canonical branch/PR fixture proving the four advisory pillars coexist.

use tracedecay_application::feedback_surface_catalog_contribution;
use tracedecay_domain::feedback::*;
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, FileOccurrenceId, ManifestDigest, ProjectId,
    ProviderId, RepositoryId, RetrievalAnchorId, SourceSpan, SymbolOccurrenceId, UtcMicros,
    WorktreeId,
};
use tracedecay_tool_catalog::{AvailabilityContract, UnavailabilityReason};

const SHA_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).expect("digest")
}

fn anchor(value: &str) -> RetrievalAnchorId {
    RetrievalAnchorId::new(value).expect("anchor")
}

fn scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.pr13.fixture").unwrap(),
        repository_id: RepositoryId::new("repository.pr13.fixture").unwrap(),
        worktree_id: WorktreeId::new("worktree.pr13.fixture").unwrap(),
        branch_ref: "refs/heads/feature/pr13".to_owned(),
        head_commit_id: CommitId::new("commit.pr13.head").unwrap(),
    }
}

fn finding(id: &str, retrieval_anchor_id: RetrievalAnchorId) -> FeedbackFindingV1 {
    FeedbackFindingV1 {
        finding_id: FeedbackFindingId::new(id).unwrap(),
        classification: FeedbackDiagnosticClassificationV1::New,
        lifecycle: FeedbackFindingLifecycleV1::Active,
        retrieval_anchor_id: Some(retrieval_anchor_id),
        provider_state: ProviderEvaluationStateV1::SupportedCompletedComplete,
        safe_bounded_preview: None,
    }
}

#[test]
fn four_pillars_share_one_cycle_result_and_reference_identities() {
    let scope = scope();
    let file = FileOccurrenceId::new("file.pr13.fixture").unwrap();
    let symbol = SymbolOccurrenceId::new("symbol.pr13.fixture").unwrap();
    let caller = SymbolOccurrenceId::new("symbol.pr13.caller").unwrap();
    let test_symbol = SymbolOccurrenceId::new("symbol.pr13.test").unwrap();
    let generation = CodeGenerationId::new("generation.pr13.fixture").unwrap();
    let span = SourceSpan {
        start_byte: 10,
        end_byte: 20,
    };
    let post_edit_anchor = anchor("anchor.pr13.post-edit");
    let ci_anchor = anchor("anchor.pr13.ci");
    let github_anchor = anchor("anchor.pr13.github");
    let proximity_anchor = anchor("anchor.pr13.proximity");

    let request = FeedbackCycleRequestV1::new(
        FeedbackCycleId::new("cycle.pr13.fixture").unwrap(),
        scope.clone(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest(SHA_A),
            file_digest: digest(SHA_B),
        },
        FeedbackTriggerV1::ExplicitDiagnostics,
        digest(SHA_A),
        digest(SHA_B),
        FeedbackBudgetV1::bounded(1_000, 1_000, 4_096, 1_000),
    )
    .unwrap();
    let findings = vec![
        finding("finding.pr13.post-edit", post_edit_anchor.clone()),
        finding("finding.pr13.ci", ci_anchor.clone()),
        finding("finding.pr13.github", github_anchor.clone()),
        finding("finding.pr13.proximity", proximity_anchor.clone()),
    ];
    let impact = FeedbackImpactV1 {
        target: FeedbackTargetV1 {
            file: file.clone(),
            span: Some(span),
            symbol: Some(symbol.clone()),
            generation_id: Some(generation.clone()),
        },
        affected_files: vec![file.clone()],
        affected_callers: vec![caller.clone()],
        affected_tests: vec![test_symbol.clone()],
        evidence_anchors: vec![post_edit_anchor.clone()],
        state: FeedbackImpactStateV1::Complete,
        affected_tests_state: FeedbackImpactStateV1::Complete,
    };
    let cycle_result = FeedbackCycleResultV1::new(
        &request,
        FeedbackCycleTerminationV1::Blocked,
        vec![ProviderEvaluationStateV1::SupportedCompletedComplete],
        vec![FeedbackBaselineStateV1::Complete],
        Some(impact),
        Some(FeedbackImpactStateV1::Complete),
        Some(FeedbackImpactStateV1::Complete),
        findings,
        4,
        4,
        0,
    )
    .unwrap();

    let ci = CiFailureLocalizationResultV1 {
        provider: ProviderId::new("provider.github-actions").unwrap(),
        run: CiFailureRunIdentityV1 {
            workflow_id: "workflow.1".to_owned(),
            job_id: "job.1".to_owned(),
            check_suite_id: "check-suite.1".to_owned(),
            check_run_id: "check-run.1".to_owned(),
            run_id: "run.1".to_owned(),
            attempt_id: "attempt.1".to_owned(),
        },
        parser: CiFailureParserIdentityV1 {
            parser_id: "parser.rust-test".to_owned(),
            parser_version: "1".to_owned(),
        },
        state: CiFailureLocalizationStateV1::Complete,
        coverage: CiFailureCoverageV1::Complete,
        failure_kind: CiFailureKindV1::TestFailure,
        failure_anchor: ci_anchor.clone(),
        failure_excerpt_digest: digest(SHA_A),
        branch: CiFailureBranchEvidenceV1 {
            scope: scope.clone(),
            provider_head_commit_id: scope.head_commit_id.clone(),
        },
        generation: Some(CiFailureGenerationEvidenceV1 {
            generation_id: generation.clone(),
            retrieval_anchor_id: anchor("anchor.pr13.ci-generation"),
        }),
        symbol: Some(CiFailureSymbolEvidenceV1 {
            retrieval_anchor_id: anchor("anchor.pr13.ci-symbol"),
            file: file.clone(),
            span,
            symbol: symbol.clone(),
        }),
        callers: vec![CiFailureCallerEvidenceV1 {
            retrieval_anchor_id: anchor("anchor.pr13.ci-caller"),
            caller_symbol: caller,
            relation: CiCallerRelationV1::DirectCall,
        }],
        tests: vec![CiFailureTestEvidenceV1 {
            retrieval_anchor_id: anchor("anchor.pr13.ci-test"),
            test_symbol,
        }],
        rerun_hints: vec![CiInertRerunHintV1 {
            target: CiInertRerunTargetV1::Test,
            retrieval_anchor_id: Some(anchor("anchor.pr13.ci-rerun-hint")),
        }],
        observed_at: UtcMicros(1),
    };
    ci.validate().unwrap();

    let provider = ProviderId::new("provider.github").unwrap();
    let pull_request_id = GitHubPullRequestIdV1::new("pull-request.421").unwrap();
    let original = GitHubReviewImmutableAnchorV1 {
        repository_id: scope.repository_id.clone(),
        commit_id: scope.head_commit_id.clone(),
        retrieval_anchor_id: github_anchor.clone(),
        file: file.clone(),
        content_digest: ContentDigest::new(SHA_A).unwrap(),
        span: Some(span),
        symbol: Some(symbol.clone()),
    };
    let github = GitHubReviewIngressResultV1 {
        provider: provider.clone(),
        scope: scope.clone(),
        pull_request_id: pull_request_id.clone(),
        provider_base_commit_id: CommitId::new("commit.pr13.base").unwrap(),
        provider_head_commit_id: scope.head_commit_id.clone(),
        merge_base_commit_id: CommitId::new("commit.pr13.merge-base").unwrap(),
        operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
        outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
        coverage: GitHubReviewCoverageV1::Complete,
        items: vec![GitHubReviewItemV1 {
            provider,
            repository_id: scope.repository_id.clone(),
            pull_request_id,
            review_id: Some(GitHubReviewIdV1::new("review.1").unwrap()),
            thread_id: Some(GitHubReviewThreadIdV1::new("thread.1").unwrap()),
            comment_id: GitHubReviewCommentIdV1::new("comment.1").unwrap(),
            reply_to_comment_id: None,
            author_anchor: anchor("anchor.pr13.github-author"),
            author_class: GitHubReviewAuthorClassV1::Maintainer,
            review_state: GitHubReviewStateV1::Commented,
            body_digest: digest(SHA_B),
            body_anchor: github_anchor.clone(),
            safe_url_anchor: Some(anchor("anchor.pr13.github-url")),
            lifecycle: GitHubReviewLifecycleV1::Current,
            provider_outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
            remap: GitHubReviewCurrentBranchRemapV1 {
                original: original.clone(),
                current_scope: scope.clone(),
                current: Some(original),
                state: GitHubReviewRemapStateV1::ExactCurrent,
            },
            observed_at: UtcMicros(1),
        }],
        fetched_at: UtcMicros(2),
    };
    github.validate().unwrap();

    let proximity = ProximityContributionV1 {
        contribution_id: ProximityContributionIdV1::new("proximity-contribution.1").unwrap(),
        warning_id: ProximityWarningIdV1::new("proximity-warning.1").unwrap(),
        warning_class: ProximityWarningClassV1::SameSymbol,
        source_observation_ids: vec![
            ProximityObservationIdV1::new("proximity-observation.1").unwrap(),
        ],
        retrieval_anchor_ids: vec![proximity_anchor.clone()],
        address: Some(ProximityAddressV1 {
            scope: scope.clone(),
            file,
            span: Some(span),
            symbol: Some(symbol),
        }),
        relation_paths: Vec::new(),
        risk_inputs: Some(ProximityRiskInputsV1 {
            overlap_size: 1,
            blast_radius_size: 1,
            relation_strength: ProximityRelationStrengthV1::Direct,
            branch_worktree_incompatibility: ProximityBranchWorktreeIncompatibilityV1::Compatible,
            freshness_decay_basis_points: 10_000,
        }),
        tier: ProximityTierV1::Immediate,
        threshold_value_basis_points: None,
        threshold_revision: None,
        raw_risk_basis_points: Some(10_000),
        observed_at: UtcMicros(1),
        expires_at: UtcMicros(100),
        coverage: ProximityCoverageV1::Complete,
        inclusion: ProximityInclusionV1::Included,
    };
    proximity.validate().unwrap();

    let reference_findings = vec![
        FeedbackReferenceFindingV1 {
            finding_id: FeedbackFindingId::new("finding.pr13.post-edit").unwrap(),
            kind: FeedbackReferenceFindingKindV1::PostEditDiagnostic,
            retrieval_anchor_id: post_edit_anchor,
            source_record_id: FeedbackReferenceSourceRecordIdV1::new("diagnostic.1").unwrap(),
            source_state: FeedbackReferenceSourceStateV1::PostEditDiagnostic(
                ProviderEvaluationStateV1::SupportedCompletedComplete,
            ),
            coverage: FeedbackReferenceCoverageV1::Complete,
            observed_at: UtcMicros(1),
            valid_at: UtcMicros(2),
            expires_at: UtcMicros(100),
            safe_bounded_preview: None,
        },
        FeedbackReferenceFindingV1 {
            finding_id: FeedbackFindingId::new("finding.pr13.ci").unwrap(),
            kind: FeedbackReferenceFindingKindV1::CiLocalization,
            retrieval_anchor_id: ci.failure_anchor.clone(),
            source_record_id: FeedbackReferenceSourceRecordIdV1::new("ci-failure.1").unwrap(),
            source_state: FeedbackReferenceSourceStateV1::CiLocalization(ci.state),
            coverage: FeedbackReferenceCoverageV1::Complete,
            observed_at: UtcMicros(1),
            valid_at: UtcMicros(2),
            expires_at: UtcMicros(100),
            safe_bounded_preview: None,
        },
        FeedbackReferenceFindingV1 {
            finding_id: FeedbackFindingId::new("finding.pr13.github").unwrap(),
            kind: FeedbackReferenceFindingKindV1::GitHubReview,
            retrieval_anchor_id: github.items[0].body_anchor.clone(),
            source_record_id: FeedbackReferenceSourceRecordIdV1::new("github-comment.1").unwrap(),
            source_state: FeedbackReferenceSourceStateV1::GitHubReview {
                lifecycle: github.items[0].lifecycle,
                provider_outcome: github.outcome,
            },
            coverage: FeedbackReferenceCoverageV1::Complete,
            observed_at: UtcMicros(1),
            valid_at: UtcMicros(2),
            expires_at: UtcMicros(100),
            safe_bounded_preview: None,
        },
        FeedbackReferenceFindingV1 {
            finding_id: FeedbackFindingId::new("finding.pr13.proximity").unwrap(),
            kind: FeedbackReferenceFindingKindV1::Proximity,
            retrieval_anchor_id: proximity_anchor,
            source_record_id: FeedbackReferenceSourceRecordIdV1::new("proximity-warning.1")
                .unwrap(),
            source_state: FeedbackReferenceSourceStateV1::Proximity(ProximityInclusionV1::Included),
            coverage: FeedbackReferenceCoverageV1::Complete,
            observed_at: UtcMicros(1),
            valid_at: UtcMicros(2),
            expires_at: UtcMicros(100),
            safe_bounded_preview: None,
        },
    ];
    let packet = FeedbackReferencePacketV1::from_cycle_result(
        &cycle_result,
        reference_findings,
        vec![proximity],
    )
    .unwrap();
    packet.validate_against(&cycle_result).unwrap();
    let packet_json = serde_json::to_string(&packet).unwrap();
    for prohibited in ["task_id", "workflow", "rank", "review_body", "ci_log"] {
        assert!(
            !packet_json.contains(prohibited),
            "reference packet leaked prohibited field {prohibited}"
        );
    }

    let catalog = feedback_surface_catalog_contribution().expect("feedback surface");
    let operations: std::collections::BTreeSet<_> = catalog
        .bindings()
        .iter()
        .map(|binding| binding.operation().as_str())
        .collect();
    assert!(operations.contains("feedback_diagnostics"));
    assert!(operations.contains("github_review_ingest"));
    assert!(operations.contains("ci_failure_localize"));
    assert!(operations.contains("feedback_proximity"));
    assert!(catalog.capabilities().iter().all(|capability| matches!(
        capability.availability(),
        AvailabilityContract::Unavailable {
            reason: UnavailabilityReason::NotImplemented,
        }
    )));
}
