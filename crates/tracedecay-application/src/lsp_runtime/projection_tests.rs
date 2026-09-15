use super::context_projection::{
    advisory_coverage, advisory_finding_matches, advisory_projection_producer,
    advisory_provider_state, advisory_provider_status, bounded_advisory_item_omissions,
    finding_item, finding_matches_document, test_run_projection,
};
use super::feedback_source::{feedback_content_is_current, ordered_context_changes};
use super::managed_test_runs::bind_test_run_document_content;
use std::collections::{BTreeMap, BTreeSet};

use super::{LspFeedbackProjectionScope, ProjectionChangeQueue};
use crate::operation_stream::{ManagedTestRunResult, ManagedTestRunSnapshot, OperationId};
use tracedecay_contracts::{Deadline, OperationTermination, RequestId};
use tracedecay_domain::feedback::{
    FeedbackAdvisoryProviderStateV1, FeedbackContentIdentityV1, FeedbackDiagnosticClassificationV1,
    FeedbackDiagnosticProducerV1, FeedbackDiagnosticProjectionV1, FeedbackFindingId,
    FeedbackFindingLifecycleV1, FeedbackFindingV1, ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, ContentDigest, DiagnosticSeverityV1, FileOccurrenceId,
    ManifestDigest, SourceSpan, UtcMicros,
};
use tracedecay_lsp::{
    AdmittedRoot, ContextCoverage, ContextFreshness, ContextProducerState, ContextProjectionChange,
    ContextProjectionKind, ContextProjectionOutcome, ContextProjectionRegistration,
    MAX_CONTEXT_PROJECTION_ITEMS, TRACEDECAY_CONTEXT_REVISION,
};

fn finding(lifecycle: FeedbackFindingLifecycleV1) -> FeedbackFindingV1 {
    FeedbackFindingV1 {
        finding_id: FeedbackFindingId::new("finding.lifecycle").expect("finding"),
        classification: FeedbackDiagnosticClassificationV1::New,
        lifecycle,
        retrieval_anchor_id: None,
        provider_state: ProviderEvaluationStateV1::SupportedCompletedComplete,
        safe_bounded_preview: Some("bounded finding".to_owned()),
        diagnostic_projection: None,
    }
}

fn advisory_finding(
    producer: FeedbackDiagnosticProducerV1,
    lifecycle: FeedbackFindingLifecycleV1,
) -> FeedbackFindingV1 {
    FeedbackFindingV1 {
        diagnostic_projection: Some(FeedbackDiagnosticProjectionV1 {
            file: FileOccurrenceId::new("src/lib.rs").expect("file"),
            span: SourceSpan {
                start_byte: 0,
                end_byte: 1,
            },
            symbol: None,
            code: "advisory".to_owned(),
            severity: DiagnosticSeverityV1::Warning,
            safe_bounded_message: "bounded advisory finding".to_owned(),
            producer,
            code_description_uri: None,
        }),
        ..finding(lifecycle)
    }
}

fn projection_scope() -> LspFeedbackProjectionScope {
    LspFeedbackProjectionScope {
        head_commit_id: CommitId::new("0123456789abcdef0123456789abcdef01234567").expect("commit"),
        code_generation_id: CodeGenerationId::new("generation.v1.aaaaaaaa.00000001")
            .expect("code generation"),
        snapshot_digest: ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
            .expect("snapshot digest"),
        invalidation_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64)))
            .expect("invalidation digest"),
        snapshot_content_digest: ContentDigest::new(format!("sha256:{}", "c".repeat(64)))
            .expect("snapshot content digest"),
        document_file_occurrence_id: Some(
            FileOccurrenceId::new("src/lib.rs").expect("document file"),
        ),
        document_content_digest: None,
        document_relative_path: Some("src/lib.rs".to_owned()),
        generation: 42,
    }
}

fn change(kind: ContextProjectionKind, generation: u64) -> ContextProjectionChange {
    ContextProjectionChange {
        root_uri: "file:///root".to_owned(),
        document_uri: Some("file:///root/src/lib.rs".to_owned()),
        kind,
        generation,
        identity: projection_scope().projection_identity(),
        freshness: ContextFreshness::Current,
        producer_state: ContextProducerState::Complete,
        coverage: ContextCoverage::Complete,
        revision: TRACEDECAY_CONTEXT_REVISION,
        retrieval_handle: None,
    }
}

