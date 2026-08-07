//! Canonical bridge from temporal candidate exports to shared retrieval and
//! authoritative selected-anchor hydration.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use tracedecay_domain::{
    CompactCandidate, ComponentRevision, CursorPayloadDigest, EvidenceRole, FixedPointScore,
    FreshnessCompatibilityV1, LogicalEvidenceId, ManifestDigest, RetrievalAnchorId,
    RetrievalRequest, RetrieverBatch, RetrieverContinuation, RetrieverKind, ScoreDomainId,
    SessionId, SessionOrThreadId, SourceFreshness, SourceInstanceKey, SourceNamespace,
    SourceOccurrenceId, TemporalCandidateChannelV1, TemporalCandidateContributionV1,
    TemporalLaneEvidenceV1, canonical_sha256,
};

use super::context::VersionedTokenEstimator;
use super::context::assembly::assemble_context_with_frames_controlled;
use super::hydration::{TemporalHydrationPort, hydrate_selected};
use super::ports::{TemporalParticipantGeneration, TemporalPortError, TemporalSourceAccess};
use super::{
    TemporalCandidateExport, TemporalHydratedResult, TemporalKernelError, TemporalKernelRequest,
    TemporalKernelResult, check_control, map_context_error, map_hydration_error,
    public_summary_omissions, temporal_context_frames,
};

impl TemporalCandidateExport {
    /// Project this frozen temporal page into the canonical compact retrieval
    /// contract without authorizing or reading any payload bytes.
    ///
    /// Every temporal occurrence remains represented. The first occurrence
    /// carries the already evaluated temporal score and corroborating
    /// occurrences carry zero aggregate score while retaining their original
    /// per-channel contribution evidence, so generic fusion cannot turn
    /// evidence multiplicity into a second ranking boost.
    pub fn to_retriever_batch(
        &self,
        request: &RetrievalRequest,
        retriever_revision: ComponentRevision,
        score_domain: ScoreDomainId,
        policy_revision: ComponentRevision,
    ) -> Result<RetrieverBatch<TemporalLaneEvidenceV1>, TemporalKernelError> {
        let participant_epoch = ManifestDigest::new(
            self.snapshot
                .participant_manifest()
                .epoch_digest()
                .to_owned(),
        )
        .map_err(candidate_export_contract)?;
        let mut candidates = Vec::new();
        let mut evidence_by_occurrence = BTreeMap::new();
        for ranked in &self.ranked {
            let contributions = ranked
                .contributions
                .iter()
                .map(temporal_contribution)
                .collect::<Result<Vec<_>, _>>()?;
            for (contribution_index, contribution) in ranked.contributions.iter().enumerate() {
                let source_occurrence =
                    SourceOccurrenceId::try_from(contribution.retriever_record_id.clone())
                        .map_err(candidate_export_contract)?;
                let session_id = ranked
                    .session
                    .as_deref()
                    .ok_or_else(|| {
                        TemporalKernelError::CandidateExportContract(
                            "temporal candidate omitted its owning session".to_owned(),
                        )
                    })
                    .and_then(|value| {
                        SessionId::new(value.to_owned()).map_err(candidate_export_contract)
                    })?;
                let source_id = contribution
                    .source
                    .as_deref()
                    .or(ranked.source.as_deref())
                    .ok_or_else(|| {
                        TemporalKernelError::CandidateExportContract(
                            "temporal candidate omitted its owning source".to_owned(),
                        )
                    })?;
                let participant = self
                    .snapshot
                    .participant_manifest()
                    .entries()
                    .iter()
                    .find(|entry| {
                        entry.session_id() == &session_id && entry.source_id() == source_id
                    })
                    .ok_or_else(|| {
                        TemporalKernelError::CandidateExportContract(
                            "temporal candidate is outside the frozen participant manifest"
                                .to_owned(),
                        )
                    })?;
                if !participant.is_authorized_for_snapshot() {
                    return Err(TemporalKernelError::CandidateExportContract(
                        "temporal candidate participant is not authorized".to_owned(),
                    ));
                }
                let source_namespace = SourceNamespace::try_from("session".to_owned())
                    .map_err(candidate_export_contract)?;
                let source_instance =
                    SourceInstanceKey::try_from(format!("{}:{source_id}", session_id))
                        .map_err(candidate_export_contract)?;
                // `stable_id` is the canonical temporal evidence identity when
                // the source does not declare a cross-occurrence logical
                // message group.
                let logical_identity = match &ranked.logical_message {
                    Some(logical_message) => logical_message.clone(),
                    None => ranked.stable_id.clone(),
                };
                let logical_evidence_id = LogicalEvidenceId::try_from(logical_identity)
                    .map_err(candidate_export_contract)?;
                let ordinal_rank = u32::try_from(candidates.len())
                    .map_err(|_| TemporalKernelError::BudgetExceeded)?;
                let raw_score = if contribution_index == 0 {
                    ranked.normalized_score_micros
                } else {
                    0
                };
                let freshness = participant_freshness(
                    participant,
                    request,
                    source_namespace.clone(),
                    source_instance,
                    policy_revision.clone(),
                );
                candidates.push(CompactCandidate {
                    anchor_id: ranked.anchor_id.clone(),
                    logical_evidence_id,
                    source_occurrence_id: source_occurrence.clone(),
                    file_occurrence_id: None,
                    source_namespace,
                    repository_id: Some(request.scope.root.repository.clone()),
                    session_or_thread_id: Some(
                        SessionOrThreadId::try_from(session_id.to_string())
                            .map_err(candidate_export_contract)?,
                    ),
                    logical_copy_cluster_id: None,
                    logical_copy_evidence_anchor: None,
                    evidence_role: temporal_evidence_role(ranked.evidence_role.as_deref()),
                    retriever: RetrieverKind::Temporal,
                    retriever_revision: retriever_revision.clone(),
                    score_domain: score_domain.clone(),
                    raw_score: FixedPointScore(raw_score),
                    ordinal_rank,
                    exact_admission_proof: None,
                    retriever_evidence_anchor: ranked.anchor_id.clone(),
                    freshness,
                });
                let prior = evidence_by_occurrence.insert(
                    source_occurrence.clone(),
                    TemporalLaneEvidenceV1 {
                        candidate_anchor: ranked.anchor_id.clone(),
                        source_occurrence,
                        authorization_revision: request.snapshot.authorization_revision.clone(),
                        participant_epoch: participant_epoch.clone(),
                        session_id: session_id.clone(),
                        source_id: source_id.to_owned(),
                        hydration_anchor: ranked.anchor_id.clone(),
                        contributions: contributions.clone(),
                    },
                );
                if prior.is_some() {
                    return Err(TemporalKernelError::CandidateExportContract(
                        "temporal export repeated a source occurrence".to_owned(),
                    ));
                }
            }
        }
        let checkpoint_material = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.anchor_id.to_string(),
                    candidate.source_occurrence_id.to_string(),
                    candidate.raw_score.micros(),
                )
            })
            .collect::<Vec<_>>();
        let checkpoint_digest = canonical_sha256(&(
            "tracedecay.temporal-lane-checkpoint.v1",
            participant_epoch.as_str(),
            request.snapshot.authorization_revision.to_string(),
            self.next_cursor.as_deref(),
            checkpoint_material,
        ))
        .map_err(candidate_export_contract)?;
        let batch = RetrieverBatch {
            candidates,
            evidence_by_occurrence,
            coverage: self.coverage,
            continuation: Some(RetrieverContinuation {
                lane: RetrieverKind::Temporal,
                checkpoint_digest: CursorPayloadDigest::new(checkpoint_digest.as_str())
                    .map_err(candidate_export_contract)?,
                exhausted: self.next_cursor.is_none(),
            }),
        };
        batch.validate().map_err(candidate_export_contract)?;
        Ok(batch)
    }
}

