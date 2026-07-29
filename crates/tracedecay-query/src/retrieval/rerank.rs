//! Optional bounded rerank runtime (Plan 15 pipeline step 10).
//!
//! Exact tiers never enter the reranker. Only a policy-bounded prefix of the
//! approximate tier is exposed through ephemeral, authorized views. Every
//! failure is fail-open to the exact pre-rerank candidate bytes and is
//! represented by a sanitized public stage status.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::{
    AuthorizedRerankView, CandidateSetDigest, ExactClass, FreshnessCompatibilityV1,
    OptionalStagePublicStatus, PrivacyDomainId, RankedCandidate, RankingDecision,
    RankingDecisionKind, RerankPolicy, RetrievalAnchorId, RetrievalRequest, SanitizedBudgetUsage,
    SanitizedStageFailure,
};

/// Request state sampled before authorization and local execution.
pub trait RerankExecutionControlV1 {
    fn elapsed_micros(&self) -> u64;

    fn is_cancelled(&self) -> bool;
}

/// A strict permit for producing one ephemeral authorized view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RerankViewPermitV1 {
    pub expected_snapshot_digest: CandidateSetDigest,
    pub expected_privacy_domain: PrivacyDomainId,
    pub remaining_input_bytes: u64,
    pub remaining_input_tokens: u64,
    pub remaining_work_units: u64,
    pub remaining_deadline_micros: Option<u64>,
}

/// Internal authorization result. Missing and denied deliberately coalesce to
/// the same public authority-unavailable status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RerankViewOutcomeV1 {
    Authorized {
        view: AuthorizedRerankView,
        input_tokens: u64,
        work_units: u64,
    },
    Missing,
    Denied,
    Unavailable(SanitizedStageFailure),
    Cancelled,
}

/// Produces source-local, authorized views for this invocation only.
///
/// Implementations must not cache or persist either returned views or their
/// approved feature bytes.
pub trait EphemeralRerankViewSourceV1 {
    fn authorize_ephemeral_view(
        &mut self,
        request: &RetrievalRequest,
        candidate: &RankedCandidate,
        permit: &RerankViewPermitV1,
    ) -> RerankViewOutcomeV1;
}

/// One borrowed, authorized executor input. It cannot outlive this invocation.
#[derive(Clone, Copy, Debug)]
pub struct LocalRerankInputV1<'a> {
    pub candidate: &'a RankedCandidate,
    pub view: &'a AuthorizedRerankView,
}

/// Strict resource permit passed to the deterministic local executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalRerankPermitV1 {
    pub input_bytes: u64,
    pub input_tokens: u64,
    pub work_units: u64,
    pub model_invocations: u32,
    pub remaining_deadline_micros: Option<u64>,
}

/// Sanitized local executor failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalRerankFailureV1 {
    Unavailable(SanitizedStageFailure),
    Rejected(SanitizedStageFailure),
    TimedOut,
    Cancelled,
}

/// Deterministic, local-only rerank execution port.
///
/// `planned_model_invocations` is a payload-free preflight and must not run
/// the model. `rerank` returns only a permutation of admitted anchors; it
/// cannot inject model-specific scores into the retrieval contract.
pub trait DeterministicLocalRerankExecutorV1 {
    fn planned_model_invocations(&self, candidate_count: u32) -> Result<u32, LocalRerankFailureV1>;

