use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    FeedbackPortFuture, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
    GitHubReviewReadRequestV1,
};
use tracedecay_domain::ManifestDigest;
use tracedecay_domain::canonical_sha256;
use tracedecay_domain::feedback::FeedbackScopeV1;

use super::{
    GitHubReadCheckpointAuthorityV1, GitHubReadCheckpointLoadOutcomeV1,
    GitHubReviewAtomicRefreshStoreV1, GitHubReviewRefreshStateV1,
    GitHubReviewRefreshStoreCommitOutcomeV1, GitHubReviewRefreshStoreReadOutcomeV1,
};
use crate::advisory::context_allows_feedback_operation;
use tracedecay_runtime_core::db::Database;

const STORE_KEY_DOMAIN_V1: &str = "tracedecay.pr13.github.store-key.v1";
const STORE_KEY_PREFIX_V1: &str = "feedback.github-review.refresh.v1.";
const MAX_STORED_REFRESH_BYTES_V1: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct ProjectGitHubReviewStoreV1 {
    database: Database,
    scope: FeedbackScopeV1,
}

impl ProjectGitHubReviewStoreV1 {
    pub fn new(database: Database, scope: FeedbackScopeV1) -> Option<Self> {
        scope.validate().ok()?;
        Some(Self { database, scope })
    }

    fn key(&self, request: &GitHubReviewReadRequestV1) -> Option<String> {
        if request.scope != self.scope {
            return None;
        }
        canonical_sha256(&(
            STORE_KEY_DOMAIN_V1,
            request.operation,
            &request.scope,
            &request.pull_request_id,
        ))
        .ok()
        .map(|digest| format!("{STORE_KEY_PREFIX_V1}{}", digest.as_str()))
    }

    fn decode(
        request: &GitHubReviewReadRequestV1,
        encoded: &str,
    ) -> Option<GitHubReviewRefreshStateV1> {
        if encoded.len() > MAX_STORED_REFRESH_BYTES_V1 {
            return None;
        }
        let state = serde_json::from_str::<GitHubReviewRefreshStateV1>(encoded).ok()?;
        state.validate_for(request).then_some(state)
    }

    async fn load_state(
        &self,
        request: &GitHubReviewReadRequestV1,
    ) -> Option<Option<GitHubReviewRefreshStateV1>> {
        let key = self.key(request)?;
        match self.database.get_metadata(&key).await.ok()? {
            Some(encoded) => Some(Some(Self::decode(request, &encoded)?)),
            None => Some(None),
        }
    }
}

impl GitHubReadCheckpointAuthorityV1 for ProjectGitHubReviewStoreV1 {
    fn load_resume<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReadCheckpointLoadOutcomeV1> {
        Box::pin(async move {
            if !context_allows_feedback_operation(
                context,
                &self.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReadCheckpointLoadOutcomeV1::Unavailable;
            }
            match self.load_state(request).await {
                Some(Some(state)) => {
                    GitHubReadCheckpointLoadOutcomeV1::Checkpoint(state.latest_attempt.checkpoint)
                }
                Some(None) => GitHubReadCheckpointLoadOutcomeV1::Empty,
                None => GitHubReadCheckpointLoadOutcomeV1::Unavailable,
            }
        })
    }
}

impl GitHubReviewAtomicRefreshStoreV1 for ProjectGitHubReviewStoreV1 {
    fn load<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreReadOutcomeV1> {
        Box::pin(async move {
            if !context_allows_feedback_operation(
                context,
                &self.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReviewRefreshStoreReadOutcomeV1::Unavailable;
            }
            match self.load_state(request).await {
                Some(Some(state)) => GitHubReviewRefreshStoreReadOutcomeV1::State(Box::new(state)),
                Some(None) => GitHubReviewRefreshStoreReadOutcomeV1::Empty,
                None => GitHubReviewRefreshStoreReadOutcomeV1::Unavailable,
            }
        })
    }

    fn compare_and_record<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
        expected_revision: Option<&'a ManifestDigest>,
        next: &'a GitHubReviewRefreshStateV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreCommitOutcomeV1> {
        Box::pin(async move {
            if !next.validate_for(request)
                || !context_allows_feedback_operation(
                    context,
                    &self.scope,
                    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                    GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
                )
            {
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            }
            let Some(key) = self.key(request) else {
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            };
            let Ok(encoded_next) = serde_json::to_string(next) else {
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            };
            if encoded_next.len() > MAX_STORED_REFRESH_BYTES_V1 {
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            }
            let Ok(transaction) = self
                .database
                .begin_write_transaction("record GitHub review refresh")
                .await
            else {
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            };
            let Ok(encoded) = self
                .database
                .get_metadata_unguarded(&transaction, &key)
                .await
            else {
                let _ = transaction.rollback().await;
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            };
            let current = match encoded {
                Some(encoded) => {
                    let Some(state) = Self::decode(request, &encoded) else {
                        let _ = transaction.rollback().await;
                        return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
                    };
                    Some(state)
                }
                None => None,
            };
            if current
                .as_ref()
                .is_some_and(|state| state.revision == next.revision)
            {
                let _ = transaction.rollback().await;
                return GitHubReviewRefreshStoreCommitOutcomeV1::Duplicate;
            }
            if current.as_ref().map(|state| &state.revision) != expected_revision {
                let _ = transaction.rollback().await;
                return GitHubReviewRefreshStoreCommitOutcomeV1::Conflict;
            }
            if !context_allows_feedback_operation(
                context,
                &self.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) || self
                .database
                .set_metadata_unguarded(&transaction, &key, &encoded_next)
                .await
                .is_err()
                || transaction.commit().await.is_err()
            {
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            }
            GitHubReviewRefreshStoreCommitOutcomeV1::Recorded
        })
    }
}
