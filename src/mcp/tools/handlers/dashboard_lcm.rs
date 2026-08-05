use std::sync::Arc;

use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalModeV1};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::session::{SessionRetrievalScope, SessionTemporalQuery};

use crate::dashboard::{
    DashboardLcmCanonicalMessageV1, DashboardLcmCanonicalPageV1, DashboardLcmReadFutureV1,
    DashboardLcmReadOutcomeV1, DashboardLcmReadPortV1, DashboardLcmReadRequestV1,
};
use crate::sessions::git_correlation::GitScopeFilter;
use crate::sessions::{SessionMessageType, SessionSearchScope, SessionSearchTimeRange};

use super::{
    SessionRetrievalCommand, SessionRetrievalFilters, SessionRetrievalPageView,
    SessionRetrievalServiceOutcome, SessionRetrievalServicePort, SessionRetrievalStoreScope,
};

pub(super) struct DashboardLcmReadAdapter {
    retrieval: Arc<dyn SessionRetrievalServicePort>,
}

impl DashboardLcmReadAdapter {
    pub(super) fn new(retrieval: Arc<dyn SessionRetrievalServicePort>) -> Self {
        Self { retrieval }
    }

    async fn execute(&self, request: DashboardLcmReadRequestV1) -> DashboardLcmReadOutcomeV1 {
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
        DashboardLcmReadOutcomeV1::Ready(DashboardLcmCanonicalPageV1 {
            messages: page
                .results
                .into_iter()
                .map(|result| DashboardLcmCanonicalMessageV1 {
                    session_id: result.message.session_id,
                    provider: result.message.provider,
                    role: result.message.role,
                    timestamp: result.message.timestamp,
                    ordinal: result.message.ordinal,
                    content: result.message.text,
                    message_id: result.message.message_id,
                    metadata_json: result.message.metadata_json,
                    tool_names: result.message.tool_names,
                })
                .collect(),
            has_more: page.temporal.cursor.is_some(),
        })
    }
}

impl DashboardLcmReadPortV1 for DashboardLcmReadAdapter {
    fn read(&self, request: DashboardLcmReadRequestV1) -> DashboardLcmReadFutureV1<'_> {
        Box::pin(async move { self.execute(request).await })
    }
}

fn unavailable(reason: &str) -> DashboardLcmReadOutcomeV1 {
    DashboardLcmReadOutcomeV1::Unavailable {
        reason: reason.to_owned(),
    }
}

fn retrieval_command(request: &DashboardLcmReadRequestV1) -> Option<SessionRetrievalCommand> {
    let (session_id, query_text, limit, retrieval_scope, roles, source, time_range) = match request
    {
        DashboardLcmReadRequestV1::Overview { query, limit } => (
            SessionId::new("session.dashboard-lcm.root").ok()?,
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
            role,
            source,
            session_id,
            since,
            until,
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
                query.as_str(),
                limit.saturating_add(*offset),
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
            ..
        } => {
            let session = SessionId::new(session_id).ok()?;
            (
                session.clone(),
                "",
                limit.saturating_add(*offset),
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
        None,
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
            include_summaries: false,
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
