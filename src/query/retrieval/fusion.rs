//! Deterministic fixed-point fusion stage contracts (Plan 15 pipeline steps
//! 4-8; Plan 25: `src/query/retrieval/fusion.rs` operates on compact
//! candidates with deterministic fixed-point contributions, complete
//! comparator provenance, and source/file caps).
//!
//! RRF may be evaluated as a profile candidate inside this generic
//! fixed-point framework; no constant or weight is production authority
//! before Plan 15 accepts it.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    CandidateContribution, CandidateSetDigest, CompactCandidate, ComponentRevision,
    CursorPayloadDigest, ExactClass, FreshnessCompatibilityV1, FusedCandidate, FusionProfile,
    LogicalEvidenceId, OccurrenceProvenance, PublicRetrieverStatus, RankedCandidate,
    RankingDecision, RankingDecisionKind, RetrievalAnchorId, RetrievalContractError,
    RetrievalCursor, RetrievalError, RetrievalRequest, RetrieverBatch, RetrieverContinuation,
    RetrieverKind, RetrieverOutcome, SourceFreshness, SourceOccurrenceId,
};

use super::dedupe::{DedupeDecisionV1, DeterministicDedupe};
use super::diversity::{DeterministicDiversity, DiversityDecisionV1, DiversityStageError};

/// Failures of the fusion stage. Fusion never substitutes or simulates a
/// missing lane; it composes the typed outcomes it is given.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FusionStageError {
    #[error("a required exact or lexical lane outcome is unavailable")]
    RequiredLaneUnavailable,
    #[error("candidate evidence is missing for a returned occurrence")]
    MissingOccurrenceEvidence,
    #[error("fixed-point arithmetic overflowed")]
    FixedPointOverflow,
    #[error("profile references a retriever outside the admitted lane set")]
    ProfileLaneMismatch,
    #[error("a retriever lane was supplied more than once")]
    DuplicateLane,
    #[error("a candidate score cannot be represented as a calibrated micros feature")]
    InvalidCalibratedFeature,
    #[error("contract violation: {0}")]
    Contract(String),
}

impl From<RetrievalContractError> for FusionStageError {
    fn from(error: RetrievalContractError) -> Self {
        match error {
            RetrievalContractError::FixedPointOverflow { .. } => Self::FixedPointOverflow,
            RetrievalContractError::MissingOccurrenceEvidence { .. }
            | RetrievalContractError::UnexpectedOccurrenceEvidence { .. } => {
                Self::MissingOccurrenceEvidence
            }
            other => Self::Contract(other.to_string()),
        }
    }
}

/// One independently typed lane admitted to compact composition.
///
/// The lane validates its typed evidence before this boundary. Composition
/// retains the one-to-one occurrence evidence keys, but does not copy or
/// interpret the evidence values owned by the lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionLaneInput {
    pub lane: RetrieverKind,
    pub outcome: RetrieverOutcome<RetrieverBatch<()>>,
}

impl CompositionLaneInput {
    pub fn new<E>(
        lane: RetrieverKind,
        outcome: RetrieverOutcome<RetrieverBatch<E>>,
    ) -> Result<Self, FusionStageError> {
        let outcome = match outcome {
            RetrieverOutcome::Complete(batch) => RetrieverOutcome::Complete(compact_batch(batch)?),
            RetrieverOutcome::Partial { value, reason } => RetrieverOutcome::Partial {
                value: compact_batch(value)?,
                reason,
            },
            RetrieverOutcome::Unavailable(reason) => RetrieverOutcome::Unavailable(reason),
            RetrieverOutcome::Denied => RetrieverOutcome::Denied,
            RetrieverOutcome::Stale(freshness) => RetrieverOutcome::Stale(freshness),
            RetrieverOutcome::BudgetExceeded(usage) => RetrieverOutcome::BudgetExceeded(usage),
            RetrieverOutcome::Cancelled => RetrieverOutcome::Cancelled,
        };
        Ok(Self { lane, outcome })
    }
}