#[test]
fn feedback_change_queue_replays_latest_advisory_state_in_delivery_order() {
    let root = AdmittedRoot::new("file:///root");
    let queue = ProjectionChangeQueue::default();
    queue.offer(
        "before-subscription".to_owned(),
        change(ContextProjectionKind::diagnostics(), 1),
    );
    assert!(queue.snapshot(&root, &BTreeSet::new()).is_empty());

    let subscriptions = [
        ContextProjectionKind::diagnostics(),
        ContextProjectionKind::post_edit_impact(),
        ContextProjectionKind::affected_tests(),
        ContextProjectionKind::test_run_results(),
        ContextProjectionKind::github_review(),
        ContextProjectionKind::ci_failure_localization(),
        ContextProjectionKind::agent_proximity(),
    ]
    .into_iter()
    .map(|kind| ContextProjectionRegistration {
        kind,
        revision: TRACEDECAY_CONTEXT_REVISION,
    })
    .collect::<BTreeSet<_>>();
    queue.offer(
        "diagnostics-1".to_owned(),
        change(ContextProjectionKind::diagnostics(), 1),
    );
    queue.offer(
        "diagnostics-2".to_owned(),
        change(ContextProjectionKind::diagnostics(), 2),
    );
    for (revision, kind) in [
        ("affected-1", ContextProjectionKind::affected_tests()),
        ("impact-1", ContextProjectionKind::post_edit_impact()),
        ("github-1", ContextProjectionKind::github_review()),
        ("ci-1", ContextProjectionKind::ci_failure_localization()),
        ("proximity-1", ContextProjectionKind::agent_proximity()),
    ] {
        queue.offer(revision.to_owned(), change(kind, 1));
    }

    let changes = queue.snapshot(&root, &subscriptions);
    assert_eq!(
        changes
            .iter()
            .map(|change| change.kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "diagnostics",
            "postEditImpact",
            "affectedTests",
            "githubReview",
            "ciFailureLocalization",
            "agentProximity",
        ]
    );
    assert_eq!(changes[0].generation, 2);
    assert_eq!(queue.snapshot(&root, &subscriptions), changes);

    queue.offer(
        "diagnostics-2".to_owned(),
        change(ContextProjectionKind::diagnostics(), 2),
    );
    assert_eq!(queue.snapshot(&root, &subscriptions), changes);
    queue.offer(
        "diagnostics-3".to_owned(),
        change(ContextProjectionKind::diagnostics(), 3),
    );
    let latest = queue.snapshot(&root, &subscriptions);
    assert_eq!(latest.len(), 6);
    assert_eq!(latest[0].generation, 3);

    let delivered = ordered_context_changes(
        latest,
        vec![change(ContextProjectionKind::test_run_results(), 1)],
    );
    assert_eq!(
        delivered
            .iter()
            .map(|change| change.kind.as_str())
            .collect::<Vec<_>>(),
        vec![
            "diagnostics",
            "postEditImpact",
            "affectedTests",
            "githubReview",
            "ciFailureLocalization",
            "agentProximity",
            "testRunResults",
        ],
        "the source appends a test execution only after its saved-edit feedback cycle"
    );
}

#[test]
fn advisory_projection_keeps_only_its_active_canonical_producer_findings() {
    let findings = [
        advisory_finding(
            FeedbackDiagnosticProducerV1::GitHubReview,
            FeedbackFindingLifecycleV1::Active,
        ),
        advisory_finding(
            FeedbackDiagnosticProducerV1::CiLocalization,
            FeedbackFindingLifecycleV1::Active,
        ),
        advisory_finding(
            FeedbackDiagnosticProducerV1::Proximity,
            FeedbackFindingLifecycleV1::Cleared,
        ),
    ];

    assert!(advisory_finding_matches(
        &findings[0],
        FeedbackDiagnosticProducerV1::GitHubReview
    ));
    assert!(!advisory_finding_matches(
        &findings[1],
        FeedbackDiagnosticProducerV1::GitHubReview
    ));
    assert!(finding_item(&findings[0]).is_some());
    assert!(
        finding_item(&findings[2]).is_none(),
        "a cleared advisory finding must clear its LSP projection"
    );
    assert_eq!(
        advisory_projection_producer(&ContextProjectionKind::github_review()),
        Some(FeedbackDiagnosticProducerV1::GitHubReview)
    );
    assert_eq!(
        advisory_projection_producer(&ContextProjectionKind::ci_failure_localization()),
        Some(FeedbackDiagnosticProducerV1::CiLocalization)
    );
    assert_eq!(
        advisory_projection_producer(&ContextProjectionKind::agent_proximity()),
        Some(FeedbackDiagnosticProducerV1::Proximity)
    );
}

