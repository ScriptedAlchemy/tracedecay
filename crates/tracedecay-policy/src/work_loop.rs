//! Pure work-loop proposal evaluation over an immutable Work snapshot.
//!
//! The evaluator explains whether a Work proposal fits the supplied evidence
//! and which explicit command is the legal next step. It never mutates the
//! graph, admits execution, accepts a task, or advances either evidence
//! frontier; accepting, rejecting, superseding, replanning, and admission
//! remain separate version-checked application commands.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{ManifestDigest, TaskId, UtcMicros};

use crate::authorization::{PolicyIdentifierV1, policy_digest};

/// Explicit cancellation fact supplied by the caller. Policy never observes a
/// live token.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkProposalCancellationV1 {
    Active,
    Cancelled { requested_at: UtcMicros },
}

/// One immutable evidence frontier. Local code/session evidence and live Git
/// evidence each carry their own frontier; the evaluator never merges,
/// substitutes, or advances one from the other.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvidenceFrontierV1 {
    pub watermark: UtcMicros,
    pub digest: ManifestDigest,
}

impl WorkEvidenceFrontierV1 {
    fn is_valid(&self) -> bool {
        self.digest.validate().is_ok()
    }
}

/// Recorded relation between the two supplied frontiers. `Incomparable` means
/// at least one side was absent; it is not collapsed into agreement.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkFrontierComparisonV1 {
    Agree,
    Disagree,
    Incomparable,
}

/// Ordinal band. Never a probability, never a scalar score.
///
/// Bands are ordered `Lowest` .. `Highest`. Comparison is the only operation
/// the evaluator performs over them, so no weighted sum can be reconstructed
/// from a recorded decision.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkOrdinalBandV1 {
    Lowest,
    Low,
    Moderate,
    High,
    Highest,
}

impl WorkOrdinalBandV1 {
    /// Widen one band toward `Highest`, saturating. Widening is the only
    /// permitted response to sparse, stale, or incomparable evidence; the
    /// evaluator never narrows a band the evidence did not earn.
    const fn widened(self) -> Self {
        match self {
            Self::Lowest => Self::Low,
            Self::Low => Self::Moderate,
            Self::Moderate => Self::High,
            Self::High | Self::Highest => Self::Highest,
        }
    }

    /// Mirror a coverage band onto the uncertainty scale. Full coverage is the
    /// lowest uncertainty the evaluator will claim before any widening.
    const fn inverted(self) -> Self {
        match self {
            Self::Lowest => Self::Highest,
            Self::Low => Self::High,
            Self::Moderate => Self::Moderate,
            Self::High => Self::Low,
            Self::Highest => Self::Lowest,
        }
    }
}

/// Where a route places task content. Declared by the authorized snapshot; the
/// evaluator classifies nothing itself and never widens the declared class.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkContentLocationClassV1 {
    Local,
    Tenant,
    External,
}

/// Declared effort class of a route. Ordered so the sizing band can take the
/// stronger of the declared effort and the derived task shape.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkEffortClassV1 {
    Minimal,
    Standard,
    Extended,
}

/// One eligible route, supplied BY THE APPLICATION from the authorized snapshot.
///
/// Policy never discovers a provider: a route absent from this list cannot be
/// ranked, recommended, or named as the deterministic baseline.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRouteCandidateV1 {
    /// Stable identity of the route. Unique within `eligible_routes`; also the
    /// total-order tiebreak that keeps ranking deterministic.
    pub route_id: String,
    /// The authorized provider capability this route exercises. Recorded, never
    /// resolved or contacted by policy.
    pub provider_capability_id: String,
    /// The model the route names. Recorded verbatim; policy never substitutes.
    pub model_id: String,
    /// Effort the route declares for this task. Contributes the floor of the
    /// calibrated sizing band.
    pub effort: WorkEffortClassV1,
    /// Budget the route would consume. Compared against the remaining envelope
    /// to decide `RouteBudgetExceeded`.
    pub declared_budget_ceiling: u64,
    /// Where the route would place content. Compared against the declared limit
    /// to decide `RouteContentLocationRefused`.
    pub content_location: WorkContentLocationClassV1,
    /// Correctness fitness band. Highest precedence ranking dimension.
    pub correctness: WorkOrdinalBandV1,
    /// Fitness for sensitive data. Second ranking dimension; never merged into
    /// correctness.
    pub sensitive_data_fitness: WorkOrdinalBandV1,
    /// Latency fitness, supplied ALREADY ORIENTED as fitness: `Highest` means
    /// best-fitting latency, not the slowest route.
    pub latency: WorkOrdinalBandV1,
    /// Cost fitness, supplied ALREADY ORIENTED as fitness: `Highest` means
    /// best-fitting cost, not the most expensive route.
    pub cost: WorkOrdinalBandV1,
    /// How much unattended progress the route is trusted to make.
    pub autonomy: WorkOrdinalBandV1,
    /// Quality of the evidence backing the other bands. Ranked separately so a
    /// well-evidenced weak route is never confused with an unevidenced strong one.
    pub evidence_quality: WorkOrdinalBandV1,
}

/// Remaining budget for this task, supplied by the application authority.
/// Policy reads no meter and estimates no spend.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkBudgetEnvelopeV1 {
    /// Total authorized budget for the task.
    pub ceiling: u64,
    /// Budget already consumed. Never exceeds `ceiling` on a valid input.
    pub spent: u64,
}

/// The content locations this task is permitted to reach. An empty `allowed`
/// list refuses every route rather than defaulting to permissive.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkContentLocationLimitV1 {
    /// Exactly the classes a route may place content in.
    pub allowed: Vec<WorkContentLocationClassV1>,
}

/// How a prior attempt on a route ended. Recorded, never inferred.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkPriorTerminalV1 {
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
}

