use std::sync::Arc;

use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalModeV1};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::session::{SessionRetrievalScope, SessionTemporalQuery};

use crate::dashboard::{
    DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1, DashboardLcmCanonicalStatsV1,
    DashboardLcmCanonicalSummaryV1, DashboardLcmReadFutureV1, DashboardLcmReadOutcomeV1,
    DashboardLcmReadPortV1, DashboardLcmReadRequestV1,
};
use crate::global_db::RegisteredGlobalDb;
use crate::sessions::git_correlation::GitScopeFilter;
use crate::sessions::lcm::{LcmDescribeRequest, LcmDescribeTarget};
use crate::sessions::{SessionMessageType, SessionSearchScope, SessionSearchTimeRange};

use super::{
    SessionRetrievalCommand, SessionRetrievalFilters, SessionRetrievalPageView,
    SessionRetrievalServiceOutcome, SessionRetrievalServicePort, SessionRetrievalStoreScope,
};

pub(super) struct DashboardLcmReadAdapter {
    retrieval: Arc<dyn SessionRetrievalServicePort>,
    database: Arc<RegisteredGlobalDb>,
    project_id: String,
}

impl DashboardLcmReadAdapter {
    pub(super) fn new(
        retrieval: Arc<dyn SessionRetrievalServicePort>,
        database: Arc<RegisteredGlobalDb>,
        project_id: String,
    ) -> Self {
        Self {
            retrieval,
            database,
            project_id,
        }
    }

