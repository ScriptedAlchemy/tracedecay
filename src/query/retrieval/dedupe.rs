//! Source-aware dedupe stage contracts (Plan 15 pipeline steps 4 and 8:
//! duplicate rows from one immutable source occurrence are collapsed before
//! fusion; cross-source copies collapse only through an evidence-backed
//! logical-copy relation; independent corroboration and contradictions are
//! preserved).
//!
//! Dedupe never collapses merely by content hash, title, timestamp, or
//! embedding similarity.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tracedecay_domain::{
    CompactCandidate, EvidenceRole, FusedCandidate, LogicalCopyClusterId, OccurrenceProvenance,
    RankingDecision, RankingDecisionKind, SourceOccurrenceId,
};

use super::fusion::compare_fused;

/// Failures of the dedupe stage.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DedupeStageError {
    #[error("duplicate candidate rows for one source occurrence lack a collapse decision")]
    UncollapsedDuplicate,
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

/// The same-source duplicate collapse contract (Plan 15 pipeline step 4).
pub trait SameSourceDedupeStage {
    /// Collapse duplicate rows for the same source occurrence before fusion,
    /// recording one decision per collapse.
    fn collapse_same_source(
        &self,
        candidates: &[OccurrenceProvenance],
    ) -> Result<Vec<DedupeDecisionV1>, DedupeStageError>;
}

/// The evidence-backed logical-copy collapse contract (Plan 15 pipeline
/// step 8): resolve copy clusters, preserve independent corroboration and
/// every admitted contradiction, then choose representatives.
pub trait LogicalCopyCollapseStage {
    /// Choose cluster representatives over fused candidates, recording one
    /// representative-selection decision per cluster.
    fn select_representatives(
        &self,
        candidates: Vec<FusedCandidate>,
    ) -> Result<Vec<FusedCandidate>, DedupeStageError>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeterministicDedupe;

impl DeterministicDedupe {
    /// Identify byte-identical source occurrence/evidence pairs before
    /// fusion. Their occurrence row collapses in the fused candidate while
    /// every retriever contribution remains attributable.
    pub fn collapse_compact_candidates(
        &self,
        mut candidates: Vec<CompactCandidate>,
    ) -> Result<(Vec<CompactCandidate>, Vec<DedupeDecisionV1>), DedupeStageError> {
        candidates.sort_by(|left, right| {
            left.source_occurrence_id
                .cmp(&right.source_occurrence_id)
                .then_with(|| {
                    left.retriever_evidence_anchor
                        .cmp(&right.retriever_evidence_anchor)
                })
                .then_with(|| left.anchor_id.cmp(&right.anchor_id))
                .then_with(|| left.logical_evidence_id.cmp(&right.logical_evidence_id))
                .then_with(|| left.retriever.cmp(&right.retriever))
        });
        let mut decisions = Vec::new();
        let mut index = 0;
        while index < candidates.len() {
            let first = &candidates[index];
            let mut end = index + 1;
            while end < candidates.len()
                && candidates[end].source_occurrence_id == first.source_occurrence_id
                && candidates[end].retriever_evidence_anchor == first.retriever_evidence_anchor
                && candidates[end].anchor_id == first.anchor_id
                && candidates[end].logical_evidence_id == first.logical_evidence_id
            {
                end += 1;
            }
            let selected = &candidates[index];
            if end - index > 1 {
                let decision = RankingDecision {
                    kind: RankingDecisionKind::SameSourceDuplicateCollapse,
                    retriever: None,
                    policy_anchor: None,
                    evidence_anchor: Some(selected.retriever_evidence_anchor.clone()),
                    detail: format!(
                        "collapsed {} duplicate occurrence rows for {}; retained every retriever contribution",
                        end - index - 1,
                        selected.source_occurrence_id
                    ),
                };
                decisions.push(DedupeDecisionV1 {
                    kept_occurrence: selected.source_occurrence_id.clone(),
                    collapsed_occurrences: candidates[index + 1..end]
                        .iter()
                        .map(|candidate| candidate.source_occurrence_id.clone())
                        .collect(),
                    collapsed_candidates: Vec::new(),
                    copy_cluster: selected.logical_copy_cluster_id.clone(),
                    decision,
                });
            }
            index = end;
        }
        Ok((candidates, decisions))
    }

