use std::collections::BTreeSet;
use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalModeV1};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ports::ExecutionLimits;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::session::{SessionRetrievalScope, SessionTemporalQuery};

use crate::dashboard::{
    DashboardLcmCanonicalMatchesV1, DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1,
    DashboardLcmCanonicalStatsV1, DashboardLcmCanonicalSummaryV1, DashboardLcmReadFutureV1,
    DashboardLcmReadOutcomeV1, DashboardLcmReadPortV1, DashboardLcmReadRequestV1,
    DashboardLcmReadStateV1,
};
use crate::sessions::git_correlation::GitScopeFilter;
use crate::sessions::lcm::{LcmDescribeResponse, LcmDescribeTarget};
use crate::sessions::{SessionMessageType, SessionSearchScope, SessionSearchTimeRange};

use super::{
    LcmDescribeServiceCommand, LcmDescribeServiceOutcome, SessionRetrievalCommand,
    SessionRetrievalFilters, SessionRetrievalPageView, SessionRetrievalServiceOutcome,
    SessionRetrievalServicePort, SessionRetrievalStoreScope,
};

const SUMMARY_DESCRIBE_CONCURRENCY: usize = 8;
const DASHBOARD_AGGREGATE_PAGE_LIMIT: usize = 16;

struct SummaryHydrationRequest {
    provider: String,
    session_id: SessionId,
    node_id: String,
    content: String,
}

pub(crate) struct DashboardLcmReadAdapter {
    retrieval: Arc<dyn SessionRetrievalServicePort>,
    project_id: String,
}

impl DashboardLcmReadAdapter {
    pub(crate) fn new(retrieval: Arc<dyn SessionRetrievalServicePort>, project_id: String) -> Self {
        Self {
            retrieval,
            project_id,
        }
    }