fn compact_batch<E>(batch: RetrieverBatch<E>) -> Result<RetrieverBatch<()>, FusionStageError> {
    batch.validate()?;
    Ok(RetrieverBatch {
        candidates: batch.candidates,
        evidence_by_occurrence: batch
            .evidence_by_occurrence
            .into_keys()
            .map(|occurrence| (occurrence, ()))
            .collect(),
        coverage: batch.coverage,
        continuation: batch.continuation,
    })
}

/// One fusion input: independently typed lane batches admitted for one
/// pinned snapshot (Plan 15 pipeline step 3: each lane contributes its entire
/// committed prefix or none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FusionStageInput {
    pub profile: FusionProfile,
    pub lanes: Vec<CompositionLaneInput>,
}

/// The deterministic fusion stage contract (Plan 15: group contributions by
/// stable anchor plus logical evidence identity; total order is exact class,
/// utility, source validity, stable anchor ID, logical evidence ID, then
/// ordered source occurrence IDs).
pub trait DeterministicFusionStage {
    /// Partition candidates into exact tiers and fuse approximate
    /// contributions with checked fixed-point arithmetic. Exact admission
    /// derives only from validated proofs.
    fn fuse(&self, input: &FusionStageInput) -> Result<Vec<FusedCandidate>, FusionStageError>;

    /// Compute the final deterministic order over fused candidates. One
    /// hundred shuffled producer/completion runs must produce byte-identical
    /// IDs, order, contributions, explanations, coverage, and cursors
    /// (Plan 25 acceptance).
    fn order(&self, candidates: Vec<FusedCandidate>) -> Vec<RankedCandidate>;
}

/// Complete comparator tuple retained for each final candidate. It records
/// every field in the total order instead of reconstructing ordering from the
/// final scalar utility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FusionComparatorRecordV1 {
    pub exact_class: ExactClass,
    pub utility_micros: u64,
    pub source_validity_rank: u8,
    pub anchor_id: RetrievalAnchorId,
    pub logical_evidence_id: LogicalEvidenceId,
    pub source_occurrence_ids: Vec<SourceOccurrenceId>,
    pub comparator_revision: ComponentRevision,
}

/// Result of compact-candidate composition before page hydration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionOutputV1 {
    pub profile_id: tracedecay_domain::FusionProfileId,
    pub ranked_candidates: Vec<RankedCandidate>,
    pub comparator_records: Vec<FusionComparatorRecordV1>,
    pub internal_lane_outcomes: BTreeMap<RetrieverKind, RetrieverOutcome<()>>,
    pub public_lane_statuses: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    pub freshness: Vec<SourceFreshness>,
    pub lane_checkpoints: Vec<RetrieverContinuation>,
    pub dedupe_decisions: Vec<DedupeDecisionV1>,
    pub diversity_decisions: Vec<DiversityDecisionV1>,
}

/// One immutable page from the saved compact candidate list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionPageV1 {
    pub ranked_candidates: Vec<RankedCandidate>,
    pub cursor: Option<RetrievalCursor>,
}

/// Generic PR9 composition kernel. Evidence values are validated by
/// `RetrieverBatch<E>` but never interpreted, copied, or hydrated here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompositionKernel {
    ranking_revision: ComponentRevision,
    fusion: DeterministicFixedPointFusion,
    dedupe: DeterministicDedupe,
    diversity: DeterministicDiversity,
}

impl CompositionKernel {
    pub fn new(ranking_revision: ComponentRevision) -> Self {
        Self {
            fusion: DeterministicFixedPointFusion::new(ranking_revision.clone()),
            ranking_revision,
            dedupe: DeterministicDedupe,
            diversity: DeterministicDiversity,
        }
    }