    pub fn select_representatives_with_decisions(
        &self,
        mut candidates: Vec<FusedCandidate>,
    ) -> Result<(Vec<FusedCandidate>, Vec<DedupeDecisionV1>), DedupeStageError> {
        candidates.sort_by(compare_fused);
        let mut independent = Vec::new();
        let mut clusters = BTreeMap::<LogicalCopyClusterId, Vec<FusedCandidate>>::new();

        for mut candidate in candidates {
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
            let contradiction = candidate
                .occurrences
                .iter()
                .any(|occurrence| occurrence.evidence_role == EvidenceRole::Contradiction);
            if contradiction {
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
                independent.push(candidate);
            } else if candidate
                .occurrences
                .iter()
                .any(|occurrence| occurrence.evidence_role == EvidenceRole::Corroboration)
            {
                // Corroboration is independent evidence, never a redundant
                // copy selected away merely because it shares a copy cluster.
                independent.push(candidate);
            } else if let Some(cluster) = cluster_ids.into_iter().next() {
                if candidate.occurrences.is_empty() {
                    return Err(DedupeStageError::CopyRelationWithoutEvidence);
                }
                clusters.entry(cluster).or_default().push(candidate);
            } else {
                independent.push(candidate);
            }
        }

        let mut decisions = Vec::new();
        for (cluster, mut copies) in clusters {
            copies.sort_by(compare_fused);
            let mut representative = copies.remove(0);
            if !copies.is_empty() {
                let evidence_anchor = representative
                    .occurrences
                    .iter()
                    .find_map(|occurrence| occurrence.logical_copy_evidence_anchor.clone())
                    .ok_or(DedupeStageError::CopyRelationWithoutEvidence)?;
                let decision = RankingDecision {
                    kind: RankingDecisionKind::LogicalCopyRepresentativeSelection,
                    retriever: None,
                    policy_anchor: None,
                    evidence_anchor: Some(evidence_anchor),
                    detail: format!(
                        "selected {} from logical-copy cluster {}; collapsed [{}]",
                        representative.anchor_id,
                        cluster,
                        copies
                            .iter()
                            .map(|candidate| candidate.anchor_id.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                };
                representative.decisions.push(decision.clone());
                decisions.push(DedupeDecisionV1 {
                    kept_occurrence: representative.occurrences[0].source_occurrence_id.clone(),
                    collapsed_occurrences: copies
                        .iter()
                        .flat_map(|candidate| {
                            candidate
                                .occurrences
                                .iter()
                                .map(|occurrence| occurrence.source_occurrence_id.clone())
                        })
                        .collect(),
                    collapsed_candidates: copies.clone(),
                    copy_cluster: Some(cluster),
                    decision,
                });
            }
            independent.push(representative);
        }
        independent.sort_by(compare_fused);
        Ok((independent, decisions))
    }
}

impl SameSourceDedupeStage for DeterministicDedupe {
    fn collapse_same_source(
        &self,
        candidates: &[OccurrenceProvenance],
    ) -> Result<Vec<DedupeDecisionV1>, DedupeStageError> {
        let mut grouped = BTreeMap::new();
        for candidate in candidates {
            grouped
                .entry((
                    candidate.source_occurrence_id.clone(),
                    candidate.retriever_evidence_anchor.clone(),
                ))
                .or_insert_with(Vec::new)
                .push(candidate);
        }
        Ok(grouped
            .into_iter()
            .filter_map(|((occurrence, evidence_anchor), duplicates)| {
                (duplicates.len() > 1).then(|| {
                    let decision = RankingDecision {
                        kind: RankingDecisionKind::SameSourceDuplicateCollapse,
                        retriever: None,
                        policy_anchor: None,
                        evidence_anchor: Some(evidence_anchor),
                        detail: format!(
                            "collapsed {} duplicate provenance rows",
                            duplicates.len() - 1
                        ),
                    };
                    DedupeDecisionV1 {
                        kept_occurrence: occurrence.clone(),
                        collapsed_occurrences: vec![occurrence; duplicates.len() - 1],
                        collapsed_candidates: Vec::new(),
                        copy_cluster: duplicates[0].logical_copy_cluster_id.clone(),
                        decision,
                    }
                })
            })
            .collect())
    }
}

impl LogicalCopyCollapseStage for DeterministicDedupe {
    fn select_representatives(
        &self,
        candidates: Vec<FusedCandidate>,
    ) -> Result<Vec<FusedCandidate>, DedupeStageError> {
        self.select_representatives_with_decisions(candidates)
            .map(|(candidates, _)| candidates)
    }
}
