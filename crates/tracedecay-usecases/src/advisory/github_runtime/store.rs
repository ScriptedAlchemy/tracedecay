use serde::{Deserialize, Serialize};
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
use tracedecay_runtime_core::db::{BoundedMetadataValue, Database};

const STORE_KEY_DOMAIN_V1: &str = "tracedecay.advisory.github.store-key.v1";
const STORE_KEY_PREFIX_V1: &str = "feedback.github-review.refresh.v1.";
const MAX_STORED_REFRESH_BYTES_V1: usize = 4 * 1024 * 1024;
const MANIFEST_KEY_DOMAIN_V1: &str = "tracedecay.advisory.github.manifest-key.v1";
const MANIFEST_KEY_PREFIX_V1: &str = "feedback.github-review.manifest.v1.";
const MANIFEST_SCHEMA_DOMAIN_V1: &str = "tracedecay.advisory.github.manifest-schema.v1";
const MAX_STORED_MANIFEST_BYTES_V1: usize = 1024 * 1024;
pub const MAX_GITHUB_REVIEW_STORE_MANIFEST_ENTRIES_V1: usize = 256;

/// One exact point-read identity in the project-owned GitHub review store.
/// The full request is retained because the hashed metadata key is not an
/// authority from which Delivery may reconstruct source identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewStoreManifestEntryV1 {
    pub request: GitHubReviewReadRequestV1,
    pub state_revision: ManifestDigest,
}

/// Bounded source-owned inventory for one exact feedback scope.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewStoreManifestV1 {
    pub schema_digest: ManifestDigest,
    pub scope: FeedbackScopeV1,
    pub entries: Vec<GitHubReviewStoreManifestEntryV1>,
}

impl GitHubReviewStoreManifestV1 {
    fn empty(scope: FeedbackScopeV1) -> Option<Self> {
        Some(Self {
            schema_digest: github_review_manifest_schema_digest_v1()?,
            scope,
            entries: Vec::new(),
        })
    }

