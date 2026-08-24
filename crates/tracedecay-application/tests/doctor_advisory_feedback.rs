mod common;

use std::future::Future;
use std::task::{Context, Poll, Waker};

use tracedecay_application::{
    AdvisoryFeedbackDoctorPort, AdvisoryFeedbackFindingReadV1, AdvisoryFeedbackReadV1,
    AdvisoryFeedbackSummaryReadV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1,
    DoctorReportComposerV1, DoctorSourceFuture, RequestContext,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, FeedbackCycleId, FeedbackCycleTerminationV1, FeedbackFindingId,
    FeedbackFindingLifecycleV1, FeedbackResultId, FeedbackScopeV1, ProjectId,
    ProviderEvaluationStateV1, RepositoryId, RetrievalAnchorId, WorktreeId,
};

struct StaticFeedback(AdvisoryFeedbackReadV1);

impl AdvisoryFeedbackDoctorPort for StaticFeedback {
    fn advisory_feedback<'a>(
        &'a self,
        _context: &'a RequestContext,
    ) -> DoctorSourceFuture<'a, AdvisoryFeedbackReadV1> {
        let read = self.0.clone();
        Box::pin(async move { read })
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = Box::pin(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("doctor fixture futures must complete immediately"),
    }
}

#[test]
fn advisory_feedback_preserves_canonical_identity_lifecycle_and_coverage() {
    let scope = FeedbackScopeV1 {
        project_id: ProjectId::new("project-1").expect("project"),
        repository_id: RepositoryId::new("repository-1").expect("repository"),
        worktree_id: WorktreeId::new("worktree-1").expect("worktree"),
        branch_ref: "refs/heads/main".to_string(),
        head_commit_id: CommitId::new("commit-1").expect("commit"),
    };
    let summary = AdvisoryFeedbackSummaryReadV1 {
        result_id: FeedbackResultId::new("feedback.result.1").expect("result"),
        cycle_id: FeedbackCycleId::new("feedback.cycle.1").expect("cycle"),
        scope: scope.clone(),
        generation_id: CodeGenerationId::new("generation.1").expect("generation"),
        generation_current: true,
        termination: FeedbackCycleTerminationV1::IncompleteCoverage,
        provider_states: vec![ProviderEvaluationStateV1::Partial],
        total_findings: 4,
        returned_findings: 1,
        omitted_findings: 3,
    };
    let port = StaticFeedback(AdvisoryFeedbackReadV1::Observed {
        summary: Box::new(summary),
        findings: vec![AdvisoryFeedbackFindingReadV1 {
            result_id: FeedbackResultId::new("feedback.result.1").expect("result"),
            cycle_id: FeedbackCycleId::new("feedback.cycle.1").expect("cycle"),
            finding_id: FeedbackFindingId::new("feedback.finding.1").expect("finding"),
            scope,
            generation_id: CodeGenerationId::new("generation.1").expect("generation"),
            generation_current: true,
            lifecycle: FeedbackFindingLifecycleV1::Active,
            provider_state: ProviderEvaluationStateV1::Partial,
            evidence_anchors: vec![RetrievalAnchorId::new("anchor-1").expect("retrieval anchor")],
            total_findings: 4,
            returned_findings: 1,
            omitted_findings: 3,
        }],
    });
    let context = common::context(&common::operation());

    let report = block_on(
        DoctorReportComposerV1::new()
            .with_advisory_feedback(&port)
            .compose(&context),
    )
    .expect("compose");
    let finding = report
        .findings()
        .find(|finding| {
            finding.family() == DoctorFindingFamilyV1::Advisory
                && finding.evidence().iter().any(|evidence| {
                    evidence.reference().as_str() == "feedback.finding:feedback.finding.1"
                })
        })
        .expect("canonical advisory finding");

    assert_eq!(finding.state(), DoctorEvidenceStateV1::Partial);
    assert_eq!(
        finding.coverage().statement(),
        "feedback coverage returned 1/4 findings; omitted 3"
    );
    for expected in [
        "feedback.result:feedback.result.1",
        "feedback.cycle:feedback.cycle.1",
        "feedback.scope.project:project-1",
        "feedback.scope.repository:repository-1",
        "feedback.scope.worktree:worktree-1",
        "feedback.scope.branch:refs/heads/main",
        "feedback.scope.head:commit-1",
        "feedback.generation:generation.1",
        "feedback.lifecycle:active",
        "feedback.provider_state:partial",
        "feedback.anchor:anchor-1",
    ] {
        assert!(
            finding
                .evidence()
                .iter()
                .any(|evidence| evidence.reference().as_str() == expected),
            "missing evidence {expected}"
        );
    }
}

