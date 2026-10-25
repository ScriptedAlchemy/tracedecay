//! Source-aware dedupe stage contracts (Plan 15 pipeline steps 4 and 8:
//! duplicate rows from one immutable source occurrence are accounted for by
//! fusion; cross-source copies collapse only through an evidence-backed
//! logical-copy relation; independent corroboration and contradictions are
//! preserved).
//!
//! Dedupe never collapses merely by content hash, title, timestamp, or
//! embedding similarity.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tracedecay_domain::{
    EvidenceRole, FusedCandidate, LogicalCopyClusterId, RankingDecision, RankingDecisionKind,
    SourceOccurrenceId,
};

use super::ordering::{OrderedFusedCandidates, decision_cmp};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DedupeStageError {
    #[error("a logical-copy relation lacks its evidence anchor")]
    CopyRelationWithoutEvidence,
    #[error("contract violation: {0}")]
    Contract(String),
}

/// One recorded dedupe decision (Plan 15: `RankingDecision` records
/// same-source duplicate collapse and logical-copy representative
/// selection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DedupeDecisionV1 {
    pub kept_occurrence: SourceOccurrenceId,
    pub collapsed_occurrences: Vec<SourceOccurrenceId>,
    /// Full compact provenance of candidates excluded by a logical-copy
    /// representative choice. It stays associated per candidate instead of
    /// being reconstructed from parallel occurrence/contribution vectors.
    pub collapsed_candidates: Vec<FusedCandidate>,
    pub copy_cluster: Option<LogicalCopyClusterId>,
    pub decision: RankingDecision,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeterministicDedupe;

impl DeterministicDedupe {
    /// Select one representative per evidence-backed logical-copy cluster.
    ///
    /// The input is already in fused order, so a cluster's representative is
    /// simply its first member and the survivors keep that order without
    /// another sort. Collapsed copies move into the decision that excluded
    /// them, keeping the comparator provenance they already carry.
    #[tracing::instrument(name = "query.dedupe.select", level = "trace", skip_all)]
    pub(super) fn select_representatives_with_decisions(
        &self,
        candidates: OrderedFusedCandidates,
    ) -> Result<(Vec<FusedCandidate>, Vec<DedupeDecisionV1>), DedupeStageError> {
        let mut candidates = candidates.into_vec();
        // cluster -> (representative index, collapsed copy indices), all in
        // fused order because the input is.
        let mut clusters = BTreeMap::<LogicalCopyClusterId, (usize, Vec<usize>)>::new();
        for (index, candidate) in candidates.iter_mut().enumerate() {
            if let Some(cluster) = copy_cluster_member(candidate)? {
                clusters
                    .entry(cluster)
                    .and_modify(|(_, copies)| copies.push(index))
                    .or_insert((index, Vec::new()));
            }
        }

        let mut decisions = Vec::new();
        // For every collapsed copy, the decision that takes ownership of it.
        let mut collapsed_into = vec![None; candidates.len()];
        for (cluster, (representative, copies)) in clusters {
            if copies.is_empty() {
                continue;
            }
            let collapsed_anchors = copies
                .iter()
                .map(|&copy| candidates[copy].anchor_id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let collapsed_occurrences = copies
                .iter()
                .flat_map(|&copy| {
                    candidates[copy]
                        .occurrences
                        .iter()
                        .map(|occurrence| occurrence.source_occurrence_id.clone())
                })
                .collect();
            let representative = &mut candidates[representative];
            let evidence_anchor = representative
                .occurrences
                .iter()
                .find_map(|occurrence| occurrence.logical_copy_evidence_anchor.clone())
                .ok_or(DedupeStageError::CopyRelationWithoutEvidence)?;
            let kept_occurrence = representative
                .occurrences
                .first()
                .map(|occurrence| occurrence.source_occurrence_id.clone())
                .ok_or(DedupeStageError::CopyRelationWithoutEvidence)?;
            let decision = RankingDecision {
                kind: RankingDecisionKind::LogicalCopyRepresentativeSelection,
                retriever: None,
                policy_anchor: None,
                evidence_anchor: Some(evidence_anchor),
                detail: format!(
                    "selected {} from logical-copy cluster {}; collapsed [{}]",
                    representative.anchor_id, cluster, collapsed_anchors
                ),
            };
            representative.decisions.push(decision.clone());
            representative.decisions.sort_by(decision_cmp);
            for &copy in &copies {
                collapsed_into[copy] = Some(decisions.len());
            }
            decisions.push(DedupeDecisionV1 {
                kept_occurrence,
                collapsed_occurrences,
                collapsed_candidates: Vec::with_capacity(copies.len()),
                copy_cluster: Some(cluster),
                decision,
            });
        }

        let mut survivors = Vec::with_capacity(candidates.len());
        for (candidate, owner) in candidates.into_iter().zip(collapsed_into) {
            match owner {
                Some(decision) => decisions[decision].collapsed_candidates.push(candidate),
                None => survivors.push(candidate),
            }
        }
        Ok((survivors, decisions))
    }
}

/// Validate one candidate's copy relation and classify it for representative
/// selection. Contradictions are recorded in place and, like corroboration,
/// stay independent; the cluster of a plain copy member is returned.
fn copy_cluster_member(
    candidate: &mut FusedCandidate,
) -> Result<Option<LogicalCopyClusterId>, DedupeStageError> {
    if candidate.occurrences.iter().any(|occurrence| {
        occurrence.logical_copy_cluster_id.is_some()
            && occurrence.logical_copy_evidence_anchor.is_none()
    }) {
        return Err(DedupeStageError::CopyRelationWithoutEvidence);
    }
    let cluster_ids = candidate
        .occurrences
        .iter()
        .filter_map(|occurrence| occurrence.logical_copy_cluster_id.clone())
        .collect::<BTreeSet<_>>();
    if cluster_ids.len() > 1 {
        return Err(DedupeStageError::Contract(
            "one fused candidate spans multiple logical-copy clusters".to_owned(),
        ));
    }
    if candidate
        .occurrences
        .iter()
        .any(|occurrence| occurrence.evidence_role == EvidenceRole::Contradiction)
    {
        let evidence_anchor = candidate
            .occurrences
            .first()
            .map(|occurrence| occurrence.retriever_evidence_anchor.clone())
            .ok_or(DedupeStageError::CopyRelationWithoutEvidence)?;
        candidate.decisions.push(RankingDecision {
            kind: RankingDecisionKind::ContradictionPreservation,
            retriever: None,
            policy_anchor: None,
            evidence_anchor: Some(evidence_anchor),
            detail: "preserved admitted contradiction".to_owned(),
        });
        candidate.decisions.sort_by(decision_cmp);
        return Ok(None);
    }
    if candidate
        .occurrences
        .iter()
        .any(|occurrence| occurrence.evidence_role == EvidenceRole::Corroboration)
    {
        // Corroboration is independent evidence, never a redundant copy
        // selected away merely because it shares a copy cluster.
        return Ok(None);
    }
    Ok(cluster_ids.into_iter().next())
}
