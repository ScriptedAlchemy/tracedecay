//! Plan 26 retrieval measurement projected from the real composition result.
//!
//! Plan 26 ("Retrieval, planner, and context measurement") requires that
//! "instrumentation remains on the canonical application retrieval pipeline
//! and its existing retriever and temporal adapters rather than a parallel
//! measurement path". This module is that instrumentation: every count below
//! is read out of values the pipeline already produced — the admitted
//! [`CompositionLaneInput`] lanes and the [`CompositionOutputV1`] the
//! composition kernel returned — and nothing here re-runs, re-ranks, or
//! re-queries.
//!
//! Three disciplines are load-bearing:
//!
//! * **Denial is not absence.** A lane whose sealed internal outcome is
//!   [`RetrieverOutcome::Denied`] contributes a denied source with no
//!   candidate counts at all, so the observation cannot reveal whether the
//!   denied source held matches. A lane that failed, timed out, was cancelled,
//!   or exhausted its budget contributes an *unknown* source, never an
//!   observed zero-match one.
//! * **Unmeasured is not zero.** Where a dimension genuinely is not available
//!   at the composition boundary — context tokens are only countable after
//!   hydration — the projection reports the weaker
//!   [`CoverageStateV1::Partial`] beside the payload instead of publishing a
//!   fabricated zero as fact.
//! * **Counts are clamped to their own denominators, never inflated.** The
//!   domain validators refuse a returned count above its eligible count, so
//!   the projection saturates against the denominator it was measured under
//!   rather than emitting a payload the observation authority would reject.

use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::{
    ContextOutcomeObservedV1, CoverageStateV1, RetrievalBudget, RetrievalPlannerObservedV1,
    RetrievalSourceObservedV1, RetrievalSynthesisObservedV1, RetrieverKind, RetrieverObservedV1,
    RetrieverOutcome,
};

use super::fusion::{CompositionLaneInput, CompositionOutputV1};

/// Revision of the lane-admission rule this projection reads. It changes when
/// the meaning of "admitted" changes, not when a lane implementation changes.
pub const PLANNER_REVISION_V1: &str = "retrieval-planner.composition.v1";
/// Revision of the per-lane candidate accounting rule.
pub const RETRIEVER_PROFILE_REVISION_V1: &str = "retriever-accounting.composition.v1";

/// One payload plus the coverage its producer must stamp on the envelope.
///
/// Coverage travels beside the payload rather than inside it because the
/// envelope is what the aggregate rollup reads when deciding whether a cell
/// may publish a point value at all.
#[derive(Clone, Debug, PartialEq)]
pub struct ObservedWithCoverageV1<T> {
    pub observation: T,
    pub coverage: CoverageStateV1,
}

/// Everything one completed composition can honestly say about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct RetrievalPipelineObservationV1 {
    pub planner: ObservedWithCoverageV1<RetrievalPlannerObservedV1>,
    pub retrievers: Vec<ObservedWithCoverageV1<RetrieverObservedV1>>,
    pub synthesis: ObservedWithCoverageV1<RetrievalSynthesisObservedV1>,
    pub sources: Vec<ObservedWithCoverageV1<RetrievalSourceObservedV1>>,
}

/// Fixed payload-safe lane label. The closed enum bounds label cardinality: a
/// lane added to [`RetrieverKind`] is a compile error here rather than an
/// unbounded metric dimension.
pub const fn retriever_lane_label(lane: RetrieverKind) -> &'static str {
    match lane {
        RetrieverKind::ExactLiteral => "exact_literal",
        RetrieverKind::Lexical => "lexical",
        RetrieverKind::Semantic => "semantic",
        RetrieverKind::Graph => "graph",
        RetrieverKind::Temporal => "temporal",
        RetrieverKind::Diagnostic => "diagnostic",
    }
}

