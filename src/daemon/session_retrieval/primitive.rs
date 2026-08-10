use std::sync::Arc;

use tracedecay_application::retrieval::{
    RetrievalPortContext, RetrievalPortOutcome, SessionLookupRequest, SessionLookupResult,
    TemporalRetrievalFailure, TemporalRetrievalFuture, TemporalRetrievalPort,
};
use tracedecay_application::{
    CancellationObservation, CancellationStage, CoverageCompleteness, CoverageDomainState,
    EvidenceCoverage, EvidenceDomain, FreshnessState, Omission, OmissionReason, OpaqueCursor,
    OperationBudgetUsage, PageState, RetrievalEvidence, TemporalState, now_micros,
};
use tracedecay_domain::{RetrievalGrainV1, UtcMicros};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_tool_catalog::SortContractId;
use tracedecay_usecases::session::{SessionDataFreshness, SessionTemporalQuery};

use super::{
    SessionApplicationRetrievalPortV1, SessionRetrievalPageView, SessionRetrievalServiceOutcome,
};

const SESSION_LOOKUP_CONTEXT_BYTES: u64 = 64 * 1024;
const SESSION_LOOKUP_SORT: &str = "sort.session.temporal.anchor.v1";

pub(crate) struct DaemonSessionLookupPrimitiveV1 {
    retrieval: Arc<dyn SessionApplicationRetrievalPortV1>,
}

impl DaemonSessionLookupPrimitiveV1 {
    pub(crate) fn new(retrieval: Arc<dyn SessionApplicationRetrievalPortV1>) -> Self {
        Self { retrieval }
    }
}

impl TemporalRetrievalPort for DaemonSessionLookupPrimitiveV1 {
    fn session_lookup<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SessionLookupRequest,
    ) -> TemporalRetrievalFuture<'a> {
        Box::pin(async move {
            let query = SessionTemporalQuery::new(
                request.session_id.clone(),
                None,
                "",
                request
                    .meta
                    .page
                    .cursor
                    .as_ref()
                    .map(|cursor| cursor.as_str().to_owned()),
                request.meta.temporal,
                RetrievalGrainV1::Occurrence,
                usize::try_from(request.meta.page.page_size)
                    .map_err(|_| TemporalRetrievalFailure::Unavailable)?,
                DiversityLimits::unbounded(),
                ContextBudget {
                    max_bytes: SESSION_LOOKUP_CONTEXT_BYTES,
                    max_tokens: SESSION_LOOKUP_CONTEXT_BYTES / 4,
                    estimator_version: "words-v1".to_owned(),
                },
            )
            .map_err(|_| TemporalRetrievalFailure::Unavailable)?;
            let outcome = self
                .retrieval
                .retrieve_admitted(context.request, query)
                .await;
            map_outcome(outcome, request, now_micros())
        })
    }
}

fn map_outcome(
    outcome: SessionRetrievalServiceOutcome,
    request: &SessionLookupRequest,
    finished_at: UtcMicros,
) -> Result<RetrievalPortOutcome<SessionLookupResult>, TemporalRetrievalFailure> {
    match outcome {
        SessionRetrievalServiceOutcome::Complete { page, freshness } => {
            complete_outcome(page, freshness, request, finished_at)
        }
        SessionRetrievalServiceOutcome::Partial {
            page,
            freshness,
            omitted,
        } => partial_outcome(page, freshness, omitted, request, finished_at),
        SessionRetrievalServiceOutcome::ResetRequired { .. } => {
            Err(TemporalRetrievalFailure::ResetRequired)
        }
        SessionRetrievalServiceOutcome::Cancelled => {
            Ok(RetrievalPortOutcome::Cancelled(terminal_evidence(
                request,
                finished_at,
                OmissionReason::Cancelled,
                Some(CancellationObservation {
                    stage: CancellationStage::DuringRead,
                    observed_at: finished_at,
                }),
            )?))
        }
        SessionRetrievalServiceOutcome::Stale { .. } => Ok(RetrievalPortOutcome::Unavailable(
            terminal_evidence(request, finished_at, OmissionReason::Stale, None)?,
        )),
        SessionRetrievalServiceOutcome::Redacted => Ok(RetrievalPortOutcome::Unavailable(
            terminal_evidence(request, finished_at, OmissionReason::Redacted, None)?,
        )),
        SessionRetrievalServiceOutcome::CompleteZero { .. }
        | SessionRetrievalServiceOutcome::WrongScope
        | SessionRetrievalServiceOutcome::Locked
        | SessionRetrievalServiceOutcome::Deleted
        | SessionRetrievalServiceOutcome::Denied
        | SessionRetrievalServiceOutcome::Unavailable(_)
        | SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { .. }
        | SessionRetrievalServiceOutcome::BudgetExhausted => Ok(RetrievalPortOutcome::Unavailable(
            terminal_evidence(request, finished_at, OmissionReason::Unavailable, None)?,
        )),
    }
}