#[test]
fn advisory_projection_uses_its_canonical_provider_state_and_document() {
    let github = advisory_finding(
        FeedbackDiagnosticProducerV1::GitHubReview,
        FeedbackFindingLifecycleV1::Active,
    );
    let mut other_file = github.clone();
    other_file
        .diagnostic_projection
        .as_mut()
        .expect("advisory projection")
        .file = FileOccurrenceId::new("src/other.rs").expect("other file");
    let scope = projection_scope();

    assert!(finding_matches_document(&github, &scope, None));
    assert!(
        !finding_matches_document(&other_file, &scope, None),
        "a finding for another document must not receive this document's handle"
    );
    let states = [
        FeedbackAdvisoryProviderStateV1 {
            producer: FeedbackDiagnosticProducerV1::CiLocalization,
            state: ProviderEvaluationStateV1::Unavailable,
        },
        FeedbackAdvisoryProviderStateV1 {
            producer: FeedbackDiagnosticProducerV1::GitHubReview,
            state: ProviderEvaluationStateV1::SupportedCompletedComplete,
        },
        FeedbackAdvisoryProviderStateV1 {
            producer: FeedbackDiagnosticProducerV1::Proximity,
            state: ProviderEvaluationStateV1::Failed,
        },
    ];
    assert_eq!(
        advisory_provider_state(&states, FeedbackDiagnosticProducerV1::GitHubReview),
        Some(ProviderEvaluationStateV1::SupportedCompletedComplete)
    );
    assert_eq!(
        advisory_provider_state(&states, FeedbackDiagnosticProducerV1::CiLocalization),
        Some(ProviderEvaluationStateV1::Unavailable)
    );
    assert_eq!(
        advisory_provider_state(&states, FeedbackDiagnosticProducerV1::Proximity),
        Some(ProviderEvaluationStateV1::Failed)
    );
    assert_eq!(
        advisory_provider_state(
            &[FeedbackAdvisoryProviderStateV1 {
                producer: FeedbackDiagnosticProducerV1::CiLocalization,
                state: ProviderEvaluationStateV1::SupportedCompletedComplete,
            }],
            FeedbackDiagnosticProducerV1::GitHubReview,
        ),
        None,
        "an untyped aggregate provider vector must not be relabelled as GitHub"
    );
    assert_eq!(
        advisory_provider_status(advisory_provider_state(
            &states,
            FeedbackDiagnosticProducerV1::GitHubReview,
        )),
        (ContextCoverage::Complete, ContextProducerState::Complete)
    );
    assert_eq!(
        advisory_provider_status(advisory_provider_state(
            &states,
            FeedbackDiagnosticProducerV1::CiLocalization,
        )),
        (
            ContextCoverage::Unavailable,
            ContextProducerState::Unavailable
        )
    );
    assert_eq!(
        advisory_provider_status(advisory_provider_state(
            &states,
            FeedbackDiagnosticProducerV1::Proximity,
        )),
        (ContextCoverage::Failed, ContextProducerState::Failed)
    );
}

#[test]
fn aggregate_advisory_omissions_are_unattributed_and_do_not_inflate_lanes() {
    assert_eq!(
        advisory_coverage(
            ContextCoverage::Complete,
            ContextProducerState::Complete,
            u64::MAX,
        ),
        (ContextCoverage::Partial, ContextProducerState::Partial),
        "cycle-wide omissions must be unattributed partiality, not a complete producer projection"
    );
    assert_eq!(
        bounded_advisory_item_omissions(7, 3),
        4,
        "only the lane's bounded items belong in its omitted count"
    );
    assert_eq!(
        bounded_advisory_item_omissions(3, 7),
        0,
        "an unattributed aggregate omission never inflates an individual lane"
    );
    assert_eq!(
        advisory_coverage(ContextCoverage::Complete, ContextProducerState::Complete, 0),
        (ContextCoverage::Complete, ContextProducerState::Complete)
    );
}