/// Cohort denominator. APPLICATION-supplied, never worker-supplied.
///
/// A worker cannot widen its own calibration cohort, so a route cannot earn
/// calibrated sizing by reporting its own successes.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkPriorOutcomeV1 {
    /// The route this outcome belongs to. May name a retired route; only
    /// outcomes matching the top-ranked route form the sizing cohort.
    pub route_id: String,
    /// Whether the produced work was accepted.
    pub accepted: bool,
    /// Whether the accepted work required rework.
    pub rework: bool,
    /// Whether a defect escaped review.
    pub escaped_defect: bool,
    /// How the attempt terminated.
    pub terminal: WorkPriorTerminalV1,
    /// When the outcome was observed. Outcomes later than `evaluated_at` are
    /// INCOMPARABLE and are excluded from support rather than trusted.
    pub observed_at: UtcMicros,
}

/// A human's explicit route selection, recorded by the application.
///
/// An override reorders ranking; it never resurrects a route that exclusion
/// already refused, so budget and content-location limits stay authoritative.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRouteOverrideV1 {
    /// The route the human named. Applied only when it survived exclusion.
    pub route_id: String,
    /// When the override was recorded. Carried for replay; never compared
    /// against a clock by policy.
    pub recorded_at: UtcMicros,
}

/// Immutable Work snapshot facts assembled by the application authority.
///
/// Every count and frontier is an explicit input; the evaluator performs no
/// storage read, clock lookup, or readiness derivation of its own.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProposalPolicyInputV1 {
    pub task_id: TaskId,
    pub based_on_version: u64,
    pub dependency_count: u32,
    pub unresolved_dependency_count: u32,
    pub accepted_proposal_present: bool,
    pub execution_admitted: bool,
    pub task_accepted: bool,
    pub runtime_evidence_count: u32,
    pub terminal_runtime_evidence_count: u32,
    pub local_evidence: Option<WorkEvidenceFrontierV1>,
    pub live_git_evidence: Option<WorkEvidenceFrontierV1>,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub deadline: UtcMicros,
    pub cancellation: WorkProposalCancellationV1,
    pub evaluated_at: UtcMicros,
    /// Every route the authorized snapshot permits for this task. Policy never
    /// discovers a provider; an empty list means there is nothing to rank.
    #[serde(default)]
    pub eligible_routes: Vec<WorkRouteCandidateV1>,
    /// The remaining budget envelope. Absent means the application declared no
    /// budget limit, not that the limit is unlimited by policy default.
    #[serde(default)]
    pub budget: Option<WorkBudgetEnvelopeV1>,
    /// The permitted content locations. Absent means the application declared
    /// no location limit; present with an empty list refuses every route.
    #[serde(default)]
    pub content_location: Option<WorkContentLocationLimitV1>,
    /// Prior outcomes forming the calibration cohort. APPLICATION-supplied.
    #[serde(default)]
    pub prior_outcomes: Vec<WorkPriorOutcomeV1>,
    /// An explicit human route selection, when one was recorded.
    #[serde(default)]
    pub human_override: Option<WorkRouteOverrideV1>,
}

impl WorkProposalPolicyInputV1 {
    fn is_valid(&self) -> bool {
        self.based_on_version > 0
            && self.policy_revision > 0
            && self.policy_digest.validate().is_ok()
            && self.configuration_digest.validate().is_ok()
            && self.unresolved_dependency_count <= self.dependency_count
            && self.terminal_runtime_evidence_count <= self.runtime_evidence_count
            && self
                .local_evidence
                .as_ref()
                .is_none_or(WorkEvidenceFrontierV1::is_valid)
            && self
                .live_git_evidence
                .as_ref()
                .is_none_or(WorkEvidenceFrontierV1::is_valid)
            && self
                .budget
                .is_none_or(|budget| budget.spent <= budget.ceiling)
            && routes_are_uniquely_identified(&self.eligible_routes)
    }
}

/// Reject an eligible-route list that names the same route twice. Ranking,
/// exclusion, and the sizing cohort are all keyed by `route_id`, so a duplicate
/// would make the plan ambiguous rather than merely redundant.
fn routes_are_uniquely_identified(routes: &[WorkRouteCandidateV1]) -> bool {
    let mut identifiers: Vec<&str> = routes.iter().map(|route| route.route_id.as_str()).collect();
    identifiers.sort_unstable();
    identifiers.windows(2).all(|pair| pair[0] != pair[1])
}

/// Exactly one disposition per decision.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProposalDispositionV1 {
    Allow,
    Deny,
    Abstain,
    Indeterminate,
}

/// The explicit command the decision recommends next. A recommendation never
/// executes; each action names a separate version-checked application command.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProposalActionV1 {
    ProceedToAcceptance,
    HoldForDependencies,
    AdmitExecution,
    Replan,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkProposalReasonV1 {
    InvalidRequest,
    RequestCancelled,
    DeadlineExceeded,
    FrontierAgreement,
    FrontierDisagreement,
    FrontierIncomparable,
    TaskAccepted,
    TerminalEvidenceObserved,
    ExecutionInFlight,
    ProposalAccepted,
    DependenciesUnresolved,
    Ready,
    InsufficientCalibrationSupport,
    RouteBudgetExceeded,
    RouteContentLocationRefused,
    RouteEvidenceSparse,
    RouteEvidenceStale,
    HumanOverrideApplied,
    NoEligibleRoutes,
    DeterministicBaselineSelected,
}

/// Kind of work the snapshot facts describe. Derived only from facts already in
/// the input; `Unclassified` when those facts do not distinguish a kind.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkTaskShapeKindV1 {
    Investigation,
    Change,
    Verification,
    Synthesis,
    Unclassified,
}