fn complete_outcome(
    page: SessionRetrievalPageView,
    freshness: SessionDataFreshness,
    request: &SessionLookupRequest,
    finished_at: UtcMicros,
) -> Result<RetrievalPortOutcome<SessionLookupResult>, TemporalRetrievalFailure> {
    if page.temporal.anchors.is_empty() {
        return Ok(RetrievalPortOutcome::Unavailable(terminal_evidence(
            request,
            finished_at,
            OmissionReason::Unavailable,
            None,
        )?));
    }
    let returned = u64::try_from(page.temporal.anchors.len())
        .map_err(|_| TemporalRetrievalFailure::Unavailable)?;
    Ok(RetrievalPortOutcome::Completed(page_evidence(
        SessionLookupResult {
            anchors: page.temporal.anchors,
        },
        request,
        finished_at,
        freshness,
        returned,
        returned,
        CoverageCompleteness::Complete,
        None,
        page.temporal.cursor,
    )?))
}

fn partial_outcome(
    page: SessionRetrievalPageView,
    freshness: SessionDataFreshness,
    omitted: u64,
    request: &SessionLookupRequest,
    finished_at: UtcMicros,
) -> Result<RetrievalPortOutcome<SessionLookupResult>, TemporalRetrievalFailure> {
    if page.temporal.anchors.is_empty() {
        return Ok(RetrievalPortOutcome::Unavailable(terminal_evidence(
            request,
            finished_at,
            OmissionReason::Unavailable,
            None,
        )?));
    }
    let returned = u64::try_from(page.temporal.anchors.len())
        .map_err(|_| TemporalRetrievalFailure::Unavailable)?;
    let eligible = returned.saturating_add(omitted);
    Ok(RetrievalPortOutcome::Partial(page_evidence(
        SessionLookupResult {
            anchors: page.temporal.anchors,
        },
        request,
        finished_at,
        freshness,
        returned,
        eligible,
        CoverageCompleteness::Partial,
        (omitted != 0).then_some(Omission {
            domain: EvidenceDomain::Temporal,
            count: omitted,
            reason: OmissionReason::Unavailable,
        }),
        page.temporal.cursor,
    )?))
}

#[allow(clippy::too_many_arguments)]
fn page_evidence(
    payload: SessionLookupResult,
    request: &SessionLookupRequest,
    finished_at: UtcMicros,
    freshness: SessionDataFreshness,
    returned: u64,
    eligible: u64,
    completeness: CoverageCompleteness,
    omission: Option<Omission>,
    cursor: Option<String>,
) -> Result<RetrievalEvidence<SessionLookupResult>, TemporalRetrievalFailure> {
    let sort_contract_id = SortContractId::new(SESSION_LOOKUP_SORT)
        .map_err(|_| TemporalRetrievalFailure::Unavailable)?;
    let cursor = cursor
        .map(OpaqueCursor::new)
        .transpose()
        .map_err(|_| TemporalRetrievalFailure::Unavailable)?;
    Ok(RetrievalEvidence {
        payload: Some(payload),
        temporal: temporal_state(request, finished_at, freshness),
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Temporal],
            visited: Some(eligible),
            eligible: Some(eligible),
            returned,
            completeness,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Temporal,
                completeness,
            }],
        },
        omissions: omission.into_iter().collect(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState {
            sort_contract_id,
            sort_revision: 1,
            total: if cursor.is_none() {
                Some(eligible)
            } else {
                None
            },
            returned,
            cursor,
            expires_at: None,
        },
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    })
}

fn terminal_evidence(
    request: &SessionLookupRequest,
    finished_at: UtcMicros,
    reason: OmissionReason,
    cancellation: Option<CancellationObservation>,
) -> Result<RetrievalEvidence<SessionLookupResult>, TemporalRetrievalFailure> {
    let sort_contract_id = SortContractId::new(SESSION_LOOKUP_SORT)
        .map_err(|_| TemporalRetrievalFailure::Unavailable)?;
    Ok(RetrievalEvidence {
        payload: None,
        temporal: TemporalState {
            requested_mode: request.meta.temporal,
            requested_at: finished_at,
            resolved_at: finished_at,
            source_generation: None,
            watermark_digest: None,
            freshness: if reason == OmissionReason::Stale {
                FreshnessState::Stale
            } else {
                FreshnessState::Unknown
            },
        },
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Temporal],
            visited: None,
            eligible: None,
            returned: 0,
            completeness: CoverageCompleteness::Unknown,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Temporal,
                completeness: CoverageCompleteness::Unknown,
            }],
        },
        omissions: vec![Omission {
            domain: EvidenceDomain::Temporal,
            count: 0,
            reason,
        }],
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(sort_contract_id, 1, None, 0)
            .map_err(|_| TemporalRetrievalFailure::Unavailable)?,
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation,
    })
}

