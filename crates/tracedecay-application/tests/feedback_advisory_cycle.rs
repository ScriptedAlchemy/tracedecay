//! Integrated feedback-cycle behavior across diagnostics, CI, review, and proximity.

use tracedecay_application::feedback::{GitHubReviewReadRequestV1, GitHubReviewReadResponseV1};
use tracedecay_application::{AdvisoryFindingContributorV1, AdvisoryFindingValidityWindowV1};
use tracedecay_domain::feedback::*;
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, FileOccurrenceId, ManifestDigest, ProjectId,
    ProviderId, RepositoryId, RetrievalAnchorId, SourceSpan, SymbolOccurrenceId, UtcMicros,
    WorktreeId,
};

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
        project_id: ProjectId::new("project.feedback.fixture").unwrap(),
        repository_id: RepositoryId::new("repository.feedback.fixture").unwrap(),
        worktree_id: WorktreeId::new("worktree.feedback.fixture").unwrap(),
        branch_ref: "refs/heads/feature/feedback".to_owned(),
        head_commit_id: CommitId::new("commit.feedback.head").unwrap(),
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
        diagnostic_projection: None,
    }
}

#[test]
fn feedback_sources_share_one_cycle_result_and_canonical_anchors() {
    let scope = scope();
    let file = FileOccurrenceId::new("file.feedback-cycle.fixture").unwrap();
    let symbol = SymbolOccurrenceId::new("symbol.feedback-cycle.fixture").unwrap();
    let caller = SymbolOccurrenceId::new("symbol.feedback-cycle.caller").unwrap();
    let test_symbol = SymbolOccurrenceId::new("symbol.feedback-cycle.test").unwrap();
    let generation = CodeGenerationId::new("generation.feedback-cycle.fixture").unwrap();
    let span = SourceSpan {
        start_byte: 10,
        end_byte: 20,
    };
    let post_edit_anchor = anchor("anchor.feedback-cycle.post-edit");
    let ci_anchor = anchor("anchor.feedback-cycle.ci");
    let github_anchor = anchor("anchor.feedback-cycle.github");
    let proximity_anchor = anchor("anchor.feedback-cycle.proximity");

    let request = FeedbackCycleRequestV1::new(
        FeedbackCycleId::new("cycle.feedback-cycle.fixture").unwrap(),
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
        finding("finding.feedback-cycle.post-edit", post_edit_anchor.clone()),
        finding("finding.feedback-cycle.ci", ci_anchor.clone()),
        finding("finding.feedback-cycle.github", github_anchor.clone()),
        finding("finding.feedback-cycle.proximity", proximity_anchor.clone()),
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
    let canonical_packet = FeedbackEvidencePacketV1::from_request(
        &request,
        cycle_result.termination,
        &cycle_result.provider_states,
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
        source_degradation: None,
        failure_kind: CiFailureKindV1::TestFailure,
        failure_anchor: ci_anchor.clone(),
        branch: CiFailureBranchEvidenceV1 {
            scope: scope.clone(),
            provider_head_commit_id: scope.head_commit_id.clone(),
        },
        generation: Some(CiFailureGenerationEvidenceV1 {
            generation_id: generation.clone(),
            retrieval_anchor_id: anchor("anchor.feedback-cycle.ci-generation"),
        }),
        symbol: Some(CiFailureSymbolEvidenceV1 {
            retrieval_anchor_id: anchor("anchor.feedback-cycle.ci-symbol"),
            file: file.clone(),
            span,
            symbol: symbol.clone(),
        }),
        callers: vec![CiFailureCallerEvidenceV1 {
            retrieval_anchor_id: anchor("anchor.feedback-cycle.ci-caller"),
            caller_symbol: caller,
            relation: CiCallerRelationV1::DirectCall,
        }],
        tests: vec![CiFailureTestEvidenceV1 {
            retrieval_anchor_id: anchor("anchor.feedback-cycle.ci-test"),
            test_symbol,
        }],
        rerun_hints: vec![CiInertRerunHintV1 {
            target: CiInertRerunTargetV1::Test,
            retrieval_anchor_id: Some(anchor("anchor.feedback-cycle.ci-rerun-hint")),
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
        provider_base_commit_id: CommitId::new("commit.feedback-cycle.base").unwrap(),
        provider_head_commit_id: scope.head_commit_id.clone(),
        merge_base_commit_id: CommitId::new("commit.feedback-cycle.merge-base").unwrap(),
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
            version_digest: digest(SHA_A),
            author_anchor: anchor("anchor.feedback-cycle.github-author"),
            author_class: GitHubReviewAuthorClassV1::Maintainer,
            review_state: GitHubReviewStateV1::Commented,
            body_digest: digest(SHA_B),
            body_anchor: github_anchor.clone(),
            safe_url_anchor: Some(anchor("anchor.feedback-cycle.github-url")),
            safe_url: Some(
                "https://github.com/ScriptedAlchemy/tracedecay/pull/13#discussion_r1".to_owned(),
            ),
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
    let mut unsafe_github = github.clone();
    unsafe_github.items[0].safe_url =
        Some("https://user:secret@github.com/ScriptedAlchemy/tracedecay/pull/13".to_owned());
    assert!(unsafe_github.validate().is_err());
    let github_request = GitHubReviewReadRequestV1 {
        operation: github.operation,
        scope: scope.clone(),
        pull_request_id: github.pull_request_id.clone(),
    };
    let mut stale_without_evidence = github.clone();
    stale_without_evidence.outcome = GitHubReviewIngressProviderOutcomeV1::Stale;
    stale_without_evidence.coverage = GitHubReviewCoverageV1::Stale;
    stale_without_evidence.items[0].provider_outcome = GitHubReviewIngressProviderOutcomeV1::Stale;
    let empty_checkpoint = GitHubReviewReadCheckpointV1 {
        etag: None,
        next_cursor: None,
        rate_limit: None,
    };
    assert!(
        GitHubReviewReadResponseV1 {
            ingress: stale_without_evidence.clone(),
            checkpoint: empty_checkpoint.clone(),
        }
        .validate_for(&github_request)
        .is_err()
    );
    GitHubReviewReadResponseV1 {
        ingress: stale_without_evidence,
        checkpoint: GitHubReviewReadCheckpointV1 {
            etag: Some(GitHubReviewEtagV1::new("W/\"advisory-fixture\"").unwrap()),
            ..empty_checkpoint.clone()
        },
    }
    .validate_for(&github_request)
    .unwrap();
    assert!(
        GitHubReviewReadResponseV1 {
            ingress: github.clone(),
            checkpoint: GitHubReviewReadCheckpointV1 {
                next_cursor: Some(
                    GitHubReviewCursorV1::new("cursor.feedback-cycle.fixture").unwrap(),
                ),
                ..empty_checkpoint
            },
        }
        .validate_for(&github_request)
        .is_err(),
        "complete coverage cannot retain a continuation cursor"
    );
    let mut stale_github = github.clone();
    stale_github.provider_head_commit_id =
        CommitId::new("commit.feedback-cycle.stale-head").unwrap();
    stale_github.outcome = GitHubReviewIngressProviderOutcomeV1::Stale;
    stale_github.coverage = GitHubReviewCoverageV1::Stale;
    stale_github.items[0].provider_outcome = GitHubReviewIngressProviderOutcomeV1::Stale;
    stale_github.validate().unwrap();
    stale_github.outcome = GitHubReviewIngressProviderOutcomeV1::Complete;
    stale_github.coverage = GitHubReviewCoverageV1::Complete;
    stale_github.items[0].provider_outcome = GitHubReviewIngressProviderOutcomeV1::Complete;
    assert!(stale_github.validate().is_err());

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
    let mut stale_proximity = proximity.clone();
    stale_proximity.inclusion = ProximityInclusionV1::Stale;
    assert!(stale_proximity.validate().is_err());
    stale_proximity.coverage = ProximityCoverageV1::Stale;
    stale_proximity.validate().unwrap();

    let validity = AdvisoryFindingValidityWindowV1 {
        valid_at: UtcMicros(2),
        expires_at: UtcMicros(99),
    };
    let github_finding = github
        .advisory_findings(validity)
        .unwrap()
        .findings
        .pop()
        .expect("GitHub finding");
    assert_eq!(
        github_finding.retrieval_anchor_id.as_ref(),
        Some(&github_anchor),
        "evidence expansion keeps the provider body anchor"
    );
    assert_eq!(
        github_finding
            .diagnostic_projection
            .as_ref()
            .map(|projection| projection.producer),
        Some(FeedbackDiagnosticProducerV1::GitHubReview)
    );
    assert_eq!(
        github_finding
            .diagnostic_projection
            .as_ref()
            .and_then(|projection| projection.code_description_uri.as_deref()),
        Some("https://github.com/ScriptedAlchemy/tracedecay/pull/13#discussion_r1")
    );
    let ci_finding = ci
        .advisory_findings(validity)
        .unwrap()
        .findings
        .pop()
        .expect("CI finding");
    assert_eq!(
        ci_finding
            .diagnostic_projection
            .as_ref()
            .map(|projection| projection.producer),
        Some(FeedbackDiagnosticProducerV1::CiLocalization)
    );
    let proximity_finding = proximity
        .advisory_findings(validity)
        .unwrap()
        .findings
        .pop()
        .expect("proximity finding");
    assert_eq!(
        proximity_finding
            .diagnostic_projection
            .as_ref()
            .map(|projection| projection.producer),
        Some(FeedbackDiagnosticProducerV1::Proximity)
    );

    assert_eq!(
        cycle_result.termination,
        FeedbackCycleTerminationV1::Blocked
    );
    assert_eq!(
        canonical_packet.termination,
        FeedbackCycleTerminationV1::Blocked,
        "the canonical packet carries exactly one terminal cycle state"
    );
    for expected_anchor in [
        &post_edit_anchor,
        &ci_anchor,
        &github_anchor,
        &proximity_anchor,
    ] {
        assert!(cycle_result.findings.iter().any(|finding| {
            finding.retrieval_anchor_id.as_ref() == Some(expected_anchor)
                && finding.safe_bounded_preview.is_none()
        }));
    }
}