#[test]
fn feedback_projection_requires_exact_saved_generation_and_file_identity() {
    let file_digest =
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).expect("file digest");
    let scope = LspFeedbackProjectionScope {
        document_content_digest: Some(
            ContentDigest::new(file_digest.as_str().to_owned()).expect("document digest"),
        ),
        ..projection_scope()
    };
    let current = FeedbackContentIdentityV1::SavedContent {
        generation_digest: scope.snapshot_digest.clone(),
        file_digest: file_digest.clone(),
    };
    let stale_generation = FeedbackContentIdentityV1::SavedContent {
        generation_digest: ManifestDigest::new(format!("sha256:{}", "e".repeat(64)))
            .expect("stale generation"),
        file_digest: file_digest.clone(),
    };
    let stale_file = FeedbackContentIdentityV1::SavedContent {
        generation_digest: scope.snapshot_digest.clone(),
        file_digest: ManifestDigest::new(format!("sha256:{}", "f".repeat(64))).expect("stale file"),
    };

    assert!(feedback_content_is_current(Some(&current), &scope));
    assert!(!feedback_content_is_current(
        Some(&stale_generation),
        &scope
    ));
    assert!(!feedback_content_is_current(Some(&stale_file), &scope));
    assert!(!feedback_content_is_current(None, &scope));
}

#[test]
fn root_latest_test_run_is_not_relabelled_as_current_code_scope() {
    let scope = projection_scope();
    let snapshot = ManagedTestRunSnapshot {
        operation_id: OperationId::from_request(
            RequestId::new("request.test-run.unbound").expect("request"),
        ),
        generation: 7,
        source_revision: 1,
        head_commit_id: None,
        code_generation_id: None,
        document_content_digests: BTreeMap::new(),
        deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        results: Vec::new(),
        result_offset: 0,
        available_results: 0,
        next_cursor: None,
        completed: 0,
        total: Some(0),
        termination: Some(OperationTermination::Completed),
        receipt: None,
    };

    assert_eq!(
        test_run_projection(
            AdmittedRoot::new("file:///root"),
            None,
            None,
            scope,
            snapshot
        ),
        ContextProjectionOutcome::Deferred {
            reason: "managed-test-run-head-unbound".to_owned(),
        }
    );
}

#[test]
fn current_complete_test_run_projects_ready_results() {
    let scope = projection_scope();
    let snapshot = ManagedTestRunSnapshot {
        operation_id: OperationId::from_request(
            RequestId::new("request.test-run.current").expect("request"),
        ),
        generation: 7,
        source_revision: 1,
        head_commit_id: Some(scope.head_commit_id.clone()),
        code_generation_id: Some(scope.code_generation_id.clone()),
        document_content_digests: BTreeMap::new(),
        deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        results: vec![ManagedTestRunResult {
            test: "suite::passes".to_owned(),
            passed: true,
        }],
        result_offset: 0,
        available_results: 1,
        next_cursor: None,
        completed: 1,
        total: Some(1),
        termination: Some(OperationTermination::Completed),
        receipt: None,
    };

    let ContextProjectionOutcome::Ready(envelope) = test_run_projection(
        AdmittedRoot::new("file:///root"),
        None,
        None,
        scope,
        snapshot,
    ) else {
        panic!("current complete run must be ready");
    };
    assert_eq!(envelope.coverage, ContextCoverage::Complete);
    assert_eq!(envelope.producer_state, ContextProducerState::Complete);
    assert_eq!(envelope.items.len(), 1);
    assert_eq!(envelope.items[0].summary, "passed: suite::passes");
}

#[test]
fn preexisting_dirty_overlay_cannot_relabel_saved_test_results() {
    let saved_digest =
        ContentDigest::new(format!("sha256:{}", "d".repeat(64))).expect("saved digest");
    let overlay_digest =
        ContentDigest::new(format!("sha256:{}", "e".repeat(64))).expect("overlay digest");
    let mut scope = LspFeedbackProjectionScope {
        document_content_digest: Some(saved_digest),
        ..projection_scope()
    };

    assert_eq!(
        bind_test_run_document_content(&mut scope, Some(overlay_digest)),
        Err("managed-test-run-document-content-stale")
    );
}