    pub fn compose(
        &self,
        input: &FusionStageInput,
        policy: &tracedecay_domain::DiversityPolicy,
    ) -> Result<CompositionOutputV1, FusionStageError> {
        let admitted = admitted_lanes(input)?;
        let (compact, dedupe_decisions) = self
            .dedupe
            .collapse_compact_candidates(admitted.candidates)
            .map_err(|error| FusionStageError::Contract(error.to_string()))?;
        let mut fused = self.fusion.fuse_compact(&input.profile, compact)?;
        attach_same_source_decisions(&mut fused, &dedupe_decisions)?;
        let ordered = self.fusion.order_fused(fused);
        let (deduped, mut copy_decisions) = self
            .dedupe
            .select_representatives_with_decisions(ordered)
            .map_err(|error| FusionStageError::Contract(error.to_string()))?;
        let ordered = self.fusion.order_fused(deduped);
        let (ranked_candidates, diversity_decisions) = self
            .diversity
            .apply_caps(policy, ordered)
            .map_err(map_diversity_error)?;
        let comparator_records = ranked_candidates
            .iter()
            .map(|ranked| self.fusion.comparator_record(&ranked.candidate))
            .collect();

        let mut all_dedupe_decisions = dedupe_decisions;
        all_dedupe_decisions.append(&mut copy_decisions);
        Ok(CompositionOutputV1 {
            profile_id: input.profile.profile_id.clone(),
            ranked_candidates,
            comparator_records,
            internal_lane_outcomes: admitted.internal_lane_outcomes,
            public_lane_statuses: admitted.public_lane_statuses,
            freshness: admitted.freshness,
            lane_checkpoints: admitted.lane_checkpoints,
            dedupe_decisions: all_dedupe_decisions,
            diversity_decisions,
        })
    }

    /// Freeze and page the already composed candidate set. Resume validates
    /// every public binding and never recomputes against a differently
    /// completed lane set.
    pub fn paginate(
        &self,
        request: &RetrievalRequest,
        output: &CompositionOutputV1,
        page_size: usize,
        cursor: Option<&RetrievalCursor>,
    ) -> Result<CompositionPageV1, RetrievalError> {
        if page_size == 0 || page_size > request.budget.max_fused_candidates as usize {
            return Err(RetrievalError::InvalidRequest(
                "composition page size exceeds its deterministic budget".to_owned(),
            ));
        }

        let query_digest = digest_cursor_value("tracedecay.retrieval-query.v1", request)?;
        let snapshot_digest = request.snapshot.compute_digest()?;
        let candidate_set_digest = digest_candidate_set(&output.ranked_candidates)?;
        let start = match cursor {
            Some(cursor) => {
                validate_cursor(
                    cursor,
                    request,
                    output,
                    &query_digest,
                    &snapshot_digest,
                    &candidate_set_digest,
                    &self.ranking_revision,
                )?;
                cursor.next_ordinal as usize
            }
            None => 0,
        };
        if start > output.ranked_candidates.len() {
            return Err(RetrievalError::CursorSetMismatch);
        }

        let end = start
            .saturating_add(page_size)
            .min(output.ranked_candidates.len());
        let ranked_candidates = output.ranked_candidates[start..end].to_vec();
        let cursor = if end < output.ranked_candidates.len() {
            Some(build_cursor(
                request,
                output,
                query_digest,
                snapshot_digest,
                candidate_set_digest,
                self.ranking_revision.clone(),
                end as u32,
            )?)
        } else {
            None
        };
        Ok(CompositionPageV1 {
            ranked_candidates,
            cursor,
        })
    }
}

fn map_diversity_error(error: DiversityStageError) -> FusionStageError {
    FusionStageError::Contract(error.to_string())
}

struct AdmittedLanes {
    candidates: Vec<CompactCandidate>,
    internal_lane_outcomes: BTreeMap<RetrieverKind, RetrieverOutcome<()>>,
    public_lane_statuses: BTreeMap<RetrieverKind, PublicRetrieverStatus>,
    freshness: Vec<SourceFreshness>,
    lane_checkpoints: Vec<RetrieverContinuation>,
}