fn temporal_state(
    request: &SessionLookupRequest,
    finished_at: UtcMicros,
    freshness: SessionDataFreshness,
) -> TemporalState {
    TemporalState {
        requested_mode: request.meta.temporal,
        requested_at: finished_at,
        resolved_at: finished_at,
        source_generation: None,
        watermark_digest: None,
        freshness: match freshness {
            SessionDataFreshness::Fresh => FreshnessState::Current,
            SessionDataFreshness::Stored { .. } | SessionDataFreshness::Partial { .. } => {
                FreshnessState::Stale
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_application::retrieval::{
        PageRequest, ResultProjection, RetrievalOrder, RetrievalRequestMeta,
    };
    use tracedecay_domain::{RetrievalAnchorId, SessionId};

    use super::*;
    use crate::daemon::session_retrieval::{
        SessionRetrievalPageView, SessionRetrievalStoreScope, SessionRetrievalUnavailable,
        SessionRetrievalUnavailableReason, SessionTemporalMetadataView,
    };

    fn request() -> SessionLookupRequest {
        SessionLookupRequest {
            session_id: SessionId::new("session.lookup.test").expect("session"),
            meta: RetrievalRequestMeta::current(
                PageRequest::first(8).expect("page"),
                ResultProjection::ReferencesOnly,
                RetrievalOrder::TemporalDescending,
            ),
        }
    }

    fn page(anchors: &[&str]) -> SessionRetrievalPageView {
        SessionRetrievalPageView {
            results: Vec::new(),
            temporal: SessionTemporalMetadataView {
                anchors: anchors
                    .iter()
                    .map(|anchor| RetrievalAnchorId::new(*anchor).expect("anchor"))
                    .collect(),
                ..SessionTemporalMetadataView::default()
            },
        }
    }

    #[test]
    fn non_empty_canonical_page_becomes_completed_anchor_evidence() {
        let outcome = map_outcome(
            SessionRetrievalServiceOutcome::Complete {
                page: page(&["anchor.one", "anchor.two"]),
                freshness: SessionDataFreshness::Fresh,
            },
            &request(),
            UtcMicros(7),
        )
        .expect("mapped");
        let RetrievalPortOutcome::Completed(evidence) = outcome else {
            panic!("non-empty canonical page must complete");
        };
        assert_eq!(evidence.payload.expect("payload").anchors.len(), 2);
        assert_eq!(evidence.coverage.returned, 2);
    }

    #[test]
    fn authoritative_zero_is_not_reported_as_success() {
        let outcome = map_outcome(
            SessionRetrievalServiceOutcome::CompleteZero {
                temporal: SessionTemporalMetadataView::default(),
                freshness: SessionDataFreshness::Fresh,
            },
            &request(),
            UtcMicros(7),
        )
        .expect("mapped");
        assert!(matches!(outcome, RetrievalPortOutcome::Unavailable(_)));
    }

    #[test]
    fn unavailable_store_remains_unavailable_without_a_payload() {
        let outcome = map_outcome(
            SessionRetrievalServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::without_worker(
                    SessionRetrievalUnavailableReason::TemporalStoreUnavailable,
                ),
            ),
            &request(),
            UtcMicros(7),
        )
        .expect("mapped");
        let RetrievalPortOutcome::Unavailable(evidence) = outcome else {
            panic!("unavailable store must remain unavailable");
        };
        assert!(evidence.payload.is_none());
        assert_eq!(
            evidence.coverage.completeness,
            CoverageCompleteness::Unknown
        );
    }

    #[test]
    fn reset_required_remains_a_distinct_terminal() {
        assert_eq!(
            map_outcome(
                SessionRetrievalServiceOutcome::ResetRequired {
                    store_scope: SessionRetrievalStoreScope::Project,
                },
                &request(),
                UtcMicros(7),
            ),
            Err(TemporalRetrievalFailure::ResetRequired)
        );
    }

    #[test]
    fn cancellation_carries_during_read_observation() {
        let outcome = map_outcome(
            SessionRetrievalServiceOutcome::Cancelled,
            &request(),
            UtcMicros(7),
        )
        .expect("mapped");
        let RetrievalPortOutcome::Cancelled(evidence) = outcome else {
            panic!("cancelled service must remain cancelled");
        };
        assert_eq!(
            evidence.cancellation,
            Some(CancellationObservation {
                stage: CancellationStage::DuringRead,
                observed_at: UtcMicros(7),
            })
        );
    }
}