#[test]
fn current_test_run_projection_reports_the_canonical_page_boundary() {
    let scope = projection_scope();
    let operation_id =
        OperationId::from_request(RequestId::new("request.test-run.bounded").expect("request"));
    let snapshot = ManagedTestRunSnapshot {
        operation_id: operation_id.clone(),
        generation: 7,
        source_revision: 1,
        head_commit_id: Some(scope.head_commit_id.clone()),
        code_generation_id: Some(scope.code_generation_id.clone()),
        document_content_digests: BTreeMap::new(),
        deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        results: (0..MAX_CONTEXT_PROJECTION_ITEMS)
            .map(|index| ManagedTestRunResult {
                test: format!("suite::test_{index}"),
                passed: true,
            })
            .collect(),
        result_offset: 0,
        available_results: MAX_CONTEXT_PROJECTION_ITEMS + 1,
        next_cursor: None,
        completed: (MAX_CONTEXT_PROJECTION_ITEMS + 1) as u64,
        total: Some((MAX_CONTEXT_PROJECTION_ITEMS + 1) as u64),
        termination: Some(OperationTermination::Completed),
        receipt: None,
    };

    let ContextProjectionOutcome::Ready(envelope) = test_run_projection(
        AdmittedRoot::new("file:///root"),
        None,
        None,
        scope,
        snapshot,
    ) else {
        panic!("bounded current run must be ready");
    };
    assert_eq!(envelope.coverage, ContextCoverage::Partial);
    assert_eq!(envelope.items.len(), MAX_CONTEXT_PROJECTION_ITEMS);
    assert_eq!(envelope.omitted_count, 1);
    assert_eq!(
        envelope.items[MAX_CONTEXT_PROJECTION_ITEMS - 1].stable_id,
        format!("{operation_id}.{}", MAX_CONTEXT_PROJECTION_ITEMS - 1)
    );
}

#[test]
fn overlay_digest_drift_invalidates_saved_test_result_currentness() {
    let saved_digest =
        ContentDigest::new(format!("sha256:{}", "d".repeat(64))).expect("saved digest");
    let drifted_digest =
        ContentDigest::new(format!("sha256:{}", "e".repeat(64))).expect("drifted digest");
    let mut scope = LspFeedbackProjectionScope {
        document_content_digest: Some(saved_digest.clone()),
        ..projection_scope()
    };

    assert_eq!(
        bind_test_run_document_content(&mut scope, Some(saved_digest.clone())),
        Ok(())
    );
    assert_eq!(
        bind_test_run_document_content(&mut scope, Some(drifted_digest)),
        Err("managed-test-run-document-content-stale")
    );
    assert_eq!(scope.document_content_digest, Some(saved_digest));
}

#[test]
fn saved_document_drift_rejects_stale_test_run_results() {
    let document_uri = "file:///root/src/lib.rs";
    let current_digest =
        ContentDigest::new(format!("sha256:{}", "d".repeat(64))).expect("current digest");
    let stale_digest =
        ContentDigest::new(format!("sha256:{}", "e".repeat(64))).expect("stale digest");
    let scope = LspFeedbackProjectionScope {
        document_content_digest: Some(current_digest),
        ..projection_scope()
    };
    let snapshot = ManagedTestRunSnapshot {
        operation_id: OperationId::from_request(
            RequestId::new("request.test-run.saved-drift").expect("request"),
        ),
        generation: 7,
        source_revision: 1,
        head_commit_id: Some(scope.head_commit_id.clone()),
        code_generation_id: Some(scope.code_generation_id.clone()),
        document_content_digests: BTreeMap::from([(document_uri.to_owned(), stale_digest)]),
        deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        results: Vec::new(),
        result_offset: 0,
        available_results: 0,
        next_cursor: None,
        completed: 0,
        total: Some(0),
        termination: Some(OperationTermination::Completed),
        receipt: None,
    };

    assert_eq!(
        test_run_projection(
            AdmittedRoot::new("file:///root"),
            Some(document_uri.to_owned()),
            Some(document_uri),
            scope,
            snapshot,
        ),
        ContextProjectionOutcome::Deferred {
            reason: "managed-test-run-document-content-stale".to_owned(),
        }
    );
}

