use std::cell::Cell;

use tracedecay_domain::{
    AuthorizedRerankView, ExactClass, FreshnessCompatibilityV1, OptionalStagePublicStatus,
    RankedCandidate, RerankPolicy, RetrievalAnchorId, RetrieverKind, RetrieverOutcome,
    SanitizedStageFailure,
};

use super::*;
use crate::retrieval::fusion::{CompositionKernel, FusionStageInput};
use crate::retrieval::rerank::{
    BoundedRerankRuntimeV1, DeterministicLocalRerankExecutorV1, EphemeralRerankViewSourceV1,
    LocalRerankFailureV1, LocalRerankInputV1, LocalRerankPermitV1, RerankExecutionControlV1,
    RerankViewOutcomeV1, RerankViewPermitV1,
};

fn ranked_candidates() -> Vec<RankedCandidate> {
    CompositionKernel::new(id("ranking.fixture.v1"))
        .compose(
            &FusionStageInput {
                profile: profile(),
                lanes: composition_lanes(vec![
                    (
                        RetrieverKind::ExactLiteral,
                        RetrieverOutcome::Complete(batch(
                            vec![exact_candidate("exact", 1_000_000)],
                            "exact",
                        )),
                    ),
                    (
                        RetrieverKind::Lexical,
                        RetrieverOutcome::Complete(batch(
                            vec![
                                candidate(RetrieverKind::Lexical, "approx-a", 900_000, 0),
                                candidate(RetrieverKind::Lexical, "approx-b", 800_000, 1),
                                candidate(RetrieverKind::Lexical, "approx-c", 700_000, 2),
                            ],
                            "lexical",
                        )),
                    ),
                    (
                        RetrieverKind::Graph,
                        RetrieverOutcome::Complete(batch(Vec::new(), "graph")),
                    ),
                ]),
            },
            &no_caps(),
        )
        .expect("fixture composition succeeds")
        .ranked_candidates
}

fn rerank_policy() -> RerankPolicy {
    RerankPolicy {
        policy_id: id("rerank.fixture.v1"),
        evaluation_result_anchor: id("evaluation.fixture"),
        max_candidates: 2,
        max_input_bytes: 64,
        max_input_tokens: 16,
        max_work_units: 16,
        max_model_invocations: 1,
        deadline_micros: Some(100),
    }
}

struct Control {
    elapsed: Cell<u64>,
    cancelled: bool,
}

