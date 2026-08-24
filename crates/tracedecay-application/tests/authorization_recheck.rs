mod common;

use std::cell::Cell;

use tracedecay_application::{ApplicationProblemKind, AuthorizationService};
use tracedecay_domain::UtcMicros;
use tracedecay_policy::authorization::{
    AuthorizationSnapshotStateV1, ExternalContentStatusV1, PolicyEvaluatorVersionV1,
    SinkAdmissionProofV1, SourceAuthorizationDecisionV1, SourceAuthorizationEvaluator,
    SourceAuthorizationEvaluatorV1, SourceAuthorizationInputV1,
};

fn requires_sink_admission(_proof: &SinkAdmissionProofV1) {}

#[test]
fn admission_preserves_source_proof_until_the_effect_recheck() {
    let operation = common::operation();
    let context = common::context(&operation);
    let initial = common::authorized_source_input();
    let mut current = initial.clone();
    current.evaluated_at.0 += 1;
    let service = AuthorizationService::new(
        common::SequencedAuthorizationPort::snapshots([
            common::source_snapshot(initial.clone()),
            common::source_snapshot(current),
        ]),
        SourceAuthorizationEvaluatorV1::default(),
    );

    let admission = service
        .admit(&context, &operation, UtcMicros(10))
        .expect("live input admits with an opaque source proof");
    assert_eq!(
        admission.source_proof().effective_grant().budgets,
        initial.requested_access.budget
    );

    let sink_proof = service
        .recheck_effect(&context, &operation, &admission, UtcMicros(11))
        .expect("unchanged authority admits immediately before the effect");
    requires_sink_admission(&sink_proof);
    assert_eq!(
        sink_proof.effective_grant().budgets,
        initial.requested_access.budget
    );
}

#[test]
fn stale_policy_at_effect_recheck_returns_stale_without_sink_proof() {
    let operation = common::operation();
    let context = common::context(&operation);
    let initial = common::authorized_source_input();
    let mut stale = initial.clone();
    stale.snapshot_state = AuthorizationSnapshotStateV1::Stale;
    stale.evaluated_at.0 += 1;
    let service = AuthorizationService::new(
        common::SequencedAuthorizationPort::snapshots([
            common::source_snapshot(initial),
            common::source_snapshot(stale),
        ]),
        SourceAuthorizationEvaluatorV1::default(),
    );

    let admission = service.admit(&context, &operation, UtcMicros(10)).unwrap();
    let problem = service
        .recheck_effect(&context, &operation, &admission, UtcMicros(11))
        .unwrap_err();

    assert_eq!(problem.kind(), ApplicationProblemKind::Stale);
}

#[test]
fn deletion_after_admission_cannot_reach_an_effect_sink() {
    let operation = common::operation();
    let context = common::context(&operation);
    let initial = common::authorized_source_input();
    let mut deleted = initial.clone();
    deleted.content_status = ExternalContentStatusV1::AuthoritativeDeleted;
    deleted.evaluated_at.0 += 1;
    let service = AuthorizationService::new(
        common::SequencedAuthorizationPort::snapshots([
            common::source_snapshot(initial),
            common::source_snapshot(deleted),
        ]),
        SourceAuthorizationEvaluatorV1::default(),
    );

    let admission = service.admit(&context, &operation, UtcMicros(10)).unwrap();
    let problem = service
        .recheck_effect(&context, &operation, &admission, UtcMicros(11))
        .unwrap_err();

    assert_eq!(
        problem.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}

#[test]
fn narrowing_budget_after_admission_cannot_widen_effect_authority() {
    let operation = common::operation();
    let context = common::context(&operation);
    let initial = common::authorized_source_input();
    let mut narrowed = initial.clone();
    narrowed.requester_grant.budgets.bytes = 999;
    narrowed.evaluated_at.0 += 1;
    let service = AuthorizationService::new(
        common::SequencedAuthorizationPort::snapshots([
            common::source_snapshot(initial),
            common::source_snapshot(narrowed),
        ]),
        SourceAuthorizationEvaluatorV1::default(),
    );

    let admission = service.admit(&context, &operation, UtcMicros(10)).unwrap();
    let problem = service
        .recheck_effect(&context, &operation, &admission, UtcMicros(11))
        .unwrap_err();

    assert_eq!(
        problem.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}

struct TamperingEvaluator {
    inner: SourceAuthorizationEvaluatorV1,
    evaluations: Cell<u8>,
}

impl TamperingEvaluator {
    fn new() -> Self {
        Self {
            inner: SourceAuthorizationEvaluatorV1::default(),
            evaluations: Cell::new(0),
        }
    }
}

impl SourceAuthorizationEvaluator for TamperingEvaluator {
    fn evaluator_version(&self) -> &PolicyEvaluatorVersionV1 {
        self.inner.evaluator_version()
    }

    fn evaluate(&self, input: &SourceAuthorizationInputV1) -> SourceAuthorizationDecisionV1 {
        let evaluation = self.evaluations.get();
        self.evaluations.set(evaluation + 1);
        let mut decision = self.inner.evaluate(input);
        if evaluation > 0 {
            decision
                .effective_grant
                .as_mut()
                .expect("fixture input is initially authorized")
                .budgets
                .requests += 1;
        }
        decision
    }
}

#[test]
fn tampered_evaluation_cannot_mint_a_source_proof_or_policy_receipt() {
    let operation = common::operation();
    let context = common::context(&operation);
    let service = AuthorizationService::new(
        common::StaticAuthorizationPort::authorized(),
        TamperingEvaluator::new(),
    );

    let problem = service
        .admit(&context, &operation, UtcMicros(10))
        .unwrap_err();

    assert_eq!(problem.kind(), ApplicationProblemKind::Unavailable);
}