fn candidate_export_contract(error: impl fmt::Display) -> TemporalKernelError {
    TemporalKernelError::CandidateExportContract(error.to_string())
}

fn temporal_contribution(
    contribution: &super::ranking::RetrieverContribution,
) -> Result<TemporalCandidateContributionV1, TemporalKernelError> {
    Ok(TemporalCandidateContributionV1 {
        channel: temporal_channel(contribution.channel),
        source_occurrence: SourceOccurrenceId::try_from(contribution.retriever_record_id.clone())
            .map_err(candidate_export_contract)?,
        source_id: contribution.source.clone(),
        retriever_ordinal: contribution.retriever_ordinal,
        raw_score: contribution.raw_score,
        calibrated_score_micros: contribution.calibrated_score_micros,
        exact_ranges: contribution.exact_ranges.clone(),
    })
}

const fn temporal_channel(
    channel: super::candidates::CandidateChannel,
) -> TemporalCandidateChannelV1 {
    match channel {
        super::candidates::CandidateChannel::Scope => TemporalCandidateChannelV1::Scope,
        super::candidates::CandidateChannel::Anchor => TemporalCandidateChannelV1::Anchor,
        super::candidates::CandidateChannel::ExactMessage => {
            TemporalCandidateChannelV1::ExactMessage
        }
        super::candidates::CandidateChannel::Phrase => TemporalCandidateChannelV1::Phrase,
        super::candidates::CandidateChannel::Entity => TemporalCandidateChannelV1::Entity,
        super::candidates::CandidateChannel::Time => TemporalCandidateChannelV1::Time,
        super::candidates::CandidateChannel::Lexical => TemporalCandidateChannelV1::Lexical,
        super::candidates::CandidateChannel::Summary => TemporalCandidateChannelV1::Summary,
        super::candidates::CandidateChannel::Span => TemporalCandidateChannelV1::Span,
        super::candidates::CandidateChannel::Burst => TemporalCandidateChannelV1::Burst,
    }
}