/// The cataloged source kind a lane reads. Plan 26 keeps source kind a closed
/// catalog dimension; it derives from the lane, never from anything the query
/// said.
pub const fn retriever_source_kind(lane: RetrieverKind) -> &'static str {
    match lane {
        RetrieverKind::ExactLiteral
        | RetrieverKind::Lexical
        | RetrieverKind::Semantic
        | RetrieverKind::Graph => "code",
        RetrieverKind::Temporal => "git",
        RetrieverKind::Diagnostic => "diagnostic",
    }
}

/// How one lane's sealed internal outcome resolves as a source census.
///
/// Exactly one of the four states is set, which keeps
/// `observed + denied + unknown <= eligible` true for a single-source event
/// and makes a denial impossible to read back as an absence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceCensusV1 {
    Observed,
    Denied,
    Unknown,
    Stale,
}

const fn source_census(outcome: &RetrieverOutcome<()>) -> SourceCensusV1 {
    match outcome {
        RetrieverOutcome::Complete(())
        | RetrieverOutcome::Partial {
            value: (),
            reason: _,
        } => SourceCensusV1::Observed,
        RetrieverOutcome::Stale(_) => SourceCensusV1::Stale,
        RetrieverOutcome::Denied => SourceCensusV1::Denied,
        // Unavailable, budget-exhausted, timed-out, and cancelled lanes did
        // not finish looking. Reporting them as an observed zero-match source
        // would turn "we do not know" into "there was nothing there".
        RetrieverOutcome::Unavailable(_)
        | RetrieverOutcome::BudgetExceeded(_)
        | RetrieverOutcome::TimedOut(_)
        | RetrieverOutcome::Cancelled => SourceCensusV1::Unknown,
    }
}

/// A lane counts as admitted only when it actually contributed evidence. A
/// denied, unavailable, or cancelled lane was requested, not admitted.
const fn lane_was_admitted(census: SourceCensusV1) -> bool {
    matches!(census, SourceCensusV1::Observed | SourceCensusV1::Stale)
}

/// Number of final ranked candidates each lane contributed to.
///
/// A candidate fused from three lanes counts once for each of them, because
/// each lane is credited with reaching that anchor. Nothing is inferred from
/// the final scalar utility.
fn unique_contributions(output: &CompositionOutputV1) -> BTreeMap<RetrieverKind, u64> {
    let mut per_lane: BTreeMap<RetrieverKind, BTreeSet<&str>> = BTreeMap::new();
    for ranked in &output.ranked_candidates {
        let anchor = ranked.candidate.anchor_id.as_str();
        for contribution in &ranked.candidate.contributions {
            per_lane
                .entry(contribution.retriever)
                .or_default()
                .insert(anchor);
        }
    }
    per_lane
        .into_iter()
        .map(|(lane, anchors)| (lane, anchors.len() as u64))
        .collect()
}

