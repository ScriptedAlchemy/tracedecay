//! Falsification tests for the work-loop shape, sizing, decomposition, and
//! route planner.
//!
//! Every test here is written to fail if the evaluator becomes non-deterministic,
//! invents a point estimate it did not earn, collapses the separate ordinal
//! dimensions into a score, or lets a human override outrank an exclusion.

use tracedecay_domain::{ManifestDigest, TaskId, UtcMicros};
use tracedecay_policy::work_loop::{
    WORK_CALIBRATION_SUPPORT_FLOOR, WorkBudgetEnvelopeV1, WorkContentLocationClassV1,
    WorkContentLocationLimitV1, WorkEffortClassV1, WorkEvidenceFrontierV1, WorkOrdinalBandV1,
    WorkPriorOutcomeV1, WorkPriorTerminalV1, WorkProposalCancellationV1, WorkProposalDecisionV1,
    WorkProposalDispositionV1, WorkProposalEvaluator, WorkProposalEvaluatorV1,
    WorkProposalPolicyInputV1, WorkProposalReasonV1, WorkProposalRuntimeCoverageV1,
    WorkRouteCandidateV1, WorkRouteOverrideV1, WorkRoutePlanV1, WorkTaskShapeKindV1,
};

const LOCAL_WATERMARK: i64 = 10;
const EVALUATED_AT: i64 = 100;

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
}

fn base_input() -> WorkProposalPolicyInputV1 {
    WorkProposalPolicyInputV1 {
        task_id: TaskId::try_from("task.policy.planner".to_owned()).expect("fixture task id"),
        based_on_version: 1,
        dependency_count: 0,
        unresolved_dependency_count: 0,
        accepted_proposal_present: false,
        execution_admitted: false,
        task_accepted: false,
        runtime: WorkProposalRuntimeCoverageV1::Complete {
            attempt_count: 0,
            terminal_attempt_count: 0,
        },
        local_evidence: Some(WorkEvidenceFrontierV1 {
            watermark: UtcMicros(LOCAL_WATERMARK),
            digest: digest('a'),
        }),
        live_git_evidence: None,
        policy_revision: 1,
        policy_digest: digest('b'),
        configuration_digest: digest('c'),
        configuration_revision: None,
        deadline: UtcMicros(1_000),
        cancellation: WorkProposalCancellationV1::Active,
        evaluated_at: UtcMicros(EVALUATED_AT),
        eligible_routes: Vec::new(),
        budget: None,
        content_location: None,
        prior_outcomes: Vec::new(),
        human_override: None,
    }
}

fn route(route_id: &str, band: WorkOrdinalBandV1) -> WorkRouteCandidateV1 {
    WorkRouteCandidateV1 {
        route_id: route_id.to_owned(),
        provider_capability_id: "capability.local".to_owned(),
        model_id: "model.local".to_owned(),
        effort: WorkEffortClassV1::Standard,
        declared_budget_ceiling: 10,
        content_location: WorkContentLocationClassV1::Local,
        correctness: band,
        sensitive_data_fitness: band,
        latency: band,
        cost: band,
        autonomy: band,
        evidence_quality: band,
    }
}

fn succeeded(route_id: &str, observed_at: i64) -> WorkPriorOutcomeV1 {
    WorkPriorOutcomeV1 {
        route_id: route_id.to_owned(),
        accepted: true,
        rework: false,
        escaped_defect: false,
        terminal: WorkPriorTerminalV1::Succeeded,
        observed_at: UtcMicros(observed_at),
    }
}

fn cohort(route_id: &str, count: i64, first_observed_at: i64) -> Vec<WorkPriorOutcomeV1> {
    (0..count)
        .map(|offset| succeeded(route_id, first_observed_at + offset))
        .collect()
}

fn evaluate(input: &WorkProposalPolicyInputV1) -> WorkProposalDecisionV1 {
    WorkProposalEvaluatorV1::default().evaluate(input)
}

fn route_plan(input: &WorkProposalPolicyInputV1) -> WorkRoutePlanV1 {
    evaluate(input)
        .route_plan
        .expect("a valid, live, in-deadline input always carries a route plan")
}

fn ranked_ids(plan: &WorkRoutePlanV1) -> Vec<String> {
    plan.ranked
        .iter()
        .map(|route| route.route_id.clone())
        .collect()
}