/// The derived shape of the task: what kind of work it is and how large the
/// declared dependency and evidence counts make it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskShapeV1 {
    /// Kind derived from the gate booleans and counts. No new discovery.
    pub kind: WorkTaskShapeKindV1,
    /// Magnitude band derived from the declared dependency and evidence counts.
    pub band: WorkOrdinalBandV1,
}

/// Calibrated sizing. Emitted ONLY when support >= floor. Every field named separately
/// per Plan 06 :84-85; `support_floor` carries the governing floor into the record.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkCalibratedSizingV1 {
    /// Identity of the cohort the sizing was calibrated over: the top-ranked
    /// route id. Sizing from one cohort is never reused for another.
    pub cohort: String,
    /// Latest comparable observation in the cohort. Nothing after
    /// `evaluated_at` contributes, so the horizon never runs ahead of the input.
    pub horizon: UtcMicros,
    /// Count of comparable in-cohort outcomes backing this sizing.
    pub support: u32,
    /// The floor that governed this emission, carried so replay shows which
    /// floor applied rather than assuming the current constant.
    pub support_floor: u32,
    /// Observed adverse-outcome band over the cohort. An ordinal band, never a
    /// rate and never a probability.
    pub error: WorkOrdinalBandV1,
    /// False when the cohort contains incomparable or stale observations, so a
    /// consumer can refuse a sizing that drifted rather than silently trust it.
    pub drift_valid: bool,
    /// The sizing band itself: the stronger of the route's declared effort and
    /// the derived shape magnitude, widened when the cohort error is high.
    pub band: WorkOrdinalBandV1,
}

/// One level only (Q3). Read-only sketch; accepting it stays a separate version-checked command.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkSubtaskSketchV1 {
    /// Position in the proposal, ascending from 0. Stable across replays of the
    /// same input.
    pub ordinal: u32,
    /// What the sketch covers, phrased from the declared counts alone.
    pub summary: String,
    /// Kind the sketch would carry if it were accepted as its own task.
    pub shape: WorkTaskShapeKindV1,
}

/// A one-level decomposition proposal. Never recursive: a deeper split is a
/// separate sequenced capability, not something this evaluator may invent.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkDecompositionProposalV1 {
    /// The proposed sketches, ordinal ascending from 0.
    pub candidates: Vec<WorkSubtaskSketchV1>,
    /// Why the proposal was emitted, in the same ordered reason vocabulary the
    /// decision uses.
    pub rationale: Vec<WorkProposalReasonV1>,
}

/// Ranked route. Dimensions stay SEPARATE — no scalar score field is permitted here.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRankedRouteV1 {
    /// Position in the ranking, ascending from 1.
    pub rank: u32,
    /// The ranked route's identity.
    pub route_id: String,
    /// Correctness fitness, echoed so the ranking can be audited without the input.
    pub correctness: WorkOrdinalBandV1,
    /// Sensitive-data fitness, echoed unmerged.
    pub sensitive_data_fitness: WorkOrdinalBandV1,
    /// Latency fitness, oriented so `Highest` is best-fitting.
    pub latency: WorkOrdinalBandV1,
    /// Cost fitness, oriented so `Highest` is best-fitting.
    pub cost: WorkOrdinalBandV1,
    /// Autonomy fitness, echoed unmerged.
    pub autonomy: WorkOrdinalBandV1,
    /// Evidence-quality fitness, echoed unmerged.
    pub evidence_quality: WorkOrdinalBandV1,
}

/// One refused route and the single reason that refused it. Exclusion is
/// recorded rather than silent so a missing route is always explained.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRouteExclusionV1 {
    /// The refused route's identity.
    pub route_id: String,
    /// The first limit the route failed, in the declared exclusion order.
    pub reason: WorkProposalReasonV1,
}

/// The explained route plan for one decision. Recommends; never dispatches.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkRoutePlanV1 {
    /// Surviving routes in total, deterministic order, rank ascending from 1.
    pub ranked: Vec<WorkRankedRouteV1>,
    /// Every refused route with its reason, in supplied order.
    pub exclusions: Vec<WorkRouteExclusionV1>,
    /// The declared deterministic baseline, selected when evidence cannot support a
    /// stronger claim. Always names a route that survived exclusion, or None when none did.
    pub deterministic_baseline: Option<String>,
    /// Fraction-free ordinal coverage of the cohort evidence over the ranked set.
    pub coverage: WorkOrdinalBandV1,
    /// Widens on sparse / stale / incomparable evidence.
    pub uncertainty: WorkOrdinalBandV1,
    /// True only when a recorded human override named a route that survived
    /// exclusion. An excluded or unknown override leaves ranking untouched.
    pub human_override_applied: bool,
}

/// Minimum in-cohort prior outcomes before calibrated sizing may be emitted.
/// Carried in every sizing payload as `support_floor` so replay shows the governing
/// floor. Changing this value is an EVALUATOR_REVISION bump, not a silent retune.
pub const WORK_CALIBRATION_SUPPORT_FLOOR: u32 = 8;

/// Upper bound on emitted subtask sketches.
///
/// A decomposition proposal is a read-only sketch, so truncating it changes no
/// gate outcome; the bound exists so a hostile `unresolved_dependency_count`
/// cannot make a pure evaluator allocate without limit.
const DECOMPOSITION_SKETCH_LIMIT: u32 = 64;