fn admitted_lanes(input: &FusionStageInput) -> Result<AdmittedLanes, FusionStageError> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut internal_lane_outcomes = BTreeMap::new();
    let mut public_lane_statuses = BTreeMap::new();
    let mut freshness = Vec::new();
    let mut lane_checkpoints = Vec::new();

    for lane_input in &input.lanes {
        let lane = lane_input.lane;
        let outcome = &lane_input.outcome;
        if !seen.insert(lane) {
            return Err(FusionStageError::DuplicateLane);
        }
        internal_lane_outcomes.insert(lane, unit_outcome(outcome));
        public_lane_statuses.insert(lane, public_status(outcome));
        match outcome {
            RetrieverOutcome::Complete(batch) | RetrieverOutcome::Partial { value: batch, .. } => {
                batch.validate()?;
                if batch
                    .candidates
                    .iter()
                    .any(|candidate| candidate.retriever != lane)
                {
                    return Err(FusionStageError::Contract(
                        "lane key does not match batch candidates".to_owned(),
                    ));
                }
                candidates.extend(batch.candidates.iter().cloned());
                freshness.extend(
                    batch
                        .candidates
                        .iter()
                        .map(|candidate| candidate.freshness.clone()),
                );
                if let Some(checkpoint) = &batch.continuation {
                    lane_checkpoints.push(checkpoint.clone());
                }
            }
            RetrieverOutcome::Unavailable(_)
            | RetrieverOutcome::Denied
            | RetrieverOutcome::Stale(_)
            | RetrieverOutcome::BudgetExceeded(_)
            | RetrieverOutcome::Cancelled => {
                if matches!(lane, RetrieverKind::ExactLiteral | RetrieverKind::Lexical) {
                    return Err(FusionStageError::RequiredLaneUnavailable);
                }
            }
        }
    }

    if !seen.contains(&RetrieverKind::ExactLiteral) || !seen.contains(&RetrieverKind::Lexical) {
        return Err(FusionStageError::RequiredLaneUnavailable);
    }
    if input
        .profile
        .calibrations
        .keys()
        .chain(input.profile.weights_micros.keys())
        .any(|lane| !seen.contains(lane))
    {
        return Err(FusionStageError::ProfileLaneMismatch);
    }

    freshness.sort_by(freshness_cmp);
    freshness.dedup();
    lane_checkpoints.sort_by(|left, right| {
        left.lane
            .cmp(&right.lane)
            .then_with(|| left.checkpoint_digest.cmp(&right.checkpoint_digest))
    });
    Ok(AdmittedLanes {
        candidates,
        internal_lane_outcomes,
        public_lane_statuses,
        freshness,
        lane_checkpoints,
    })
}

fn unit_outcome(outcome: &RetrieverOutcome<RetrieverBatch<()>>) -> RetrieverOutcome<()> {
    match outcome {
        RetrieverOutcome::Complete(_) => RetrieverOutcome::Complete(()),
        RetrieverOutcome::Partial { reason, .. } => RetrieverOutcome::Partial {
            value: (),
            reason: reason.clone(),
        },
        RetrieverOutcome::Unavailable(reason) => RetrieverOutcome::Unavailable(reason.clone()),
        RetrieverOutcome::Denied => RetrieverOutcome::Denied,
        RetrieverOutcome::Stale(freshness) => RetrieverOutcome::Stale(freshness.clone()),
        RetrieverOutcome::BudgetExceeded(usage) => RetrieverOutcome::BudgetExceeded(*usage),
        RetrieverOutcome::Cancelled => RetrieverOutcome::Cancelled,
    }
}

fn public_status(outcome: &RetrieverOutcome<RetrieverBatch<()>>) -> PublicRetrieverStatus {
    match outcome {
        RetrieverOutcome::Complete(_) => PublicRetrieverStatus::Complete,
        RetrieverOutcome::Partial { .. } | RetrieverOutcome::BudgetExceeded(_) => {
            PublicRetrieverStatus::Partial
        }
        RetrieverOutcome::Stale(_) => PublicRetrieverStatus::Stale,
        RetrieverOutcome::Unavailable(_)
        | RetrieverOutcome::Denied
        | RetrieverOutcome::Cancelled => PublicRetrieverStatus::Unavailable,
    }
}

/// Checked fixed-point implementation of the fusion stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicFixedPointFusion {
    comparator_revision: ComponentRevision,
}

impl DeterministicFixedPointFusion {
    pub fn new(comparator_revision: ComponentRevision) -> Self {
        Self {
            comparator_revision,
        }
    }