#[test]
fn identical_canonical_inputs_serialize_to_byte_identical_decisions() {
    let mut first_input = base_input();
    first_input.eligible_routes = vec![
        route("route.alpha", WorkOrdinalBandV1::High),
        route("route.beta", WorkOrdinalBandV1::Moderate),
    ];
    first_input.prior_outcomes = cohort("route.alpha", 8, 20);
    first_input.budget = Some(WorkBudgetEnvelopeV1 {
        ceiling: 100,
        spent: 40,
    });
    first_input.human_override = Some(WorkRouteOverrideV1 {
        route_id: "route.beta".to_owned(),
        recorded_at: UtcMicros(50),
    });
    let second_input = first_input.clone();

    let first = serde_json::to_vec(&evaluate(&first_input)).expect("decision serializes");
    let second = serde_json::to_vec(&evaluate(&second_input)).expect("decision serializes");

    assert_eq!(first, second);
}

#[test]
fn a_changed_planner_input_changes_the_input_digest() {
    let base = base_input();
    let mut with_routes = base.clone();
    with_routes.eligible_routes = vec![route("route.alpha", WorkOrdinalBandV1::High)];
    let mut with_outcomes = with_routes.clone();
    with_outcomes.prior_outcomes = cohort("route.alpha", 8, 20);

    let base_digest = evaluate(&base).input_digest;
    let route_digest = evaluate(&with_routes).input_digest;
    let outcome_digest = evaluate(&with_outcomes).input_digest;

    assert_ne!(base_digest, route_digest);
    assert_ne!(route_digest, outcome_digest);
}

#[test]
fn support_below_the_floor_refuses_sizing_and_widens_uncertainty() {
    let mut input = base_input();
    input.eligible_routes = vec![route("route.alpha", WorkOrdinalBandV1::High)];
    input.prior_outcomes = cohort("route.alpha", 7, 20);
    let decision = evaluate(&input);
    let plan = decision
        .route_plan
        .clone()
        .expect("routes survived, so a plan is present");

    assert_eq!(decision.sizing, None);
    assert!(
        decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::InsufficientCalibrationSupport)
    );
    assert_eq!(plan.coverage, WorkOrdinalBandV1::Highest);
    assert_eq!(plan.uncertainty, WorkOrdinalBandV1::Low);
    assert_eq!(
        plan.deterministic_baseline,
        Some("route.alpha".to_owned()),
        "absent sizing selects the declared baseline rather than a fabricated estimate"
    );
    assert!(decision.deterministic_fallback);
}

#[test]
fn support_at_the_floor_emits_sizing_carrying_the_governing_floor() {
    let mut input = base_input();
    input.eligible_routes = vec![route("route.alpha", WorkOrdinalBandV1::High)];
    input.prior_outcomes = cohort("route.alpha", 8, 20);
    let decision = evaluate(&input);
    let sizing = decision
        .sizing
        .clone()
        .expect("support at the floor emits calibrated sizing");
    let plan = decision.route_plan.clone().expect("plan is present");

    assert_eq!(sizing.support, WORK_CALIBRATION_SUPPORT_FLOOR);
    assert_eq!(sizing.support_floor, WORK_CALIBRATION_SUPPORT_FLOOR);
    assert_eq!(sizing.cohort, "route.alpha");
    assert_eq!(sizing.horizon, UtcMicros(27));
    assert_eq!(sizing.error, WorkOrdinalBandV1::Lowest);
    assert!(sizing.drift_valid);
    assert_eq!(plan.uncertainty, WorkOrdinalBandV1::Lowest);
    assert_eq!(plan.deterministic_baseline, None);
    assert!(!decision.deterministic_fallback);
    assert!(
        !decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::InsufficientCalibrationSupport)
    );
}

#[test]
fn outcomes_later_than_the_evaluation_instant_never_count_as_support() {
    let mut input = base_input();
    input.eligible_routes = vec![route("route.alpha", WorkOrdinalBandV1::High)];
    input.prior_outcomes = cohort("route.alpha", 8, EVALUATED_AT + 1);
    let decision = evaluate(&input);
    let plan = decision.route_plan.clone().expect("plan is present");

    assert_eq!(decision.sizing, None);
    assert!(
        decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::RouteEvidenceSparse)
    );
    assert_eq!(plan.coverage, WorkOrdinalBandV1::Lowest);
    assert_eq!(plan.uncertainty, WorkOrdinalBandV1::Highest);
}

