use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_domain::{RetrievalGrainV1, SessionId, TemporalModeV1, canonical_sha256};
use tracedecay_temporal_query::context::ContextBudget;
use tracedecay_temporal_query::ranking::DiversityLimits;
use tracedecay_usecases::session::{
    SessionFreshnessPolicy, SessionRetrievalScope, SessionTemporalQuery,
};

use crate::mcp::tools::handlers::session::message_search::{
    SessionRetrievalCommand, SessionRetrievalFilters, SessionRetrievalServiceOutcome,
    SessionRetrievalServicePort, SessionRetrievalStoreScope,
};
use crate::sessions::git_correlation::GitScopeFilter;
use crate::sessions::{SessionMessageType, SessionSearchScope, SessionSearchTimeRange};

use super::{
    CanonicalCompactionSource, CanonicalCompactionSourceOutcome, CanonicalSourceFuture,
    LcmCanonicalCompactionSourcePort,
};

const CANONICAL_COMPACTION_SOURCE_LIMIT: usize = 500;
const CANONICAL_COMPACTION_MAX_PAGES: usize = 64;

pub(super) struct DaemonCanonicalCompactionSource {
    retrieval: Arc<dyn SessionRetrievalServicePort>,
    store_scope: SessionRetrievalStoreScope,
}

impl DaemonCanonicalCompactionSource {
    pub(super) fn new(
        retrieval: Arc<dyn SessionRetrievalServicePort>,
        store_scope: SessionRetrievalStoreScope,
    ) -> Self {
        Self {
            retrieval,
            store_scope,
        }
    }

    async fn hydrate_source(
        &self,
        provider: &str,
        session_id: &str,
    ) -> CanonicalCompactionSourceOutcome {
        let Ok(session_id) = SessionId::new(session_id) else {
            return CanonicalCompactionSourceOutcome::Unavailable;
        };
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        let mut messages = Vec::new();
        let mut anchors = Vec::new();
        let mut snapshot_pages = Vec::new();
        for _ in 0..CANONICAL_COMPACTION_MAX_PAGES {
            let Ok(query) = SessionTemporalQuery::new(
                session_id.clone(),
                Some(provider.to_owned()),
                "",
                cursor,
                TemporalModeV1::Current,
                RetrievalGrainV1::Occurrence,
                CANONICAL_COMPACTION_SOURCE_LIMIT,
                DiversityLimits::default(),
                ContextBudget {
                    max_bytes: 1024 * 1024,
                    max_tokens: 256 * 1024,
                    estimator_version: "words-v1".to_owned(),
                },
            )
            .map(|query| {
                query
                    .with_retrieval_scope(SessionRetrievalScope::Session(session_id.clone()))
                    .with_freshness_policy(SessionFreshnessPolicy::RequireFresh)
            }) else {
                return CanonicalCompactionSourceOutcome::Unavailable;
            };
            let command = SessionRetrievalCommand::new(
                query,
                SessionRetrievalFilters {
                    project_key: None,
                    parent_session_id: None,
                    source: None,
                    include_summaries: false,
                    scope: SessionSearchScope::All,
                    message_type: SessionMessageType::All,
                    roles: Vec::new(),
                    time_range: SessionSearchTimeRange::default(),
                    git_filter: GitScopeFilter::default(),
                    workflow_scope: None,
                },
                false,
                self.store_scope,
            );
            let SessionRetrievalServiceOutcome::Complete { page, .. } =
                self.retrieval.execute(command).await
            else {
                return CanonicalCompactionSourceOutcome::Unavailable;
            };
            if page.results.is_empty()
                || page.temporal.anchors.len() != page.results.len()
                || !page.temporal.omissions.is_empty()
            {
                return CanonicalCompactionSourceOutcome::Unavailable;
            }
            for (result, anchor) in page.results.into_iter().zip(&page.temporal.anchors) {
                messages.push(serde_json::json!({
                    "id": result.message.message_id,
                    "role": result.message.role,
                    "content": result.message.text,
                    "timestamp": result.message.timestamp,
                    "ordinal": result.message.ordinal,
                    "provider": result.message.provider,
                    "retrieval_anchor": anchor,
                }));
            }
            anchors.extend(page.temporal.anchors.iter().cloned());
            cursor = page.temporal.cursor.clone();
            snapshot_pages.push(page.temporal);
            let Some(next_cursor) = cursor.as_ref() else {
                let Ok(snapshot_state) =
                    canonical_sha256(&(provider, session_id.as_str(), &snapshot_pages, &messages))
                else {
                    return CanonicalCompactionSourceOutcome::Unavailable;
                };
                return CanonicalCompactionSourceOutcome::Ready(CanonicalCompactionSource {
                    messages,
                    anchors,
                    snapshot_state,
                });
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return CanonicalCompactionSourceOutcome::Unavailable;
            }
        }
        // The source snapshot is intentionally all-or-unavailable. Publishing
        // a native summary against a silently truncated source set would give
        // it false lineage.
        CanonicalCompactionSourceOutcome::Unavailable
    }
}

impl LcmCanonicalCompactionSourcePort for DaemonCanonicalCompactionSource {
    fn hydrate<'a>(&'a self, provider: &'a str, session_id: &'a str) -> CanonicalSourceFuture<'a> {
        Box::pin(async move { self.hydrate_source(provider, session_id).await })
    }
}