    fn rerank(
        &self,
        policy: &RerankPolicy,
        inputs: &[LocalRerankInputV1<'_>],
        permit: LocalRerankPermitV1,
    ) -> Result<Vec<RetrievalAnchorId>, LocalRerankFailureV1>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RerankUsageV1 {
    pub candidates: u32,
    pub input_bytes: u64,
    pub input_tokens: u64,
    pub work_units: u64,
    pub model_invocations: u32,
    pub elapsed_micros: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedRerankOutcomeV1 {
    pub ordered_candidates: Vec<RankedCandidate>,
    pub public_status: OptionalStagePublicStatus,
    pub usage: RerankUsageV1,
}

/// Executes the optional rerank stage without retaining authorized views.
pub struct BoundedRerankRuntimeV1<'a, S: ?Sized, E: ?Sized> {
    views: &'a mut S,
    executor: &'a E,
}

impl<'a, S, E> BoundedRerankRuntimeV1<'a, S, E>
where
    S: EphemeralRerankViewSourceV1 + ?Sized,
    E: DeterministicLocalRerankExecutorV1 + ?Sized,
{
    pub fn new(views: &'a mut S, executor: &'a E) -> Self {
        Self { views, executor }
    }

    pub fn rerank(
        &mut self,
        request: &RetrievalRequest,
        policy: &RerankPolicy,
        pre_rerank: &[RankedCandidate],
        control: &dyn RerankExecutionControlV1,
    ) -> BoundedRerankOutcomeV1 {
        let original = pre_rerank.to_vec();
        let mut usage = RerankUsageV1 {
            elapsed_micros: control.elapsed_micros(),
            ..RerankUsageV1::default()
        };
        if !valid_pre_rerank_order(pre_rerank) || policy.max_candidates == 0 {
            return fallback(
                original,
                OptionalStagePublicStatus::Rejected(SanitizedStageFailure::Invalid),
                usage,
            );
        }
        if control.is_cancelled() {
            return fallback(original, OptionalStagePublicStatus::Cancelled, usage);
        }
        if deadline_exhausted(policy, control) {
            return budget_fallback(original, usage, control.elapsed_micros());
        }

        let approximate_start = pre_rerank
            .iter()
            .position(|candidate| candidate.candidate.exact_class == ExactClass::Approximate);
        let Some(approximate_start) = approximate_start else {
            return complete(original, usage);
        };
        let prefix_len = (pre_rerank.len() - approximate_start).min(policy.max_candidates as usize);
        let prefix = &pre_rerank[approximate_start..approximate_start + prefix_len];
        usage.candidates = prefix_len as u32;

        let snapshot_digest = match request.snapshot.compute_digest() {
            Ok(digest) => digest,
            Err(_) => {
                return fallback(
                    original,
                    OptionalStagePublicStatus::Rejected(SanitizedStageFailure::Invalid),
                    usage,
                );
            }
        };
        let mut authorized = Vec::with_capacity(prefix.len());
        for candidate in prefix {
            usage.elapsed_micros = control.elapsed_micros();
            if control.is_cancelled() {
                return fallback(original, OptionalStagePublicStatus::Cancelled, usage);
            }
            if deadline_exhausted(policy, control) {
                return budget_fallback(original, usage, control.elapsed_micros());
            }
            let permit = RerankViewPermitV1 {
                expected_snapshot_digest: snapshot_digest.clone(),
                expected_privacy_domain: request.scope.privacy_domain.clone(),
                remaining_input_bytes: policy.max_input_bytes.saturating_sub(usage.input_bytes),
                remaining_input_tokens: policy.max_input_tokens.saturating_sub(usage.input_tokens),
                remaining_work_units: policy.max_work_units.saturating_sub(usage.work_units),
                remaining_deadline_micros: remaining_deadline(policy, control),
            };
            match self
                .views
                .authorize_ephemeral_view(request, candidate, &permit)
            {
                RerankViewOutcomeV1::Authorized {
                    view,
                    input_tokens,
                    work_units,
                } => {
                    let input_bytes = view.approved_features.len() as u64;
                    if input_bytes > permit.remaining_input_bytes
                        || input_tokens > permit.remaining_input_tokens
                        || work_units > permit.remaining_work_units
                    {
                        return budget_fallback(original, usage, control.elapsed_micros());
                    }
                    if view.anchor_id != candidate.candidate.anchor_id {
                        return fallback(
                            original,
                            OptionalStagePublicStatus::Rejected(SanitizedStageFailure::Invalid),
                            usage,
                        );
                    }
                    if view.snapshot_digest != snapshot_digest
                        || view.privacy_domain != request.scope.privacy_domain
                    {
                        return fallback(
                            original,
                            OptionalStagePublicStatus::Rejected(
                                SanitizedStageFailure::Incompatible,
                            ),
                            usage,
                        );
                    }
                    match view.compatibility {
                        FreshnessCompatibilityV1::Current => {}
                        FreshnessCompatibilityV1::Stale => {
                            return fallback(
                                original,
                                OptionalStagePublicStatus::Unavailable(
                                    SanitizedStageFailure::Stale,
                                ),
                                usage,
                            );
                        }
                        FreshnessCompatibilityV1::Missing => {
                            return fallback(
                                original,
                                OptionalStagePublicStatus::Unavailable(
                                    SanitizedStageFailure::AuthorityUnavailable,
                                ),
                                usage,
                            );
                        }
                        FreshnessCompatibilityV1::Incompatible
                        | FreshnessCompatibilityV1::Unknown => {
                            return fallback(
                                original,
                                OptionalStagePublicStatus::Rejected(
                                    SanitizedStageFailure::Incompatible,
                                ),
                                usage,
                            );
                        }
                    }
                    usage.input_bytes += input_bytes;
                    usage.input_tokens += input_tokens;
                    usage.work_units += work_units;
                    authorized.push(view);
                }
                RerankViewOutcomeV1::Missing | RerankViewOutcomeV1::Denied => {
                    return fallback(
                        original,
                        OptionalStagePublicStatus::Unavailable(
                            SanitizedStageFailure::AuthorityUnavailable,
                        ),
                        usage,
                    );
                }
                RerankViewOutcomeV1::Unavailable(reason) => {
                    return fallback(
                        original,
                        OptionalStagePublicStatus::Unavailable(reason),
                        usage,
                    );
                }
                RerankViewOutcomeV1::Cancelled => {
                    return fallback(original, OptionalStagePublicStatus::Cancelled, usage);
                }
            }
        }

        let planned_invocations = match self.executor.planned_model_invocations(prefix_len as u32) {
            Ok(invocations) => invocations,
            Err(failure) => return executor_fallback(original, failure, usage, control),
        };
        if planned_invocations > policy.max_model_invocations {
            return budget_fallback(original, usage, control.elapsed_micros());
        }
        usage.model_invocations = planned_invocations;
        if control.is_cancelled() {
            return fallback(original, OptionalStagePublicStatus::Cancelled, usage);
        }
        if deadline_exhausted(policy, control) {
            return budget_fallback(original, usage, control.elapsed_micros());
        }

        let inputs = prefix
            .iter()
            .zip(&authorized)
            .map(|(candidate, view)| LocalRerankInputV1 { candidate, view })
            .collect::<Vec<_>>();
        let permit = LocalRerankPermitV1 {
            input_bytes: usage.input_bytes,
            input_tokens: usage.input_tokens,
            work_units: usage.work_units,
            model_invocations: usage.model_invocations,
            remaining_deadline_micros: remaining_deadline(policy, control),
        };
        let order = match self.executor.rerank(policy, &inputs, permit) {
            Ok(order) => order,
            Err(failure) => return executor_fallback(original, failure, usage, control),
        };
        usage.elapsed_micros = control.elapsed_micros();
        if control.is_cancelled() {
            return fallback(original, OptionalStagePublicStatus::Cancelled, usage);
        }
        if deadline_exhausted(policy, control) {
            return budget_fallback(original, usage, control.elapsed_micros());
        }
        match apply_order(pre_rerank, approximate_start, prefix, &order, policy) {
            Some(ordered) => complete(ordered, usage),
            None => fallback(
                original,
                OptionalStagePublicStatus::Rejected(SanitizedStageFailure::Invalid),
                usage,
            ),
        }
    }
}

fn valid_pre_rerank_order(candidates: &[RankedCandidate]) -> bool {
    let mut approximate_seen = false;
    let mut anchors = BTreeSet::new();
    candidates.iter().enumerate().all(|(ordinal, candidate)| {
        approximate_seen |= candidate.candidate.exact_class == ExactClass::Approximate;
        candidate.final_ordinal == ordinal as u32
            && (candidate.candidate.exact_class == ExactClass::Approximate || !approximate_seen)
            && candidate.candidate.validate().is_ok()
            && anchors.insert(candidate.candidate.anchor_id.clone())
    })
}

fn apply_order(
    all: &[RankedCandidate],
    approximate_start: usize,
    prefix: &[RankedCandidate],
    order: &[RetrievalAnchorId],
    policy: &RerankPolicy,
) -> Option<Vec<RankedCandidate>> {
    if order.len() != prefix.len() || order.iter().collect::<BTreeSet<_>>().len() != prefix.len() {
        return None;
    }
    let mut by_anchor = prefix
        .iter()
        .map(|candidate| (candidate.candidate.anchor_id.clone(), candidate.clone()))
        .collect::<BTreeMap<_, _>>();
    if by_anchor.len() != prefix.len() || order.iter().any(|anchor| !by_anchor.contains_key(anchor))
    {
        return None;
    }

    let mut result = all[..approximate_start].to_vec();
    for anchor in order {
        let mut candidate = by_anchor.remove(anchor)?;
        candidate.candidate.decisions.push(RankingDecision {
            kind: RankingDecisionKind::RerankAdmission,
            retriever: None,
            policy_anchor: Some(policy.evaluation_result_anchor.clone()),
            evidence_anchor: None,
            detail: format!(
                "admitted by bounded rerank policy {}",
                policy.policy_id.as_str()
            ),
        });
        result.push(candidate);
    }
    result.extend_from_slice(&all[approximate_start + prefix.len()..]);
    for (ordinal, candidate) in result.iter_mut().enumerate() {
        candidate.final_ordinal = ordinal as u32;
    }
    Some(result)
}

fn executor_fallback(
    original: Vec<RankedCandidate>,
    failure: LocalRerankFailureV1,
    mut usage: RerankUsageV1,
    control: &dyn RerankExecutionControlV1,
) -> BoundedRerankOutcomeV1 {
    usage.elapsed_micros = control.elapsed_micros();
    match failure {
        LocalRerankFailureV1::Unavailable(reason) => fallback(
            original,
            OptionalStagePublicStatus::Unavailable(reason),
            usage,
        ),
        LocalRerankFailureV1::Rejected(reason) => {
            fallback(original, OptionalStagePublicStatus::Rejected(reason), usage)
        }
        LocalRerankFailureV1::TimedOut => {
            budget_fallback(original, usage, control.elapsed_micros())
        }
        LocalRerankFailureV1::Cancelled => {
            fallback(original, OptionalStagePublicStatus::Cancelled, usage)
        }
    }
}

fn deadline_exhausted(policy: &RerankPolicy, control: &dyn RerankExecutionControlV1) -> bool {
    policy
        .deadline_micros
        .is_some_and(|deadline| control.elapsed_micros() >= deadline)
}

fn remaining_deadline(
    policy: &RerankPolicy,
    control: &dyn RerankExecutionControlV1,
) -> Option<u64> {
    policy
        .deadline_micros
        .map(|deadline| deadline.saturating_sub(control.elapsed_micros()))
}

fn complete(
    ordered_candidates: Vec<RankedCandidate>,
    usage: RerankUsageV1,
) -> BoundedRerankOutcomeV1 {
    BoundedRerankOutcomeV1 {
        ordered_candidates,
        public_status: OptionalStagePublicStatus::Complete,
        usage,
    }
}

fn fallback(
    ordered_candidates: Vec<RankedCandidate>,
    public_status: OptionalStagePublicStatus,
    usage: RerankUsageV1,
) -> BoundedRerankOutcomeV1 {
    BoundedRerankOutcomeV1 {
        ordered_candidates,
        public_status,
        usage,
    }
}

fn budget_fallback(
    ordered_candidates: Vec<RankedCandidate>,
    mut usage: RerankUsageV1,
    elapsed_micros: u64,
) -> BoundedRerankOutcomeV1 {
    usage.elapsed_micros = elapsed_micros;
    fallback(
        ordered_candidates,
        OptionalStagePublicStatus::BudgetExceeded(SanitizedBudgetUsage {
            elapsed_micros,
            truncated: true,
        }),
        usage,
    )
}