#[test]
fn cohort_evidence_behind_the_local_frontier_is_recorded_as_stale_drift() {
    let mut input = base_input();
    input.eligible_routes = vec![route("route.alpha", WorkOrdinalBandV1::High)];
    input.prior_outcomes = cohort("route.alpha", 8, 1);
    let decision = evaluate(&input);
    let sizing = decision.sizing.clone().expect("support reached the floor");
    let plan = decision.route_plan.clone().expect("plan is present");

    assert!(
        decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::RouteEvidenceStale)
    );
    assert!(
        !sizing.drift_valid,
        "stale cohort evidence must be marked undrifted rather than silently trusted"
    );
    assert_eq!(plan.uncertainty, WorkOrdinalBandV1::Low);
}

#[test]
fn a_route_over_the_remaining_budget_is_excluded_and_unranked() {
    let mut input = base_input();
    let mut expensive = route("route.alpha", WorkOrdinalBandV1::Highest);
    expensive.declared_budget_ceiling = 100;
    input.eligible_routes = vec![expensive, route("route.beta", WorkOrdinalBandV1::Low)];
    input.budget = Some(WorkBudgetEnvelopeV1 {
        ceiling: 120,
        spent: 60,
    });
    let plan = route_plan(&input);

    assert_eq!(plan.exclusions.len(), 1);
    assert_eq!(plan.exclusions[0].route_id, "route.alpha");
    assert_eq!(
        plan.exclusions[0].reason,
        WorkProposalReasonV1::RouteBudgetExceeded
    );
    assert_eq!(ranked_ids(&plan), vec!["route.beta".to_owned()]);
}

#[test]
fn a_route_outside_the_declared_content_locations_is_excluded_and_unranked() {
    let mut input = base_input();
    let mut external = route("route.alpha", WorkOrdinalBandV1::Highest);
    external.content_location = WorkContentLocationClassV1::External;
    input.eligible_routes = vec![external, route("route.beta", WorkOrdinalBandV1::Low)];
    input.content_location = Some(WorkContentLocationLimitV1 {
        allowed: vec![WorkContentLocationClassV1::Local],
    });
    let plan = route_plan(&input);

    assert_eq!(plan.exclusions.len(), 1);
    assert_eq!(plan.exclusions[0].route_id, "route.alpha");
    assert_eq!(
        plan.exclusions[0].reason,
        WorkProposalReasonV1::RouteContentLocationRefused
    );
    assert_eq!(ranked_ids(&plan), vec!["route.beta".to_owned()]);
}

#[test]
fn budget_is_evaluated_before_content_location() {
    let mut input = base_input();
    let mut refused_twice = route("route.alpha", WorkOrdinalBandV1::Highest);
    refused_twice.declared_budget_ceiling = 100;
    refused_twice.content_location = WorkContentLocationClassV1::External;
    input.eligible_routes = vec![refused_twice];
    input.budget = Some(WorkBudgetEnvelopeV1 {
        ceiling: 10,
        spent: 0,
    });
    input.content_location = Some(WorkContentLocationLimitV1 {
        allowed: vec![WorkContentLocationClassV1::Local],
    });
    let plan = route_plan(&input);

    assert_eq!(
        plan.exclusions[0].reason,
        WorkProposalReasonV1::RouteBudgetExceeded
    );
}

#[test]
fn no_eligible_routes_claims_the_widest_uncertainty_and_no_baseline() {
    let decision = evaluate(&base_input());
    let plan = decision.route_plan.clone().expect("plan is always present");

    assert!(plan.ranked.is_empty());
    assert_eq!(plan.deterministic_baseline, None);
    assert_eq!(plan.uncertainty, WorkOrdinalBandV1::Highest);
    assert_eq!(plan.coverage, WorkOrdinalBandV1::Lowest);
    assert!(
        decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::NoEligibleRoutes)
    );
    assert!(decision.sizing.is_none());
    assert!(
        !decision.deterministic_fallback,
        "an empty candidate set names no baseline, so no fallback is claimed"
    );
}