fn temporal_evidence_role(role: Option<&str>) -> EvidenceRole {
    match role {
        Some("corroboration") => EvidenceRole::Corroboration,
        Some("contradiction") => EvidenceRole::Contradiction,
        Some("context" | "summary") => EvidenceRole::Context,
        Some(_) | None => EvidenceRole::Primary,
    }
}

fn participant_freshness(
    participant: &TemporalParticipantGeneration,
    request: &RetrievalRequest,
    source_namespace: SourceNamespace,
    source_instance: SourceInstanceKey,
    policy_revision: ComponentRevision,
) -> SourceFreshness {
    let watermarks = participant.watermarks();
    SourceFreshness {
        source_namespace,
        source_instance,
        source_watermark: Some(watermarks.source),
        projection_watermark: Some(watermarks.projection),
        observed_at: request.snapshot.captured_at,
        source_generation: Some(participant.generation()),
        generation_lag: Some(watermarks.source.saturating_sub(watermarks.projection)),
        compatibility: match participant.access() {
            TemporalSourceAccess::Available => FreshnessCompatibilityV1::Current,
            TemporalSourceAccess::Unavailable
            | TemporalSourceAccess::Locked
            | TemporalSourceAccess::RetentionWithheld
            | TemporalSourceAccess::Deleted
            | TemporalSourceAccess::Redacted
            | TemporalSourceAccess::LegacyUnauthorized => FreshnessCompatibilityV1::Missing,
        },
        policy_revision,
    }
}

/// Hydrate only the globally selected temporal anchors, in the supplied
/// selected order, through the canonical temporal content authority.
pub async fn hydrate_temporal_candidate_selection(
    request: &TemporalKernelRequest,
    mut export: TemporalCandidateExport,
    selected_anchors: &[RetrievalAnchorId],
    hydration_port: &impl TemporalHydrationPort,
    token_estimator: &impl VersionedTokenEstimator,
) -> Result<TemporalKernelResult, TemporalKernelError> {
    if selected_anchors.len() > request.snapshot.request().limits().hydration_limit {
        return Err(TemporalKernelError::BudgetExceeded);
    }
    let mut ranked_by_anchor = export
        .ranked
        .iter()
        .cloned()
        .map(|candidate| (candidate.anchor_id.clone(), candidate))
        .collect::<BTreeMap<_, _>>();
    if ranked_by_anchor.len() != export.ranked.len() {
        return Err(TemporalKernelError::CandidateExportContract(
            "temporal export repeated a hydration anchor".to_owned(),
        ));
    }
    let mut selected = Vec::with_capacity(selected_anchors.len());
    let mut unique = BTreeSet::new();
    for anchor in selected_anchors {
        if !unique.insert(anchor.clone()) {
            return Err(TemporalKernelError::CandidateExportContract(
                "temporal hydration selection repeated an anchor".to_owned(),
            ));
        }
        let candidate = ranked_by_anchor.remove(anchor).ok_or_else(|| {
            TemporalKernelError::CandidateExportContract(
                "temporal hydration selection is outside the frozen export".to_owned(),
            )
        })?;
        selected.push(candidate);
    }
    export.ranked = selected;
    hydrate_temporal_candidate_export(request, export, hydration_port, token_estimator).await
}

/// Hydrate the entire temporal page without changing its lane-local selection.
pub async fn hydrate_temporal_candidate_export(
    request: &TemporalKernelRequest,
    export: TemporalCandidateExport,
    hydration_port: &impl TemporalHydrationPort,
    token_estimator: &impl VersionedTokenEstimator,
) -> Result<TemporalKernelResult, TemporalKernelError> {
    if export.snapshot != request.snapshot {
        return Err(TemporalKernelError::Port(
            TemporalPortError::InvalidBinding {
                field: "temporal candidate export snapshot",
            },
        ));
    }
    let TemporalCandidateExport {
        snapshot,
        ranked,
        next_cursor,
        coverage: _,
        all_candidate_anchors,
        visible_anchors,
        resolution,
        summaries,
        summary_eligibility,
    } = export;
    check_control(&snapshot)?;
    let ranked_anchors = ranked
        .iter()
        .map(|candidate| candidate.anchor_id.clone())
        .collect::<BTreeSet<_>>();
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
        &resolution,
        &resolution.lineage_edges,
        &hydration,
        &summaries,
        &ranked_anchors,
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
    let summary_omissions = public_summary_omissions(&summary_eligibility);
    let hydrated = TemporalHydratedResult::from_batch(hydration, &ranked);
    Ok(TemporalKernelResult {
        coverage: context.bundle.coverage,
        conflicts: context.bundle.conflicts.clone(),
        lineage: context.bundle.lineage.clone(),
        snapshot,
        ranked,
        hydrated,
        context,
        summary_omissions,
        next_cursor,
    })
}
