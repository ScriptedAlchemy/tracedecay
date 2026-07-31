use std::collections::BTreeMap;
use std::future::Future;
use std::io::{self, Write};
use std::pin::Pin;

use serde::Serialize;
use tracedecay_domain::{LogicalCopyRecordV1, SessionSummaryRecordV1};

use super::{
    BoundedPage, CANDIDATE_READ_BUDGET, CandidateFieldCaps, CandidatePageSink, CandidateReadState,
    PageRequest, PageStatus, RECORD_READ_BUDGET, ReadBudgetResources, ReadState,
    TemporalExecutionSnapshot, TemporalPortError, TemporalRecordPageSink, TemporalRecordReadState,
    TemporalRetrievalScope, await_controlled,
};
use crate::candidates::{CandidateChannel, CandidatePlan};
use crate::ranking::RankingCandidate;
use crate::resolution::summary::SummarySourceState;
use crate::resolution::types::{ResolutionAssertion, ResolutionOccurrence};

const MAX_READ_ITEM_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TemporalRecordBatch {
    pub occurrences: Vec<ResolutionOccurrence>,
    pub copies: Vec<LogicalCopyRecordV1>,
    pub assertions: Vec<ResolutionAssertion>,
    pub summaries: Vec<SessionSummaryRecordV1>,
    pub summary_sources: BTreeMap<tracedecay_domain::RetrievalAnchorId, SummarySourceState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummarySourceRecord {
    pub anchor_id: tracedecay_domain::RetrievalAnchorId,
    pub state: SummarySourceState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemporalRecord {
    Occurrence(ResolutionOccurrence),
    Copy(LogicalCopyRecordV1),
    Assertion(ResolutionAssertion),
    Summary(SessionSummaryRecordV1),
    SummarySource(SummarySourceRecord),
}

pub type PortFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TemporalPortError>> + Send + 'a>>;

pub trait TemporalReadPort: Send + Sync {
    fn produce_candidate_page<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus>;

    fn produce_candidate_page_for_scope<'a>(
        &'a self,
        scope: &'a TemporalRetrievalScope,
        snapshot: &'a TemporalExecutionSnapshot,
        plan: &'a CandidatePlan,
        request: PageRequest,
        sink: &'a mut CandidatePageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        match scope {
            TemporalRetrievalScope::Session(_) => {
                self.produce_candidate_page(snapshot, plan, request, sink)
            }
            TemporalRetrievalScope::AllSessionsInAuthorizedRoot => Box::pin(async {
                Err(TemporalPortError::Read {
                    operation: "produce candidate page for scope",
                    message:
                        "root-wide retrieval requires an explicit scope-aware port implementation"
                            .to_string(),
                })
            }),
        }
    }

    fn produce_temporal_record_page<'a>(
        &'a self,
        snapshot: &'a TemporalExecutionSnapshot,
        candidates: &'a [RankingCandidate],
        request: PageRequest,
        sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus>;

    fn produce_temporal_record_page_for_scope<'a>(
        &'a self,
        scope: &'a TemporalRetrievalScope,
        snapshot: &'a TemporalExecutionSnapshot,
        candidates: &'a [RankingCandidate],
        request: PageRequest,
        sink: &'a mut TemporalRecordPageSink<'_>,
    ) -> PortFuture<'a, PageStatus> {
        match scope {
            TemporalRetrievalScope::Session(_) => {
                self.produce_temporal_record_page(snapshot, candidates, request, sink)
            }
            TemporalRetrievalScope::AllSessionsInAuthorizedRoot => Box::pin(async {
                Err(TemporalPortError::Read {
                    operation: "produce temporal record page for scope",
                    message:
                        "root-wide retrieval requires an explicit scope-aware port implementation"
                            .to_string(),
                })
            }),
        }
    }
}

pub async fn pull_candidate_page(
    port: &impl TemporalReadPort,
    snapshot: &TemporalExecutionSnapshot,
    plan: &CandidatePlan,
    state: &mut CandidateReadState,
) -> Result<BoundedPage<RankingCandidate>, TemporalPortError> {
    snapshot.request().execution_control().checkpoint()?;
    let limits = snapshot.request().limits().validate()?;
    state.require_within_limits(
        limits.candidate_limit,
        limits.candidate_total_bytes,
        limits.candidate_item_bytes,
        CANDIDATE_READ_BUDGET,
    )?;
    if state.is_exhausted() {
        // Caps exhausted with unread producer work must not synthesize Complete.
        return Err(state.incomplete_coverage_error(CANDIDATE_READ_BUDGET));
    }
    let control = snapshot.request().execution_control();
    let field_caps = CandidateFieldCaps::new(
        limits.candidate_stable_id_bytes,
        limits.candidate_anchor_id_bytes,
        limits.candidate_metadata_field_bytes,
    );
    let request = state.request(limits.candidate_key_bytes, Some(field_caps));
    let mut sink = state.begin_page(
        control,
        limits.candidate_key_bytes,
        Some(field_caps),
        CANDIDATE_READ_BUDGET,
    );
    let status = await_controlled(
        control,
        port.produce_candidate_page_for_scope(
            snapshot.request().retrieval_scope(),
            snapshot,
            plan,
            request,
            &mut sink,
        ),
    )
    .await?;
    let page = sink.finish(status)?;
    commit_pulled_page(state, page, CANDIDATE_READ_BUDGET)
}