#[test]
fn a_fully_excluded_candidate_set_is_treated_as_no_eligible_routes() {
    let mut input = base_input();
    let mut expensive = route("route.alpha", WorkOrdinalBandV1::Highest);
    expensive.declared_budget_ceiling = 100;
    input.eligible_routes = vec![expensive];
    input.budget = Some(WorkBudgetEnvelopeV1 {
        ceiling: 10,
        spent: 5,
    });
    let decision = evaluate(&input);
    let plan = decision.route_plan.clone().expect("plan is always present");

    assert!(plan.ranked.is_empty());
    assert_eq!(plan.exclusions.len(), 1);
    assert_eq!(plan.deterministic_baseline, None);
    assert_eq!(plan.uncertainty, WorkOrdinalBandV1::Highest);
    assert!(
        decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::NoEligibleRoutes)
    );
}

#[test]
fn ranking_precedence_is_lexicographic_and_never_a_weighted_sum() {
    let mut input = base_input();
    let mut correct_only = route("route.zulu", WorkOrdinalBandV1::Lowest);
    correct_only.correctness = WorkOrdinalBandV1::High;
    let mut strong_elsewhere = route("route.alpha", WorkOrdinalBandV1::Highest);
    strong_elsewhere.correctness = WorkOrdinalBandV1::Moderate;
    input.eligible_routes = vec![strong_elsewhere, correct_only];
    let plan = route_plan(&input);

    assert_eq!(
        ranked_ids(&plan),
        vec!["route.zulu".to_owned(), "route.alpha".to_owned()],
        "correctness outranks every later dimension; a summed score would invert this"
    );
    assert_eq!(plan.ranked[0].rank, 1);
    assert_eq!(plan.ranked[1].rank, 2);
}

#[test]
fn routes_identical_in_every_band_are_ordered_by_route_id_ascending() {
    let mut input = base_input();
    input.eligible_routes = vec![
        route("route.zulu", WorkOrdinalBandV1::Moderate),
        route("route.mike", WorkOrdinalBandV1::Moderate),
        route("route.alpha", WorkOrdinalBandV1::Moderate),
    ];
    let plan = route_plan(&input);

    assert_eq!(
        ranked_ids(&plan),
        vec![
            "route.alpha".to_owned(),
            "route.mike".to_owned(),
            "route.zulu".to_owned()
        ]
    );
}

#[test]
fn a_human_override_forces_an_eligible_route_to_rank_one() {
    let mut input = base_input();
    input.eligible_routes = vec![
        route("route.alpha", WorkOrdinalBandV1::Highest),
        route("route.beta", WorkOrdinalBandV1::Low),
        route("route.gamma", WorkOrdinalBandV1::Moderate),
    ];
    input.human_override = Some(WorkRouteOverrideV1 {
        route_id: "route.beta".to_owned(),
        recorded_at: UtcMicros(50),
    });
    let decision = evaluate(&input);
    let plan = decision.route_plan.clone().expect("plan is present");

    assert!(plan.human_override_applied);
    assert_eq!(
        ranked_ids(&plan),
        vec![
            "route.beta".to_owned(),
            "route.alpha".to_owned(),
            "route.gamma".to_owned()
        ],
        "the override moves one route; the rest keep their relative order"
    );
    assert!(
        decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::HumanOverrideApplied)
    );
}

#[test]
fn an_excluded_route_cannot_be_resurrected_by_a_human_override() {
    let mut input = base_input();
    let mut expensive = route("route.beta", WorkOrdinalBandV1::Highest);
    expensive.declared_budget_ceiling = 100;
    input.eligible_routes = vec![route("route.alpha", WorkOrdinalBandV1::Low), expensive];
    input.budget = Some(WorkBudgetEnvelopeV1 {
        ceiling: 40,
        spent: 10,
    });
    input.human_override = Some(WorkRouteOverrideV1 {
        route_id: "route.beta".to_owned(),
        recorded_at: UtcMicros(50),
    });
    let decision = evaluate(&input);
    let plan = decision.route_plan.clone().expect("plan is present");

    assert!(!plan.human_override_applied);
    assert_eq!(ranked_ids(&plan), vec!["route.alpha".to_owned()]);
    assert!(
        !decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::HumanOverrideApplied)
    );
}