/// Project the Plan 26 retrieval families from one completed composition.
///
/// `context_tokens` is `None` whenever the caller has not hydrated the page
/// yet; the synthesis observation then reports partial coverage rather than a
/// zero token count.
pub fn observe_composition(
    lanes: &[CompositionLaneInput],
    output: &CompositionOutputV1,
    budget: &RetrievalBudget,
    context_tokens: Option<u64>,
) -> RetrievalPipelineObservationV1 {
    let contributions = unique_contributions(output);
    let census_for = |lane: &CompositionLaneInput| {
        output
            .internal_lane_outcomes
            .get(&lane.lane)
            .map(source_census)
    };

    let requested_lanes = lanes
        .iter()
        .map(|lane| retriever_lane_label(lane.lane).to_owned())
        .collect::<Vec<_>>();
    let admitted_lanes = lanes
        .iter()
        .filter(|lane| census_for(lane).is_some_and(lane_was_admitted))
        .map(|lane| retriever_lane_label(lane.lane).to_owned())
        .collect::<Vec<_>>();
    // The planner's census is complete by construction: it enumerated every
    // lane it requested. Coverage weakens only when a requested lane produced
    // no sealed outcome at all, which means the composition could not say what
    // happened to it.
    let planner_coverage = if lanes.iter().all(|lane| census_for(lane).is_some()) {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    let planner = ObservedWithCoverageV1 {
        observation: RetrievalPlannerObservedV1 {
            planner_revision: PLANNER_REVISION_V1.to_owned(),
            requested_lanes,
            abstained: admitted_lanes.is_empty(),
            admitted_lanes,
        },
        coverage: planner_coverage,
    };

    let requested_candidates = u64::from(budget.max_candidates_per_lane);
    let mut retrievers = Vec::with_capacity(lanes.len());
    let mut sources = Vec::with_capacity(lanes.len());
    let mut candidate_count = 0_u64;
    for lane in lanes {
        let census = census_for(lane).unwrap_or(SourceCensusV1::Unknown);
        let batch = match &lane.outcome {
            RetrieverOutcome::Complete(batch)
            | RetrieverOutcome::Partial {
                value: batch,
                reason: _,
            } => Some(batch),
            _ => None,
        };
        if let Some(batch) = batch {
            let returned = batch.candidates.len() as u64;
            candidate_count = candidate_count.saturating_add(returned);
            // `RetrieverObservedV1::validate` refuses consumed > requested and
            // returned > eligible. Saturating at the measured denominator
            // keeps a mis-measured lane out of the store instead of letting it
            // publish a count larger than the population it came from.
            let observation = RetrieverObservedV1 {
                retriever_kind: retriever_lane_label(lane.lane).to_owned(),
                profile_revision: RETRIEVER_PROFILE_REVISION_V1.to_owned(),
                requested_candidates,
                consumed_candidates: batch.coverage.examined.min(requested_candidates),
                eligible_candidates: batch.coverage.eligible.max(returned),
                returned_candidates: returned,
                unique_contributions: contributions
                    .get(&lane.lane)
                    .copied()
                    .unwrap_or(0)
                    .min(returned),
            };
            // A lane that capped or could not classify part of its own
            // population did not observe all of it.
            let coverage = if batch.coverage.capped > 0 {
                CoverageStateV1::Capped
            } else if batch.coverage.unknown > 0 || batch.coverage.examined > requested_candidates {
                CoverageStateV1::Partial
            } else {
                CoverageStateV1::Known
            };
            retrievers.push(ObservedWithCoverageV1 {
                observation,
                coverage,
            });
        }

        // Denied sources carry no counts at all: eligible stays at the single
        // source and every candidate dimension is zero, so the event proves a
        // denial happened without proving anything existed behind it.
        let (observed, denied, unknown) = match census {
            SourceCensusV1::Observed | SourceCensusV1::Stale => (1, 0, 0),
            SourceCensusV1::Denied => (0, 1, 0),
            SourceCensusV1::Unknown => (0, 0, 1),
        };
        sources.push(ObservedWithCoverageV1 {
            observation: RetrievalSourceObservedV1 {
                source_kind: retriever_source_kind(lane.lane).to_owned(),
                eligible: 1,
                observed,
                denied,
                unknown,
            },
            coverage: match census {
                SourceCensusV1::Observed => CoverageStateV1::Known,
                SourceCensusV1::Stale => CoverageStateV1::Stale,
                SourceCensusV1::Denied => CoverageStateV1::Partial,
                SourceCensusV1::Unknown => CoverageStateV1::Unknown,
            },
        });
    }

    let context_count = output.ranked_candidates.len() as u64;
    let synthesis = ObservedWithCoverageV1 {
        observation: RetrievalSynthesisObservedV1 {
            candidate_count: candidate_count.max(context_count),
            context_count,
            // Tokens are only countable after hydration. `0` here means "not
            // yet measured", which is exactly why the coverage below drops to
            // `Partial` instead of claiming a known empty context.
            context_tokens: context_tokens.unwrap_or(0),
            abstained: context_count == 0,
        },
        coverage: if context_tokens.is_some() {
            CoverageStateV1::Known
        } else {
            CoverageStateV1::Partial
        },
    };

    RetrievalPipelineObservationV1 {
        planner,
        retrievers,
        synthesis,
        sources,
    }
}

/// The closed linkage vocabulary Plan 26 defines for a supplied context packet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextUseOutcomeV1 {
    Supplied,
    EvidenceCited,
    IndependentlyVerified,
    NoUseObserved,
    Unknown,
}