    async fn execute(
        &self,
        project_id: Option<&str>,
        request: DashboardLcmReadRequestV1,
    ) -> DashboardLcmReadOutcomeV1 {
        if project_id != Some(self.project_id.as_str()) {
            return not_ready(
                DashboardLcmReadStateV1::Unavailable,
                "lcm_selected_project_authority_unavailable",
            );
        }
        if let DashboardLcmReadRequestV1::Overview { query, limit } = &request
            && !query.trim().is_empty()
        {
            return self
                .execute_overview_with_matches(project_id, query.clone(), *limit)
                .await;
        }
        let aggregate = matches!(
            request,
            DashboardLcmReadRequestV1::Overview { .. } | DashboardLcmReadRequestV1::Timeline { .. }
        );
        let mut cursor = initial_cursor(&request);
        let mut seen_cursors = BTreeSet::new();
        let mut aggregate_results = Vec::new();
        let mut aggregate_omitted = 0_u64;
        let mut aggregate_pages = 0_usize;
        // A non-aggregate session read serves one window: a Partial outcome
        // (continuation cursor or genuine omission) keeps the page visibly
        // partial. Aggregate reads drain to the terminal cursor, so only
        // their accumulated omissions matter.
        let mut window_partial = false;
        // Aggregate reads consume the daemon-issued continuation to its
        // terminal page. The opaque cursor binds the frozen participant/source
        // manifest and ordering, while each execute call reauthorizes and
        // canonically hydrates that page.
        let temporal = loop {
            let Some(command) = retrieval_command(&request, cursor.clone(), aggregate) else {
                return not_ready(
                    DashboardLcmReadStateV1::Unavailable,
                    "lcm_dashboard_request_invalid",
                );
            };
            let (page, omitted, paged_partial) = match self.retrieval.execute(command).await {
                SessionRetrievalServiceOutcome::Complete { page, .. } => (page, 0, false),
                SessionRetrievalServiceOutcome::CompleteZero { temporal, .. } => (
                    SessionRetrievalPageView {
                        results: Vec::new(),
                        temporal,
                    },
                    0,
                    false,
                ),
                SessionRetrievalServiceOutcome::Partial { page, omitted, .. } => {
                    (page, omitted, true)
                }
                SessionRetrievalServiceOutcome::Stale { .. } => {
                    return not_ready(
                        DashboardLcmReadStateV1::Stale,
                        "lcm_temporal_projection_stale",
                    );
                }
                SessionRetrievalServiceOutcome::WrongScope => return wrong_scope_not_ready(),
                SessionRetrievalServiceOutcome::Locked => {
                    return not_ready(DashboardLcmReadStateV1::Locked, "lcm_temporal_read_locked");
                }
                SessionRetrievalServiceOutcome::Redacted => {
                    return not_ready(
                        DashboardLcmReadStateV1::Redacted,
                        "lcm_temporal_read_redacted",
                    );
                }
                SessionRetrievalServiceOutcome::Deleted => {
                    return not_ready(DashboardLcmReadStateV1::Absent, "lcm_session_absent");
                }
                SessionRetrievalServiceOutcome::Denied => {
                    return not_ready(DashboardLcmReadStateV1::Denied, "lcm_temporal_read_denied");
                }
                SessionRetrievalServiceOutcome::Unavailable(_) => {
                    return not_ready(
                        DashboardLcmReadStateV1::Unavailable,
                        "lcm_temporal_authority_unavailable",
                    );
                }
                SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { .. }
                | SessionRetrievalServiceOutcome::BudgetExhausted
                | SessionRetrievalServiceOutcome::Cancelled => {
                    return not_ready(
                        DashboardLcmReadStateV1::Unavailable,
                        "lcm_temporal_read_incomplete",
                    );
                }
            };
            aggregate_pages = aggregate_pages.saturating_add(1);
            aggregate_omitted = aggregate_omitted.saturating_add(omitted);
            if !aggregate {
                window_partial |= paged_partial;
            }
            let next_cursor = page.temporal.cursor.clone();
            aggregate_results.extend(page.results);
            let temporal = page.temporal;
            if !aggregate || next_cursor.is_none() {
                break temporal;
            }
            if aggregate_pages >= DASHBOARD_AGGREGATE_PAGE_LIMIT {
                // The daemon cursor proves more frozen-manifest records exist,
                // but this aggregate view is deliberately bounded. Preserve a
                // truthful partial state instead of turning a read into
                // unbounded background work.
                aggregate_omitted = aggregate_omitted.saturating_add(1);
                break temporal;
            }
            let Some(next_cursor) = next_cursor else {
                break temporal;
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return not_ready(
                    DashboardLcmReadStateV1::Unavailable,
                    "lcm_temporal_cursor_did_not_advance",
                );
            }
            cursor = Some(next_cursor);
        };
        let page = SessionRetrievalPageView {
            results: aggregate_results,
            temporal,
        };
        let omitted = aggregate_omitted;

        let mut partial_description_count = 0_u64;
        // A session read's stats come from the canonical describe authority,
        // addressed by the session's measured provider — taken from the
        // hydrated page itself, never a wildcard the exact-identity describe
        // reads would treat as a provider named "all".
        let session_request_id = match &request {
            DashboardLcmReadRequestV1::Session { session_id, .. } => {
                match SessionId::new(session_id) {
                    Ok(session_id) => Some(session_id),
                    Err(_) => {
                        return not_ready(
                            DashboardLcmReadStateV1::Unavailable,
                            "lcm_dashboard_request_invalid",
                        );
                    }
                }
            }
            DashboardLcmReadRequestV1::Search { .. }
            | DashboardLcmReadRequestV1::Overview { .. }
            | DashboardLcmReadRequestV1::Timeline { .. } => None,
        };
        let session_provider = page
            .results
            .first()
            .map(|result| result.message.provider.clone());

        let mut messages = Vec::new();
        let mut summary_requests = Vec::new();
        for result in page.results {
            if result.message.role == "summary" {
                let session_id = match SessionId::new(&result.message.session_id) {
                    Ok(session_id) => session_id,
                    Err(_) => {
                        return not_ready(
                            DashboardLcmReadStateV1::Unavailable,
                            "lcm_summary_metadata_unavailable",
                        );
                    }
                };
                summary_requests.push(SummaryHydrationRequest {
                    provider: result.message.provider,
                    session_id,
                    node_id: result.message.message_id,
                    content: result.message.text,
                });
            } else {
                messages.push(DashboardLcmCanonicalMessageV1 {
                    session_id: result.message.session_id,
                    provider: result.message.provider,
                    role: result.message.role,
                    timestamp: result.message.timestamp,
                    ordinal: result.message.ordinal,
                    content: result.message.text,
                    message_id: result.message.message_id,
                    metadata_json: result.message.metadata_json,
                    tool_names: result.message.tool_names,
                });
            }
        }
        let hydrated_summaries = stream::iter(summary_requests)
            .map(|request| self.hydrate_summary(request))
            .buffered(SUMMARY_DESCRIBE_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        let mut summary_nodes = Vec::with_capacity(hydrated_summaries.len());
        for result in hydrated_summaries {
            match result {
                Ok((summary, partial)) => {
                    partial_description_count =
                        partial_description_count.saturating_add(u64::from(partial));
                    summary_nodes.push(summary);
                }
                Err((state, _)) => {
                    return not_ready(state, "lcm_summary_metadata_unavailable");
                }
            }
        }

        let stats = if let Some(session_id) = session_request_id {
            match session_provider {
                Some(provider) => {
                    let (description, partial) = match self
                        .describe(
                            &provider,
                            session_id,
                            LcmDescribeTarget::Session,
                            RetrievalGrainV1::Session,
                        )
                        .await
                    {
                        Ok(described) => described,
                        Err((state, reason)) => return not_ready(state, reason),
                    };
                    partial_description_count =
                        partial_description_count.saturating_add(u64::from(partial));
                    DashboardLcmCanonicalStatsV1 {
                        message_count: description.raw_message_count,
                        summary_node_count: description.summary_node_count,
                        summary_token_count: None,
                        source_token_count: None,
                        token_estimate_total: description.session_token_estimate,
                    }
                }
                // No temporal record surfaced for the session, so there is no
                // measured provider to describe under: the zero stats feed the
                // typed session-absent state below.
                None => DashboardLcmCanonicalStatsV1::default(),
            }
        } else {
            DashboardLcmCanonicalStatsV1::default()
        };

        if messages.is_empty()
            && summary_nodes.is_empty()
            && stats.message_count == 0
            && stats.summary_node_count == 0
        {
            // Only a session read has a subject that can be absent. An
            // aggregate read over a readable store with zero temporal
            // results is a measured zero, served as a complete empty page —
            // never collapsed into the Absent state.
            if let DashboardLcmReadRequestV1::Session { .. } = request {
                return not_ready(DashboardLcmReadStateV1::Absent, "lcm_session_absent");
            }
        }

        let next_cursor = page.temporal.cursor;
        let canonical_page = DashboardLcmCanonicalPageV1 {
            messages,
            summary_nodes,
            overview_matches: None,
            stats,
            has_more: next_cursor.is_some(),
            next_cursor,
        };
        let total_omitted = omitted.saturating_add(partial_description_count);
        // A windowed page with a continuation cursor stays visibly partial
        // even when nothing was genuinely omitted (omitted stays 0): the
        // partiality is a state carried from the retrieval outcome, never
        // re-derived from the count.
        if total_omitted > 0 || window_partial {
            DashboardLcmReadOutcomeV1::Partial {
                page: canonical_page,
                omitted: total_omitted,
            }
        } else {
            DashboardLcmReadOutcomeV1::Ready(canonical_page)
        }
    }

    async fn execute_overview_with_matches(
        &self,
        project_id: Option<&str>,
        query: String,
        limit: i64,
    ) -> DashboardLcmReadOutcomeV1 {
        let base = Box::pin(self.execute(
            project_id,
            DashboardLcmReadRequestV1::Overview {
                query: String::new(),
                limit,
            },
        ))
        .await;
        let (mut page, base_omitted) = match base {
            DashboardLcmReadOutcomeV1::Ready(page) => (page, 0),
            DashboardLcmReadOutcomeV1::Partial { page, omitted } => (page, omitted),
            not_ready @ DashboardLcmReadOutcomeV1::NotReady { .. } => return not_ready,
        };
        let matches = Box::pin(self.execute(
            project_id,
            DashboardLcmReadRequestV1::Search {
                query,
                limit,
                cursor: None,
                role: None,
                source: None,
                session_id: None,
                since: None,
                until: None,
            },
        ))
        .await;
        let (matches, match_omitted) = match matches {
            DashboardLcmReadOutcomeV1::Ready(matches) => (matches, 0),
            DashboardLcmReadOutcomeV1::Partial {
                page: matches,
                omitted,
            } => (matches, omitted),
            DashboardLcmReadOutcomeV1::NotReady {
                state: DashboardLcmReadStateV1::Absent,
                ..
            } => (
                DashboardLcmCanonicalPageV1 {
                    messages: Vec::new(),
                    summary_nodes: Vec::new(),
                    overview_matches: None,
                    stats: DashboardLcmCanonicalStatsV1::default(),
                    has_more: false,
                    next_cursor: None,
                },
                0,
            ),
            not_ready @ DashboardLcmReadOutcomeV1::NotReady { .. } => return not_ready,
        };
        page.overview_matches = Some(DashboardLcmCanonicalMatchesV1 {
            messages: matches.messages,
            summary_nodes: matches.summary_nodes,
        });
        let omitted = base_omitted.saturating_add(match_omitted);
        if omitted > 0 {
            DashboardLcmReadOutcomeV1::Partial { page, omitted }
        } else {
            DashboardLcmReadOutcomeV1::Ready(page)
        }
    }

    async fn hydrate_summary(
        &self,
        request: SummaryHydrationRequest,
    ) -> Result<(DashboardLcmCanonicalSummaryV1, bool), (DashboardLcmReadStateV1, &'static str)>
    {
        let (description, partial) = self
            .describe(
                &request.provider,
                request.session_id,
                LcmDescribeTarget::SummaryNode {
                    node_id: request.node_id,
                },
                RetrievalGrainV1::Summary,
            )
            .await?;
        let Some(summary) = description.summary_node else {
            return Err((
                DashboardLcmReadStateV1::Unavailable,
                "lcm_summary_metadata_unavailable",
            ));
        };
        let Some(expand_hint) = summary.expand_hint else {
            return Err((
                DashboardLcmReadStateV1::Unavailable,
                "lcm_summary_metadata_unavailable",
            ));
        };
        Ok((
            DashboardLcmCanonicalSummaryV1 {
                node_id: summary.node_id,
                session_id: summary.conversation_id,
                depth: summary.depth,
                token_count: Some(summary.summary_token_count),
                source_token_count: Some(summary.source_token_count),
                latest_at: summary.source_time_end,
                created_at: summary.created_at,
                expand_hint,
                summary: request.content,
            },
            partial,
        ))
    }

    async fn describe(
        &self,
        provider: &str,
        session_id: SessionId,
        target: LcmDescribeTarget,
        grain: RetrievalGrainV1,
    ) -> Result<(LcmDescribeResponse, bool), (DashboardLcmReadStateV1, &'static str)> {
        match self
            .retrieval
            .describe_lcm(LcmDescribeServiceCommand::new(
                provider,
                session_id,
                target,
                grain,
                SessionRetrievalStoreScope::Project,
            ))
            .await
        {
            LcmDescribeServiceOutcome::Complete { description, .. } => Ok((description, false)),
            LcmDescribeServiceOutcome::Partial {
                description: Some(description),
                ..
            } => Ok((description, true)),
            LcmDescribeServiceOutcome::Stale { .. } => Err((
                DashboardLcmReadStateV1::Stale,
                "lcm_temporal_projection_stale",
            )),
            LcmDescribeServiceOutcome::WrongScope => Err(wrong_scope_error()),
            LcmDescribeServiceOutcome::Locked => {
                Err((DashboardLcmReadStateV1::Locked, "lcm_temporal_read_locked"))
            }
            LcmDescribeServiceOutcome::Redacted => Err((
                DashboardLcmReadStateV1::Redacted,
                "lcm_temporal_read_redacted",
            )),
            LcmDescribeServiceOutcome::Deleted => {
                Err((DashboardLcmReadStateV1::Absent, "lcm_session_absent"))
            }
            LcmDescribeServiceOutcome::Denied => {
                Err((DashboardLcmReadStateV1::Denied, "lcm_temporal_read_denied"))
            }
            LcmDescribeServiceOutcome::Partial {
                description: None, ..
            }
            | LcmDescribeServiceOutcome::Unavailable(_)
            | LcmDescribeServiceOutcome::BudgetExhausted
            | LcmDescribeServiceOutcome::Cancelled => Err((
                DashboardLcmReadStateV1::Unavailable,
                "lcm_session_description_unavailable",
            )),
        }
    }
}

impl DashboardLcmReadPortV1 for DashboardLcmReadAdapter {
    fn read(
        &self,
        project_id: Option<&str>,
        request: DashboardLcmReadRequestV1,
    ) -> DashboardLcmReadFutureV1<'_> {
        let project_id = project_id.map(str::to_owned);
        Box::pin(async move { self.execute(project_id.as_deref(), request).await })
    }
}

fn not_ready(state: DashboardLcmReadStateV1, reason: &str) -> DashboardLcmReadOutcomeV1 {
    DashboardLcmReadOutcomeV1::NotReady {
        state,
        reason: reason.to_owned(),
    }
}

fn wrong_scope_not_ready() -> DashboardLcmReadOutcomeV1 {
    let (state, reason) = wrong_scope_error();
    not_ready(state, reason)
}

fn wrong_scope_error() -> (DashboardLcmReadStateV1, &'static str) {
    (
        DashboardLcmReadStateV1::Unavailable,
        "lcm_temporal_wrong_scope",
    )
}

fn initial_cursor(request: &DashboardLcmReadRequestV1) -> Option<String> {
    match request {
        DashboardLcmReadRequestV1::Search { cursor, .. }
        | DashboardLcmReadRequestV1::Session { cursor, .. } => cursor.clone(),
        DashboardLcmReadRequestV1::Overview { .. } | DashboardLcmReadRequestV1::Timeline { .. } => {
            None
        }
    }
}

fn retrieval_command(
    request: &DashboardLcmReadRequestV1,
    cursor: Option<String>,
    aggregate: bool,
) -> Option<SessionRetrievalCommand> {
    let (session_id, cursor, query_text, limit, retrieval_scope, roles, source, time_range) =
        match request {
            DashboardLcmReadRequestV1::Overview { query, .. } => (
                SessionId::new("session.dashboard-lcm.root").ok()?,
                cursor,
                query.as_str(),
                500,
                SessionRetrievalScope::AllSessionsInAuthorizedRoot,
                Vec::new(),
                None,
                SessionSearchTimeRange::default(),
            ),
            DashboardLcmReadRequestV1::Search {
                query,
                limit,
                cursor: _,
                role,
                source,
                session_id,
                since,
                until,
            } => {
                let root = session_id
                    .as_deref()
                    .map_or("session.dashboard-lcm.root", |session_id| session_id);
                let session = SessionId::new(root).ok()?;
                let scope = if session_id.is_some() {
                    SessionRetrievalScope::Session(session.clone())
                } else {
                    SessionRetrievalScope::AllSessionsInAuthorizedRoot
                };
                (
                    session,
                    cursor,
                    query.as_str(),
                    *limit,
                    scope,
                    role.iter().cloned().collect(),
                    source.clone(),
                    SessionSearchTimeRange {
                        start_time: *since,
                        end_time: *until,
                    },
                )
            }
            DashboardLcmReadRequestV1::Session {
                session_id,
                limit,
                cursor: _,
            } => {
                let session = SessionId::new(session_id).ok()?;
                (
                    session.clone(),
                    cursor,
                    "",
                    *limit,
                    SessionRetrievalScope::Session(session),
                    Vec::new(),
                    None,
                    SessionSearchTimeRange::default(),
                )
            }
            DashboardLcmReadRequestV1::Timeline { session_id, .. } => {
                let root = session_id
                    .as_deref()
                    .unwrap_or("session.dashboard-lcm.root");
                let session = SessionId::new(root).ok()?;
                let scope = if session_id.is_some() {
                    SessionRetrievalScope::Session(session.clone())
                } else {
                    SessionRetrievalScope::AllSessionsInAuthorizedRoot
                };
                (
                    session,
                    cursor,
                    "",
                    500,
                    scope,
                    Vec::new(),
                    None,
                    SessionSearchTimeRange::default(),
                )
            }
        };
    let limit = usize::try_from(limit.clamp(1, 500)).ok()?;
    // The default execution limits hydrate at most 64 records per request;
    // dashboard reads legitimately page up to 500 rows, so the request
    // carries execution limits sized to its own page (still validated
    // against the port's absolute read caps by the executor).
    let mut execution_limits = ExecutionLimits::default();
    if limit > execution_limits.hydration_limit {
        execution_limits.hydration_limit = limit;
    }
    let query = SessionTemporalQuery::new(
        session_id,
        None,
        query_text,
        cursor,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        limit,
        if aggregate {
            DiversityLimits::unbounded()
        } else {
            DiversityLimits::default()
        },
        ContextBudget {
            max_bytes: 1024 * 1024,
            max_tokens: 256 * 1024,
            estimator_version: "words-v1".to_owned(),
        },
    )
    .ok()?
    .with_execution_limits(execution_limits)
    .with_retrieval_scope(retrieval_scope);
    Some(SessionRetrievalCommand::new(
        query,
        SessionRetrievalFilters {
            project_key: None,
            parent_session_id: None,
            source,
            include_summaries: true,
            scope: SessionSearchScope::All,
            message_type: SessionMessageType::All,
            roles,
            time_range,
            git_filter: GitScopeFilter::default(),
            workflow_scope: None,
        },
        false,
        SessionRetrievalStoreScope::Project,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_scope_is_unavailable_and_never_reported_as_locked() {
        let DashboardLcmReadOutcomeV1::NotReady { state, reason } = wrong_scope_not_ready() else {
            panic!("wrong scope must be terminal");
        };

        assert_eq!(state, DashboardLcmReadStateV1::Unavailable);
        assert_eq!(reason, "lcm_temporal_wrong_scope");
        assert_eq!(
            wrong_scope_error(),
            (
                DashboardLcmReadStateV1::Unavailable,
                "lcm_temporal_wrong_scope"
            )
        );
    }

    #[test]
    fn summary_hydration_has_a_small_fixed_concurrency_bound() {
        assert_eq!(SUMMARY_DESCRIBE_CONCURRENCY, 8);
    }

    #[test]
    fn dashboard_session_page_preserves_temporal_cursor_and_exact_limit() {
        let request = DashboardLcmReadRequestV1::Session {
            session_id: "session.dashboard.cursor".to_owned(),
            limit: 100,
            cursor: Some("opaque-temporal-cursor".to_owned()),
        };
        let command = retrieval_command(&request, initial_cursor(&request), false)
            .expect("cursor-backed dashboard page");

        assert_eq!(command.query().limit(), 100);
        assert_eq!(command.query().cursor(), Some("opaque-temporal-cursor"));
        assert!(command.filters().include_summaries);
    }

    #[test]
    fn dashboard_aggregate_pages_use_the_canonical_cursor_and_authorized_root() {
        let request = DashboardLcmReadRequestV1::Timeline {
            bucket: crate::dashboard::DashboardLcmTimelineBucketV1::Day,
            session_id: None,
            limit: 400,
        };
        let command = retrieval_command(
            &request,
            Some("opaque-frozen-manifest-cursor".to_owned()),
            true,
        )
        .expect("aggregate continuation");

        assert_eq!(command.query().limit(), 500);
        assert_eq!(
            command.query().cursor(),
            Some("opaque-frozen-manifest-cursor")
        );
        assert_eq!(
            command.query().retrieval_scope(),
            &SessionRetrievalScope::AllSessionsInAuthorizedRoot
        );
        assert!(command.filters().include_summaries);
    }
}