    fn fuse_compact(
        &self,
        profile: &FusionProfile,
        mut candidates: Vec<CompactCandidate>,
    ) -> Result<Vec<FusedCandidate>, FusionStageError> {
        candidates.sort_by(compact_candidate_cmp);
        let mut fused = BTreeMap::<(RetrievalAnchorId, LogicalEvidenceId), FusedCandidate>::new();

        for candidate in candidates {
            let calibration_profile_id = profile
                .calibrations
                .get(&candidate.retriever)
                .cloned()
                .ok_or(FusionStageError::ProfileLaneMismatch)?;
            let weight_micros = *profile
                .weights_micros
                .get(&candidate.retriever)
                .ok_or(FusionStageError::ProfileLaneMismatch)?;
            let calibrated_feature_micros = u32::try_from(candidate.raw_score.micros())
                .map_err(|_| FusionStageError::InvalidCalibratedFeature)?;
            let weighted_contribution_micros = candidate.raw_score.checked_weight(weight_micros)?;
            let exact_class = candidate.exact_class();
            let occurrence = occurrence_from(&candidate);
            let contribution = CandidateContribution {
                retriever: candidate.retriever,
                retriever_revision: candidate.retriever_revision.clone(),
                source_occurrence_id: candidate.source_occurrence_id.clone(),
                ordinal_rank: candidate.ordinal_rank,
                raw_score: candidate.raw_score,
                score_domain: candidate.score_domain.clone(),
                calibration_profile_id,
                calibrated_feature_micros,
                weight_micros,
                weighted_contribution_micros,
            };
            let key = (
                candidate.anchor_id.clone(),
                candidate.logical_evidence_id.clone(),
            );
            let entry = fused.entry(key).or_insert_with(|| FusedCandidate {
                anchor_id: candidate.anchor_id.clone(),
                logical_evidence_id: candidate.logical_evidence_id.clone(),
                occurrences: Vec::new(),
                exact_class,
                utility_micros: 0,
                contributions: Vec::new(),
                freshness: Vec::new(),
                decisions: Vec::new(),
            });
            entry.exact_class = strongest_exact_class(entry.exact_class, exact_class);
            entry.utility_micros = entry
                .utility_micros
                .checked_add(weighted_contribution_micros)
                .ok_or(FusionStageError::FixedPointOverflow)?;
            entry.occurrences.push(occurrence);
            entry.contributions.push(contribution);
            entry.freshness.push(candidate.freshness.clone());
            if exact_class != ExactClass::Approximate {
                entry.decisions.push(RankingDecision {
                    kind: RankingDecisionKind::ExactTierAdmission,
                    retriever: Some(RetrieverKind::ExactLiteral),
                    policy_anchor: Some(profile.evaluation_result_anchor.clone()),
                    evidence_anchor: Some(candidate.retriever_evidence_anchor.clone()),
                    detail: "validated exact admission proof".to_owned(),
                });
            }
        }

        let mut fused = fused.into_values().collect::<Vec<_>>();
        for candidate in &mut fused {
            candidate.occurrences.sort_by(occurrence_cmp);
            candidate.occurrences.dedup();
            candidate.contributions.sort_by(contribution_cmp);
            candidate.freshness.sort_by(freshness_cmp);
            candidate.freshness.dedup();
            candidate.decisions.sort_by(decision_cmp);
            candidate.decisions.dedup();
            candidate.validate()?;
        }
        Ok(fused)
    }