impl ContextUseOutcomeV1 {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Supplied => "context_supplied",
            Self::EvidenceCited => "evidence_cited",
            Self::IndependentlyVerified => "independently_verified_use",
            Self::NoUseObserved => "no_use_observed",
            Self::Unknown => "unknown",
        }
    }
}

/// Link one supplied context packet to what was independently observed about
/// its use.
///
/// Plan 26: "`ContextSupplied | EvidenceCited | IndependentlyVerifiedUse |
/// NoUseObserved` describes linkage, not causality", and a worker self-report
/// "cannot produce Plan 26 `Accepted`". Independent observation is therefore a
/// required input that only `IndependentlyVerified` may carry, never something
/// inferred from the outcome label alone.
pub fn observe_context_outcome(
    outcome: ContextUseOutcomeV1,
    independently_observed: bool,
    censored: bool,
) -> ObservedWithCoverageV1<ContextOutcomeObservedV1> {
    // The domain validator refuses an observation that is both censored and
    // independently observed; censoring wins because it is the stronger
    // statement about what may be retained.
    let independently_observed = independently_observed
        && !censored
        && outcome == ContextUseOutcomeV1::IndependentlyVerified;
    ObservedWithCoverageV1 {
        observation: ContextOutcomeObservedV1 {
            outcome: outcome.label().to_owned(),
            independently_observed,
            censored,
        },
        coverage: if censored || outcome == ContextUseOutcomeV1::Unknown {
            CoverageStateV1::Partial
        } else {
            CoverageStateV1::Known
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        FusionProfileId, ObservabilityPayloadV1, RetrievalFailure, RetrieverBatch,
        RetrieverCoverage,
    };

    fn empty_batch(eligible: u64) -> RetrieverBatch<()> {
        RetrieverBatch {
            candidates: Vec::new(),
            evidence_by_occurrence: BTreeMap::new(),
            coverage: RetrieverCoverage {
                examined: eligible,
                eligible,
                excluded: 0,
                capped: 0,
                unknown: 0,
            },
            continuation: None,
        }
    }

    fn composition(outcomes: Vec<(RetrieverKind, RetrieverOutcome<()>)>) -> CompositionOutputV1 {
        CompositionOutputV1 {
            profile_id: FusionProfileId::new("profile.observation.test.v1").unwrap(),
            ranked_candidates: Vec::new(),
            comparator_records: Vec::new(),
            internal_lane_outcomes: outcomes.into_iter().collect(),
            public_lane_statuses: BTreeMap::new(),
            freshness: Vec::new(),
            lane_checkpoints: Vec::new(),
            dedupe_decisions: Vec::new(),
            diversity_decisions: Vec::new(),
        }
    }

    const fn budget() -> RetrievalBudget {
        RetrievalBudget {
            max_candidates_per_lane: 64,
            max_fused_candidates: 32,
            max_hydrated_results: 16,
            max_hydration_bytes: 1_024,
            deadline_micros: None,
        }
    }

    fn unavailable<T>() -> RetrieverOutcome<T> {
        RetrieverOutcome::Unavailable(RetrievalFailure::AuthorityUnavailable {
            detail: "lane owner is not mounted".to_owned(),
        })
    }

    #[test]
    fn a_denied_lane_never_reports_an_observed_empty_source() {
        let lanes = vec![CompositionLaneInput {
            lane: RetrieverKind::Lexical,
            outcome: RetrieverOutcome::Denied,
        }];
        let output = composition(vec![(RetrieverKind::Lexical, RetrieverOutcome::Denied)]);
        let observed = observe_composition(&lanes, &output, &budget(), None);

        let source = observed.sources[0].observation.clone();
        assert_eq!(source.eligible, 1);
        assert_eq!(source.observed, 0, "a denial is not an observation");
        assert_eq!(source.denied, 1);
        assert_eq!(source.unknown, 0);
        ObservabilityPayloadV1::RetrievalSource(source)
            .validate()
            .expect("denied source validates");
        assert!(
            observed.planner.observation.abstained,
            "no lane contributed evidence"
        );
        assert!(
            observed.retrievers.is_empty(),
            "a denied lane publishes no candidate counts"
        );
    }

    #[test]
    fn an_unavailable_lane_reports_unknown_rather_than_zero_matches() {
        let lanes = vec![CompositionLaneInput {
            lane: RetrieverKind::Graph,
            outcome: unavailable(),
        }];
        let output = composition(vec![(RetrieverKind::Graph, unavailable())]);
        let observed = observe_composition(&lanes, &output, &budget(), None);

        let source = &observed.sources[0].observation;
        assert_eq!(source.unknown, 1);
        assert_eq!(source.observed, 0);
        assert_eq!(source.denied, 0);
        assert_eq!(observed.sources[0].coverage, CoverageStateV1::Unknown);
    }

    #[test]
    fn an_admitted_lane_is_requested_once_and_admitted_once() {
        let lanes = vec![
            CompositionLaneInput {
                lane: RetrieverKind::ExactLiteral,
                outcome: RetrieverOutcome::Complete(empty_batch(3)),
            },
            CompositionLaneInput {
                lane: RetrieverKind::Lexical,
                outcome: RetrieverOutcome::Denied,
            },
        ];
        let output = composition(vec![
            (RetrieverKind::ExactLiteral, RetrieverOutcome::Complete(())),
            (RetrieverKind::Lexical, RetrieverOutcome::Denied),
        ]);
        let observed = observe_composition(&lanes, &output, &budget(), None);

        let planner = observed.planner.observation.clone();
        assert_eq!(planner.requested_lanes, vec!["exact_literal", "lexical"]);
        assert_eq!(planner.admitted_lanes, vec!["exact_literal"]);
        assert!(!planner.abstained);
        ObservabilityPayloadV1::RetrievalPlanner(planner)
            .validate()
            .expect("admitted lanes are a subset of requested lanes");

        let retriever = observed.retrievers[0].observation.clone();
        assert_eq!(retriever.retriever_kind, "exact_literal");
        assert_eq!(retriever.requested_candidates, 64);
        assert_eq!(retriever.consumed_candidates, 3);
        ObservabilityPayloadV1::Retriever(retriever)
            .validate()
            .expect("retriever counts validate");
    }

    #[test]
    fn unhydrated_synthesis_reports_partial_coverage_instead_of_zero_tokens() {
        let lanes = vec![CompositionLaneInput {
            lane: RetrieverKind::Lexical,
            outcome: RetrieverOutcome::Complete(empty_batch(0)),
        }];
        let output = composition(vec![(
            RetrieverKind::Lexical,
            RetrieverOutcome::Complete(()),
        )]);

        let observed = observe_composition(&lanes, &output, &budget(), None);
        assert_eq!(observed.synthesis.coverage, CoverageStateV1::Partial);
        assert_eq!(observed.synthesis.observation.context_tokens, 0);

        let hydrated = observe_composition(&lanes, &output, &budget(), Some(512));
        assert_eq!(hydrated.synthesis.coverage, CoverageStateV1::Known);
        assert_eq!(hydrated.synthesis.observation.context_tokens, 512);
    }

    #[test]
    fn a_self_reported_use_cannot_become_an_independently_verified_one() {
        let observed = observe_context_outcome(ContextUseOutcomeV1::EvidenceCited, true, false);
        assert!(
            !observed.observation.independently_observed,
            "only IndependentlyVerified may claim independent observation"
        );

        let censored =
            observe_context_outcome(ContextUseOutcomeV1::IndependentlyVerified, true, true);
        assert!(!censored.observation.independently_observed);
        assert!(censored.observation.censored);
        assert_eq!(censored.coverage, CoverageStateV1::Partial);
        ObservabilityPayloadV1::ContextOutcome(censored.observation)
            .validate()
            .expect("censored context outcome validates");
    }
}