pub async fn pull_temporal_record_page(
    port: &impl TemporalReadPort,
    snapshot: &TemporalExecutionSnapshot,
    candidates: &[RankingCandidate],
    state: &mut TemporalRecordReadState,
) -> Result<BoundedPage<TemporalRecord>, TemporalPortError> {
    snapshot.request().execution_control().checkpoint()?;
    let limits = snapshot.request().limits().validate()?;
    state.require_within_limits(
        limits.record_limit,
        limits.record_total_bytes,
        limits.record_item_bytes,
        RECORD_READ_BUDGET,
    )?;
    if state.is_exhausted() {
        // Caps exhausted with unread producer work must not synthesize Complete.
        return Err(state.incomplete_coverage_error(RECORD_READ_BUDGET));
    }
    let control = snapshot.request().execution_control();
    let request = state.request(limits.record_key_bytes, None);
    let mut sink = state.begin_page(control, limits.record_key_bytes, None, RECORD_READ_BUDGET);
    let status = await_controlled(
        control,
        port.produce_temporal_record_page_for_scope(
            snapshot.request().retrieval_scope(),
            snapshot,
            candidates,
            request,
            &mut sink,
        ),
    )
    .await?;
    let page = sink.finish(status)?;
    commit_pulled_page(state, page, RECORD_READ_BUDGET)
}

fn commit_pulled_page<T>(
    state: &mut ReadState<T>,
    page: BoundedPage<T>,
    resources: ReadBudgetResources,
) -> Result<BoundedPage<T>, TemporalPortError> {
    if page.status() == PageStatus::More && state.is_exhausted() {
        // Producer still has pages, but item/total caps already consumed the
        // read budget. Propagate incomplete coverage — never downgrade to Complete.
        return Err(state.incomplete_coverage_error(resources));
    }
    state.advanced_page(page.continuation.clone());
    Ok(page)
}

pub trait MeasuredTemporalValue {
    fn measured_encoded_bytes(&self) -> Result<usize, TemporalPortError>;

    fn validate_candidate_fields(
        &self,
        _caps: Option<CandidateFieldCaps>,
    ) -> Result<(), TemporalPortError> {
        Ok(())
    }
}

#[derive(Serialize)]
struct CandidateWire<'a> {
    stable_id: &'a str,
    anchor_id: &'a tracedecay_domain::RetrievalAnchorId,
    retriever_record_id: &'a str,
    channel: &'static str,
    raw_score: i64,
    knowledge_at_micros: i64,
    logical_message: &'a Option<String>,
    turn: &'a Option<String>,
    session: &'a Option<String>,
    source: &'a Option<String>,
    evidence_role: &'a Option<String>,
    exact_ranges: &'a [tracedecay_domain::ByteRangeV1],
}

impl MeasuredTemporalValue for RankingCandidate {
    fn measured_encoded_bytes(&self) -> Result<usize, TemporalPortError> {
        let channel = match self.channel {
            CandidateChannel::Scope => "scope",
            CandidateChannel::Anchor => "anchor",
            CandidateChannel::ExactMessage => "exact_message",
            CandidateChannel::Phrase => "phrase",
            CandidateChannel::Entity => "entity",
            CandidateChannel::Time => "time",
            CandidateChannel::Lexical => "lexical",
            CandidateChannel::Summary => "summary",
            CandidateChannel::Span => "span",
            CandidateChannel::Burst => "burst",
        };
        measured_json_bytes(
            "encode candidate",
            &CandidateWire {
                stable_id: &self.stable_id,
                anchor_id: &self.anchor_id,
                retriever_record_id: &self.retriever_record_id,
                channel,
                raw_score: self.raw_score,
                knowledge_at_micros: self.knowledge_at_micros,
                logical_message: &self.logical_message,
                turn: &self.turn,
                session: &self.session,
                source: &self.source,
                evidence_role: &self.evidence_role,
                exact_ranges: &self.exact_ranges,
            },
        )
    }