/// One explained, replayable work-loop decision.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProposalDecisionV1 {
    pub evaluator_id: PolicyIdentifierV1,
    pub evaluator_revision: u64,
    pub input_digest: ManifestDigest,
    pub task_id: TaskId,
    pub based_on_version: u64,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub disposition: WorkProposalDispositionV1,
    pub recommended_action: Option<WorkProposalActionV1>,
    /// True when the recommendation is the declared deterministic baseline
    /// selected because the evidence cannot support a stronger claim.
    pub deterministic_fallback: bool,
    pub ordered_reason_codes: Vec<WorkProposalReasonV1>,
    /// The local code/session frontier, returned exactly as supplied.
    pub local_evidence: Option<WorkEvidenceFrontierV1>,
    /// The live Git frontier, returned exactly as supplied.
    pub live_git_evidence: Option<WorkEvidenceFrontierV1>,
    pub frontier_comparison: WorkFrontierComparisonV1,
    /// Derived task shape. Absent on the invalid, cancelled, and deadline
    /// short-circuits, where no planner claim is licensed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<WorkTaskShapeV1>,
    /// Calibrated sizing, present only when in-cohort support reached the
    /// governing floor. Absence is a recorded fact, never a fabricated estimate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sizing: Option<WorkCalibratedSizingV1>,
    /// One-level decomposition sketch, present only when more than one
    /// dependency is unresolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decomposition: Option<WorkDecompositionProposalV1>,
    /// The explained route plan. Present on every evaluation that reached the
    /// gates, including the one that found no eligible route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_plan: Option<WorkRoutePlanV1>,
}

pub trait WorkProposalEvaluator {
    fn evaluate(&self, input: &WorkProposalPolicyInputV1) -> WorkProposalDecisionV1;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkProposalEvaluatorV1 {
    evaluator_id: PolicyIdentifierV1,
}

impl Default for WorkProposalEvaluatorV1 {
    fn default() -> Self {
        Self {
            evaluator_id: PolicyIdentifierV1::new("work_proposal.v1")
                .expect("static evaluator identifier is valid"),
        }
    }
}

impl WorkProposalEvaluatorV1 {
    /// Revision of this reviewed implementation, recorded with every decision
    /// so replay can refuse a substituted evaluator. It is a property of the
    /// code, not of an instance.
    const EVALUATOR_REVISION: u64 = 2;

    /// Assemble a decision that carries no planner claim. Reserved for the
    /// invalid, cancelled, and deadline short-circuits.
    fn decision(
        &self,
        input: &WorkProposalPolicyInputV1,
        disposition: WorkProposalDispositionV1,
        recommended_action: Option<WorkProposalActionV1>,
        deterministic_fallback: bool,
        ordered_reason_codes: Vec<WorkProposalReasonV1>,
        frontier_comparison: WorkFrontierComparisonV1,
    ) -> WorkProposalDecisionV1 {
        WorkProposalDecisionV1 {
            evaluator_id: self.evaluator_id.clone(),
            evaluator_revision: Self::EVALUATOR_REVISION,
            input_digest: policy_digest("tracedecay.policy.work-proposal-input.v1", input),
            task_id: input.task_id.clone(),
            based_on_version: input.based_on_version,
            policy_revision: input.policy_revision,
            policy_digest: input.policy_digest.clone(),
            configuration_digest: input.configuration_digest.clone(),
            disposition,
            recommended_action,
            deterministic_fallback,
            ordered_reason_codes,
            local_evidence: input.local_evidence.clone(),
            live_git_evidence: input.live_git_evidence.clone(),
            frontier_comparison,
            shape: None,
            sizing: None,
            decomposition: None,
            route_plan: None,
        }
    }

