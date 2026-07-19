pub mod candidates;
pub mod context;
pub mod cursor;
pub mod hydration;
pub mod ports;
pub mod ranking;
pub mod resolution;

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tracedecay_domain::{
    CompactContextConflictV1, CompactContextLineageEdgeV1, CompactContextOmissionV1,
    ContextOmissionReasonV1, HydrationStateV1, RetrievalAnchorId, SessionSummaryRecordV1,
    TemporalAssertionKindV1, TemporalCoverageCountsV1, TemporalModeV1,
};

use self::context::{
    CompactContext, ContextBudget, ContextError, TemporalContextFrames, VersionedTokenEstimator,
    assemble_context_with_frames_controlled,
};
use self::cursor::{CursorError, StableSortKey, encode_cursor, verify_cursor};
use self::hydration::{HydrationBatch, HydrationError, TemporalHydrationPort, hydrate_selected};
use self::ports::{
    CandidateReadState, PageLimits, PageStatus, SessionCursorAuthenticator,
    TemporalExecutionSnapshot, TemporalPortError, TemporalReadPort, TemporalRecord,
    TemporalRecordBatch, TemporalRecordReadState, TemporalRetrievalScope, pull_candidate_page,
    pull_temporal_record_page,
};
use self::ranking::{DiversityLimits, RankedCandidate, RankingError, rank_candidates};
use self::resolution::{
    ResolutionLineageEdge, ResolutionLineageEdgeKind, ResolvedOccurrence,
    SummaryLineageEligibility, SummaryLineageRejection, SummaryOmission, SummarySourceState,
    evaluate_summary_lineage_eligibility_controlled, resolve_temporal_controlled,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalKernelRequest {
    pub snapshot: TemporalExecutionSnapshot,
    pub query: String,
    pub cursor: Option<String>,
    pub limit: usize,
    pub diversity: DiversityLimits,
    pub context_budget: ContextBudget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporalKernelResult {
    pub snapshot: TemporalExecutionSnapshot,
    pub ranked: Vec<RankedCandidate>,
    pub context: CompactContext,
    pub coverage: TemporalCoverageCountsV1,
    pub conflicts: Vec<CompactContextConflictV1>,
    pub lineage: Vec<CompactContextLineageEdgeV1>,
    pub summary_omissions: Vec<SummaryOmission>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TemporalKernelError {
    #[error("temporal query limit must be non-zero")]
    InvalidLimit,
    #[error("temporal query was cancelled")]
    Cancelled,
    #[error("temporal query deadline elapsed")]
    DeadlineExceeded,
    #[error("temporal query exceeded its frozen execution limits")]
    BudgetExceeded,
    #[error(transparent)]
    Port(#[from] TemporalPortError),
    #[error(transparent)]
    Cursor(#[from] CursorError),
    #[error(transparent)]
    Ranking(#[from] RankingError),
    #[error(transparent)]
    Hydration(#[from] HydrationError),
    #[error(transparent)]
    Context(#[from] ContextError),
}

pub async fn execute_temporal_kernel(
    request: &TemporalKernelRequest,
    read_port: &impl TemporalReadPort,
    hydration_port: &impl TemporalHydrationPort,
    authenticator: &impl SessionCursorAuthenticator,
    token_estimator: &impl VersionedTokenEstimator,
) -> Result<TemporalKernelResult, TemporalKernelError> {
    if request.limit == 0 {
        return Err(TemporalKernelError::InvalidLimit);
    }
    let snapshot = &request.snapshot;
    check_control(&snapshot)?;
    let limits = snapshot.request().limits();
    if request.limit > limits.hydration_limit {
        return Err(TemporalKernelError::BudgetExceeded);
    }

    let after = request
        .cursor
        .as_deref()
        .map(|cursor| verify_cursor(cursor, &snapshot, authenticator))
        .transpose()?;
    check_control(&snapshot)?;
    let plan = if request.query.trim().is_empty()
        && snapshot.temporal_mode() == TemporalModeV1::Forensic
        && matches!(
            snapshot.request().retrieval_scope(),
            TemporalRetrievalScope::Session(_)
        ) {
        candidates::plan_scope_candidates()
    } else {
        candidates::plan_candidates(&request.query)
    };
    let candidate_page_items = limits.candidate_limit.min(64);
    let candidate_limits = PageLimits::new(
        limits.candidate_limit,
        limits.candidate_total_bytes,
        limits.candidate_item_bytes,
        candidate_page_items,
    )
    .map_err(map_port_error)?;
    let mut candidate_state = CandidateReadState::new(candidate_limits);
    let mut candidates = Vec::with_capacity(limits.candidate_limit.min(256));
    loop {
        let page = pull_candidate_page(read_port, &snapshot, &plan, &mut candidate_state)
            .await
            .map_err(map_port_error)?;
        let status = page.status();
        candidates.extend(page.into_items());
        if status == PageStatus::Complete {
            break;
        }
    }

    let record_page_items = limits.record_limit.min(64);
    let record_limits = PageLimits::new(
        limits.record_limit,
        limits.record_total_bytes,
        limits.record_item_bytes,
        record_page_items,
    )
    .map_err(map_port_error)?;
    let mut record_state = TemporalRecordReadState::new(record_limits);
    let mut records = TemporalRecordBatch::default();
    loop {
        let page = pull_temporal_record_page(read_port, &snapshot, &candidates, &mut record_state)
            .await
            .map_err(map_port_error)?;
        let status = page.status();
        for record in page.into_items() {
            match record {
                TemporalRecord::Occurrence(value) => records.occurrences.push(value),
                TemporalRecord::Copy(value) => records.copies.push(value),
                TemporalRecord::Assertion(value) => records.assertions.push(value),
                TemporalRecord::Summary(value) => records.summaries.push(value),
                TemporalRecord::SummarySource(value) => {
                    if records
                        .summary_sources
                        .insert(value.anchor_id, value.state)
                        .is_some()
                    {
                        return Err(TemporalKernelError::Port(TemporalPortError::Read {
                            operation: "collect summary source states",
                            message: "adapter returned a duplicate summary source state"
                                .to_string(),
                        }));
                    }
                }
            }
        }
        if status == PageStatus::Complete {
            break;
        }
    }

    let resolved = resolve_temporal_controlled(
        &records.occurrences,
        &records.copies,
        &records.assertions,
        snapshot.temporal_mode(),
        snapshot.request().execution_control(),
    )
    .map_err(map_port_error)?;
    let mut visible_anchors = resolved
        .iter()
        .map(|resolved| resolved.occurrence.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    let summary_eligibility = evaluate_summaries_for_scope(
        &records.summaries,
        &records.summary_sources,
        snapshot.request().retrieval_scope(),
        snapshot.temporal_mode(),
        snapshot.request().execution_control(),
    )
    .map_err(map_port_error)?;
    visible_anchors.extend(summary_eligibility.eligible_anchor_ids.clone());
    check_control(&snapshot)?;
    let all_candidates = candidates;
    let all_candidate_anchors = all_candidates
        .iter()
        .map(|candidate| candidate.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    let visible_candidates = all_candidates
        .into_iter()
        .filter(|candidate| visible_anchors.contains(&candidate.anchor_id))
        .collect::<Vec<_>>();
    let mut ranked = rank_candidates(&visible_candidates, request.diversity)?;
    if let Some(after) = &after {
        ranked.retain(|candidate| is_after(candidate, after));
    }
    let mut ranked_anchors = BTreeSet::new();
    ranked.retain(|candidate| ranked_anchors.insert(candidate.anchor_id.clone()));

    let has_more = ranked.len() > request.limit;
    ranked.truncate(request.limit);
    let anchors = ranked
        .iter()
        .map(|candidate| candidate.anchor_id.clone())
        .collect::<Vec<_>>();
    let hydration = hydrate_selected(hydration_port, &snapshot, &anchors)
        .await
        .map_err(map_hydration_error)?;
    check_control(&snapshot)?;
    let frames = temporal_context_frames(
        &all_candidate_anchors,
        &visible_anchors,
        &resolved,
        &resolved.lineage_edges,
        &hydration,
        &summary_eligibility,
    );
    let context = assemble_context_with_frames_controlled(
        &hydration,
        snapshot.grain(),
        frames,
        request.context_budget.clone(),
        token_estimator,
        snapshot.request().execution_control(),
    )
    .map_err(map_context_error)?;
    check_control(&snapshot)?;
    let next_cursor = if has_more {
        ranked
            .last()
            .map(stable_sort_key)
            .map(|sort_key| encode_cursor(&snapshot, &sort_key, authenticator))
            .transpose()?
    } else {
        None
    };

    let summary_omissions = public_summary_omissions(&summary_eligibility);
    Ok(TemporalKernelResult {
        coverage: context.bundle.coverage,
        conflicts: context.bundle.conflicts.clone(),
        lineage: context.bundle.lineage.clone(),
        snapshot: snapshot.clone(),
        ranked,
        context,
        summary_omissions,
        next_cursor,
    })
}

fn evaluate_summaries_for_scope(
    summaries: &[SessionSummaryRecordV1],
    source_states: &BTreeMap<RetrievalAnchorId, SummarySourceState>,
    scope: &TemporalRetrievalScope,
    mode: tracedecay_domain::TemporalModeV1,
    control: &ports::ExecutionControl,
) -> Result<SummaryLineageEligibility, TemporalPortError> {
    match scope {
        TemporalRetrievalScope::Session(session_id) => {
            evaluate_summary_lineage_eligibility_controlled(
                summaries,
                source_states,
                session_id,
                mode,
                control,
            )
        }
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => {
            let mut summaries_by_session = BTreeMap::new();
            for summary in summaries {
                control.checkpoint()?;
                summaries_by_session
                    .entry(summary.session_id().clone())
                    .or_insert_with(Vec::new)
                    .push(summary.clone());
            }
            let mut combined = SummaryLineageEligibility {
                eligible_anchor_ids: BTreeSet::new(),
                suppressed_summary_ids: BTreeSet::new(),
                rejections: BTreeMap::new(),
                omissions: Vec::new(),
            };
            for (session_id, session_summaries) in summaries_by_session {
                control.checkpoint()?;
                let eligibility = evaluate_summary_lineage_eligibility_controlled(
                    &session_summaries,
                    source_states,
                    &session_id,
                    mode,
                    control,
                )?;
                combined
                    .eligible_anchor_ids
                    .extend(eligibility.eligible_anchor_ids);
                combined
                    .suppressed_summary_ids
                    .extend(eligibility.suppressed_summary_ids);
                combined.rejections.extend(eligibility.rejections);
                combined.omissions.extend(eligibility.omissions);
            }
            Ok(combined)
        }
    }
}

fn check_control(snapshot: &TemporalExecutionSnapshot) -> Result<(), TemporalKernelError> {
    snapshot
        .request()
        .execution_control()
        .checkpoint()
        .map_err(map_port_error)
}

fn map_port_error(error: TemporalPortError) -> TemporalKernelError {
    match error {
        TemporalPortError::Cancelled => TemporalKernelError::Cancelled,
        TemporalPortError::DeadlineExceeded => TemporalKernelError::DeadlineExceeded,
        TemporalPortError::BudgetExceeded { .. }
        | TemporalPortError::ParticipantLimitExceeded { .. }
        | TemporalPortError::ParticipantManifestBytesExceeded { .. } => {
            TemporalKernelError::BudgetExceeded
        }
        TemporalPortError::InvalidBinding { .. }
        | TemporalPortError::EmptyParticipantManifest
        | TemporalPortError::DuplicateParticipant
        | TemporalPortError::ZeroGeneration
        | TemporalPortError::UnauthorizedSnapshot
        | TemporalPortError::ZeroVersion { .. }
        | TemporalPortError::Read { .. } => TemporalKernelError::Port(error),
    }
}

fn map_context_error(error: ContextError) -> TemporalKernelError {
    match error {
        ContextError::Interrupted(error) => map_port_error(error),
        ContextError::BudgetExceeded { .. } => TemporalKernelError::BudgetExceeded,
        ContextError::EstimatorVersionMismatch | ContextError::InvalidBundle(_) => {
            TemporalKernelError::Context(error)
        }
    }
}

fn map_hydration_error(error: HydrationError) -> TemporalKernelError {
    match error {
        HydrationError::Interrupted(error) => map_port_error(error),
        HydrationError::BudgetExceeded { .. } => TemporalKernelError::BudgetExceeded,
        HydrationError::Unavailable { .. } | HydrationError::InvalidDenial => {
            TemporalKernelError::Hydration(error)
        }
    }
}

fn stable_sort_key(candidate: &RankedCandidate) -> StableSortKey {
    StableSortKey {
        normalized_score_micros: candidate.normalized_score_micros,
        knowledge_at_micros: candidate.knowledge_at_micros,
        stable_id: candidate.stable_id.clone(),
    }
}

fn is_after(candidate: &RankedCandidate, after: &StableSortKey) -> bool {
    candidate.normalized_score_micros < after.normalized_score_micros
        || (candidate.normalized_score_micros == after.normalized_score_micros
            && (candidate.knowledge_at_micros < after.knowledge_at_micros
                || (candidate.knowledge_at_micros == after.knowledge_at_micros
                    && candidate.stable_id > after.stable_id)))
}

fn temporal_context_frames(
    all_candidate_anchors: &BTreeSet<RetrievalAnchorId>,
    visible_anchors: &BTreeSet<RetrievalAnchorId>,
    resolved: &[ResolvedOccurrence],
    lineage_edges: &[ResolutionLineageEdge],
    hydration: &HydrationBatch,
    summary_eligibility: &SummaryLineageEligibility,
) -> TemporalContextFrames {
    let unknown_anchors = resolved
        .iter()
        .filter(|item| item.uncertain)
        .map(|item| item.occurrence.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    let hydration_states = hydration
        .unavailable
        .iter()
        .map(|item| (item.anchor_id().clone(), item.state()))
        .collect::<BTreeMap<_, _>>();
    let summary_states = summary_eligibility
        .omissions
        .iter()
        .map(|omission| {
            (
                omission.anchor_id.clone(),
                summary_rejection_class(&omission.rejection, &summary_eligibility.rejections)
                    .coverage(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut coverage = TemporalCoverageCountsV1::default();
    for anchor_id in all_candidate_anchors {
        if let Some(state) = hydration_states.get(anchor_id) {
            increment_hydration_coverage(&mut coverage, *state);
        } else if let Some(class) = summary_states.get(anchor_id) {
            increment_coverage(&mut coverage, *class);
        } else if unknown_anchors.contains(anchor_id) {
            coverage.unknown += 1;
        } else if visible_anchors.contains(anchor_id) {
            coverage.visible += 1;
        } else {
            coverage.hidden += 1;
        }
    }
    let conflicts = resolved
        .iter()
        .filter(|item| item.conflicted)
        .map(|item| CompactContextConflictV1 {
            anchor_id: item.occurrence.anchor_id.clone(),
            supporting_anchor_ids: item.supporting_anchor_ids.clone(),
        })
        .collect();
    let lineage = lineage_edges.iter().map(context_lineage_edge).collect();
    let summary_omissions = public_summary_omissions(summary_eligibility);
    let omissions = summary_omissions
        .iter()
        .map(|omission| CompactContextOmissionV1 {
            anchor_id: Some(omission.anchor_id.clone()),
            reason: summary_rejection_reason(&omission.rejection),
        })
        .collect();

    TemporalContextFrames {
        coverage,
        conflicts,
        lineage,
        omissions,
        summary_omissions,
    }
}

#[derive(Clone, Copy)]
enum CoverageClass {
    Hidden,
    Unknown,
    Redacted,
}

fn increment_coverage(coverage: &mut TemporalCoverageCountsV1, class: CoverageClass) {
    match class {
        CoverageClass::Hidden => coverage.hidden += 1,
        CoverageClass::Unknown => coverage.unknown += 1,
        CoverageClass::Redacted => coverage.redacted += 1,
    }
}

fn increment_hydration_coverage(coverage: &mut TemporalCoverageCountsV1, state: HydrationStateV1) {
    let class = match state {
        HydrationStateV1::Unauthorized => CoverageClass::Hidden,
        HydrationStateV1::Redacted
        | HydrationStateV1::Deleted
        | HydrationStateV1::RetentionExpired => CoverageClass::Redacted,
        HydrationStateV1::RetainedButUnavailable
        | HydrationStateV1::Locked
        | HydrationStateV1::UnverifiableLegacy => CoverageClass::Unknown,
        HydrationStateV1::Available => return,
    };
    increment_coverage(coverage, class);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SummaryRejectionClass {
    Unauthorized,
    SessionMismatch,
    Redacted,
    Unknown,
}

impl SummaryRejectionClass {
    const fn coverage(self) -> CoverageClass {
        match self {
            Self::Unauthorized | Self::SessionMismatch => CoverageClass::Hidden,
            Self::Redacted => CoverageClass::Redacted,
            Self::Unknown => CoverageClass::Unknown,
        }
    }

    const fn hides_details(self) -> bool {
        match self {
            Self::Unauthorized | Self::SessionMismatch => true,
            Self::Redacted | Self::Unknown => false,
        }
    }
}

fn summary_rejection_class(
    rejection: &SummaryLineageRejection,
    rejections: &BTreeMap<tracedecay_domain::SessionSummaryIdV1, SummaryLineageRejection>,
) -> SummaryRejectionClass {
    let mut rejection = rejection;
    let mut visited = BTreeSet::new();
    loop {
        match rejection {
            SummaryLineageRejection::UnauthorizedSource { .. } => {
                return SummaryRejectionClass::Unauthorized;
            }
            SummaryLineageRejection::SessionMismatch => {
                return SummaryRejectionClass::SessionMismatch;
            }
            SummaryLineageRejection::DeletedSource { .. }
            | SummaryLineageRejection::RedactedSource { .. }
            | SummaryLineageRejection::ExpiredSource { .. } => {
                return SummaryRejectionClass::Redacted;
            }
            SummaryLineageRejection::IneligiblePredecessor {
                predecessor_summary_id,
            } if visited.insert(predecessor_summary_id.clone()) => {
                let Some(predecessor_rejection) = rejections.get(predecessor_summary_id) else {
                    return SummaryRejectionClass::Unknown;
                };
                rejection = predecessor_rejection;
            }
            SummaryLineageRejection::CreatedAfterCutoff
            | SummaryLineageRejection::HorizonAfterCutoff
            | SummaryLineageRejection::MissingValidHorizon
            | SummaryLineageRejection::StaleSource { .. }
            | SummaryLineageRejection::MissingSource { .. }
            | SummaryLineageRejection::LockedSource { .. }
            | SummaryLineageRejection::UnavailableSource { .. }
            | SummaryLineageRejection::CycleSource { .. }
            | SummaryLineageRejection::SourceBeyondKnowledgeHorizon { .. }
            | SummaryLineageRejection::UnknownSourceValidTime { .. }
            | SummaryLineageRejection::SourceBeyondValidHorizon { .. }
            | SummaryLineageRejection::MissingPredecessor { .. }
            | SummaryLineageRejection::IneligiblePredecessor { .. }
            | SummaryLineageRejection::HorizonRegression { .. }
            | SummaryLineageRejection::Cycle => return SummaryRejectionClass::Unknown,
        }
    }
}

fn public_summary_omissions(eligibility: &SummaryLineageEligibility) -> Vec<SummaryOmission> {
    eligibility
        .omissions
        .iter()
        .filter(|omission| {
            !summary_rejection_class(&omission.rejection, &eligibility.rejections).hides_details()
        })
        .cloned()
        .collect()
}

fn summary_rejection_reason(rejection: &SummaryLineageRejection) -> ContextOmissionReasonV1 {
    match rejection {
        SummaryLineageRejection::UnauthorizedSource { .. }
        | SummaryLineageRejection::SessionMismatch => ContextOmissionReasonV1::Unauthorized,
        SummaryLineageRejection::DeletedSource { .. } => ContextOmissionReasonV1::Deleted,
        SummaryLineageRejection::RedactedSource { .. } => ContextOmissionReasonV1::Redacted,
        SummaryLineageRejection::ExpiredSource { .. } => ContextOmissionReasonV1::RetentionExpired,
        SummaryLineageRejection::CreatedAfterCutoff
        | SummaryLineageRejection::HorizonAfterCutoff
        | SummaryLineageRejection::MissingValidHorizon
        | SummaryLineageRejection::StaleSource { .. }
        | SummaryLineageRejection::MissingSource { .. }
        | SummaryLineageRejection::LockedSource { .. }
        | SummaryLineageRejection::UnavailableSource { .. }
        | SummaryLineageRejection::CycleSource { .. }
        | SummaryLineageRejection::SourceBeyondKnowledgeHorizon { .. }
        | SummaryLineageRejection::UnknownSourceValidTime { .. }
        | SummaryLineageRejection::SourceBeyondValidHorizon { .. }
        | SummaryLineageRejection::MissingPredecessor { .. }
        | SummaryLineageRejection::IneligiblePredecessor { .. }
        | SummaryLineageRejection::HorizonRegression { .. }
        | SummaryLineageRejection::Cycle => ContextOmissionReasonV1::SummaryHorizonMismatch,
    }
}

fn context_lineage_edge(edge: &ResolutionLineageEdge) -> CompactContextLineageEdgeV1 {
    CompactContextLineageEdgeV1 {
        kind: match edge.kind {
            ResolutionLineageEdgeKind::Correction => TemporalAssertionKindV1::Corrects,
            ResolutionLineageEdgeKind::Contradiction => TemporalAssertionKindV1::Contradicts,
            ResolutionLineageEdgeKind::Supersession => TemporalAssertionKindV1::Supersedes,
        },
        subject_anchor_id: edge.subject_anchor_id.clone(),
        object_anchor_id: edge.object_anchor_id.clone(),
        knowledge_at: edge.knowledge_at,
        authority: edge.evidence.authority,
        authorized: edge.evidence.is_authorized(),
        supporting_anchor_ids: edge.evidence.supporting_anchor_ids.clone(),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod scope_tests {
    use std::collections::{BTreeMap, BTreeSet};

    use tracedecay_domain::{
        RetrievalAnchorId, SessionId, SessionSummaryIdV1, SessionSummaryRecordV1,
        SummarySourceHorizonV1, TemporalModeV1, TemporalValidityV1, UtcMicros,
    };

    use super::hydration::HydrationBatch;
    use super::ports::{ExecutionControl, TemporalRetrievalScope};
    use super::resolution::{
        SummaryLineageEligibility, SummaryLineageRejection, SummaryOmission, SummarySourceState,
    };
    use super::{evaluate_summaries_for_scope, public_summary_omissions, temporal_context_frames};

    fn anchor(value: &str) -> RetrievalAnchorId {
        RetrievalAnchorId::new(value).expect("valid anchor")
    }

    fn summary(session: &str, id: &str, source: &str) -> SessionSummaryRecordV1 {
        SessionSummaryRecordV1::new(
            SessionSummaryIdV1::new(id).expect("valid summary id"),
            SessionId::new(session).expect("valid session id"),
            anchor(&format!("summary-{id}")),
            vec![anchor(source)],
            SummarySourceHorizonV1 {
                knowledge_through: UtcMicros(10),
                valid_through: Some(UtcMicros(10)),
            },
            UtcMicros(10),
        )
        .expect("valid summary")
    }

    fn omission(id: &str, anchor_id: &str, rejection: SummaryLineageRejection) -> SummaryOmission {
        SummaryOmission {
            summary_id: SessionSummaryIdV1::new(id).expect("valid summary id"),
            anchor_id: anchor(anchor_id),
            rejection,
        }
    }

    #[test]
    fn root_wide_summary_evaluation_preserves_each_session_lineage() {
        let summaries = [
            summary("session-1", "one", "source-1"),
            summary("session-2", "two", "source-2"),
        ];
        let source_states = BTreeMap::from([
            (
                anchor("source-1"),
                SummarySourceState::Covered {
                    knowledge_at: UtcMicros(10),
                    valid_time: TemporalValidityV1::Known {
                        valid_at: UtcMicros(10),
                    },
                },
            ),
            (
                anchor("source-2"),
                SummarySourceState::Covered {
                    knowledge_at: UtcMicros(10),
                    valid_time: TemporalValidityV1::Known {
                        valid_at: UtcMicros(10),
                    },
                },
            ),
        ]);

        let eligibility = evaluate_summaries_for_scope(
            &summaries,
            &source_states,
            &TemporalRetrievalScope::AllSessionsInAuthorizedRoot,
            TemporalModeV1::Current,
            &ExecutionControl::default(),
        )
        .expect("root-wide summaries");

        assert_eq!(
            eligibility.eligible_anchor_ids,
            [anchor("summary-one"), anchor("summary-two")].into()
        );
        assert!(eligibility.omissions.is_empty());
    }

    #[test]
    fn hidden_summary_rejections_preserve_coverage_without_public_details() {
        let unauthorized = omission(
            "unauthorized",
            "summary-unauthorized",
            SummaryLineageRejection::UnauthorizedSource {
                anchor_id: anchor("source-unauthorized"),
            },
        );
        let mismatch = omission(
            "mismatch",
            "summary-mismatch",
            SummaryLineageRejection::SessionMismatch,
        );
        let eligibility = SummaryLineageEligibility {
            rejections: [
                (
                    unauthorized.summary_id.clone(),
                    unauthorized.rejection.clone(),
                ),
                (mismatch.summary_id.clone(), mismatch.rejection.clone()),
            ]
            .into(),
            omissions: vec![unauthorized, mismatch],
            ..SummaryLineageEligibility::default()
        };
        let candidate_anchors = [anchor("summary-unauthorized"), anchor("summary-mismatch")].into();

        let frames = temporal_context_frames(
            &candidate_anchors,
            &BTreeSet::new(),
            &[],
            &[],
            &HydrationBatch::default(),
            &eligibility,
        );

        assert_eq!(frames.coverage.hidden, 2);
        assert!(frames.omissions.is_empty());
        assert!(frames.summary_omissions.is_empty());
        assert!(public_summary_omissions(&eligibility).is_empty());
        let rendered = format!("{frames:?}");
        assert!(!rendered.contains("summary-unauthorized"));
        assert!(!rendered.contains("summary-mismatch"));
    }

    #[test]
    fn three_level_hidden_predecessor_chain_conceals_all_identifiers() {
        let predecessor_id =
            SessionSummaryIdV1::new("hidden-predecessor").expect("valid summary id");
        let predecessor = SummaryOmission {
            summary_id: predecessor_id.clone(),
            anchor_id: anchor("summary-hidden-predecessor"),
            rejection: SummaryLineageRejection::UnauthorizedSource {
                anchor_id: anchor("source-hidden-predecessor"),
            },
        };
        let first_id = SessionSummaryIdV1::new("first-dependent").expect("valid summary id");
        let first = omission(
            "first-dependent",
            "summary-first-dependent",
            SummaryLineageRejection::IneligiblePredecessor {
                predecessor_summary_id: predecessor_id,
            },
        );
        let second_id = SessionSummaryIdV1::new("second-dependent").expect("valid summary id");
        let second = omission(
            "second-dependent",
            "summary-second-dependent",
            SummaryLineageRejection::IneligiblePredecessor {
                predecessor_summary_id: first_id,
            },
        );
        let third = omission(
            "third-dependent",
            "summary-third-dependent",
            SummaryLineageRejection::IneligiblePredecessor {
                predecessor_summary_id: second_id,
            },
        );
        let eligibility = SummaryLineageEligibility {
            rejections: [
                (
                    predecessor.summary_id.clone(),
                    predecessor.rejection.clone(),
                ),
                (first.summary_id.clone(), first.rejection.clone()),
                (second.summary_id.clone(), second.rejection.clone()),
                (third.summary_id.clone(), third.rejection.clone()),
            ]
            .into(),
            omissions: vec![predecessor, first, second, third],
            ..SummaryLineageEligibility::default()
        };
        let candidate_anchors = [
            anchor("summary-hidden-predecessor"),
            anchor("summary-first-dependent"),
            anchor("summary-second-dependent"),
            anchor("summary-third-dependent"),
        ]
        .into();

        let frames = temporal_context_frames(
            &candidate_anchors,
            &BTreeSet::new(),
            &[],
            &[],
            &HydrationBatch::default(),
            &eligibility,
        );

        assert_eq!(frames.coverage.hidden, 4);
        assert_eq!(
            frames.coverage.visible
                + frames.coverage.hidden
                + frames.coverage.unknown
                + frames.coverage.redacted,
            4
        );
        assert!(frames.omissions.is_empty());
        assert!(frames.summary_omissions.is_empty());
        assert!(public_summary_omissions(&eligibility).is_empty());
        let rendered = format!("{frames:?}");
        for private_id in [
            "hidden-predecessor",
            "first-dependent",
            "second-dependent",
            "third-dependent",
        ] {
            assert!(!rendered.contains(private_id));
        }
    }

    #[test]
    fn hidden_redacted_unknown_and_visible_share_one_exact_denominator() {
        let hidden = omission(
            "hidden",
            "summary-hidden",
            SummaryLineageRejection::UnauthorizedSource {
                anchor_id: anchor("source-hidden"),
            },
        );
        let redacted = omission(
            "redacted",
            "summary-redacted",
            SummaryLineageRejection::RedactedSource {
                anchor_id: anchor("source-redacted"),
            },
        );
        let unknown = omission(
            "unknown",
            "summary-unknown",
            SummaryLineageRejection::MissingSource {
                anchor_id: anchor("source-missing"),
            },
        );
        let eligibility = SummaryLineageEligibility {
            rejections: [
                (hidden.summary_id.clone(), hidden.rejection.clone()),
                (redacted.summary_id.clone(), redacted.rejection.clone()),
                (unknown.summary_id.clone(), unknown.rejection.clone()),
            ]
            .into(),
            omissions: vec![hidden, redacted, unknown],
            ..SummaryLineageEligibility::default()
        };
        let candidate_anchors = [
            anchor("summary-hidden"),
            anchor("summary-redacted"),
            anchor("summary-unknown"),
            anchor("summary-visible"),
        ]
        .into();
        let visible_anchors = [anchor("summary-visible")].into();

        let frames = temporal_context_frames(
            &candidate_anchors,
            &visible_anchors,
            &[],
            &[],
            &HydrationBatch::default(),
            &eligibility,
        );

        assert_eq!(frames.coverage.visible, 1);
        assert_eq!(frames.coverage.hidden, 1);
        assert_eq!(frames.coverage.redacted, 1);
        assert_eq!(frames.coverage.unknown, 1);
        assert_eq!(
            frames.coverage.visible
                + frames.coverage.hidden
                + frames.coverage.unknown
                + frames.coverage.redacted,
            candidate_anchors.len() as u64
        );
        assert_eq!(frames.omissions.len(), 2);
        assert_eq!(frames.summary_omissions.len(), 2);
        let rendered = format!("{frames:?}");
        assert!(!rendered.contains("summary-hidden"));
    }
}

#[cfg(test)]
mod test_support {
    use std::future::Future;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;
    use std::time::Duration;

    struct ThreadWake(thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    pub fn block_on<F: Future>(future: F) -> F::Output {
        let mut future = Box::pin(future);
        let waker = Waker::from(Arc::new(ThreadWake(thread::current())));
        let mut context = Context::from_waker(&waker);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => thread::park_timeout(Duration::from_millis(10)),
            }
        }
    }
}
