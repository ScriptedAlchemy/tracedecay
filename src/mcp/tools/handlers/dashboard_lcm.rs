use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalModeV1};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::session::{SessionRetrievalScope, SessionTemporalQuery};

use crate::dashboard::{
    DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1, DashboardLcmCanonicalStatsV1,
    DashboardLcmCanonicalSummaryV1, DashboardLcmReadFutureV1, DashboardLcmReadOutcomeV1,
    DashboardLcmReadPortV1, DashboardLcmReadRequestV1, DashboardLcmReadStateV1,
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

struct SummaryHydrationRequest {
    provider: String,
    session_id: SessionId,
    node_id: String,
    content: String,
}

pub(super) struct DashboardLcmReadAdapter {
    retrieval: Arc<dyn SessionRetrievalServicePort>,
    project_id: String,
}

impl DashboardLcmReadAdapter {
    pub(super) fn new(retrieval: Arc<dyn SessionRetrievalServicePort>, project_id: String) -> Self {
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
        let Some(command) = retrieval_command(&request) else {
            return not_ready(
                DashboardLcmReadStateV1::Unavailable,
                "lcm_dashboard_request_invalid",
            );
        };
        let (page, omitted) = match self.retrieval.execute(command).await {
            SessionRetrievalServiceOutcome::Complete { page, .. } => (page, 0),
            SessionRetrievalServiceOutcome::CompleteZero { temporal, .. } => (
                SessionRetrievalPageView {
                    results: Vec::new(),
                    temporal,
                },
                0,
            ),
            SessionRetrievalServiceOutcome::Partial { page, omitted, .. } => (page, omitted),
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

        let mut partial_description_count = 0_u64;
        let session_description = match &request {
            DashboardLcmReadRequestV1::Session { session_id, .. } => {
                let session_id = match SessionId::new(session_id) {
                    Ok(session_id) => session_id,
                    Err(_) => {
                        return not_ready(
                            DashboardLcmReadStateV1::Unavailable,
                            "lcm_dashboard_request_invalid",
                        );
                    }
                };
                match self
                    .describe(
                        "all",
                        session_id,
                        LcmDescribeTarget::Session,
                        RetrievalGrainV1::Session,
                    )
                    .await
                {
                    Ok((description, partial)) => {
                        partial_description_count = u64::from(partial);
                        Some(description)
                    }
                    Err((state, reason)) => return not_ready(state, reason),
                }
            }
            DashboardLcmReadRequestV1::Search { .. } => None,
        };

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

        let stats = if let Some(description) = session_description {
            DashboardLcmCanonicalStatsV1 {
                message_count: description.raw_message_count,
                summary_node_count: description.summary_node_count,
                summary_token_count: None,
                source_token_count: None,
            }
        } else {
            DashboardLcmCanonicalStatsV1::default()
        };

        if messages.is_empty()
            && summary_nodes.is_empty()
            && stats.message_count == 0
            && stats.summary_node_count == 0
        {
            let reason = match request {
                DashboardLcmReadRequestV1::Search { .. } => "lcm_no_temporal_results",
                DashboardLcmReadRequestV1::Session { .. } => "lcm_session_absent",
            };
            return not_ready(DashboardLcmReadStateV1::Absent, reason);
        }

        let next_cursor = page.temporal.cursor;
        let canonical_page = DashboardLcmCanonicalPageV1 {
            messages,
            summary_nodes,
            stats,
            has_more: next_cursor.is_some(),
            next_cursor,
        };
        let total_omitted = omitted.saturating_add(partial_description_count);
        if total_omitted > 0 {
            DashboardLcmReadOutcomeV1::Partial {
                page: canonical_page,
                omitted: total_omitted,
            }
        } else {
            DashboardLcmReadOutcomeV1::Ready(canonical_page)
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

fn retrieval_command(request: &DashboardLcmReadRequestV1) -> Option<SessionRetrievalCommand> {
    let (session_id, cursor, query_text, limit, retrieval_scope, roles, source, time_range) =
        match request {
            DashboardLcmReadRequestV1::Search {
                query,
                limit,
                cursor,
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
                    cursor.clone(),
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
                cursor,
            } => {
                let session = SessionId::new(session_id).ok()?;
                (
                    session.clone(),
                    cursor.clone(),
                    "",
                    *limit,
                    SessionRetrievalScope::Session(session),
                    Vec::new(),
                    None,
                    SessionSearchTimeRange::default(),
                )
            }
        };
    let limit = usize::try_from(limit.clamp(1, 500)).ok()?;
    let query = SessionTemporalQuery::new(
        session_id,
        None,
        query_text,
        cursor,
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        limit,
        DiversityLimits::default(),
        ContextBudget {
            max_bytes: 1024 * 1024,
            max_tokens: 256 * 1024,
            estimator_version: "words-v1".to_owned(),
        },
    )
    .ok()?
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
        let command = retrieval_command(&DashboardLcmReadRequestV1::Session {
            session_id: "session.dashboard.cursor".to_owned(),
            limit: 100,
            cursor: Some("opaque-temporal-cursor".to_owned()),
        })
        .expect("cursor-backed dashboard page");

        assert_eq!(command.query().limit(), 100);
        assert_eq!(command.query().cursor(), Some("opaque-temporal-cursor"));
        assert!(command.filters().include_summaries);
    }
}