    /// Assemble a decision for a gate that ran on a valid, live, in-deadline
    /// input, merging the planner claim computed once for that evaluation.
    ///
    /// Planner reasons are appended after the gate reasons so the ordered
    /// vocabulary still reads gate-first, and the planner may only turn the
    /// deterministic-fallback flag on, never off.
    fn planned_decision(
        &self,
        input: &WorkProposalPolicyInputV1,
        disposition: WorkProposalDispositionV1,
        recommended_action: Option<WorkProposalActionV1>,
        deterministic_fallback: bool,
        mut ordered_reason_codes: Vec<WorkProposalReasonV1>,
        frontier_comparison: WorkFrontierComparisonV1,
        plan: WorkPlannerOutcome,
    ) -> WorkProposalDecisionV1 {
        ordered_reason_codes.extend(plan.reasons);
        let mut decision = self.decision(
            input,
            disposition,
            recommended_action,
            deterministic_fallback || plan.deterministic_fallback,
            ordered_reason_codes,
            frontier_comparison,
        );
        decision.shape = Some(plan.shape);
        decision.sizing = plan.sizing;
        decision.decomposition = plan.decomposition;
        decision.route_plan = Some(plan.route_plan);
        decision
    }
}

/// Planner claim assembled once per surviving evaluation and merged into
/// whichever gate branch terminates it.
///
/// Never serialized: the decision carries each part as its own declared field,
/// so no planner-shaped envelope leaks into the wire contract.
struct WorkPlannerOutcome {
    shape: WorkTaskShapeV1,
    sizing: Option<WorkCalibratedSizingV1>,
    decomposition: Option<WorkDecompositionProposalV1>,
    route_plan: WorkRoutePlanV1,
    reasons: Vec<WorkProposalReasonV1>,
    deterministic_fallback: bool,
}

/// Saturating count conversion. A count that cannot be represented is clamped
/// rather than panicking, because policy must stay total over any input.
fn count_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Derive the task shape from facts already present in the snapshot.
///
/// The ladder reads the gate booleans in the same precedence the gates use, so
/// the shape can never contradict the disposition that accompanies it.
fn derive_shape(input: &WorkProposalPolicyInputV1) -> WorkTaskShapeV1 {
    let kind = if input.task_accepted {
        WorkTaskShapeKindV1::Verification
    } else if input.terminal_runtime_evidence_count > 0 {
        WorkTaskShapeKindV1::Synthesis
    } else if input.execution_admitted || input.accepted_proposal_present {
        WorkTaskShapeKindV1::Change
    } else if input.unresolved_dependency_count > 0 {
        WorkTaskShapeKindV1::Investigation
    } else if input.dependency_count > 0 || input.runtime_evidence_count > 0 {
        WorkTaskShapeKindV1::Change
    } else {
        WorkTaskShapeKindV1::Unclassified
    };
    let scale = input
        .dependency_count
        .saturating_add(input.runtime_evidence_count);
    let band = match scale {
        0 => WorkOrdinalBandV1::Lowest,
        1..=2 => WorkOrdinalBandV1::Low,
        3..=5 => WorkOrdinalBandV1::Moderate,
        6..=9 => WorkOrdinalBandV1::High,
        _ => WorkOrdinalBandV1::Highest,
    };
    WorkTaskShapeV1 { kind, band }
}

/// Propose one level of subtasks, one per unresolved dependency.
///
/// Emitted only when more than one dependency is unresolved: a single
/// unresolved dependency is already the whole task and splitting it would
/// manufacture structure the snapshot does not contain.
fn derive_decomposition(input: &WorkProposalPolicyInputV1) -> Option<WorkDecompositionProposalV1> {
    if input.unresolved_dependency_count <= 1 {
        return None;
    }
    let declared = input.unresolved_dependency_count;
    let emitted = declared.min(DECOMPOSITION_SKETCH_LIMIT);
    let candidates = (0..emitted)
        .map(|ordinal| WorkSubtaskSketchV1 {
            ordinal,
            summary: format!(
                "resolve unresolved dependency {} of {declared}",
                ordinal.saturating_add(1)
            ),
            shape: WorkTaskShapeKindV1::Investigation,
        })
        .collect();
    Some(WorkDecompositionProposalV1 {
        candidates,
        rationale: vec![WorkProposalReasonV1::DependenciesUnresolved],
    })
}

/// Split the eligible routes into survivors and recorded exclusions.
///
/// Budget is evaluated before content location, and a route records exactly the
/// first limit it failed, so an exclusion reason is never ambiguous.
fn partition_routes(
    input: &WorkProposalPolicyInputV1,
) -> (Vec<&WorkRouteCandidateV1>, Vec<WorkRouteExclusionV1>) {
    let remaining_budget = input
        .budget
        .map(|budget| budget.ceiling.saturating_sub(budget.spent));
    let mut survivors = Vec::new();
    let mut exclusions = Vec::new();
    for route in &input.eligible_routes {
        if remaining_budget.is_some_and(|remaining| route.declared_budget_ceiling > remaining) {
            exclusions.push(WorkRouteExclusionV1 {
                route_id: route.route_id.clone(),
                reason: WorkProposalReasonV1::RouteBudgetExceeded,
            });
            continue;
        }
        if input
            .content_location
            .as_ref()
            .is_some_and(|limit| !limit.allowed.contains(&route.content_location))
        {
            exclusions.push(WorkRouteExclusionV1 {
                route_id: route.route_id.clone(),
                reason: WorkProposalReasonV1::RouteContentLocationRefused,
            });
            continue;
        }
        survivors.push(route);
    }
    (survivors, exclusions)
}

/// Order the survivors by the separate ordinal dimensions.
///
/// The precedence is fixed and lexicographic — correctness, sensitive-data
/// fitness, evidence quality, autonomy, latency, cost — with `route_id`
/// ascending as the final tiebreak, so the order is total and no scalar score
/// is ever formed.
fn rank_survivors(survivors: &mut [&WorkRouteCandidateV1]) {
    survivors.sort_by(|left, right| {
        right
            .correctness
            .cmp(&left.correctness)
            .then_with(|| {
                right
                    .sensitive_data_fitness
                    .cmp(&left.sensitive_data_fitness)
            })
            .then_with(|| right.evidence_quality.cmp(&left.evidence_quality))
            .then_with(|| right.autonomy.cmp(&left.autonomy))
            .then_with(|| right.latency.cmp(&left.latency))
            .then_with(|| right.cost.cmp(&left.cost))
            .then_with(|| left.route_id.cmp(&right.route_id))
    });
}

/// Fraction-free ordinal coverage of the cohort evidence over the ranked set.
///
/// Comparison is by integer cross-multiplication so no ratio, percentage, or
/// floating-point value is ever formed.
fn coverage_band(covered: usize, total: usize) -> WorkOrdinalBandV1 {
    if total == 0 || covered == 0 {
        return WorkOrdinalBandV1::Lowest;
    }
    let scaled = covered.saturating_mul(4);
    if scaled >= total.saturating_mul(4) {
        WorkOrdinalBandV1::Highest
    } else if scaled >= total.saturating_mul(3) {
        WorkOrdinalBandV1::High
    } else if scaled >= total.saturating_mul(2) {
        WorkOrdinalBandV1::Moderate
    } else if scaled >= total {
        WorkOrdinalBandV1::Low
    } else {
        WorkOrdinalBandV1::Lowest
    }
}

/// Band the observed adverse-outcome share of a cohort.
///
/// Fraction-free like [`coverage_band`]: an adverse count is compared against
/// integer multiples of the support, never divided into a rate.
fn error_band(adverse: usize, support: usize) -> WorkOrdinalBandV1 {
    if support == 0 {
        return WorkOrdinalBandV1::Highest;
    }
    if adverse == 0 {
        return WorkOrdinalBandV1::Lowest;
    }
    if adverse.saturating_mul(8) <= support {
        WorkOrdinalBandV1::Low
    } else if adverse.saturating_mul(4) <= support {
        WorkOrdinalBandV1::Moderate
    } else if adverse.saturating_mul(2) <= support {
        WorkOrdinalBandV1::High
    } else {
        WorkOrdinalBandV1::Highest
    }
}

/// True when a prior outcome counts against the route.
///
/// Anything short of an accepted, rework-free, defect-free success is adverse,
/// so a cohort cannot look clean by reporting a non-terminal ending.
fn is_adverse(outcome: &WorkPriorOutcomeV1) -> bool {
    !outcome.accepted
        || outcome.rework
        || outcome.escaped_defect
        || outcome.terminal != WorkPriorTerminalV1::Succeeded
}

/// Floor of the sizing band contributed by the route's declared effort.
const fn effort_band(effort: WorkEffortClassV1) -> WorkOrdinalBandV1 {
    match effort {
        WorkEffortClassV1::Minimal => WorkOrdinalBandV1::Low,
        WorkEffortClassV1::Standard => WorkOrdinalBandV1::Moderate,
        WorkEffortClassV1::Extended => WorkOrdinalBandV1::High,
    }
}

/// Build the whole planner claim for one valid, live, in-deadline input.
///
/// Pure: it reads no clock, opens no store, and discovers no provider. Every
/// route it can name arrived in `eligible_routes`, and every outcome it counts
/// arrived in `prior_outcomes`.
fn plan_work(input: &WorkProposalPolicyInputV1) -> WorkPlannerOutcome {
    let shape = derive_shape(input);
    let decomposition = derive_decomposition(input);
    let (mut survivors, exclusions) = partition_routes(input);
    rank_survivors(&mut survivors);

    let mut human_override_applied = false;
    if let Some(override_request) = input.human_override.as_ref()
        && let Some(position) = survivors
            .iter()
            .position(|route| route.route_id == override_request.route_id)
    {
        let promoted = survivors.remove(position);
        survivors.insert(0, promoted);
        human_override_applied = true;
    }

    let ranked: Vec<WorkRankedRouteV1> = survivors
        .iter()
        .enumerate()
        .map(|(index, route)| WorkRankedRouteV1 {
            rank: count_u32(index.saturating_add(1)),
            route_id: route.route_id.clone(),
            correctness: route.correctness,
            sensitive_data_fitness: route.sensitive_data_fitness,
            latency: route.latency,
            cost: route.cost,
            autonomy: route.autonomy,
            evidence_quality: route.evidence_quality,
        })
        .collect();

    let mut reasons = Vec::new();
    if human_override_applied {
        reasons.push(WorkProposalReasonV1::HumanOverrideApplied);
    }

    let Some(top) = survivors.first().copied() else {
        // Nothing survived. The plan records the refusals verbatim and claims
        // the widest uncertainty rather than inventing a route to fall back on.
        reasons.push(WorkProposalReasonV1::NoEligibleRoutes);
        return WorkPlannerOutcome {
            shape,
            sizing: None,
            decomposition,
            route_plan: WorkRoutePlanV1 {
                ranked,
                exclusions,
                deterministic_baseline: None,
                coverage: WorkOrdinalBandV1::Lowest,
                uncertainty: WorkOrdinalBandV1::Highest,
                human_override_applied,
            },
            reasons,
            deterministic_fallback: false,
        };
    };

    let in_cohort_count = input
        .prior_outcomes
        .iter()
        .filter(|outcome| outcome.route_id == top.route_id)
        .count();
    // An observation later than the evaluation instant is INCOMPARABLE: it
    // cannot be reconciled against this snapshot, so it never counts as support.
    let comparable: Vec<&WorkPriorOutcomeV1> = input
        .prior_outcomes
        .iter()
        .filter(|outcome| {
            outcome.route_id == top.route_id && outcome.observed_at <= input.evaluated_at
        })
        .collect();
    let incomparable_count = in_cohort_count.saturating_sub(comparable.len());
    let support = count_u32(comparable.len());
    let horizon = comparable
        .iter()
        .map(|outcome| outcome.observed_at)
        .max()
        .unwrap_or(UtcMicros(0));

    let sparse = comparable.is_empty();
    let stale = !sparse
        && input.local_evidence.as_ref().is_some_and(|frontier| {
            comparable
                .iter()
                .all(|outcome| outcome.observed_at < frontier.watermark)
        });

    let covered = ranked
        .iter()
        .filter(|candidate| {
            input.prior_outcomes.iter().any(|outcome| {
                outcome.route_id == candidate.route_id && outcome.observed_at <= input.evaluated_at
            })
        })
        .count();
    let coverage = coverage_band(covered, ranked.len());

    let sizing = if support >= WORK_CALIBRATION_SUPPORT_FLOOR {
        let adverse = comparable
            .iter()
            .copied()
            .filter(|outcome| is_adverse(outcome))
            .count();
        let error = error_band(adverse, comparable.len());
        let mut band = effort_band(top.effort).max(shape.band);
        if matches!(error, WorkOrdinalBandV1::High | WorkOrdinalBandV1::Highest) {
            band = band.widened();
        }
        Some(WorkCalibratedSizingV1 {
            cohort: top.route_id.clone(),
            horizon,
            support,
            support_floor: WORK_CALIBRATION_SUPPORT_FLOOR,
            error,
            drift_valid: incomparable_count == 0 && !stale,
            band,
        })
    } else {
        None
    };

    let mut uncertainty = coverage.inverted();
    if sparse {
        reasons.push(WorkProposalReasonV1::RouteEvidenceSparse);
        uncertainty = uncertainty.widened();
    }
    if stale {
        reasons.push(WorkProposalReasonV1::RouteEvidenceStale);
        uncertainty = uncertainty.widened();
    }
    if sizing.is_none() {
        reasons.push(WorkProposalReasonV1::InsufficientCalibrationSupport);
        uncertainty = uncertainty.widened();
    }

    // No calibrated sizing, or sizing the evidence only weakly supports, means
    // the declared baseline governs instead of a stronger claim.
    let baseline_selected = sizing.is_none()
        || matches!(
            uncertainty,
            WorkOrdinalBandV1::High | WorkOrdinalBandV1::Highest
        );
    let deterministic_baseline = if baseline_selected {
        reasons.push(WorkProposalReasonV1::DeterministicBaselineSelected);
        Some(top.route_id.clone())
    } else {
        None
    };

    WorkPlannerOutcome {
        shape,
        sizing,
        decomposition,
        route_plan: WorkRoutePlanV1 {
            ranked,
            exclusions,
            deterministic_baseline,
            coverage,
            uncertainty,
            human_override_applied,
        },
        reasons,
        deterministic_fallback: baseline_selected,
    }
}

fn compare_frontiers(input: &WorkProposalPolicyInputV1) -> WorkFrontierComparisonV1 {
    match (&input.local_evidence, &input.live_git_evidence) {
        (Some(local), Some(live)) => {
            if local.digest == live.digest {
                WorkFrontierComparisonV1::Agree
            } else {
                WorkFrontierComparisonV1::Disagree
            }
        }
        _ => WorkFrontierComparisonV1::Incomparable,
    }
}

const fn comparison_reason(comparison: WorkFrontierComparisonV1) -> WorkProposalReasonV1 {
    match comparison {
        WorkFrontierComparisonV1::Agree => WorkProposalReasonV1::FrontierAgreement,
        WorkFrontierComparisonV1::Disagree => WorkProposalReasonV1::FrontierDisagreement,
        WorkFrontierComparisonV1::Incomparable => WorkProposalReasonV1::FrontierIncomparable,
    }
}

impl WorkProposalEvaluator for WorkProposalEvaluatorV1 {
    fn evaluate(&self, input: &WorkProposalPolicyInputV1) -> WorkProposalDecisionV1 {
        if !input.is_valid() {
            return self.decision(
                input,
                WorkProposalDispositionV1::Indeterminate,
                None,
                false,
                vec![WorkProposalReasonV1::InvalidRequest],
                WorkFrontierComparisonV1::Incomparable,
            );
        }
        let comparison = compare_frontiers(input);
        if matches!(
            input.cancellation,
            WorkProposalCancellationV1::Cancelled { .. }
        ) {
            return self.decision(
                input,
                WorkProposalDispositionV1::Indeterminate,
                None,
                false,
                vec![WorkProposalReasonV1::RequestCancelled],
                comparison,
            );
        }
        if input.evaluated_at >= input.deadline {
            return self.decision(
                input,
                WorkProposalDispositionV1::Indeterminate,
                None,
                false,
                vec![WorkProposalReasonV1::DeadlineExceeded],
                comparison,
            );
        }
        // Past the short-circuits the input is valid, live, and inside its
        // deadline, so the planner claim is licensed. It is computed once and
        // merged into whichever gate terminates the evaluation.
        let plan = plan_work(input);
        let mut reasons = vec![comparison_reason(comparison)];
        if comparison == WorkFrontierComparisonV1::Disagree {
            // Disagreeing frontiers cannot support a recommendation. Both
            // frontiers are preserved verbatim; neither substitutes for the
            // other, and no baseline is invented from a merged view.
            return self.planned_decision(
                input,
                WorkProposalDispositionV1::Abstain,
                None,
                false,
                reasons,
                comparison,
                plan,
            );
        }
        if input.task_accepted {
            // The task is complete: further proposals against it are refused,
            // not merely out of scope, so acceptance cannot be re-litigated.
            reasons.push(WorkProposalReasonV1::TaskAccepted);
            return self.planned_decision(
                input,
                WorkProposalDispositionV1::Deny,
                None,
                false,
                reasons,
                comparison,
                plan,
            );
        }
        if input.execution_admitted {
            if input.terminal_runtime_evidence_count > 0 {
                reasons.push(WorkProposalReasonV1::TerminalEvidenceObserved);
                return self.planned_decision(
                    input,
                    WorkProposalDispositionV1::Allow,
                    Some(WorkProposalActionV1::Replan),
                    false,
                    reasons,
                    comparison,
                    plan,
                );
            }
            reasons.push(WorkProposalReasonV1::ExecutionInFlight);
            return self.planned_decision(
                input,
                WorkProposalDispositionV1::Abstain,
                None,
                false,
                reasons,
                comparison,
                plan,
            );
        }
        if input.accepted_proposal_present {
            reasons.push(WorkProposalReasonV1::ProposalAccepted);
            return self.planned_decision(
                input,
                WorkProposalDispositionV1::Allow,
                Some(WorkProposalActionV1::AdmitExecution),
                false,
                reasons,
                comparison,
                plan,
            );
        }
        if input.unresolved_dependency_count > 0 {
            reasons.push(WorkProposalReasonV1::DependenciesUnresolved);
            return self.planned_decision(
                input,
                WorkProposalDispositionV1::Allow,
                Some(WorkProposalActionV1::HoldForDependencies),
                true,
                reasons,
                comparison,
                plan,
            );
        }
        reasons.push(WorkProposalReasonV1::Ready);
        self.planned_decision(
            input,
            WorkProposalDispositionV1::Allow,
            Some(WorkProposalActionV1::ProceedToAcceptance),
            false,
            reasons,
            comparison,
            plan,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn frontier(watermark: i64, byte: char) -> WorkEvidenceFrontierV1 {
        WorkEvidenceFrontierV1 {
            watermark: UtcMicros(watermark),
            digest: digest(byte),
        }
    }

    fn input() -> WorkProposalPolicyInputV1 {
        WorkProposalPolicyInputV1 {
            task_id: TaskId::try_from("task.policy.fixture".to_owned()).unwrap(),
            based_on_version: 1,
            dependency_count: 0,
            unresolved_dependency_count: 0,
            accepted_proposal_present: false,
            execution_admitted: false,
            task_accepted: false,
            runtime_evidence_count: 0,
            terminal_runtime_evidence_count: 0,
            local_evidence: Some(frontier(10, 'a')),
            live_git_evidence: None,
            policy_revision: 1,
            policy_digest: digest('b'),
            configuration_digest: digest('c'),
            deadline: UtcMicros(1_000),
            cancellation: WorkProposalCancellationV1::Active,
            evaluated_at: UtcMicros(100),
            eligible_routes: Vec::new(),
            budget: None,
            content_location: None,
            prior_outcomes: Vec::new(),
            human_override: None,
        }
    }

    #[test]
    fn identical_inputs_produce_identical_decisions() {
        let evaluator = WorkProposalEvaluatorV1::default();
        let request = input();
        assert_eq!(evaluator.evaluate(&request), evaluator.evaluate(&request));
    }

    #[test]
    fn ready_work_is_recommended_for_acceptance() {
        let decision = WorkProposalEvaluatorV1::default().evaluate(&input());
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Allow);
        assert_eq!(
            decision.recommended_action,
            Some(WorkProposalActionV1::ProceedToAcceptance)
        );
        assert!(!decision.deterministic_fallback);
        assert_eq!(
            decision.frontier_comparison,
            WorkFrontierComparisonV1::Incomparable
        );
        assert_eq!(decision.local_evidence, input().local_evidence);
    }

    #[test]
    fn unresolved_dependencies_select_the_deterministic_hold_baseline() {
        let mut request = input();
        request.dependency_count = 2;
        request.unresolved_dependency_count = 1;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Allow);
        assert_eq!(
            decision.recommended_action,
            Some(WorkProposalActionV1::HoldForDependencies)
        );
        assert!(decision.deterministic_fallback);
    }