#[test]
fn an_unknown_human_override_leaves_the_ranking_untouched() {
    let mut input = base_input();
    input.eligible_routes = vec![
        route("route.alpha", WorkOrdinalBandV1::Highest),
        route("route.beta", WorkOrdinalBandV1::Low),
    ];
    input.human_override = Some(WorkRouteOverrideV1 {
        route_id: "route.retired".to_owned(),
        recorded_at: UtcMicros(50),
    });
    let plan = route_plan(&input);

    assert!(!plan.human_override_applied);
    assert_eq!(
        ranked_ids(&plan),
        vec!["route.alpha".to_owned(), "route.beta".to_owned()]
    );
}

#[test]
fn the_sizing_cohort_follows_the_overridden_top_route() {
    let mut input = base_input();
    input.eligible_routes = vec![
        route("route.alpha", WorkOrdinalBandV1::Highest),
        route("route.beta", WorkOrdinalBandV1::Low),
    ];
    input.prior_outcomes = cohort("route.beta", 8, 20);
    input.human_override = Some(WorkRouteOverrideV1 {
        route_id: "route.beta".to_owned(),
        recorded_at: UtcMicros(50),
    });
    let sizing = evaluate(&input)
        .sizing
        .expect("the overridden route carries enough support");

    assert_eq!(sizing.cohort, "route.beta");
    assert_eq!(sizing.support, 8);
}

#[test]
fn adverse_cohort_outcomes_raise_the_error_band_and_widen_the_sizing_band() {
    let mut input = base_input();
    input.eligible_routes = vec![route("route.alpha", WorkOrdinalBandV1::High)];
    let mut outcomes = cohort("route.alpha", 8, 20);
    for outcome in outcomes.iter_mut().take(5) {
        outcome.rework = true;
    }
    input.prior_outcomes = outcomes;
    let sizing = evaluate(&input).sizing.expect("support reached the floor");

    assert_eq!(sizing.error, WorkOrdinalBandV1::Highest);
    assert_eq!(
        sizing.band,
        WorkOrdinalBandV1::High,
        "a Standard-effort route widens from Moderate once the cohort error is high"
    );
}

