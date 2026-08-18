use std::collections::HashMap;
use std::sync::Arc;

use tracedecay_domain::{MessageOccurrenceIdV1, SessionId};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_temporal_query::candidates::CandidateChannel;
use tracedecay_temporal_query::ports::{
    ExecutionControl, PageRequest, TemporalExecutionSnapshot, TemporalPortError,
    TemporalRetrievalScope,
};
use tracedecay_temporal_query::ranking::RankingCandidate;

use super::super::super::relations::{
    SessionRelationError, SessionRelationGraphStore, SessionRelationScope, SummarySourceRef,
    SummarySourceVisitKind,
};
use super::super::{MAX_SUMMARY_SOURCES_PER_RECORD, RECORD_OPERATION};
use super::{read_error, read_message};

#[derive(Clone, Debug)]
pub(in crate::session_temporal::retrieval) struct RecordRelationBatch {
    pub(super) copies: Vec<RecordCopyRelation>,
    pub(super) summaries: Vec<RecordSummaryRelation>,
    pub(super) summary_sources: Vec<RecordSummarySourceRelation>,
    pub(super) retained_summary_anchors: Vec<RecordRetainedSummaryAnchor>,
}

#[cfg(test)]
impl RecordRelationBatch {
    pub(in crate::session_temporal::retrieval) fn empty() -> Self {
        Self {
            copies: Vec::new(),
            summaries: Vec::new(),
            summary_sources: Vec::new(),
            retained_summary_anchors: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct RecordCopyRelation {
    pub(super) candidate: usize,
    pub(super) session_id: SessionId,
    pub(super) occurrence_id: MessageOccurrenceIdV1,
    pub(super) copied_from_occurrence_id: MessageOccurrenceIdV1,
    pub(super) proof_json: String,
    pub(super) knowledge_at: i64,
    pub(super) valid_time_json: String,
}

#[derive(Clone, Debug)]
pub(super) struct RecordSummaryRelation {
    pub(super) candidate: usize,
    pub(super) session_id: SessionId,
    pub(super) summary_id: String,
    pub(super) predecessor_summary_id: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct RecordSummarySourceRelation {
    pub(super) candidate: usize,
    pub(super) session_id: SessionId,
    pub(super) summary_id: String,
    pub(super) ordinal: u32,
    pub(super) source_anchor_id: Option<String>,
    pub(super) source_summary_id: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct RecordRetainedSummaryAnchor {
    pub(super) candidate: usize,
    pub(super) session_id: SessionId,
    pub(super) summary_id: String,
    pub(super) anchor_id: String,
}

#[derive(Clone, Debug)]
struct TemporalGraphCancellation(ExecutionControl);

impl GraphCancellation for TemporalGraphCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.checkpoint().is_err()
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::session_temporal::retrieval) fn load_record_relations(
    store: &SessionRelationGraphStore,
    relation_scope: &SessionRelationScope,
    scope: &TemporalRetrievalScope,
    snapshot: &TemporalExecutionSnapshot,
    candidates: &[RankingCandidate],
    candidate_offset: usize,
    request: &PageRequest,
) -> Result<RecordRelationBatch, TemporalPortError> {
    let control = snapshot.request().execution_control();
    control.checkpoint()?;
    let relation_limit = request.page_item_limit().saturating_add(1);
    if relation_limit == 0 {
        return Err(TemporalPortError::BudgetExceeded {
            resource: "record relations",
        });
    }
    let cancellation: Arc<dyn GraphCancellation> =
        Arc::new(TemporalGraphCancellation(control.clone()));
    let mut copies = Vec::new();
    let mut summaries = Vec::new();
    let mut summary_sources = Vec::new();
    let mut retained_summary_anchors = Vec::new();
    let mut relation_bytes = 0usize;
    // One lookup table for the whole page: the frozen participant manifest is
    // scanned once instead of linearly per candidate.
    let participant_generations = snapshot.has_authoritative_participant_manifest().then(|| {
        snapshot
            .participant_manifest()
            .entries()
            .iter()
            .map(|participant| {
                (
                    (participant.session_id().as_str(), participant.source_id()),
                    (participant.generation(), participant.graph_watermark()),
                )
            })
            .collect::<HashMap<_, _>>()
    });
    for (local, candidate) in candidates.iter().enumerate() {
        control.checkpoint()?;
        let session_id = candidate_session_id(scope, candidate)?;
        let generation = candidate_generation(
            snapshot,
            participant_generations.as_ref(),
            candidate,
            &session_id,
        )?;
        if names_summary_record(candidate) {
            let candidate_index = candidate_offset.saturating_add(local);
            let summary_id = candidate.retriever_record_id.clone();
            let reads = store
                .summary_relations(
                    relation_scope,
                    &session_id,
                    generation,
                    std::slice::from_ref(&summary_id),
                    MAX_SUMMARY_SOURCES_PER_RECORD.saturating_add(2),
                    Arc::clone(&cancellation),
                )
                .map_err(|error| map_relation_error(error, control))?;
            let read = reads
                .into_iter()
                .next()
                .filter(|read| read.summary_id == summary_id)
                .ok_or_else(|| read_message(RECORD_OPERATION, "summary relation is missing"))?;
            for (ordinal, source) in read.sources.into_iter().enumerate() {
                let ordinal =
                    u32::try_from(ordinal).map_err(|error| read_error(RECORD_OPERATION, error))?;
                let (source_anchor_id, source_summary_id) = match source {
                    SummarySourceRef::Anchor { anchor_id } => (Some(anchor_id.to_string()), None),
                    SummarySourceRef::Summary { summary_id } => (None, Some(summary_id)),
                };
                let source_bytes = source_anchor_id
                    .as_deref()
                    .or(source_summary_id.as_deref())
                    .map_or(0, str::len);
                if source_bytes > request.max_item_bytes() {
                    return Err(TemporalPortError::BudgetExceeded {
                        resource: "summary source bytes",
                    });
                }
                relation_bytes = relation_bytes.saturating_add(source_bytes);
                summary_sources.push(RecordSummarySourceRelation {
                    candidate: candidate_index,
                    session_id: session_id.clone(),
                    summary_id: summary_id.clone(),
                    ordinal,
                    source_anchor_id,
                    source_summary_id,
                });
            }
            let visits = store
                .summary_sources(
                    relation_scope,
                    &session_id,
                    generation,
                    &summary_id,
                    MAX_SUMMARY_SOURCES_PER_RECORD.saturating_add(1),
                    Arc::clone(&cancellation),
                )
                .map_err(|error| map_relation_error(error, control))?;
            for visit in visits {
                if let SummarySourceVisitKind::Anchor { anchor_id } = visit.source {
                    relation_bytes = relation_bytes.saturating_add(anchor_id.as_str().len());
                    retained_summary_anchors.push(RecordRetainedSummaryAnchor {
                        candidate: candidate_index,
                        session_id: session_id.clone(),
                        summary_id: summary_id.clone(),
                        anchor_id: anchor_id.to_string(),
                    });
                }
            }
            relation_bytes = relation_bytes
                .saturating_add(summary_id.len())
                .saturating_add(read.predecessor_summary_id.as_deref().map_or(0, str::len));
            summaries.push(RecordSummaryRelation {
                candidate: candidate_index,
                session_id,
                summary_id,
                predecessor_summary_id: read.predecessor_summary_id,
            });
            continue;
        }
        if matches!(
            candidate.channel,
            CandidateChannel::Span | CandidateChannel::Burst
        ) {
            continue;
        }
        let occurrence_id = MessageOccurrenceIdV1::new(&candidate.retriever_record_id)
            .map_err(|error| read_error(RECORD_OPERATION, error))?;
        let remaining = relation_limit.saturating_sub(copies.len());
        if remaining == 0 {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "record relations",
            });
        }
        let batches = store
            .logical_copies(
                relation_scope,
                &session_id,
                generation,
                std::slice::from_ref(&occurrence_id),
                remaining,
                Arc::clone(&cancellation),
            )
            .map_err(|error| map_relation_error(error, control))?;
        let relations = batches
            .into_iter()
            .next()
            .ok_or_else(|| read_message(RECORD_OPERATION, "logical-copy batch is missing"))?;
        for relation in relations {
            if copies.len() == relation_limit {
                return Err(TemporalPortError::BudgetExceeded {
                    resource: "record relations",
                });
            }
            let proof_json = serde_json::to_string(&relation.proof)
                .map_err(|error| read_error(RECORD_OPERATION, error))?;
            let valid_time_json = serde_json::to_string(&relation.valid_time)
                .map_err(|error| read_error(RECORD_OPERATION, error))?;
            let copy_bytes = relation
                .occurrence_id
                .as_str()
                .len()
                .saturating_add(relation.copied_from_occurrence_id.as_str().len())
                .saturating_add(proof_json.len())
                .saturating_add(valid_time_json.len());
            if copy_bytes > request.max_item_bytes() {
                return Err(TemporalPortError::BudgetExceeded {
                    resource: "record relation bytes",
                });
            }
            relation_bytes = relation_bytes.saturating_add(copy_bytes);
            copies.push(RecordCopyRelation {
                candidate: candidate_offset.saturating_add(local),
                session_id: session_id.clone(),
                occurrence_id: relation.occurrence_id,
                copied_from_occurrence_id: relation.copied_from_occurrence_id,
                proof_json,
                knowledge_at: relation.knowledge_at.0,
                valid_time_json,
            });
        }
    }
    if relation_bytes > request.page_total_byte_limit() {
        return Err(TemporalPortError::BudgetExceeded {
            resource: "record relation batch bytes",
        });
    }
    control.checkpoint()?;
    Ok(RecordRelationBatch {
        copies,
        summaries,
        summary_sources,
        retained_summary_anchors,
    })
}

/// The evidence role every candidate query reports for a summary node row.
const SUMMARY_EVIDENCE_ROLE: &str = "summary";

/// Reports whether a candidate names a summary node rather than an occurrence.
///
/// The summary channel's listing and full-text matches are summaries by
/// construction, and an anchor lookup resolves either an occurrence or a
/// summary node — a summary describe hydrates over its summary anchor and lands
/// on the anchor channel. Both carry the summary identity in
/// `retriever_record_id` and the summary evidence role, and neither has an
/// occurrence identity whose logical copies could be loaded.
fn names_summary_record(candidate: &RankingCandidate) -> bool {
    match candidate.channel {
        CandidateChannel::Summary => true,
        CandidateChannel::Anchor => {
            candidate.evidence_role.as_deref() == Some(SUMMARY_EVIDENCE_ROLE)
        }
        CandidateChannel::Scope
        | CandidateChannel::ExactMessage
        | CandidateChannel::Phrase
        | CandidateChannel::Entity
        | CandidateChannel::Time
        | CandidateChannel::Lexical
        | CandidateChannel::Span
        | CandidateChannel::Burst => false,
    }
}

fn candidate_session_id(
    scope: &TemporalRetrievalScope,
    candidate: &RankingCandidate,
) -> Result<SessionId, TemporalPortError> {
    match scope {
        TemporalRetrievalScope::Session(session_id) => Ok(session_id.clone()),
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => candidate
            .session
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| read_message(RECORD_OPERATION, "candidate session is missing"))
            .and_then(|value| {
                SessionId::new(value).map_err(|error| read_error(RECORD_OPERATION, error))
            }),
    }
}

/// Frozen participant manifest lookup: `(session_id, source_id)` to that
/// participant's `(generation, graph_watermark)`.
type ParticipantGenerations<'a> = HashMap<(&'a str, &'a str), (u64, u64)>;

fn candidate_generation(
    snapshot: &TemporalExecutionSnapshot,
    participants: Option<&ParticipantGenerations<'_>>,
    candidate: &RankingCandidate,
    session_id: &SessionId,
) -> Result<u64, TemporalPortError> {
    let Some(participants) = participants else {
        return Ok(snapshot.watermarks().generation);
    };
    let source = candidate
        .source
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| read_message(RECORD_OPERATION, "candidate provider is missing"))?;
    let (generation, graph_watermark) = participants
        .get(&(session_id.as_str(), source))
        .copied()
        .ok_or_else(|| {
        read_message(
            RECORD_OPERATION,
            "candidate is absent from the frozen participant manifest",
        )
    })?;
    if graph_watermark != generation {
        return Err(read_message(
            RECORD_OPERATION,
            "candidate graph watermark is stale for the frozen projection",
        ));
    }
    Ok(generation)
}

fn map_relation_error(
    error: SessionRelationError,
    control: &ExecutionControl,
) -> TemporalPortError {
    if let Err(control_error) = control.checkpoint() {
        return control_error;
    }
    match error {
        SessionRelationError::BudgetExhausted => TemporalPortError::BudgetExceeded {
            resource: "record relations",
        },
        SessionRelationError::Cancelled => TemporalPortError::Cancelled,
        SessionRelationError::DeadlineExceeded => TemporalPortError::DeadlineExceeded,
        SessionRelationError::ResetRequired => TemporalPortError::ResetRequired {
            resource: "session relation projection",
        },
        SessionRelationError::Invalid
        | SessionRelationError::Cycle
        | SessionRelationError::NotFound
        | SessionRelationError::Unavailable
        | SessionRelationError::Conflict
        | SessionRelationError::DurabilityUncertain
        | SessionRelationError::Corrupt
        | SessionRelationError::Storage(_) => read_error(RECORD_OPERATION, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_relation_reset_remains_typed() {
        assert_eq!(
            map_relation_error(
                SessionRelationError::ResetRequired,
                &ExecutionControl::default(),
            ),
            TemporalPortError::ResetRequired {
                resource: "session relation projection",
            }
        );
    }
}