    #[test]
    fn an_accepted_proposal_recommends_explicit_admission() {
        let mut request = input();
        request.accepted_proposal_present = true;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(
            decision.recommended_action,
            Some(WorkProposalActionV1::AdmitExecution)
        );
    }

    #[test]
    fn terminal_runtime_evidence_after_admission_recommends_a_replan() {
        let mut request = input();
        request.accepted_proposal_present = true;
        request.execution_admitted = true;
        request.runtime_evidence_count = 2;
        request.terminal_runtime_evidence_count = 1;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Allow);
        assert_eq!(
            decision.recommended_action,
            Some(WorkProposalActionV1::Replan)
        );
        assert!(
            decision
                .ordered_reason_codes
                .contains(&WorkProposalReasonV1::TerminalEvidenceObserved)
        );
    }

    #[test]
    fn in_flight_execution_without_terminal_evidence_abstains() {
        let mut request = input();
        request.accepted_proposal_present = true;
        request.execution_admitted = true;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Abstain);
        assert_eq!(decision.recommended_action, None);
    }

    #[test]
    fn an_accepted_task_denies_further_proposals() {
        let mut request = input();
        request.task_accepted = true;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Deny);
        assert_eq!(decision.recommended_action, None);
        assert!(
            decision
                .ordered_reason_codes
                .contains(&WorkProposalReasonV1::TaskAccepted)
        );
    }

    #[test]
    fn agreeing_frontiers_are_returned_unchanged_and_recorded_as_agreement() {
        let mut request = input();
        request.local_evidence = Some(frontier(10, 'a'));
        request.live_git_evidence = Some(frontier(20, 'a'));
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(
            decision.frontier_comparison,
            WorkFrontierComparisonV1::Agree
        );
        assert_eq!(decision.local_evidence, request.local_evidence);
        assert_eq!(decision.live_git_evidence, request.live_git_evidence);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Allow);
    }

    #[test]
    fn disagreeing_frontiers_abstain_without_substitution() {
        let mut request = input();
        request.local_evidence = Some(frontier(10, 'a'));
        request.live_git_evidence = Some(frontier(10, 'f'));
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(decision.disposition, WorkProposalDispositionV1::Abstain);
        assert_eq!(decision.recommended_action, None);
        assert_eq!(
            decision.frontier_comparison,
            WorkFrontierComparisonV1::Disagree
        );
        assert_eq!(decision.local_evidence, request.local_evidence);
        assert_eq!(decision.live_git_evidence, request.live_git_evidence);
    }

    #[test]
    fn cancellation_and_deadline_are_indeterminate() {
        let mut cancelled = input();
        cancelled.cancellation = WorkProposalCancellationV1::Cancelled {
            requested_at: UtcMicros(50),
        };
        assert_eq!(
            WorkProposalEvaluatorV1::default()
                .evaluate(&cancelled)
                .disposition,
            WorkProposalDispositionV1::Indeterminate
        );

        let mut elapsed = input();
        elapsed.evaluated_at = elapsed.deadline;
        assert_eq!(
            WorkProposalEvaluatorV1::default()
                .evaluate(&elapsed)
                .disposition,
            WorkProposalDispositionV1::Indeterminate
        );
    }

    #[test]
    fn inconsistent_counts_are_an_invalid_request() {
        let mut request = input();
        request.terminal_runtime_evidence_count = 3;
        request.runtime_evidence_count = 1;
        let decision = WorkProposalEvaluatorV1::default().evaluate(&request);
        assert_eq!(
            decision.disposition,
            WorkProposalDispositionV1::Indeterminate
        );
        assert_eq!(
            decision.ordered_reason_codes,
            vec![WorkProposalReasonV1::InvalidRequest]
        );
    }
}