#[test]
fn more_than_one_unresolved_dependency_proposes_one_level_of_subtasks() {
    let mut input = base_input();
    input.dependency_count = 4;
    input.unresolved_dependency_count = 3;
    let decomposition = evaluate(&input)
        .decomposition
        .expect("more than one unresolved dependency proposes a split");

    assert_eq!(decomposition.candidates.len(), 3);
    assert_eq!(
        decomposition
            .candidates
            .iter()
            .map(|sketch| sketch.ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(
        decomposition
            .candidates
            .iter()
            .all(|sketch| sketch.shape == WorkTaskShapeKindV1::Investigation)
    );
    assert_eq!(
        decomposition.rationale,
        vec![WorkProposalReasonV1::DependenciesUnresolved]
    );
}

#[test]
fn a_single_unresolved_dependency_proposes_no_split() {
    let mut input = base_input();
    input.dependency_count = 2;
    input.unresolved_dependency_count = 1;
    assert_eq!(evaluate(&input).decomposition, None);
}

#[test]
fn shape_is_derived_only_from_facts_already_in_the_snapshot() {
    let undistinguished = evaluate(&base_input())
        .shape
        .expect("a valid input always carries a shape");
    assert_eq!(undistinguished.kind, WorkTaskShapeKindV1::Unclassified);
    assert_eq!(undistinguished.band, WorkOrdinalBandV1::Lowest);

    let mut investigation = base_input();
    investigation.dependency_count = 4;
    investigation.unresolved_dependency_count = 2;
    let shape = evaluate(&investigation).shape.expect("shape is present");
    assert_eq!(shape.kind, WorkTaskShapeKindV1::Investigation);
    assert_eq!(shape.band, WorkOrdinalBandV1::Moderate);

    let mut synthesis = base_input();
    synthesis.accepted_proposal_present = true;
    synthesis.execution_admitted = true;
    synthesis.runtime = WorkProposalRuntimeCoverageV1::Complete {
        attempt_count: 8,
        terminal_attempt_count: 1,
    };
    let shape = evaluate(&synthesis).shape.expect("shape is present");
    assert_eq!(shape.kind, WorkTaskShapeKindV1::Synthesis);
    assert_eq!(shape.band, WorkOrdinalBandV1::High);
}

#[test]
fn planner_reasons_are_appended_after_the_gate_reasons() {
    let mut input = base_input();
    input.eligible_routes = vec![route("route.alpha", WorkOrdinalBandV1::High)];
    let decision = evaluate(&input);

    assert_eq!(
        decision.ordered_reason_codes,
        vec![
            WorkProposalReasonV1::FrontierIncomparable,
            WorkProposalReasonV1::Ready,
            WorkProposalReasonV1::RouteEvidenceSparse,
            WorkProposalReasonV1::InsufficientCalibrationSupport,
            WorkProposalReasonV1::DeterministicBaselineSelected,
        ]
    );
}

#[test]
fn the_evaluator_revision_records_the_planner_implementation() {
    assert_eq!(evaluate(&base_input()).evaluator_revision, 3);
}

#[test]
fn the_short_circuit_paths_carry_no_planner_claim() {
    let mut populated = base_input();
    populated.eligible_routes = vec![route("route.alpha", WorkOrdinalBandV1::High)];
    populated.prior_outcomes = cohort("route.alpha", 8, 20);
    populated.dependency_count = 4;
    populated.unresolved_dependency_count = 3;

    let mut invalid = populated.clone();
    invalid.runtime = WorkProposalRuntimeCoverageV1::Complete {
        attempt_count: 1,
        terminal_attempt_count: 3,
    };

    let mut cancelled = populated.clone();
    cancelled.cancellation = WorkProposalCancellationV1::Cancelled {
        requested_at: UtcMicros(50),
    };

    let mut elapsed = populated;
    elapsed.evaluated_at = elapsed.deadline;

    for (label, input) in [
        ("invalid", invalid),
        ("cancelled", cancelled),
        ("deadline", elapsed),
    ] {
        let decision = evaluate(&input);
        assert_eq!(
            decision.disposition,
            WorkProposalDispositionV1::Indeterminate,
            "{label} short-circuit stays indeterminate"
        );
        assert_eq!(decision.shape, None, "{label} carries no shape");
        assert_eq!(decision.sizing, None, "{label} carries no sizing");
        assert_eq!(
            decision.decomposition, None,
            "{label} carries no decomposition"
        );
        assert_eq!(decision.route_plan, None, "{label} carries no route plan");
        assert!(
            !decision.deterministic_fallback,
            "{label} claims no baseline"
        );
    }

    let mut accepted = base_input();
    accepted.task_accepted = true;
    accepted.runtime = WorkProposalRuntimeCoverageV1::Unavailable;
    let decision = evaluate(&accepted);
    assert_eq!(decision.disposition, WorkProposalDispositionV1::Deny);
    assert_eq!(decision.shape, None);
    assert_eq!(decision.sizing, None);
    assert_eq!(decision.decomposition, None);
    assert_eq!(decision.route_plan, None);
    assert!(!decision.deterministic_fallback);
}

#[test]
fn a_duplicate_route_identity_is_an_invalid_request() {
    let mut input = base_input();
    input.eligible_routes = vec![
        route("route.alpha", WorkOrdinalBandV1::High),
        route("route.alpha", WorkOrdinalBandV1::Low),
    ];
    let decision = evaluate(&input);

    assert_eq!(
        decision.ordered_reason_codes,
        vec![WorkProposalReasonV1::InvalidRequest]
    );
    assert_eq!(decision.route_plan, None);
}

#[test]
fn budget_spent_beyond_the_ceiling_is_an_invalid_request() {
    let mut input = base_input();
    input.budget = Some(WorkBudgetEnvelopeV1 {
        ceiling: 10,
        spent: 11,
    });
    let decision = evaluate(&input);

    assert_eq!(
        decision.ordered_reason_codes,
        vec![WorkProposalReasonV1::InvalidRequest]
    );
    assert_eq!(decision.route_plan, None);
}

#[test]
fn prior_outcomes_may_name_a_retired_route_without_invalidating_the_request() {
    let mut input = base_input();
    input.eligible_routes = vec![route("route.alpha", WorkOrdinalBandV1::High)];
    input.prior_outcomes = cohort("route.retired", 8, 20);
    let decision = evaluate(&input);

    assert_eq!(decision.disposition, WorkProposalDispositionV1::Allow);
    assert_eq!(
        decision.sizing, None,
        "an out-of-cohort outcome contributes no support"
    );
    assert!(
        decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::RouteEvidenceSparse)
    );
}
