//! Work evidence adapters over the daemon's mounted session retrieval owner.
//!
//! Work authorizes its exact task/version root first. This module then
//! delegates an ordinary Plan 23 session query or exact direct-anchor query to
//! the same project retrieval service used by message search and LCM. It never
//! reopens a store or routes through MCP rendering.

use std::sync::Arc;

use tracedecay_application::{
    OpaqueCursor, RequestContext, WorkAnchorHydrationFuture, WorkAnchorHydrationPortV1,
    WorkAnchorHydrationRequestV1, WorkAnchorHydrationV1, WorkEvidenceCoverageStateV1,
    WorkEvidenceFreshnessV1, WorkEvidenceHydrationErrorV1, WorkSessionNarrativeFuture,
    WorkSessionNarrativePortV1, WorkSessionNarrativeRequestV1, WorkSessionNarrativeV1,
};
use tracedecay_domain::{
    HydrationStateV1, ObservationSourceIdentityV1, RetrievalAnchorId, RetrievalGrainV1, SessionId,
};
use tracedecay_sessions::runtime::SessionMessageSearchResult;
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ports::ExecutionLimits;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::session::{
    SessionDataFreshness, SessionRetrievalScope, SessionTemporalQuery,
};

use crate::daemon::session_retrieval::{
    SessionApplicationRetrievalPortV1, SessionRetrievalPageView, SessionRetrievalServiceOutcome,
    SessionTemporalMetadataView,
};

// Root-wide direct-anchor queries retain the same non-authoritative
// compatibility session value as ordinary message search. The selected real
// authority is the direct anchor plus `AllSessionsInAuthorizedRoot`.
const ROOT_QUERY_COMPATIBILITY_SESSION: &str = "session.message-search.root";
const WORK_EVIDENCE_CONTEXT_BYTES: u64 = 64 * 1024;

/// Cloneable adapter for the canonical project session retrieval authority.
#[derive(Clone)]
pub(crate) struct DaemonWorkEvidenceRetrievalV1 {
    retrieval: Arc<dyn SessionApplicationRetrievalPortV1>,
}

impl DaemonWorkEvidenceRetrievalV1 {
    pub(crate) fn new(retrieval: Arc<dyn SessionApplicationRetrievalPortV1>) -> Self {
        Self { retrieval }
    }

    fn query(
        &self,
        session_id: SessionId,
        provider: Option<String>,
        retrieval_scope: SessionRetrievalScope,
        temporal: tracedecay_domain::TemporalModeV1,
        page_size: u32,
        continuation: Option<OpaqueCursor>,
        direct_anchor: Option<RetrievalAnchorId>,
    ) -> Result<SessionTemporalQuery, WorkEvidenceHydrationErrorV1> {
        let page_size =
            usize::try_from(page_size).map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?;
        let execution_limits = ExecutionLimits {
            candidate_total_bytes: WORK_EVIDENCE_CONTEXT_BYTES as usize,
            candidate_item_bytes: WORK_EVIDENCE_CONTEXT_BYTES as usize,
            record_total_bytes: WORK_EVIDENCE_CONTEXT_BYTES as usize,
            record_item_bytes: WORK_EVIDENCE_CONTEXT_BYTES as usize,
            hydration_limit: page_size,
            hydration_total_bytes: WORK_EVIDENCE_CONTEXT_BYTES as usize,
            hydration_payload_bytes: WORK_EVIDENCE_CONTEXT_BYTES as usize,
            hydration_chunk_bytes: WORK_EVIDENCE_CONTEXT_BYTES as usize,
            ..ExecutionLimits::default()
        };
        let mut query = SessionTemporalQuery::new(
            session_id,
            provider,
            "",
            continuation.map(|cursor| cursor.as_str().to_owned()),
            temporal,
            RetrievalGrainV1::Occurrence,
            page_size,
            DiversityLimits::default(),
            ContextBudget {
                max_bytes: WORK_EVIDENCE_CONTEXT_BYTES,
                max_tokens: WORK_EVIDENCE_CONTEXT_BYTES / 4,
                estimator_version: "words-v1".to_owned(),
            },
        )
        .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?
        .with_retrieval_scope(retrieval_scope)
        .with_execution_limits(execution_limits);
        if let Some(anchor) = direct_anchor {
            query = query.with_direct_anchor(anchor);
        }
        Ok(query)
    }
}