    fn order_fused(&self, mut candidates: Vec<FusedCandidate>) -> Vec<FusedCandidate> {
        candidates.sort_by(compare_fused);
        for candidate in &mut candidates {
            candidate
                .decisions
                .retain(|decision| decision.kind != RankingDecisionKind::ComparatorProvenance);
            let record = self.comparator_record(candidate);
            candidate.decisions.push(RankingDecision {
                kind: RankingDecisionKind::ComparatorProvenance,
                retriever: None,
                policy_anchor: None,
                evidence_anchor: candidate
                    .occurrences
                    .first()
                    .map(|occurrence| occurrence.retriever_evidence_anchor.clone()),
                detail: format!(
                    "exact={:?};utility={};source_validity={};anchor={};logical={};occurrences=[{}];revision={}",
                    record.exact_class,
                    record.utility_micros,
                    record.source_validity_rank,
                    record.anchor_id,
                    record.logical_evidence_id,
                    record
                        .source_occurrence_ids
                        .iter()
                        .map(|id| id.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                    record.comparator_revision,
                ),
            });
            candidate.decisions.sort_by(decision_cmp);
        }
        candidates
    }

    pub fn comparator_record(&self, candidate: &FusedCandidate) -> FusionComparatorRecordV1 {
        FusionComparatorRecordV1 {
            exact_class: candidate.exact_class,
            utility_micros: candidate.utility_micros,
            source_validity_rank: source_validity_rank(candidate),
            anchor_id: candidate.anchor_id.clone(),
            logical_evidence_id: candidate.logical_evidence_id.clone(),
            source_occurrence_ids: ordered_occurrence_ids(candidate),
            comparator_revision: self.comparator_revision.clone(),
        }
    }
}

impl DeterministicFusionStage for DeterministicFixedPointFusion {
    fn fuse(&self, input: &FusionStageInput) -> Result<Vec<FusedCandidate>, FusionStageError> {
        let admitted = admitted_lanes(input)?;
        self.fuse_compact(&input.profile, admitted.candidates)
    }