    fn validate_candidate_fields(
        &self,
        caps: Option<CandidateFieldCaps>,
    ) -> Result<(), TemporalPortError> {
        let Some(caps) = caps else {
            return Ok(());
        };
        if self.stable_id.len() > caps.stable_id_bytes() {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "candidate stable id bytes",
            });
        }
        if self.anchor_id.to_string().len() > caps.anchor_id_bytes() {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "candidate anchor id bytes",
            });
        }
        if self.retriever_record_id.len() > caps.metadata_field_bytes() {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "candidate retriever record id bytes",
            });
        }
        for field in [
            &self.logical_message,
            &self.turn,
            &self.session,
            &self.source,
            &self.evidence_role,
        ] {
            if field
                .as_ref()
                .is_some_and(|value| value.len() > caps.metadata_field_bytes())
            {
                return Err(TemporalPortError::BudgetExceeded {
                    resource: "candidate metadata field bytes",
                });
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct EvidenceWire<'a> {
    authority: tracedecay_domain::SessionAuthorityClassV1,
    authorized: bool,
    supporting_anchor_ids: &'a std::collections::BTreeSet<tracedecay_domain::RetrievalAnchorId>,
}

#[derive(Serialize)]
struct OccurrenceWire<'a> {
    kind: &'static str,
    occurrence_id: &'a tracedecay_domain::MessageOccurrenceIdV1,
    anchor_id: &'a tracedecay_domain::RetrievalAnchorId,
    knowledge_at: tracedecay_domain::UtcMicros,
    valid_time: tracedecay_domain::TemporalValidityV1,
    evidence: EvidenceWire<'a>,
}

#[derive(Serialize)]
struct AssertionWire<'a> {
    kind: &'static str,
    assertion_kind: tracedecay_domain::TemporalAssertionKindV1,
    subject_anchor_id: &'a tracedecay_domain::RetrievalAnchorId,
    object_anchor_id: &'a tracedecay_domain::RetrievalAnchorId,
    knowledge_at: tracedecay_domain::UtcMicros,
    valid_time: tracedecay_domain::TemporalValidityV1,
    evidence: EvidenceWire<'a>,
}

impl MeasuredTemporalValue for TemporalRecord {
    fn measured_encoded_bytes(&self) -> Result<usize, TemporalPortError> {
        match self {
            Self::Occurrence(value) => measured_json_bytes(
                "encode occurrence",
                &OccurrenceWire {
                    kind: "occurrence",
                    occurrence_id: &value.occurrence_id,
                    anchor_id: &value.anchor_id,
                    knowledge_at: value.knowledge_at,
                    valid_time: value.valid_time,
                    evidence: EvidenceWire {
                        authority: value.evidence.authority,
                        authorized: value.evidence.is_authorized(),
                        supporting_anchor_ids: &value.evidence.supporting_anchor_ids,
                    },
                },
            ),
            Self::Copy(value) => measured_json_bytes("encode copy", &("copy", value)),
            Self::Assertion(value) => measured_json_bytes(
                "encode assertion",
                &AssertionWire {
                    kind: "assertion",
                    assertion_kind: value.kind,
                    subject_anchor_id: &value.subject_anchor_id,
                    object_anchor_id: &value.object_anchor_id,
                    knowledge_at: value.knowledge_at,
                    valid_time: value.valid_time,
                    evidence: EvidenceWire {
                        authority: value.evidence.authority,
                        authorized: value.evidence.is_authorized(),
                        supporting_anchor_ids: &value.evidence.supporting_anchor_ids,
                    },
                },
            ),
            Self::Summary(value) => measured_json_bytes("encode summary", &("summary", value)),
            Self::SummarySource(value) => measured_json_bytes(
                "encode summary source",
                &("summary_source", value.anchor_id.clone(), value.state),
            ),
        }
    }
}

struct BoundedByteCounter {
    count: usize,
    stop_after: usize,
}

impl Write for BoundedByteCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.count = self.count.saturating_add(buf.len());
        if self.count > self.stop_after {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "encoded item exceeds absolute measurement ceiling",
            ));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn measured_json_bytes(
    operation: &'static str,
    value: &impl Serialize,
) -> Result<usize, TemporalPortError> {
    let mut counter = BoundedByteCounter {
        count: 0,
        stop_after: MAX_READ_ITEM_BYTES,
    };
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => Ok(counter.count),
        Err(_) if counter.count > MAX_READ_ITEM_BYTES => Err(TemporalPortError::BudgetExceeded {
            resource: "encoded item bytes",
        }),
        Err(error) => Err(TemporalPortError::Read {
            operation,
            message: error.to_string(),
        }),
    }
}