impl WorkSessionNarrativePortV1 for DaemonWorkEvidenceRetrievalV1 {
    fn retrieve_session<'a>(
        &'a self,
        context: &'a RequestContext,
        request: WorkSessionNarrativeRequestV1,
    ) -> WorkSessionNarrativeFuture<'a> {
        Box::pin(async move {
            let query = self.query(
                request.source.session_id().clone(),
                Some(request.source.provider().as_str().to_owned()),
                SessionRetrievalScope::Session(request.source.session_id().clone()),
                request.temporal,
                request.page_size,
                request.continuation,
                None,
            )?;
            session_narrative(
                request.source,
                self.retrieval.retrieve_admitted(context, query).await,
            )
        })
    }
}

impl WorkAnchorHydrationPortV1 for DaemonWorkEvidenceRetrievalV1 {
    fn hydrate_anchor<'a>(
        &'a self,
        context: &'a RequestContext,
        request: WorkAnchorHydrationRequestV1,
    ) -> WorkAnchorHydrationFuture<'a> {
        Box::pin(async move {
            let root = SessionId::new(ROOT_QUERY_COMPATIBILITY_SESSION)
                .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)?;
            let query = self.query(
                root,
                None,
                SessionRetrievalScope::AllSessionsInAuthorizedRoot,
                request.temporal,
                request.page_size,
                request.continuation,
                Some(request.anchor_id.clone()),
            )?;
            anchor_hydration(
                request.anchor_id,
                self.retrieval.retrieve_admitted(context, query).await,
            )
        })
    }
}

fn session_narrative(
    source: ObservationSourceIdentityV1,
    outcome: SessionRetrievalServiceOutcome,
) -> Result<WorkSessionNarrativeV1, WorkEvidenceHydrationErrorV1> {
    match outcome {
        SessionRetrievalServiceOutcome::Complete { page, freshness } => narrative_from_page(
            source,
            page,
            freshness,
            WorkEvidenceCoverageStateV1::Complete,
        ),
        SessionRetrievalServiceOutcome::Partial {
            page, freshness, ..
        } => narrative_from_page(
            source,
            page,
            freshness,
            WorkEvidenceCoverageStateV1::Partial,
        ),
        SessionRetrievalServiceOutcome::CompleteZero {
            temporal,
            freshness,
        } => narrative_from_parts(
            source,
            Vec::new(),
            temporal,
            freshness,
            WorkEvidenceCoverageStateV1::Complete,
        ),
        SessionRetrievalServiceOutcome::Stale {
            temporal,
            freshness,
        } => narrative_from_parts(
            source,
            Vec::new(),
            temporal,
            freshness,
            WorkEvidenceCoverageStateV1::Partial,
        ),
        SessionRetrievalServiceOutcome::Redacted => Ok(WorkSessionNarrativeV1 {
            source,
            anchors: Vec::new(),
            compact_narrative: Vec::new(),
            coverage: WorkEvidenceCoverageStateV1::Unknown,
            freshness: WorkEvidenceFreshnessV1::Unknown,
            redacted: true,
            continuation: None,
        }),
        other => Err(hydration_error(other)),
    }
}

fn narrative_from_page(
    source: ObservationSourceIdentityV1,
    page: SessionRetrievalPageView,
    freshness: SessionDataFreshness,
    coverage: WorkEvidenceCoverageStateV1,
) -> Result<WorkSessionNarrativeV1, WorkEvidenceHydrationErrorV1> {
    narrative_from_parts(source, page.results, page.temporal, freshness, coverage)
}

fn narrative_from_parts(
    source: ObservationSourceIdentityV1,
    results: Vec<SessionMessageSearchResult>,
    temporal: SessionTemporalMetadataView,
    freshness: SessionDataFreshness,
    coverage: WorkEvidenceCoverageStateV1,
) -> Result<WorkSessionNarrativeV1, WorkEvidenceHydrationErrorV1> {
    let redacted = temporal
        .omissions
        .iter()
        .any(|omission| omission.reason == HydrationStateV1::Redacted);
    Ok(WorkSessionNarrativeV1 {
        source,
        anchors: temporal.anchors,
        compact_narrative: results
            .into_iter()
            .map(|result| result.message.text)
            .collect(),
        coverage,
        freshness: work_freshness(freshness),
        redacted,
        continuation: opaque_cursor(temporal.cursor)?,
    })
}