#[test]
fn overlay_projection_requires_test_run_document_identity() {
    let document_uri = "file:///root/src/lib.rs";
    let scope = LspFeedbackProjectionScope {
        document_content_digest: Some(
            ContentDigest::new(format!("sha256:{}", "d".repeat(64))).expect("document digest"),
        ),
        ..projection_scope()
    };
    let snapshot = ManagedTestRunSnapshot {
        operation_id: OperationId::from_request(
            RequestId::new("request.test-run.overlay-unbound").expect("request"),
        ),
        generation: 7,
        source_revision: 1,
        head_commit_id: Some(scope.head_commit_id.clone()),
        code_generation_id: Some(scope.code_generation_id.clone()),
        document_content_digests: BTreeMap::new(),
        deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        results: Vec::new(),
        result_offset: 0,
        available_results: 0,
        next_cursor: None,
        completed: 0,
        total: Some(0),
        termination: Some(OperationTermination::Completed),
        receipt: None,
    };

    assert_eq!(
        test_run_projection(
            AdmittedRoot::new("file:///root"),
            Some(document_uri.to_owned()),
            Some(document_uri),
            scope,
            snapshot,
        ),
        ContextProjectionOutcome::Deferred {
            reason: "managed-test-run-document-content-unbound".to_owned(),
        }
    );
}

#[test]
fn expired_unfinished_test_run_projects_timed_out_unavailable() {
    let scope = projection_scope();
    let snapshot = ManagedTestRunSnapshot {
        operation_id: OperationId::from_request(
            RequestId::new("request.test-run.expired").expect("request"),
        ),
        generation: 7,
        source_revision: 1,
        head_commit_id: Some(scope.head_commit_id.clone()),
        code_generation_id: Some(scope.code_generation_id.clone()),
        document_content_digests: BTreeMap::new(),
        deadline: Deadline::new(UtcMicros(1)).expect("deadline"),
        results: Vec::new(),
        result_offset: 0,
        available_results: 0,
        next_cursor: None,
        completed: 0,
        total: Some(1),
        termination: None,
        receipt: None,
    };

    let ContextProjectionOutcome::Ready(envelope) = test_run_projection(
        AdmittedRoot::new("file:///root"),
        None,
        None,
        scope,
        snapshot,
    ) else {
        panic!("expired run must produce a terminal projection");
    };
    assert_eq!(envelope.coverage, ContextCoverage::Unavailable);
    assert_eq!(envelope.producer_state, ContextProducerState::TimedOut);
    assert!(envelope.items.is_empty());
}

#[test]
fn noncomplete_test_terminations_use_protocol_valid_state_pairs() {
    for (termination, coverage, producer_state) in [
        (
            OperationTermination::Partial,
            ContextCoverage::Partial,
            ContextProducerState::Partial,
        ),
        (
            OperationTermination::Cancelled,
            ContextCoverage::Unavailable,
            ContextProducerState::Cancelled,
        ),
        (
            OperationTermination::EffectUnknown,
            ContextCoverage::Unavailable,
            ContextProducerState::Unavailable,
        ),
    ] {
        let scope = projection_scope();
        let snapshot = ManagedTestRunSnapshot {
            operation_id: OperationId::from_request(
                RequestId::new(format!("request.test-run.{termination:?}")).expect("request"),
            ),
            generation: 7,
            source_revision: 1,
            head_commit_id: Some(scope.head_commit_id.clone()),
            code_generation_id: Some(scope.code_generation_id.clone()),
            document_content_digests: BTreeMap::new(),
            deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            results: Vec::new(),
            result_offset: 0,
            available_results: 0,
            next_cursor: None,
            completed: 0,
            total: Some(1),
            termination: Some(termination),
            receipt: None,
        };
        let ContextProjectionOutcome::Ready(envelope) = test_run_projection(
            AdmittedRoot::new("file:///root"),
            None,
            None,
            scope,
            snapshot,
        ) else {
            panic!("{termination:?} run must be ready");
        };
        assert_eq!(envelope.coverage, coverage);
        assert_eq!(envelope.producer_state, producer_state);
    }
}