impl RerankExecutionControlV1 for Control {
    fn elapsed_micros(&self) -> u64 {
        self.elapsed.get()
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

#[derive(Default)]
struct Views {
    outcome: Option<RerankViewOutcomeV1>,
    calls: usize,
}

impl EphemeralRerankViewSourceV1 for Views {
    fn authorize_ephemeral_view(
        &mut self,
        request: &RetrievalRequest,
        candidate: &RankedCandidate,
        _permit: &RerankViewPermitV1,
    ) -> RerankViewOutcomeV1 {
        self.calls += 1;
        self.outcome
            .clone()
            .unwrap_or_else(|| RerankViewOutcomeV1::Authorized {
                view: AuthorizedRerankView {
                    anchor_id: candidate.candidate.anchor_id.clone(),
                    snapshot_digest: request.snapshot.compute_digest().unwrap(),
                    privacy_domain: request.scope.privacy_domain.clone(),
                    compatibility: FreshnessCompatibilityV1::Current,
                    approved_features: candidate.candidate.anchor_id.as_str().as_bytes().to_vec(),
                },
                input_tokens: 1,
                work_units: 1,
            })
    }
}

#[derive(Default)]
struct ReverseExecutor {
    calls: Cell<usize>,
    failure: Option<LocalRerankFailureV1>,
}

impl DeterministicLocalRerankExecutorV1 for ReverseExecutor {
    fn planned_model_invocations(
        &self,
        _candidate_count: u32,
    ) -> Result<u32, LocalRerankFailureV1> {
        Ok(1)
    }

    fn rerank(
        &self,
        _policy: &RerankPolicy,
        inputs: &[LocalRerankInputV1<'_>],
        _permit: LocalRerankPermitV1,
    ) -> Result<Vec<RetrievalAnchorId>, LocalRerankFailureV1> {
        self.calls.set(self.calls.get() + 1);
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        Ok(inputs
            .iter()
            .rev()
            .map(|input| input.candidate.candidate.anchor_id.clone())
            .collect())
    }
}

#[test]
fn reranks_only_the_bounded_approximate_prefix_and_bypasses_exact() {
    let request = request();
    let policy = rerank_policy();
    let before = ranked_candidates();
    let exact_before = before
        .iter()
        .filter(|candidate| candidate.candidate.exact_class != ExactClass::Approximate)
        .cloned()
        .collect::<Vec<_>>();
    let approximate_before = before
        .iter()
        .filter(|candidate| candidate.candidate.exact_class == ExactClass::Approximate)
        .map(|candidate| candidate.candidate.anchor_id.clone())
        .collect::<Vec<_>>();
    let mut views = Views::default();
    let executor = ReverseExecutor::default();
    let control = Control {
        elapsed: Cell::new(1),
        cancelled: false,
    };

    let outcome = BoundedRerankRuntimeV1::new(&mut views, &executor)
        .rerank(&request, &policy, &before, &control);

    assert_eq!(outcome.public_status, OptionalStagePublicStatus::Complete);
    assert_eq!(
        outcome.ordered_candidates[..exact_before.len()],
        exact_before[..]
    );
    let approximate_after = outcome
        .ordered_candidates
        .iter()
        .filter(|candidate| candidate.candidate.exact_class == ExactClass::Approximate)
        .map(|candidate| candidate.candidate.anchor_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(approximate_after[0], approximate_before[1]);
    assert_eq!(approximate_after[1], approximate_before[0]);
    assert_eq!(approximate_after[2], approximate_before[2]);
    assert_eq!(views.calls, 2);
    assert_eq!(executor.calls.get(), 1);
}

#[test]
fn mounted_trait_object_authorities_execute_the_bounded_runtime() {
    let request = request();
    let policy = rerank_policy();
    let before = ranked_candidates();
    let mut views = Views::default();
    let executor = ReverseExecutor::default();
    let views: &mut dyn EphemeralRerankViewSourceV1 = &mut views;
    let executor: &dyn DeterministicLocalRerankExecutorV1 = &executor;
    let control = Control {
        elapsed: Cell::new(1),
        cancelled: false,
    };

    let outcome =
        BoundedRerankRuntimeV1::new(views, executor).rerank(&request, &policy, &before, &control);

    assert_eq!(outcome.public_status, OptionalStagePublicStatus::Complete);
    assert_eq!(
        outcome
            .ordered_candidates
            .iter()
            .map(|candidate| candidate.candidate.anchor_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "anchor.exact",
            "anchor.approx-b",
            "anchor.approx-a",
            "anchor.approx-c"
        ]
    );
}

#[test]
fn missing_view_preserves_the_exact_pre_rerank_bytes() {
    let request = request();
    let policy = rerank_policy();
    let before = ranked_candidates();
    let before_bytes = serde_json::to_vec(&before).unwrap();
    let mut views = Views {
        outcome: Some(RerankViewOutcomeV1::Missing),
        calls: 0,
    };
    let executor = ReverseExecutor::default();
    let control = Control {
        elapsed: Cell::new(1),
        cancelled: false,
    };

    let outcome = BoundedRerankRuntimeV1::new(&mut views, &executor)
        .rerank(&request, &policy, &before, &control);

    assert_eq!(
        outcome.public_status,
        OptionalStagePublicStatus::Unavailable(SanitizedStageFailure::AuthorityUnavailable)
    );
    assert_eq!(
        serde_json::to_vec(&outcome.ordered_candidates).unwrap(),
        before_bytes
    );
    assert_eq!(executor.calls.get(), 0);
}

#[test]
fn executor_error_timeout_and_cancellation_preserve_pre_rerank_bytes() {
    let request = request();
    let policy = rerank_policy();
    let before = ranked_candidates();
    let before_bytes = serde_json::to_vec(&before).unwrap();
    let cases = [
        LocalRerankFailureV1::Unavailable(SanitizedStageFailure::Internal),
        LocalRerankFailureV1::TimedOut,
        LocalRerankFailureV1::Cancelled,
    ];

    for failure in cases {
        let mut views = Views::default();
        let executor = ReverseExecutor {
            calls: Cell::new(0),
            failure: Some(failure),
        };
        let control = Control {
            elapsed: Cell::new(1),
            cancelled: false,
        };
        let outcome = BoundedRerankRuntimeV1::new(&mut views, &executor)
            .rerank(&request, &policy, &before, &control);
        assert_eq!(
            serde_json::to_vec(&outcome.ordered_candidates).unwrap(),
            before_bytes
        );
        assert_ne!(outcome.public_status, OptionalStagePublicStatus::Complete);
    }

    let mut views = Views::default();
    let executor = ReverseExecutor::default();
    let cancelled = Control {
        elapsed: Cell::new(1),
        cancelled: true,
    };
    let outcome = BoundedRerankRuntimeV1::new(&mut views, &executor)
        .rerank(&request, &policy, &before, &cancelled);
    assert_eq!(
        serde_json::to_vec(&outcome.ordered_candidates).unwrap(),
        before_bytes
    );
    assert_eq!(outcome.public_status, OptionalStagePublicStatus::Cancelled);
}

#[test]
fn resource_and_deadline_limits_fail_before_executor_work() {
    let request = request();
    let before = ranked_candidates();
    let before_bytes = serde_json::to_vec(&before).unwrap();
    let mut policy = rerank_policy();
    policy.max_input_bytes = 1;
    let mut views = Views::default();
    let executor = ReverseExecutor::default();
    let control = Control {
        elapsed: Cell::new(1),
        cancelled: false,
    };

    let outcome = BoundedRerankRuntimeV1::new(&mut views, &executor)
        .rerank(&request, &policy, &before, &control);
    assert!(matches!(
        outcome.public_status,
        OptionalStagePublicStatus::BudgetExceeded(_)
    ));
    assert_eq!(
        serde_json::to_vec(&outcome.ordered_candidates).unwrap(),
        before_bytes
    );
    assert_eq!(executor.calls.get(), 0);

    let mut work_policy = rerank_policy();
    work_policy.max_work_units = 1;
    let mut views = Views {
        outcome: Some(RerankViewOutcomeV1::Authorized {
            view: AuthorizedRerankView {
                anchor_id: id("anchor.unused"),
                snapshot_digest: digest_id('9'),
                privacy_domain: id("privacy.fixture"),
                compatibility: FreshnessCompatibilityV1::Current,
                approved_features: Vec::new(),
            },
            input_tokens: 0,
            work_units: 2,
        }),
        calls: 0,
    };
    let executor = ReverseExecutor::default();
    let outcome = BoundedRerankRuntimeV1::new(&mut views, &executor).rerank(
        &request,
        &work_policy,
        &before,
        &control,
    );
    assert!(matches!(
        outcome.public_status,
        OptionalStagePublicStatus::BudgetExceeded(_)
    ));
    assert_eq!(
        serde_json::to_vec(&outcome.ordered_candidates).unwrap(),
        before_bytes
    );
    assert_eq!(executor.calls.get(), 0);

    let mut model_policy = rerank_policy();
    model_policy.max_model_invocations = 0;
    let mut views = Views::default();
    let executor = ReverseExecutor::default();
    let outcome = BoundedRerankRuntimeV1::new(&mut views, &executor).rerank(
        &request,
        &model_policy,
        &before,
        &control,
    );
    assert!(matches!(
        outcome.public_status,
        OptionalStagePublicStatus::BudgetExceeded(_)
    ));
    assert_eq!(
        serde_json::to_vec(&outcome.ordered_candidates).unwrap(),
        before_bytes
    );
    assert_eq!(executor.calls.get(), 0);

    let mut views = Views::default();
    let executor = ReverseExecutor::default();
    let expired = Control {
        elapsed: Cell::new(policy.deadline_micros.unwrap()),
        cancelled: false,
    };
    let outcome = BoundedRerankRuntimeV1::new(&mut views, &executor)
        .rerank(&request, &policy, &before, &expired);
    assert!(matches!(
        outcome.public_status,
        OptionalStagePublicStatus::BudgetExceeded(_)
    ));
    assert_eq!(views.calls, 0);
    assert_eq!(executor.calls.get(), 0);
}