fn anchor_hydration(
    anchor_id: RetrievalAnchorId,
    outcome: SessionRetrievalServiceOutcome,
) -> Result<WorkAnchorHydrationV1, WorkEvidenceHydrationErrorV1> {
    match outcome {
        SessionRetrievalServiceOutcome::Complete { page, freshness } => anchor_from_page(
            anchor_id,
            page,
            freshness,
            WorkEvidenceCoverageStateV1::Complete,
        ),
        SessionRetrievalServiceOutcome::Partial {
            page, freshness, ..
        } => anchor_from_page(
            anchor_id,
            page,
            freshness,
            WorkEvidenceCoverageStateV1::Partial,
        ),
        SessionRetrievalServiceOutcome::Stale { freshness, .. } => Ok(WorkAnchorHydrationV1 {
            // A stale Plan 23 outcome carries no selectable temporal page.
            // The Work relation still names this exact Plan 13 anchor, so
            // retain that binding while reporting its content as unavailable
            // and freshness as stale rather than concealing it as absent.
            exact_anchors: vec![anchor_id.clone()],
            anchor_id,
            content: Vec::new(),
            coverage: WorkEvidenceCoverageStateV1::Partial,
            freshness: work_freshness(freshness),
            redacted: false,
            continuation: None,
        }),
        SessionRetrievalServiceOutcome::Redacted => Ok(WorkAnchorHydrationV1 {
            exact_anchors: vec![anchor_id.clone()],
            anchor_id,
            content: Vec::new(),
            coverage: WorkEvidenceCoverageStateV1::Unknown,
            freshness: WorkEvidenceFreshnessV1::Unknown,
            redacted: true,
            continuation: None,
        }),
        SessionRetrievalServiceOutcome::CompleteZero { .. } => {
            Err(WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized)
        }
        other => Err(hydration_error(other)),
    }
}

fn anchor_from_page(
    anchor_id: RetrievalAnchorId,
    page: SessionRetrievalPageView,
    freshness: SessionDataFreshness,
    coverage: WorkEvidenceCoverageStateV1,
) -> Result<WorkAnchorHydrationV1, WorkEvidenceHydrationErrorV1> {
    anchor_from_parts(anchor_id, page.results, page.temporal, freshness, coverage)
}

fn anchor_from_parts(
    anchor_id: RetrievalAnchorId,
    results: Vec<SessionMessageSearchResult>,
    temporal: SessionTemporalMetadataView,
    freshness: SessionDataFreshness,
    coverage: WorkEvidenceCoverageStateV1,
) -> Result<WorkAnchorHydrationV1, WorkEvidenceHydrationErrorV1> {
    let requested_anchor_redacted = temporal.omissions.iter().any(|omission| {
        omission.anchor == anchor_id && omission.reason == HydrationStateV1::Redacted
    });
    if !temporal.anchors.contains(&anchor_id) && !requested_anchor_redacted {
        return Err(WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized);
    }
    // Plan 23 deliberately omits a redacted anchor from visible temporal
    // results. It is nevertheless the exact source Plan 13 authorized this
    // expansion to describe, so retain its identity for the Work-layer
    // binding check without claiming that its content was disclosed.
    let mut exact_anchors = temporal.anchors;
    if requested_anchor_redacted && !exact_anchors.contains(&anchor_id) {
        exact_anchors.push(anchor_id.clone());
    }
    Ok(WorkAnchorHydrationV1 {
        anchor_id,
        exact_anchors,
        content: results
            .into_iter()
            .map(|result| result.message.text)
            .collect(),
        coverage,
        freshness: work_freshness(freshness),
        redacted: requested_anchor_redacted,
        continuation: opaque_cursor(temporal.cursor)?,
    })
}

fn opaque_cursor(
    cursor: Option<String>,
) -> Result<Option<OpaqueCursor>, WorkEvidenceHydrationErrorV1> {
    cursor
        .map(OpaqueCursor::new)
        .transpose()
        .map_err(|_| WorkEvidenceHydrationErrorV1::Unavailable)
}

const fn work_freshness(freshness: SessionDataFreshness) -> WorkEvidenceFreshnessV1 {
    match freshness {
        SessionDataFreshness::Fresh => WorkEvidenceFreshnessV1::Current,
        SessionDataFreshness::Stored { .. } | SessionDataFreshness::Partial { .. } => {
            WorkEvidenceFreshnessV1::Stale
        }
    }
}