    pub fn validate(&self) -> bool {
        self.scope.validate().is_ok()
            && github_review_manifest_schema_digest_v1()
                .is_some_and(|expected| self.schema_digest == expected)
            && self.entries.len() <= MAX_GITHUB_REVIEW_STORE_MANIFEST_ENTRIES_V1
            && self.entries.iter().all(|entry| {
                entry.request.validate().is_ok()
                    && entry.request.scope == self.scope
                    && entry.state_revision.validate().is_ok()
            })
            && manifest_entries_are_strictly_ordered(&self.entries)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubReviewStoreManifestLoadOutcomeV1 {
    Manifest(GitHubReviewStoreManifestV1),
    Empty,
    Unavailable,
}

fn github_review_manifest_schema_digest_v1() -> Option<ManifestDigest> {
    canonical_sha256(&MANIFEST_SCHEMA_DOMAIN_V1).ok()
}

fn manifest_entry_sort_key(entry: &GitHubReviewStoreManifestEntryV1) -> Option<ManifestDigest> {
    canonical_sha256(&entry.request).ok()
}

fn manifest_entries_are_strictly_ordered(entries: &[GitHubReviewStoreManifestEntryV1]) -> bool {
    entries
        .iter()
        .map(manifest_entry_sort_key)
        .collect::<Option<Vec<_>>>()
        .is_some_and(|keys| keys.windows(2).all(|pair| pair[0] < pair[1]))
}

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

    fn manifest_key(&self) -> Option<String> {
        canonical_sha256(&(MANIFEST_KEY_DOMAIN_V1, &self.scope))
            .ok()
            .map(|digest| format!("{MANIFEST_KEY_PREFIX_V1}{}", digest.as_str()))
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

    fn decode_bounded_entry(
        entry: &GitHubReviewStoreManifestEntryV1,
        encoded: &str,
        max_encoded_bytes: usize,
    ) -> Result<Option<(Box<GitHubReviewRefreshStateV1>, usize)>, ()> {
        let encoded_bytes = encoded.len();
        if encoded_bytes > max_encoded_bytes {
            return Err(());
        }
        if entry.request.validate().is_err()
            || entry.state_revision.validate().is_err()
            || encoded_bytes > MAX_STORED_REFRESH_BYTES_V1
        {
            return Ok(None);
        }
        let Some(state) = Self::decode(&entry.request, encoded) else {
            return Ok(None);
        };
        if state.revision != entry.state_revision {
            return Ok(None);
        }
        Ok(Some((Box::new(state), encoded_bytes)))
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

    fn decode_manifest(&self, encoded: &str) -> Option<GitHubReviewStoreManifestV1> {
        if encoded.len() > MAX_STORED_MANIFEST_BYTES_V1 {
            return None;
        }
        let manifest = serde_json::from_str::<GitHubReviewStoreManifestV1>(encoded).ok()?;
        (manifest.scope == self.scope && manifest.validate()).then_some(manifest)
    }

    fn update_manifest_entry(
        &self,
        manifest: &mut GitHubReviewStoreManifestV1,
        request: &GitHubReviewReadRequestV1,
        state_revision: &ManifestDigest,
    ) -> Option<()> {
        if !manifest.validate() || request.scope != self.scope {
            return None;
        }
        let entry = GitHubReviewStoreManifestEntryV1 {
            request: request.clone(),
            state_revision: state_revision.clone(),
        };
        let entry_key = manifest_entry_sort_key(&entry)?;
        if let Some(existing) = manifest
            .entries
            .iter_mut()
            .find(|candidate| candidate.request == *request)
        {
            *existing = entry;
        } else {
            if manifest.entries.len() == MAX_GITHUB_REVIEW_STORE_MANIFEST_ENTRIES_V1 {
                return None;
            }
            manifest.entries.push(entry);
        }
        manifest.entries.sort_by(|left, right| {
            manifest_entry_sort_key(left).cmp(&manifest_entry_sort_key(right))
        });
        manifest
            .entries
            .iter()
            .find(|candidate| candidate.request == *request)
            .and_then(manifest_entry_sort_key)
            .filter(|candidate_key| *candidate_key == entry_key)?;
        manifest.validate().then_some(())
    }

    /// Loads the bounded exact-scope inventory and verifies every referenced
    /// point record. A partial or corrupt inventory is never reported as an
    /// empty or complete source.
    pub async fn load_manifest(
        &self,
        context: &RequestContext,
        scope: &FeedbackScopeV1,
    ) -> GitHubReviewStoreManifestLoadOutcomeV1 {
        let manifest = match self.load_inventory_manifest(context, scope).await {
            GitHubReviewStoreManifestLoadOutcomeV1::Manifest(manifest) => manifest,
            GitHubReviewStoreManifestLoadOutcomeV1::Empty => {
                return GitHubReviewStoreManifestLoadOutcomeV1::Empty;
            }
            GitHubReviewStoreManifestLoadOutcomeV1::Unavailable => {
                return GitHubReviewStoreManifestLoadOutcomeV1::Unavailable;
            }
        };
        for entry in &manifest.entries {
            if self
                .load_bounded_entry(context, entry, MAX_STORED_REFRESH_BYTES_V1)
                .await
                .is_none()
            {
                return GitHubReviewStoreManifestLoadOutcomeV1::Unavailable;
            }
        }
        GitHubReviewStoreManifestLoadOutcomeV1::Manifest(manifest)
    }

    /// Loads only the bounded, structurally validated exact-scope inventory.
    /// Point records remain caller-budgeted and are loaded separately through
    /// [`Self::load_bounded_entry`].
    pub async fn load_inventory_manifest(
        &self,
        context: &RequestContext,
        scope: &FeedbackScopeV1,
    ) -> GitHubReviewStoreManifestLoadOutcomeV1 {
        if scope != &self.scope
            || !context_allows_feedback_operation(
                context,
                &self.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            )
        {
            return GitHubReviewStoreManifestLoadOutcomeV1::Unavailable;
        }
        let Some(manifest_key) = self.manifest_key() else {
            return GitHubReviewStoreManifestLoadOutcomeV1::Unavailable;
        };
        let encoded = match self
            .database
            .get_metadata_bounded(&manifest_key, MAX_STORED_MANIFEST_BYTES_V1)
            .await
        {
            Ok(BoundedMetadataValue::Value { value, .. }) => Some(value),
            Ok(BoundedMetadataValue::Missing) => None,
            Ok(BoundedMetadataValue::Oversized { .. }) | Err(_) => {
                return GitHubReviewStoreManifestLoadOutcomeV1::Unavailable;
            }
        };
        if !context_allows_feedback_operation(
            context,
            &self.scope,
            GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        ) {
            return GitHubReviewStoreManifestLoadOutcomeV1::Unavailable;
        }
        let Some(encoded) = encoded else {
            return GitHubReviewStoreManifestLoadOutcomeV1::Empty;
        };
        let Some(manifest) = self.decode_manifest(&encoded) else {
            return GitHubReviewStoreManifestLoadOutcomeV1::Unavailable;
        };
        if !context_allows_feedback_operation(
            context,
            &self.scope,
            GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        ) {
            return GitHubReviewStoreManifestLoadOutcomeV1::Unavailable;
        }
        GitHubReviewStoreManifestLoadOutcomeV1::Manifest(manifest)
    }

    /// Loads one inventory-bound point without decoding bytes beyond the
    /// caller's remaining budget. `None` covers absent, malformed, oversized,
    /// revision-mismatched, or no-longer-authorized records.
    pub async fn load_bounded_entry(
        &self,
        context: &RequestContext,
        entry: &GitHubReviewStoreManifestEntryV1,
        max_encoded_bytes: usize,
    ) -> Option<(Box<GitHubReviewRefreshStateV1>, usize)> {
        if entry.request.scope != self.scope
            || entry.request.validate().is_err()
            || entry.state_revision.validate().is_err()
            || !context_allows_feedback_operation(
                context,
                &self.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            )
        {
            return None;
        }
        let key = self.key(&entry.request)?;
        let remaining = max_encoded_bytes.min(MAX_STORED_REFRESH_BYTES_V1);
        let Ok(BoundedMetadataValue::Value {
            value: encoded,
            encoded_bytes,
        }) = self.database.get_metadata_bounded(&key, remaining).await
        else {
            return None;
        };
        if !context_allows_feedback_operation(
            context,
            &self.scope,
            GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        ) {
            return None;
        }
        let (state, decoded_bytes) = Self::decode_bounded_entry(entry, &encoded, remaining)
            .ok()
            .flatten()?;
        if decoded_bytes != encoded_bytes {
            return None;
        }
        context_allows_feedback_operation(
            context,
            &self.scope,
            GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        )
        .then_some((state, encoded_bytes))
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
            let Some(manifest_key) = self.manifest_key() else {
                let _ = transaction.rollback().await;
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            };
            let Ok(encoded_manifest) = self
                .database
                .get_metadata_unguarded(&transaction, &manifest_key)
                .await
            else {
                let _ = transaction.rollback().await;
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            };
            let mut manifest = match encoded_manifest {
                Some(encoded) => {
                    let Some(manifest) = self.decode_manifest(&encoded) else {
                        let _ = transaction.rollback().await;
                        return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
                    };
                    manifest
                }
                None if current.is_none() => {
                    let Some(manifest) = GitHubReviewStoreManifestV1::empty(self.scope.clone())
                    else {
                        let _ = transaction.rollback().await;
                        return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
                    };
                    manifest
                }
                None => {
                    let _ = transaction.rollback().await;
                    return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
                }
            };
            let manifest_entry = manifest
                .entries
                .iter()
                .find(|entry| entry.request == *request);
            if current.is_some() != manifest_entry.is_some()
                || current.as_ref().is_some_and(|state| {
                    manifest_entry.is_none_or(|entry| entry.state_revision != state.revision)
                })
            {
                let _ = transaction.rollback().await;
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            }
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
            if self
                .update_manifest_entry(&mut manifest, request, &next.revision)
                .is_none()
            {
                let _ = transaction.rollback().await;
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            }
            let Ok(encoded_manifest) = serde_json::to_string(&manifest) else {
                let _ = transaction.rollback().await;
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
            };
            if encoded_manifest.len() > MAX_STORED_MANIFEST_BYTES_V1 {
                let _ = transaction.rollback().await;
                return GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable;
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
                || self
                    .database
                    .set_metadata_unguarded(&transaction, &manifest_key, &encoded_manifest)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_application::feedback::GitHubReviewReadResponseV1;
    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::feedback::{
        GitHubPullRequestIdV1, GitHubReviewCoverageV1, GitHubReviewIngressProviderOutcomeV1,
        GitHubReviewIngressResultV1, GitHubReviewReadCheckpointV1, GitHubReviewReadOperationV1,
    };
    use tracedecay_domain::{
        ActorId, CommitId, ProjectId, ProviderId, RefId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::super::{
        GitHubReviewRefreshAttemptDispositionV1, GitHubReviewRefreshAttemptReceiptV1,
        github_review_scan_digest,
    };
    use super::*;

    const SHA: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn context_and_request() -> (RequestContext, GitHubReviewReadRequestV1) {
        let project_id = ProjectId::new("project.github.store-manifest").unwrap();
        let repository_id = RepositoryId::new("repository.github.store-manifest").unwrap();
        let worktree_id = WorktreeId::new("worktree.github.store-manifest").unwrap();
        let resolved_scope = ResolvedScope::new(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            Some(RefId::new("refs/heads/github-store-manifest").unwrap()),
        )
        .unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.github.store-manifest").unwrap(),
            1,
            ManifestDigest::new(SHA).unwrap(),
            ActorId::new("actor.github.store-manifest.issuer").unwrap(),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            resolved_scope.clone(),
            BTreeSet::from([CapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1).unwrap()]),
            BTreeSet::from([UseCaseId::new(GITHUB_REVIEW_INGEST_USE_CASE_ID_V1).unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        let context = RequestContext::new(
            ActorId::new("actor.github.store-manifest").unwrap(),
            resolved_scope,
            grant,
            RequestId::new("request.github.store-manifest").unwrap(),
            Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
            CancellationContext::active("cancel.github.store-manifest").unwrap(),
        )
        .unwrap();
        let scope = FeedbackScopeV1 {
            project_id,
            repository_id,
            worktree_id,
            branch_ref: "refs/heads/github-store-manifest".to_owned(),
            head_commit_id: CommitId::new("commit.github.store-manifest").unwrap(),
        };
        (
            context,
            GitHubReviewReadRequestV1 {
                operation: GitHubReviewReadOperationV1::RestListPullRequestReviewComments,
                scope,
                pull_request_id: GitHubPullRequestIdV1::new("pull-request.github.store-manifest")
                    .unwrap(),
            },
        )
    }

    fn complete_response(request: &GitHubReviewReadRequestV1) -> GitHubReviewReadResponseV1 {
        GitHubReviewReadResponseV1 {
            ingress: GitHubReviewIngressResultV1 {
                provider: ProviderId::new("github").unwrap(),
                scope: request.scope.clone(),
                pull_request_id: request.pull_request_id.clone(),
                provider_base_commit_id: CommitId::new("commit.github.base").unwrap(),
                provider_head_commit_id: request.scope.head_commit_id.clone(),
                merge_base_commit_id: CommitId::new("commit.github.merge-base").unwrap(),
                operation: request.operation,
                outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
                coverage: GitHubReviewCoverageV1::Complete,
                items: Vec::new(),
                pull_request: None,
                fetched_at: UtcMicros(11),
            },
            checkpoint: GitHubReviewReadCheckpointV1 {
                etag: None,
                next_cursor: None,
                rate_limit: None,
            },
        }
    }

    fn context_without_github_grant(context: &RequestContext) -> RequestContext {
        let mut grant = context.grant().clone();
        grant.allowed_capabilities =
            BTreeSet::from([
                CapabilityId::new("capability.github.store-manifest.foreign").unwrap(),
            ]);
        grant.allowed_use_cases =
            BTreeSet::from([UseCaseId::new("use-case.github.store-manifest.foreign").unwrap()]);
        RequestContext::new(
            context.actor().clone(),
            context.scope().clone(),
            grant,
            context.request_id().clone(),
            context.deadline().clone(),
            context.cancellation().clone(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn inventory_manifest_requires_the_exact_github_grant() {
        let (context, request) = context_and_request();
        let denied = context_without_github_grant(&context);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("github-review-inventory-auth.db");
        crate::register_test_schema_installer();
        let authority =
            DatabaseAuthority::acquire_test(&path, "github-source-inventory-auth").unwrap();
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        let store = ProjectGitHubReviewStoreV1::new(database, request.scope.clone()).unwrap();
        let manifest = GitHubReviewStoreManifestV1::empty(request.scope.clone()).unwrap();
        store
            .database
            .set_metadata(
                &store.manifest_key().unwrap(),
                &serde_json::to_string(&manifest).unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            store.load_inventory_manifest(&denied, &request.scope).await,
            GitHubReviewStoreManifestLoadOutcomeV1::Unavailable
        );
    }

    #[tokio::test]
    async fn inventory_manifest_rejects_malformed_data_without_point_reads() {
        let (context, request) = context_and_request();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("github-review-inventory-structure.db");
        crate::register_test_schema_installer();
        let authority =
            DatabaseAuthority::acquire_test(&path, "github-source-inventory-structure").unwrap();
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        let store = ProjectGitHubReviewStoreV1::new(database, request.scope.clone()).unwrap();
        let manifest = GitHubReviewStoreManifestV1 {
            schema_digest: github_review_manifest_schema_digest_v1().unwrap(),
            scope: request.scope.clone(),
            entries: vec![GitHubReviewStoreManifestEntryV1 {
                request: request.clone(),
                state_revision: ManifestDigest::new(SHA).unwrap(),
            }],
        };
        store
            .database
            .set_metadata(
                &store.manifest_key().unwrap(),
                &serde_json::to_string(&manifest).unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .load_inventory_manifest(&context, &request.scope)
                .await,
            GitHubReviewStoreManifestLoadOutcomeV1::Manifest(manifest)
        );

        store
            .database
            .set_metadata(
                &store.manifest_key().unwrap(),
                "{\"schema_digest\":\"corrupt\"}",
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .load_inventory_manifest(&context, &request.scope)
                .await,
            GitHubReviewStoreManifestLoadOutcomeV1::Unavailable
        );
    }

    #[tokio::test]
    async fn bounded_entry_rejects_encoded_bytes_before_deserialization() {
        let (context, request) = context_and_request();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("github-review-bounded-entry.db");
        crate::register_test_schema_installer();
        let authority =
            DatabaseAuthority::acquire_test(&path, "github-source-bounded-entry").unwrap();
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        let store = ProjectGitHubReviewStoreV1::new(database, request.scope.clone()).unwrap();
        let entry = GitHubReviewStoreManifestEntryV1 {
            request: request.clone(),
            state_revision: ManifestDigest::new(SHA).unwrap(),
        };
        store
            .database
            .set_metadata(&store.key(&request).unwrap(), "{")
            .await
            .unwrap();

        assert_eq!(
            ProjectGitHubReviewStoreV1::decode_bounded_entry(&entry, "{", 0),
            Err(())
        );
        assert_eq!(
            ProjectGitHubReviewStoreV1::decode_bounded_entry(&entry, "{", 1),
            Ok(None)
        );
        assert_eq!(store.load_bounded_entry(&context, &entry, 0).await, None);
    }

    #[tokio::test]
    async fn bounded_inventory_reads_fail_closed_after_cancellation() {
        let (context, request) = context_and_request();
        let cancelled = context.with_cancellation(
            CancellationContext::cancelled("cancel.github.store-bounded", UtcMicros(12)).unwrap(),
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("github-review-bounded-cancelled.db");
        crate::register_test_schema_installer();
        let authority =
            DatabaseAuthority::acquire_test(&path, "github-source-bounded-cancelled").unwrap();
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        let store = ProjectGitHubReviewStoreV1::new(database, request.scope.clone()).unwrap();
        let entry = GitHubReviewStoreManifestEntryV1 {
            request: request.clone(),
            state_revision: ManifestDigest::new(SHA).unwrap(),
        };

        assert_eq!(
            store
                .load_inventory_manifest(&cancelled, &request.scope)
                .await,
            GitHubReviewStoreManifestLoadOutcomeV1::Unavailable
        );
        assert_eq!(store.load_bounded_entry(&cancelled, &entry, 1).await, None);
    }

    #[tokio::test]
    async fn project_store_restarts_and_replays_the_bounded_github_manifest() {
        let (context, request) = context_and_request();
        let response = complete_response(&request);
        let digest = github_review_scan_digest(&request, &response).unwrap();
        let next = GitHubReviewRefreshStateV1::transition_with_receipt(
            &request,
            None,
            response.clone(),
            Some(GitHubReviewRefreshAttemptReceiptV1 {
                disposition: GitHubReviewRefreshAttemptDispositionV1::Agreed,
                scan_digests: vec![digest.clone(), digest],
                observed_at: response.ingress.fetched_at,
            }),
        )
        .expect("complete GitHub response creates a refresh state");
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("github-review-manifest.db");
        crate::register_test_schema_installer();
        let authority =
            DatabaseAuthority::acquire_test(&path, "github-source-manifest-restart").unwrap();
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        let store =
            ProjectGitHubReviewStoreV1::new(database.clone(), request.scope.clone()).unwrap();

        assert_eq!(
            store
                .compare_and_record(&context, &request, None, &next)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Recorded
        );
        drop(store);
        database.close();

        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Existing)
                .await
                .unwrap();
        let store =
            ProjectGitHubReviewStoreV1::new(database.clone(), request.scope.clone()).unwrap();
        assert_eq!(
            store.load(&context, &request).await,
            GitHubReviewRefreshStoreReadOutcomeV1::State(Box::new(next.clone()))
        );
        assert_eq!(
            store.load_manifest(&context, &request.scope).await,
            GitHubReviewStoreManifestLoadOutcomeV1::Manifest(GitHubReviewStoreManifestV1 {
                schema_digest: github_review_manifest_schema_digest_v1().unwrap(),
                scope: request.scope.clone(),
                entries: vec![GitHubReviewStoreManifestEntryV1 {
                    request: request.clone(),
                    state_revision: next.revision.clone(),
                }],
            })
        );
        let encoded_bytes = serde_json::to_string(&next).unwrap().len();
        let entry = GitHubReviewStoreManifestEntryV1 {
            request: request.clone(),
            state_revision: next.revision.clone(),
        };
        assert_eq!(
            store
                .load_bounded_entry(&context, &entry, encoded_bytes)
                .await,
            Some((Box::new(next.clone()), encoded_bytes))
        );
        let mismatched_entry = GitHubReviewStoreManifestEntryV1 {
            state_revision: ManifestDigest::new(SHA).unwrap(),
            ..entry
        };
        assert_eq!(
            store
                .load_bounded_entry(&context, &mismatched_entry, encoded_bytes)
                .await,
            None
        );
        let mut foreign_scope = request.scope.clone();
        foreign_scope.branch_ref = "refs/heads/foreign".to_owned();
        assert_eq!(
            store.load_manifest(&context, &foreign_scope).await,
            GitHubReviewStoreManifestLoadOutcomeV1::Unavailable
        );
        assert_eq!(
            store
                .compare_and_record(&context, &request, Some(&next.revision), &next)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Duplicate
        );

        let manifest_key = store.manifest_key().unwrap();
        database
            .set_metadata(&manifest_key, "{\"schema_digest\":\"corrupt\"}")
            .await
            .unwrap();
        assert_eq!(
            store.load_manifest(&context, &request.scope).await,
            GitHubReviewStoreManifestLoadOutcomeV1::Unavailable
        );
        assert_eq!(
            store
                .compare_and_record(&context, &request, Some(&next.revision), &next)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable
        );
        assert_eq!(
            store.load(&context, &request).await,
            GitHubReviewRefreshStoreReadOutcomeV1::State(Box::new(next))
        );
    }
}