#[test]
fn advisory_feedback_keeps_omitted_only_result_distinct_from_absence() {
    let scope = FeedbackScopeV1 {
        project_id: ProjectId::new("project-1").expect("project"),
        repository_id: RepositoryId::new("repository-1").expect("repository"),
        worktree_id: WorktreeId::new("worktree-1").expect("worktree"),
        branch_ref: "refs/heads/main".to_string(),
        head_commit_id: CommitId::new("commit-1").expect("commit"),
    };
    let port = StaticFeedback(AdvisoryFeedbackReadV1::Observed {
        summary: Box::new(AdvisoryFeedbackSummaryReadV1 {
            result_id: FeedbackResultId::new("feedback.result.omitted").expect("result"),
            cycle_id: FeedbackCycleId::new("feedback.cycle.omitted").expect("cycle"),
            scope,
            generation_id: CodeGenerationId::new("generation.omitted").expect("generation"),
            generation_current: true,
            termination: FeedbackCycleTerminationV1::Blocked,
            provider_states: vec![ProviderEvaluationStateV1::Partial],
            total_findings: 3,
            returned_findings: 0,
            omitted_findings: 3,
        }),
        findings: Vec::new(),
    });
    let report = block_on(
        DoctorReportComposerV1::new()
            .with_advisory_feedback(&port)
            .compose(&common::context(&common::operation())),
    )
    .expect("compose");
    let finding = report
        .findings()
        .find(|finding| {
            finding.evidence().iter().any(|evidence| {
                evidence.reference().as_str() == "feedback.result:feedback.result.omitted"
            })
        })
        .expect("omitted-only summary");
    assert_eq!(finding.state(), DoctorEvidenceStateV1::Partial);
    assert_eq!(
        finding.coverage().statement(),
        "feedback coverage returned 0/3 findings; omitted 3"
    );
}

#[test]
fn advisory_feedback_rejects_summary_row_identity_disagreement() {
    let scope = FeedbackScopeV1 {
        project_id: ProjectId::new("project-1").expect("project"),
        repository_id: RepositoryId::new("repository-1").expect("repository"),
        worktree_id: WorktreeId::new("worktree-1").expect("worktree"),
        branch_ref: "refs/heads/main".to_string(),
        head_commit_id: CommitId::new("commit-1").expect("commit"),
    };
    let port = StaticFeedback(AdvisoryFeedbackReadV1::Observed {
        summary: Box::new(AdvisoryFeedbackSummaryReadV1 {
            result_id: FeedbackResultId::new("feedback.result.expected").expect("result"),
            cycle_id: FeedbackCycleId::new("feedback.cycle.1").expect("cycle"),
            scope: scope.clone(),
            generation_id: CodeGenerationId::new("generation.1").expect("generation"),
            generation_current: true,
            termination: FeedbackCycleTerminationV1::Blocked,
            provider_states: vec![ProviderEvaluationStateV1::Partial],
            total_findings: 1,
            returned_findings: 1,
            omitted_findings: 0,
        }),
        findings: vec![AdvisoryFeedbackFindingReadV1 {
            result_id: FeedbackResultId::new("feedback.result.foreign").expect("result"),
            cycle_id: FeedbackCycleId::new("feedback.cycle.1").expect("cycle"),
            finding_id: FeedbackFindingId::new("feedback.finding.1").expect("finding"),
            scope,
            generation_id: CodeGenerationId::new("generation.1").expect("generation"),
            generation_current: true,
            lifecycle: FeedbackFindingLifecycleV1::Active,
            provider_state: ProviderEvaluationStateV1::Partial,
            evidence_anchors: Vec::new(),
            total_findings: 1,
            returned_findings: 1,
            omitted_findings: 0,
        }],
    });

    assert!(
        block_on(
            DoctorReportComposerV1::new()
                .with_advisory_feedback(&port)
                .compose(&common::context(&common::operation()))
        )
        .is_err()
    );
}