fn hydration_error(outcome: SessionRetrievalServiceOutcome) -> WorkEvidenceHydrationErrorV1 {
    match outcome {
        SessionRetrievalServiceOutcome::Locked
        | SessionRetrievalServiceOutcome::Deleted
        | SessionRetrievalServiceOutcome::Denied => {
            WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized
        }
        SessionRetrievalServiceOutcome::Cancelled => WorkEvidenceHydrationErrorV1::Cancelled,
        SessionRetrievalServiceOutcome::Stale { .. } => WorkEvidenceHydrationErrorV1::Stale,
        SessionRetrievalServiceOutcome::WrongScope
        | SessionRetrievalServiceOutcome::Unavailable(_)
        | SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { .. }
        | SessionRetrievalServiceOutcome::BudgetExhausted
        | SessionRetrievalServiceOutcome::Complete { .. }
        | SessionRetrievalServiceOutcome::CompleteZero { .. }
        | SessionRetrievalServiceOutcome::Partial { .. }
        | SessionRetrievalServiceOutcome::Redacted => WorkEvidenceHydrationErrorV1::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Mutex;

    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, TemporalModeV1, UtcMicros,
        WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;
    use crate::daemon::session_retrieval::{
        SessionApplicationRetrievalFutureV1, SessionApplicationRetrievalPortV1,
        SessionRetrievalOmissionView,
    };

    #[derive(Default)]
    struct RecordingRetrieval {
        calls: Mutex<Vec<(RequestContext, SessionTemporalQuery)>>,
    }

    impl SessionApplicationRetrievalPortV1 for RecordingRetrieval {
        fn retrieve_admitted<'a>(
            &'a self,
            context: &'a RequestContext,
            query: SessionTemporalQuery,
        ) -> SessionApplicationRetrievalFutureV1<'a> {
            self.calls
                .lock()
                .expect("recording retrieval lock")
                .push((context.clone(), query));
            Box::pin(async {
                SessionRetrievalServiceOutcome::CompleteZero {
                    temporal: SessionTemporalMetadataView::default(),
                    freshness: SessionDataFreshness::Fresh,
                }
            })
        }
    }

    fn context() -> RequestContext {
        let actor = ActorId::new("actor.work-evidence").expect("actor");
        let scope = ResolvedScope::new(
            ProjectId::new("project.work-evidence").expect("project"),
            RepositoryId::new("repository.work-evidence").expect("repository"),
            WorktreeId::new("worktree.work-evidence").expect("worktree"),
            Some(RefId::new("refs/heads/main").expect("reference")),
        )
        .expect("scope");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.work-evidence").expect("grant id"),
            7,
            ManifestDigest::new(format!("sha256:{}", "1".repeat(64))).expect("grant digest"),
            actor.clone(),
            UtcMicros(10),
            UtcMicros(1_000),
            scope.clone(),
            BTreeSet::from([
                CapabilityId::new("capability.work.evidence.read").expect("capability")
            ]),
            BTreeSet::from([UseCaseId::new("use-case.work.evidence.read").expect("use case")]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        RequestContext::new(
            actor,
            scope,
            grant,
            RequestId::new("request.work-evidence").expect("request id"),
            Deadline::new(UtcMicros(900)).expect("deadline"),
            CancellationContext::active("cancellation.work-evidence").expect("cancellation"),
        )
        .expect("request context")
    }

    #[tokio::test]
    async fn session_narrative_passes_the_admitted_context_and_real_session_to_plan23() {
        let retrieval = Arc::new(RecordingRetrieval::default());
        let adapter = DaemonWorkEvidenceRetrievalV1::new(retrieval.clone());
        let context = context();
        let source = ObservationSourceIdentityV1::for_provider(
            tracedecay_domain::ProviderId::new("codex").expect("provider"),
            SessionId::new("session.provider.real").expect("session id"),
        )
        .expect("provider session");

        let result = adapter
            .retrieve_session(
                &context,
                WorkSessionNarrativeRequestV1 {
                    source: source.clone(),
                    temporal: TemporalModeV1::Current,
                    page_size: 25,
                    continuation: None,
                    observed_at: UtcMicros(20),
                },
            )
            .await
            .expect("session narrative");

        let calls = retrieval.calls.lock().expect("recording retrieval lock");
        let (recorded_context, query) = calls.first().expect("one Plan23 call");
        assert_eq!(calls.len(), 1);
        assert_eq!(recorded_context, &context);
        assert_eq!(query.session_id(), source.session_id());
        assert_eq!(query.provider(), Some("codex"));
        assert_eq!(
            query.retrieval_scope(),
            &SessionRetrievalScope::Session(source.session_id().clone())
        );
        assert_eq!(result.source, source);
    }

    #[tokio::test]
    async fn anchor_hydration_passes_the_admitted_context_and_exact_anchor_to_plan23() {
        let retrieval = Arc::new(RecordingRetrieval::default());
        let adapter = DaemonWorkEvidenceRetrievalV1::new(retrieval.clone());
        let context = context();
        let anchor_id = RetrievalAnchorId::new("anchor.work-evidence").expect("anchor id");

        let result = adapter
            .hydrate_anchor(
                &context,
                WorkAnchorHydrationRequestV1 {
                    anchor_id: anchor_id.clone(),
                    temporal: TemporalModeV1::Current,
                    page_size: 10,
                    continuation: None,
                    observed_at: UtcMicros(20),
                },
            )
            .await;

        assert_eq!(
            result,
            Err(WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized)
        );
        let calls = retrieval.calls.lock().expect("recording retrieval lock");
        let (recorded_context, query) = calls.first().expect("one Plan23 call");
        assert_eq!(calls.len(), 1);
        assert_eq!(recorded_context, &context);
        assert_eq!(query.provider(), None);
        assert_eq!(
            query.retrieval_scope(),
            &SessionRetrievalScope::AllSessionsInAuthorizedRoot
        );
        assert_eq!(query.direct_anchor(), Some(&anchor_id));
    }

    #[test]
    fn redacted_direct_anchor_remains_bound_without_disclosing_content() {
        let anchor_id = RetrievalAnchorId::new("anchor.work-evidence.redacted").expect("anchor");
        let hydration = anchor_from_parts(
            anchor_id.clone(),
            Vec::new(),
            SessionTemporalMetadataView {
                omissions: vec![SessionRetrievalOmissionView {
                    rank: 0,
                    anchor: anchor_id.clone(),
                    reason: HydrationStateV1::Redacted,
                }],
                ..SessionTemporalMetadataView::default()
            },
            SessionDataFreshness::Fresh,
            WorkEvidenceCoverageStateV1::Partial,
        )
        .expect("redacted direct anchor is an authorized typed source");

        assert_eq!(hydration.anchor_id, anchor_id);
        assert_eq!(hydration.exact_anchors, vec![anchor_id]);
        assert!(hydration.redacted);
        assert!(hydration.content.is_empty());
    }

    #[test]
    fn redaction_of_a_different_anchor_cannot_authorize_the_requested_anchor() {
        let requested = RetrievalAnchorId::new("anchor.work-evidence.requested").expect("anchor");
        let redacted_other = RetrievalAnchorId::new("anchor.work-evidence.other").expect("anchor");

        let result = anchor_from_parts(
            requested,
            Vec::new(),
            SessionTemporalMetadataView {
                omissions: vec![SessionRetrievalOmissionView {
                    rank: 0,
                    anchor: redacted_other,
                    reason: HydrationStateV1::Redacted,
                }],
                ..SessionTemporalMetadataView::default()
            },
            SessionDataFreshness::Fresh,
            WorkEvidenceCoverageStateV1::Partial,
        );

        assert_eq!(
            result,
            Err(WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized)
        );
    }

    #[test]
    fn stale_direct_anchor_remains_bound_with_typed_staleness() {
        let anchor_id = RetrievalAnchorId::new("anchor.work-evidence.stale").expect("anchor");
        let hydration = anchor_hydration(
            anchor_id.clone(),
            SessionRetrievalServiceOutcome::Stale {
                temporal: SessionTemporalMetadataView::default(),
                freshness: SessionDataFreshness::Stored { generation_lag: 1 },
            },
        )
        .expect("stale direct anchor is an exact typed source");

        assert_eq!(hydration.anchor_id, anchor_id);
        assert_eq!(hydration.exact_anchors, vec![anchor_id]);
        assert_eq!(hydration.freshness, WorkEvidenceFreshnessV1::Stale);
        assert_eq!(hydration.coverage, WorkEvidenceCoverageStateV1::Partial);
        assert!(!hydration.redacted);
        assert!(hydration.content.is_empty());
    }
}