    async fn execute(
        &self,
        project_id: Option<&str>,
        request: DashboardLcmReadRequestV1,
    ) -> DashboardLcmReadOutcomeV1 {
        if project_id != Some(self.project_id.as_str()) {
            return unavailable("lcm_selected_project_authority_unavailable");
        }
        let Some(command) = retrieval_command(&request) else {
            return unavailable("lcm_dashboard_request_invalid");
        };
        let page = match self.retrieval.execute(command).await {
            SessionRetrievalServiceOutcome::Complete { page, .. } => page,
            SessionRetrievalServiceOutcome::CompleteZero { temporal, .. } => {
                SessionRetrievalPageView {
                    results: Vec::new(),
                    temporal,
                }
            }
            SessionRetrievalServiceOutcome::Partial { .. } => {
                return unavailable("lcm_temporal_read_incomplete");
            }
            SessionRetrievalServiceOutcome::Stale { .. } => {
                return unavailable("lcm_temporal_projection_stale");
            }
            SessionRetrievalServiceOutcome::WrongScope
            | SessionRetrievalServiceOutcome::Locked
            | SessionRetrievalServiceOutcome::Redacted
            | SessionRetrievalServiceOutcome::Deleted
            | SessionRetrievalServiceOutcome::Denied => {
                return unavailable("lcm_temporal_read_denied");
            }
            SessionRetrievalServiceOutcome::Unavailable(_) => {
                return unavailable("lcm_temporal_authority_unavailable");
            }
            SessionRetrievalServiceOutcome::CursorManifestLimitExceeded { .. }
            | SessionRetrievalServiceOutcome::BudgetExhausted
            | SessionRetrievalServiceOutcome::Cancelled => {
                return unavailable("lcm_temporal_read_incomplete");
            }
        };
        let status = match self
            .database
            .lcm_status("all", request_session_id(&request))
            .await
        {
            Ok(status) => status,
            Err(_) => return unavailable("lcm_status_authority_unavailable"),
        };
        let mut messages = Vec::new();
        let mut summary_nodes = Vec::new();
        for result in page.results {
            if result.message.role == "summary" {
                let description = match self
                    .database
                    .lcm_describe(LcmDescribeRequest {
                        provider: result.message.provider.clone(),
                        session_id: result.message.session_id.clone(),
                        target: LcmDescribeTarget::SummaryNode {
                            node_id: result.message.message_id.clone(),
                        },
                    })
                    .await
                {
                    Ok(description) => description,
                    Err(_) => return unavailable("lcm_summary_metadata_unavailable"),
                };
                let Some(summary) = description.summary_node else {
                    return unavailable("lcm_summary_metadata_unavailable");
                };
                summary_nodes.push(DashboardLcmCanonicalSummaryV1 {
                    node_id: summary.node_id,
                    session_id: summary.conversation_id,
                    depth: summary.depth,
                    token_count: summary.summary_token_count,
                    source_token_count: summary.source_token_count,
                    latest_at: summary.source_time_end,
                    created_at: summary.created_at,
                    expand_hint: summary.expand_hint.unwrap_or_default(),
                    summary: result.message.text,
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
        let next_cursor = page.temporal.cursor;
        DashboardLcmReadOutcomeV1::Ready(DashboardLcmCanonicalPageV1 {
            messages,
            summary_nodes,
            stats: DashboardLcmCanonicalStatsV1 {
                message_count: status.raw_message_count,
                summary_node_count: status.summary_node_count,
                summary_token_count: status.dag.total_tokens,
                source_token_count: status.dag.total_source_tokens,
                depth_counts: status
                    .dag
                    .depths
                    .into_iter()
                    .filter_map(|(depth, status)| {
                        depth
                            .strip_prefix('d')
                            .and_then(|depth| depth.parse().ok())
                            .map(|depth| (depth, status.count))
                    })
                    .collect(),
            },
            has_more: next_cursor.is_some(),
            next_cursor,
        })
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

fn unavailable(reason: &str) -> DashboardLcmReadOutcomeV1 {
    DashboardLcmReadOutcomeV1::Unavailable {
        reason: reason.to_owned(),
    }
}

fn retrieval_command(request: &DashboardLcmReadRequestV1) -> Option<SessionRetrievalCommand> {
    let (session_id, cursor, query_text, limit, retrieval_scope, roles, source, time_range) =
        match request {
            DashboardLcmReadRequestV1::Overview { query, limit } => (
                SessionId::new("session.dashboard-lcm.root").ok()?,
                None,
                query.as_str(),
                *limit,
                SessionRetrievalScope::AllSessionsInAuthorizedRoot,
                Vec::new(),
                None,
                SessionSearchTimeRange::default(),
            ),
            DashboardLcmReadRequestV1::Search {
                query,
                limit,
                offset,
                cursor,
                role,
                source,
                session_id,
                since,
                until,
            } => {
                if *offset != 0 && cursor.is_none() {
                    return None;
                }
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
                offset,
                cursor,
                order,
            } => {
                if order == "desc" || (*offset != 0 && cursor.is_none()) {
                    return None;
                }
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
            DashboardLcmReadRequestV1::Timeline {
                session_id, limit, ..
            } => {
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
                    None,
                    "",
                    *limit,
                    scope,
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

fn request_session_id(request: &DashboardLcmReadRequestV1) -> Option<&str> {
    match request {
        DashboardLcmReadRequestV1::Search { session_id, .. }
        | DashboardLcmReadRequestV1::Timeline { session_id, .. } => session_id.as_deref(),
        DashboardLcmReadRequestV1::Session { session_id, .. } => Some(session_id),
        DashboardLcmReadRequestV1::Overview { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_session_page_preserves_temporal_cursor_and_exact_limit() {
        let command = retrieval_command(&DashboardLcmReadRequestV1::Session {
            session_id: "session.dashboard.cursor".to_owned(),
            limit: 100,
            offset: 300,
            cursor: Some("opaque-temporal-cursor".to_owned()),
            order: "asc".to_owned(),
        })
        .expect("cursor-backed dashboard page");

        assert_eq!(command.query().limit(), 100);
        assert_eq!(command.query().cursor(), Some("opaque-temporal-cursor"));
        assert!(command.filters().include_summaries);
    }

    #[test]
    fn dashboard_session_page_rejects_offset_without_temporal_cursor() {
        assert!(
            retrieval_command(&DashboardLcmReadRequestV1::Session {
                session_id: "session.dashboard.offset".to_owned(),
                limit: 100,
                offset: 100,
                cursor: None,
                order: "asc".to_owned(),
            })
            .is_none()
        );
    }
}