    fn order(&self, candidates: Vec<FusedCandidate>) -> Vec<RankedCandidate> {
        self.order_fused(candidates)
            .into_iter()
            .enumerate()
            .map(|(ordinal, candidate)| RankedCandidate {
                candidate,
                final_ordinal: ordinal as u32,
            })
            .collect()
    }
}

pub(super) fn compare_fused(left: &FusedCandidate, right: &FusedCandidate) -> Ordering {
    exact_class_rank(left.exact_class)
        .cmp(&exact_class_rank(right.exact_class))
        .then_with(|| right.utility_micros.cmp(&left.utility_micros))
        .then_with(|| source_validity_rank(right).cmp(&source_validity_rank(left)))
        .then_with(|| left.anchor_id.cmp(&right.anchor_id))
        .then_with(|| left.logical_evidence_id.cmp(&right.logical_evidence_id))
        .then_with(|| ordered_occurrence_ids(left).cmp(&ordered_occurrence_ids(right)))
}

fn compact_candidate_cmp(left: &CompactCandidate, right: &CompactCandidate) -> Ordering {
    left.anchor_id
        .cmp(&right.anchor_id)
        .then_with(|| left.logical_evidence_id.cmp(&right.logical_evidence_id))
        .then_with(|| left.source_occurrence_id.cmp(&right.source_occurrence_id))
        .then_with(|| left.retriever.cmp(&right.retriever))
        .then_with(|| {
            left.retriever_evidence_anchor
                .cmp(&right.retriever_evidence_anchor)
        })
}

fn occurrence_from(candidate: &CompactCandidate) -> OccurrenceProvenance {
    OccurrenceProvenance {
        source_occurrence_id: candidate.source_occurrence_id.clone(),
        retriever_evidence_anchor: candidate.retriever_evidence_anchor.clone(),
        source_namespace: candidate.source_namespace.clone(),
        repository_id: candidate.repository_id.clone(),
        session_or_thread_id: candidate.session_or_thread_id.clone(),
        logical_copy_cluster_id: candidate.logical_copy_cluster_id.clone(),
        evidence_role: candidate.evidence_role,
        freshness: candidate.freshness.clone(),
    }
}

fn strongest_exact_class(left: ExactClass, right: ExactClass) -> ExactClass {
    if exact_class_rank(left) <= exact_class_rank(right) {
        left
    } else {
        right
    }
}

fn exact_class_rank(class: ExactClass) -> u8 {
    match class {
        ExactClass::ExactMessage => 0,
        ExactClass::ExactLiteralPhrase => 1,
        ExactClass::Approximate => 2,
    }
}

fn source_validity_rank(candidate: &FusedCandidate) -> u8 {
    candidate
        .freshness
        .iter()
        .map(|freshness| match freshness.compatibility {
            FreshnessCompatibilityV1::Current => 4,
            FreshnessCompatibilityV1::Unknown => 3,
            FreshnessCompatibilityV1::Stale => 2,
            FreshnessCompatibilityV1::Missing => 1,
            FreshnessCompatibilityV1::Incompatible => 0,
        })
        .max()
        .unwrap_or(0)
}

fn ordered_occurrence_ids(candidate: &FusedCandidate) -> Vec<SourceOccurrenceId> {
    let mut occurrences = candidate
        .occurrences
        .iter()
        .map(|occurrence| occurrence.source_occurrence_id.clone())
        .collect::<Vec<_>>();
    occurrences.sort();
    occurrences.dedup();
    occurrences
}

fn occurrence_cmp(left: &OccurrenceProvenance, right: &OccurrenceProvenance) -> Ordering {
    left.source_occurrence_id
        .cmp(&right.source_occurrence_id)
        .then_with(|| {
            left.retriever_evidence_anchor
                .cmp(&right.retriever_evidence_anchor)
        })
}

fn contribution_cmp(left: &CandidateContribution, right: &CandidateContribution) -> Ordering {
    left.retriever
        .cmp(&right.retriever)
        .then_with(|| left.ordinal_rank.cmp(&right.ordinal_rank))
        .then_with(|| left.source_occurrence_id.cmp(&right.source_occurrence_id))
        .then_with(|| left.score_domain.cmp(&right.score_domain))
}

fn freshness_cmp(left: &SourceFreshness, right: &SourceFreshness) -> Ordering {
    left.source_namespace
        .cmp(&right.source_namespace)
        .then_with(|| left.source_instance.cmp(&right.source_instance))
        .then_with(|| left.source_generation.cmp(&right.source_generation))
        .then_with(|| left.projection_watermark.cmp(&right.projection_watermark))
        .then_with(|| left.policy_revision.cmp(&right.policy_revision))
}

fn decision_cmp(left: &RankingDecision, right: &RankingDecision) -> Ordering {
    left.kind
        .cmp(&right.kind)
        .then_with(|| left.retriever.cmp(&right.retriever))
        .then_with(|| left.policy_anchor.cmp(&right.policy_anchor))
        .then_with(|| left.evidence_anchor.cmp(&right.evidence_anchor))
        .then_with(|| left.detail.cmp(&right.detail))
}

fn attach_same_source_decisions(
    candidates: &mut [FusedCandidate],
    decisions: &[DedupeDecisionV1],
) -> Result<(), FusionStageError> {
    for recorded in decisions {
        let candidate = candidates
            .iter_mut()
            .find(|candidate| {
                candidate.occurrences.iter().any(|occurrence| {
                    occurrence.source_occurrence_id == recorded.kept_occurrence
                        && recorded.decision.evidence_anchor.as_ref()
                            == Some(&occurrence.retriever_evidence_anchor)
                })
            })
            .ok_or_else(|| {
                FusionStageError::Contract(
                    "same-source collapse decision lost its fused candidate".to_owned(),
                )
            })?;
        candidate.decisions.push(recorded.decision.clone());
        candidate.decisions.sort_by(decision_cmp);
        candidate.decisions.dedup();
    }
    Ok(())
}

fn digest_candidate_set(
    candidates: &[RankedCandidate],
) -> Result<CandidateSetDigest, RetrievalError> {
    digest_value("tracedecay.retrieval-candidate-set.v1", candidates)
        .and_then(|value| CandidateSetDigest::new(value).map_err(RetrievalError::from))
}

fn digest_cursor_value<T: Serialize>(
    domain: &'static str,
    value: &T,
) -> Result<CursorPayloadDigest, RetrievalError> {
    digest_value(domain, value)
        .and_then(|value| CursorPayloadDigest::new(value).map_err(RetrievalError::from))
}

fn digest_value<T: Serialize + ?Sized>(
    domain: &'static str,
    value: &T,
) -> Result<String, RetrievalError> {
    let bytes = serde_json::to_vec(&(domain, value))
        .map_err(|error| RetrievalError::InvalidRequest(error.to_string()))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

#[allow(clippy::too_many_arguments)]
fn build_cursor(
    request: &RetrievalRequest,
    output: &CompositionOutputV1,
    query_digest: CursorPayloadDigest,
    snapshot_digest: CandidateSetDigest,
    candidate_set_digest: CandidateSetDigest,
    ranking_revision: ComponentRevision,
    next_ordinal: u32,
) -> Result<RetrievalCursor, RetrievalError> {
    let mut cursor = RetrievalCursor {
        query_digest,
        profile_id: output.profile_id.clone(),
        snapshot_digest,
        freshness_digest: request.snapshot.freshness_digest.clone(),
        authorization_revision: request.snapshot.authorization_revision.clone(),
        candidate_set_digest,
        public_lane_statuses: output.public_lane_statuses.clone(),
        lane_checkpoints: output.lane_checkpoints.clone(),
        ranking_revision: tracedecay_domain::RankingRevision::new(
            ranking_revision.as_str().to_owned(),
        )?,
        next_ordinal,
        expiry: None,
        payload_digest: CursorPayloadDigest::new(format!("sha256:{}", "0".repeat(64)))?,
    };
    cursor.payload_digest = cursor_payload_digest(&cursor)?;
    Ok(cursor)
}

fn cursor_payload_digest(cursor: &RetrievalCursor) -> Result<CursorPayloadDigest, RetrievalError> {
    #[derive(Serialize)]
    struct CursorPayload<'a> {
        domain: &'static str,
        query_digest: &'a CursorPayloadDigest,
        profile_id: &'a tracedecay_domain::FusionProfileId,
        snapshot_digest: &'a CandidateSetDigest,
        freshness_digest: &'a tracedecay_domain::FreshnessVectorDigest,
        authorization_revision: &'a tracedecay_domain::AuthorizationRevision,
        candidate_set_digest: &'a CandidateSetDigest,
        public_lane_statuses: &'a BTreeMap<RetrieverKind, PublicRetrieverStatus>,
        lane_checkpoints: &'a [RetrieverContinuation],
        ranking_revision: &'a tracedecay_domain::RankingRevision,
        next_ordinal: u32,
        expiry: &'a Option<tracedecay_domain::UtcMicros>,
    }
    digest_cursor_value(
        "tracedecay.retrieval-cursor.v1",
        &CursorPayload {
            domain: "tracedecay.retrieval-cursor.v1",
            query_digest: &cursor.query_digest,
            profile_id: &cursor.profile_id,
            snapshot_digest: &cursor.snapshot_digest,
            freshness_digest: &cursor.freshness_digest,
            authorization_revision: &cursor.authorization_revision,
            candidate_set_digest: &cursor.candidate_set_digest,
            public_lane_statuses: &cursor.public_lane_statuses,
            lane_checkpoints: &cursor.lane_checkpoints,
            ranking_revision: &cursor.ranking_revision,
            next_ordinal: cursor.next_ordinal,
            expiry: &cursor.expiry,
        },
    )
}

fn validate_cursor(
    cursor: &RetrievalCursor,
    request: &RetrievalRequest,
    output: &CompositionOutputV1,
    query_digest: &CursorPayloadDigest,
    snapshot_digest: &CandidateSetDigest,
    candidate_set_digest: &CandidateSetDigest,
    ranking_revision: &ComponentRevision,
) -> Result<(), RetrievalError> {
    cursor.validate()?;
    let expected_ranking_revision =
        tracedecay_domain::RankingRevision::new(ranking_revision.as_str().to_owned())?;
    if cursor.query_digest != *query_digest
        || cursor.profile_id != output.profile_id
        || cursor.snapshot_digest != *snapshot_digest
        || cursor.freshness_digest != request.snapshot.freshness_digest
        || cursor.authorization_revision != request.snapshot.authorization_revision
        || cursor.candidate_set_digest != *candidate_set_digest
        || cursor.public_lane_statuses != output.public_lane_statuses
        || cursor.lane_checkpoints != output.lane_checkpoints
        || cursor.ranking_revision != expected_ranking_revision
        || cursor.payload_digest != cursor_payload_digest(cursor)?
    {
        return Err(RetrievalError::CursorSetMismatch);
    }
    Ok(())
}
