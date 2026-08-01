use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{FeedbackPortFuture, GitHubReviewReadRequestV1};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubReviewCurrentBranchRemapV1, GitHubReviewImmutableAnchorV1,
    GitHubReviewRemapStateV1,
};
use tracedecay_domain::git::GitOidV1;
use tracedecay_domain::{
    CommitId, ContentDigest, FileOccurrenceId, ManifestDigest, RetrievalAnchorId, SourceSpan,
    canonical_sha256,
};

use super::{
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCanonicalReviewAnchorsV1,
    GitHubProviderLifecycleV1, GitHubReviewAnchorSeedV1, GitHubSourceAccessAuthorityV1,
};
use crate::advisory::{GitHubCurrentBranchRemapper, context_matches_scope};
use tracedecay_application::git::{GitHistoricalBlobReadPort, GitHistoricalBlobRequestV1};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::params;
// SEAM: the native `git` spawn adapter is still root-owned
// (`src/git_intelligence.rs`). See `SEAMS.md`.
use crate::git_intelligence::NativeGitIntelligence;

const ANCHOR_KEY_PREFIX_V1: &str = "feedback.github-review.anchor.v1.";
const ANCHOR_ID_DOMAIN_V1: &str = "tracedecay.pr13.github.code-anchor.v1";
const FILE_ID_DOMAIN_V1: &str = "tracedecay.pr13.github.file-occurrence.v1";
const RELATED_ANCHOR_DOMAIN_V1: &str = "tracedecay.pr13.github.related-anchor.v1";
const BODY_KEY_PREFIX_V1: &str = "feedback.github-review.body.v1.";
const BODY_DIGEST_DOMAIN_V1: &str = "tracedecay.pr13.github.retained-body.v1";
const MAX_GIT_BLOB_BYTES_V1: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct ProjectGitHubAnchorAuthorityV1 {
    database: Database,
    project_root: Arc<PathBuf>,
    scope: FeedbackScopeV1,
    code_index_identity:
        Option<Arc<dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1>>,
}

pub struct ProjectGitHubRegistrarAuthoritiesV1<A> {
    pub github_remapper: A,
    pub github_anchors: A,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredGitHubAnchorV1 {
    seed: GitHubReviewAnchorSeedV1,
    anchors: GitHubCanonicalReviewAnchorsV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredGitHubReviewBodyV1 {
    scope: FeedbackScopeV1,
    pull_request_id: tracedecay_domain::feedback::GitHubPullRequestIdV1,
    comment_id: tracedecay_domain::feedback::GitHubReviewCommentIdV1,
    body_anchor: RetrievalAnchorId,
    provider_body_digest: ManifestDigest,
    retained_body_digest: ManifestDigest,
    retained_body: String,
}

struct HistoricalBlobLineIndexV1 {
    bytes: Vec<u8>,
    line_starts: Vec<usize>,
}

impl HistoricalBlobLineIndexV1 {
    fn new(bytes: Vec<u8>) -> Self {
        let mut line_starts = vec![0_usize];
        line_starts.extend(
            bytes
                .iter()
                .enumerate()
                .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1)),
        );
        Self { bytes, line_starts }
    }

    fn source_span(
        &self,
        start_line: Option<u64>,
        end_line: Option<u64>,
    ) -> Option<Option<SourceSpan>> {
        let Some(end_line) = end_line else {
            return Some(None);
        };
        let start_line = start_line.unwrap_or(end_line);
        if start_line == 0 || end_line < start_line {
            return None;
        }
        let start = *self
            .line_starts
            .get(usize::try_from(start_line - 1).ok()?)?;
        let end = self
            .line_starts
            .get(usize::try_from(end_line).ok()?)
            .copied()
            .unwrap_or(self.bytes.len());
        let span = SourceSpan {
            start_byte: u64::try_from(start).ok()?,
            end_byte: u64::try_from(end).ok()?,
        };
        span.validate().ok()?;
        Some(Some(span))
    }
}

/// Sanitized review prose returned only after exact route and GitHub source
/// authorization have both been rechecked. `Debug` and Serde are
/// intentionally absent so ordinary receipts cannot expose the body.
pub struct GitHubReviewBodyEvidenceV1 {
    pub body_anchor: RetrievalAnchorId,
    pub provider_body_digest: ManifestDigest,
    pub retained_body_digest: ManifestDigest,
    retained_body: String,
}

impl GitHubReviewBodyEvidenceV1 {
    pub fn body(&self) -> &str {
        &self.retained_body
    }
}

pub enum GitHubReviewBodyReadOutcomeV1 {
    Current(Box<GitHubReviewBodyEvidenceV1>),
    Denied,
    Stale,
    Unavailable,
}

/// Authorized expansion boundary for retained GitHub review prose. The
/// provider source is re-authorized by the concrete authority for every read.
pub trait GitHubReviewBodyEvidenceAuthorityV1: Send + Sync {
    fn read_retained_body<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
        body_anchor: &'a RetrievalAnchorId,
        source_access: &'a dyn GitHubSourceAccessAuthorityV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewBodyReadOutcomeV1>;
}

impl ProjectGitHubAnchorAuthorityV1 {
    pub fn new(
        database: Database,
        project_root: impl Into<PathBuf>,
        scope: FeedbackScopeV1,
    ) -> Option<Self> {
        let project_root = project_root.into();
        scope.validate().ok()?;
        project_root.is_absolute().then_some(Self {
            database,
            project_root: Arc::new(project_root),
            scope,
            code_index_identity: None,
        })
    }

    fn with_code_index_identity(
        mut self,
        code_index_identity: Arc<
            dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1,
        >,
    ) -> Self {
        self.code_index_identity = Some(code_index_identity);
        self
    }

    async fn resolve_seeds(
        &self,
        request: &GitHubReviewReadRequestV1,
        seeds: &[GitHubReviewAnchorSeedV1],
    ) -> Option<Vec<GitHubCanonicalReviewAnchorsV1>> {
        let mut missing = BTreeMap::new();
        for seed in seeds {
            if request.scope != self.scope
                || !valid_relative_path(&seed.path)
                || !safe_github_review_url(&seed.safe_url)
            {
                return None;
            }
            let existing_id = original_anchor_id(&self.scope, seed).ok()?;
            if self.load(&existing_id).await?.is_none() {
                missing.insert(
                    (
                        seed.original_commit_id.as_str().to_owned(),
                        seed.path.clone(),
                    ),
                    (),
                );
            }
        }

        let mut blobs = BTreeMap::new();
        for key in missing.into_keys() {
            let blob_key = key.clone();
            let project_root = Arc::clone(&self.project_root);
            let scope = self.scope.clone();
            let bytes = tokio::task::spawn_blocking(move || {
                let commit = CommitId::new(blob_key.0).ok()?;
                git_historical_blob(&project_root, &scope, &commit, &blob_key.1)
            })
            .await
            .ok()
            .flatten();
            blobs.insert(key, bytes.map(HistoricalBlobLineIndexV1::new));
        }

        let mut resolved = Vec::with_capacity(seeds.len());
        for seed in seeds {
            let existing_id = original_anchor_id(&self.scope, seed).ok()?;
            let anchors = match self.load(&existing_id).await? {
                Some(stored) => self.resolve_stored_seed(request, seed, stored).await,
                None => {
                    let key = (
                        seed.original_commit_id.as_str().to_owned(),
                        seed.path.clone(),
                    );
                    let blob = blobs.get(&key).and_then(Option::as_ref)?;
                    self.resolve_new_seed(request, seed, existing_id, blob)
                        .await
                }
            }?;
            resolved.push(anchors);
        }
        Some(resolved)
    }

    async fn resolve_stored_seed(
        &self,
        request: &GitHubReviewReadRequestV1,
        seed: &GitHubReviewAnchorSeedV1,
        stored: StoredGitHubAnchorV1,
    ) -> Option<GitHubCanonicalReviewAnchorsV1> {
        if !same_original_locator(&stored.seed, seed) {
            return None;
        }
        let original = stored.anchors.original;
        let initial_remap = self.remap_original(&original, &self.scope).await?;
        let anchors = GitHubCanonicalReviewAnchorsV1 {
            original,
            initial_remap,
            author_anchor: related_anchor("author", &self.scope, seed, &seed.author_node_id)?,
            body_anchor: related_anchor("body", &self.scope, seed, seed.body_digest.as_str())?,
            safe_url_anchor: if seed.safe_url.is_empty() {
                None
            } else {
                Some(related_anchor("url", &self.scope, seed, &seed.safe_url)?)
            },
        };
        let body = stored_body(request, seed, &anchors)?;
        self.persist_body(&body).await.then_some(anchors)
    }

    async fn resolve_new_seed(
        &self,
        request: &GitHubReviewReadRequestV1,
        seed: &GitHubReviewAnchorSeedV1,
        existing_id: RetrievalAnchorId,
        blob: &HistoricalBlobLineIndexV1,
    ) -> Option<GitHubCanonicalReviewAnchorsV1> {
        let content_digest = content_digest(&blob.bytes)?;
        let original = immutable_anchor(
            &self.scope,
            &seed.original_commit_id,
            &seed.path,
            &content_digest,
            blob.source_span(seed.original_start_line, seed.original_line)?,
            Some(existing_id.clone()),
        )?;
        if original.retrieval_anchor_id != existing_id {
            return None;
        }
        let initial_remap = self.remap_seed(&original, &self.scope, seed).await?;
        let anchors = GitHubCanonicalReviewAnchorsV1 {
            original,
            initial_remap,
            author_anchor: related_anchor("author", &self.scope, seed, &seed.author_node_id)?,
            body_anchor: related_anchor("body", &self.scope, seed, seed.body_digest.as_str())?,
            safe_url_anchor: if seed.safe_url.is_empty() {
                None
            } else {
                Some(related_anchor("url", &self.scope, seed, &seed.safe_url)?)
            },
        };
        let stored = StoredGitHubAnchorV1 {
            seed: seed.clone(),
            anchors: anchors.clone(),
        };
        let body = stored_body(request, seed, &anchors)?;
        self.persist(&stored, &body).await.then_some(anchors)
    }

    async fn remap_original(
        &self,
        original: &GitHubReviewImmutableAnchorV1,
        current_scope: &FeedbackScopeV1,
    ) -> Option<GitHubReviewCurrentBranchRemapV1> {
        if current_scope != &self.scope || original.repository_id != self.scope.repository_id {
            return None;
        }
        let stored = self.load(&original.retrieval_anchor_id).await??;
        if stored.anchors.original != *original {
            return None;
        }
        self.remap_seed(original, current_scope, &stored.seed).await
    }

    async fn remap_seed(
        &self,
        original: &GitHubReviewImmutableAnchorV1,
        current_scope: &FeedbackScopeV1,
        seed: &GitHubReviewAnchorSeedV1,
    ) -> Option<GitHubReviewCurrentBranchRemapV1> {
        let current_file = self.database.get_file(&seed.path).await.ok()?;
        let Some(current_file) = current_file else {
            return remap_state(original.clone(), current_scope.clone(), None, true);
        };
        let current_digest = ContentDigest::new(current_file.content_hash).ok()?;
        if current_digest != original.content_digest {
            return remap_state(original.clone(), current_scope.clone(), None, true);
        }
        let mut current = immutable_anchor(
            current_scope,
            &current_scope.head_commit_id,
            &seed.path,
            &current_digest,
            original.span,
            None,
        )?;
        if let Some(resolver) = self.code_index_identity.as_ref() {
            let identity = resolver.resolve(self.project_root.as_ref().clone()).await?;
            if identity.source_revision() != Some(&current_scope.head_commit_id) {
                return remap_state(original.clone(), current_scope.clone(), None, true);
            }
            let Some((file, indexed_digest)) = identity.file(&seed.path) else {
                return remap_state(original.clone(), current_scope.clone(), None, true);
            };
            if indexed_digest != &current_digest {
                return remap_state(original.clone(), current_scope.clone(), None, true);
            }
            current.file = file.clone();
            current.validate().ok()?;
        }
        remap_state(
            original.clone(),
            current_scope.clone(),
            Some(current),
            false,
        )
    }

    async fn load(&self, anchor_id: &RetrievalAnchorId) -> Option<Option<StoredGitHubAnchorV1>> {
        let key = anchor_key(anchor_id);
        match self.database.get_metadata(&key).await.ok()? {
            Some(encoded) => Some(serde_json::from_str(&encoded).ok()),
            None => Some(None),
        }
    }

    async fn persist(
        &self,
        candidate: &StoredGitHubAnchorV1,
        body: &StoredGitHubReviewBodyV1,
    ) -> bool {
        let key = anchor_key(&candidate.anchors.original.retrieval_anchor_id);
        let body_key = body_key(&body.body_anchor);
        let Ok(encoded) = serde_json::to_string(candidate) else {
            return false;
        };
        let Ok(encoded_body) = serde_json::to_string(body) else {
            return false;
        };
        let Ok(transaction) = self
            .database
            .begin_write_transaction("record GitHub canonical review anchor")
            .await
        else {
            return false;
        };
        let Ok(mut rows) = transaction
            .query_engine(
                "SELECT value FROM metadata WHERE key = ?1",
                params![key.as_str()],
            )
            .await
        else {
            return false;
        };
        let existing = match rows.next().await {
            Ok(row) => row
                .map(|row| row.get::<String>(0))
                .transpose()
                .ok()
                .flatten(),
            Err(_) => return false,
        };
        drop(rows);
        if let Some(existing) = existing.as_ref()
            && !serde_json::from_str::<StoredGitHubAnchorV1>(existing)
                .is_ok_and(|stored| stored == *candidate)
        {
            let _ = transaction.rollback().await;
            return false;
        }
        let Ok(existing_body) = self
            .database
            .get_metadata_unguarded(&transaction, &body_key)
            .await
        else {
            let _ = transaction.rollback().await;
            return false;
        };
        if existing_body
            .as_deref()
            .is_some_and(|value| value != encoded_body)
        {
            let _ = transaction.rollback().await;
            return false;
        }
        if (existing.is_none()
            && self
                .database
                .set_metadata_unguarded(&transaction, &key, &encoded)
                .await
                .is_err())
            || (existing_body.is_none()
                && self
                    .database
                    .set_metadata_unguarded(&transaction, &body_key, &encoded_body)
                    .await
                    .is_err())
        {
            let _ = transaction.rollback().await;
            return false;
        }
        transaction.commit().await.is_ok()
    }

    async fn persist_body(&self, body: &StoredGitHubReviewBodyV1) -> bool {
        let key = body_key(&body.body_anchor);
        let Ok(encoded) = serde_json::to_string(body) else {
            return false;
        };
        let Ok(transaction) = self
            .database
            .begin_write_transaction("record GitHub review body evidence")
            .await
        else {
            return false;
        };
        let Ok(existing) = self
            .database
            .get_metadata_unguarded(&transaction, &key)
            .await
        else {
            let _ = transaction.rollback().await;
            return false;
        };
        if let Some(existing) = existing {
            let matches = existing == encoded;
            let _ = transaction.rollback().await;
            return matches;
        }
        if self
            .database
            .set_metadata_unguarded(&transaction, &key, &encoded)
            .await
            .is_err()
        {
            let _ = transaction.rollback().await;
            return false;
        }
        transaction.commit().await.is_ok()
    }

    /// Expands one retained body. Possession of an anchor is insufficient:
    /// exact project/repository/worktree/ref identity and the current GitHub
    /// source authorization are checked before and after storage access.
    pub fn read_body<'a, A>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
        body_anchor: &'a RetrievalAnchorId,
        source_access: &'a A,
    ) -> FeedbackPortFuture<'a, GitHubReviewBodyReadOutcomeV1>
    where
        A: GitHubSourceAccessAuthorityV1 + Sync + ?Sized,
    {
        Box::pin(async move {
            if request.validate().is_err()
                || request.scope != self.scope
                || !context_matches_scope(context, &self.scope)
                || body_anchor.validate().is_err()
            {
                return GitHubReviewBodyReadOutcomeV1::Denied;
            }
            match source_access.authorize(context, request).await {
                GitHubProviderLifecycleV1::Ready => {}
                GitHubProviderLifecycleV1::Stale => {
                    return GitHubReviewBodyReadOutcomeV1::Stale;
                }
                GitHubProviderLifecycleV1::Denied | GitHubProviderLifecycleV1::Ambiguous => {
                    return GitHubReviewBodyReadOutcomeV1::Denied;
                }
                GitHubProviderLifecycleV1::Unavailable => {
                    return GitHubReviewBodyReadOutcomeV1::Unavailable;
                }
            }
            let Some(encoded) = self
                .database
                .get_metadata(&body_key(body_anchor))
                .await
                .ok()
                .flatten()
            else {
                return GitHubReviewBodyReadOutcomeV1::Denied;
            };
            let Some(body) = serde_json::from_str::<StoredGitHubReviewBodyV1>(&encoded)
                .ok()
                .filter(valid_stored_body)
                .filter(|body| {
                    body.scope == request.scope
                        && body.pull_request_id == request.pull_request_id
                        && &body.body_anchor == body_anchor
                })
            else {
                return GitHubReviewBodyReadOutcomeV1::Denied;
            };
            match source_access.authorize(context, request).await {
                GitHubProviderLifecycleV1::Ready => {}
                GitHubProviderLifecycleV1::Stale => {
                    return GitHubReviewBodyReadOutcomeV1::Stale;
                }
                GitHubProviderLifecycleV1::Denied | GitHubProviderLifecycleV1::Ambiguous => {
                    return GitHubReviewBodyReadOutcomeV1::Denied;
                }
                GitHubProviderLifecycleV1::Unavailable => {
                    return GitHubReviewBodyReadOutcomeV1::Unavailable;
                }
            }
            GitHubReviewBodyReadOutcomeV1::Current(Box::new(GitHubReviewBodyEvidenceV1 {
                body_anchor: body.body_anchor,
                provider_body_digest: body.provider_body_digest,
                retained_body_digest: body.retained_body_digest,
                retained_body: body.retained_body,
            }))
        })
    }
}

impl GitHubReviewBodyEvidenceAuthorityV1 for ProjectGitHubAnchorAuthorityV1 {
    fn read_retained_body<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
        body_anchor: &'a RetrievalAnchorId,
        source_access: &'a dyn GitHubSourceAccessAuthorityV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewBodyReadOutcomeV1> {
        self.read_body(context, request, body_anchor, source_access)
    }
}

impl GitHubReviewBodyEvidenceAuthorityV1 for Arc<ProjectGitHubAnchorAuthorityV1> {
    fn read_retained_body<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
        body_anchor: &'a RetrievalAnchorId,
        source_access: &'a dyn GitHubSourceAccessAuthorityV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewBodyReadOutcomeV1> {
        self.as_ref()
            .read_body(context, request, body_anchor, source_access)
    }
}

impl GitHubCanonicalReviewAnchorAuthorityV1 for ProjectGitHubAnchorAuthorityV1 {
    fn resolve<'a>(
        &'a self,
        request: &'a GitHubReviewReadRequestV1,
        seed: &'a GitHubReviewAnchorSeedV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubCanonicalReviewAnchorsV1>> {
        Box::pin(async move {
            self.resolve_seeds(request, std::slice::from_ref(seed))
                .await?
                .pop()
        })
    }

    fn resolve_many<'a>(
        &'a self,
        request: &'a GitHubReviewReadRequestV1,
        seeds: &'a [GitHubReviewAnchorSeedV1],
    ) -> FeedbackPortFuture<'a, Option<Vec<GitHubCanonicalReviewAnchorsV1>>> {
        Box::pin(async move { self.resolve_seeds(request, seeds).await })
    }
}

impl GitHubCurrentBranchRemapper for ProjectGitHubAnchorAuthorityV1 {
    fn remap<'a>(
        &'a self,
        context: &'a RequestContext,
        current_scope: &'a FeedbackScopeV1,
        original: &'a GitHubReviewImmutableAnchorV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubReviewCurrentBranchRemapV1>> {
        Box::pin(async move {
            if !context_matches_scope(context, current_scope) {
                return None;
            }
            self.remap_original(original, current_scope).await
        })
    }
}

impl GitHubCanonicalReviewAnchorAuthorityV1 for Arc<ProjectGitHubAnchorAuthorityV1> {
    fn resolve<'a>(
        &'a self,
        request: &'a GitHubReviewReadRequestV1,
        seed: &'a GitHubReviewAnchorSeedV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubCanonicalReviewAnchorsV1>> {
        self.as_ref().resolve(request, seed)
    }

    fn resolve_many<'a>(
        &'a self,
        request: &'a GitHubReviewReadRequestV1,
        seeds: &'a [GitHubReviewAnchorSeedV1],
    ) -> FeedbackPortFuture<'a, Option<Vec<GitHubCanonicalReviewAnchorsV1>>> {
        self.as_ref().resolve_many(request, seeds)
    }
}

impl GitHubCurrentBranchRemapper for Arc<ProjectGitHubAnchorAuthorityV1> {
    fn remap<'a>(
        &'a self,
        context: &'a RequestContext,
        current_scope: &'a FeedbackScopeV1,
        original: &'a GitHubReviewImmutableAnchorV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubReviewCurrentBranchRemapV1>> {
        self.as_ref().remap(context, current_scope, original)
    }
}

pub fn github_anchor_authorities_v1(
    database: Database,
    project_root: impl Into<PathBuf>,
    scope: FeedbackScopeV1,
) -> Option<ProjectGitHubRegistrarAuthoritiesV1<ProjectGitHubAnchorAuthorityV1>> {
    let authority = ProjectGitHubAnchorAuthorityV1::new(database, project_root, scope)?;
    Some(ProjectGitHubRegistrarAuthoritiesV1 {
        github_remapper: authority.clone(),
        github_anchors: authority,
    })
}

pub fn github_anchor_authorities_arc_v1(
    database: Database,
    project_root: impl Into<PathBuf>,
    scope: FeedbackScopeV1,
    code_index_identity: Arc<
        dyn crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1,
    >,
) -> Option<ProjectGitHubRegistrarAuthoritiesV1<Arc<ProjectGitHubAnchorAuthorityV1>>> {
    let authority = Arc::new(
        ProjectGitHubAnchorAuthorityV1::new(database, project_root, scope)?
            .with_code_index_identity(code_index_identity),
    );
    Some(ProjectGitHubRegistrarAuthoritiesV1 {
        github_remapper: Arc::clone(&authority),
        github_anchors: authority,
    })
}

fn immutable_anchor(
    scope: &FeedbackScopeV1,
    commit: &CommitId,
    path: &str,
    content_digest: &ContentDigest,
    span: Option<SourceSpan>,
    retrieval_anchor_id: Option<RetrievalAnchorId>,
) -> Option<GitHubReviewImmutableAnchorV1> {
    let file = file_occurrence_id(scope, commit, path, content_digest)?;
    let retrieval_anchor_id = retrieval_anchor_id.or_else(|| {
        RetrievalAnchorId::new(format!(
            "anchor.github-code.{}",
            digest_suffix(
                &canonical_sha256(&(
                    ANCHOR_ID_DOMAIN_V1,
                    &scope.repository_id,
                    commit,
                    path,
                    content_digest,
                    span,
                ))
                .ok()?,
            )?
        ))
        .ok()
    })?;
    let anchor = GitHubReviewImmutableAnchorV1 {
        repository_id: scope.repository_id.clone(),
        commit_id: commit.clone(),
        retrieval_anchor_id,
        file,
        content_digest: content_digest.clone(),
        span,
        symbol: None,
    };
    anchor.validate().ok()?;
    Some(anchor)
}

fn safe_github_review_url(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    let segments = url
        .path_segments()
        .map(std::iter::Iterator::collect::<Vec<_>>)
        .unwrap_or_default();
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && segments.len() == 4
        && segments[2] == "pull"
        && segments[3].parse::<u64>().is_ok_and(|number| number > 0)
}

fn file_occurrence_id(
    scope: &FeedbackScopeV1,
    commit: &CommitId,
    path: &str,
    content_digest: &ContentDigest,
) -> Option<FileOccurrenceId> {
    let digest = canonical_sha256(&(
        FILE_ID_DOMAIN_V1,
        &scope.repository_id,
        commit,
        path,
        content_digest,
    ))
    .ok()?;
    FileOccurrenceId::new(format!("file.github.{}", digest_suffix(&digest)?)).ok()
}

fn original_anchor_id(
    scope: &FeedbackScopeV1,
    seed: &GitHubReviewAnchorSeedV1,
) -> Result<RetrievalAnchorId, ()> {
    let digest = canonical_sha256(&(
        ANCHOR_ID_DOMAIN_V1,
        &scope.repository_id,
        &seed.original_commit_id,
        &seed.path,
        seed.original_start_line,
        seed.original_line,
    ))
    .map_err(|_| ())?;
    RetrievalAnchorId::new(format!(
        "anchor.github-code.{}",
        digest_suffix(&digest).ok_or(())?
    ))
    .map_err(|_| ())
}

fn related_anchor(
    role: &str,
    scope: &FeedbackScopeV1,
    seed: &GitHubReviewAnchorSeedV1,
    value: &str,
) -> Option<RetrievalAnchorId> {
    related_anchor_from_comment(role, scope, &seed.comment_id, value)
}

fn related_anchor_from_comment(
    role: &str,
    scope: &FeedbackScopeV1,
    comment_id: &tracedecay_domain::feedback::GitHubReviewCommentIdV1,
    value: &str,
) -> Option<RetrievalAnchorId> {
    let digest = canonical_sha256(&(
        RELATED_ANCHOR_DOMAIN_V1,
        role,
        &scope.repository_id,
        comment_id,
        value,
    ))
    .ok()?;
    RetrievalAnchorId::new(format!("anchor.github-{role}.{}", digest_suffix(&digest)?)).ok()
}

fn stored_body(
    request: &GitHubReviewReadRequestV1,
    seed: &GitHubReviewAnchorSeedV1,
    anchors: &GitHubCanonicalReviewAnchorsV1,
) -> Option<StoredGitHubReviewBodyV1> {
    let body = StoredGitHubReviewBodyV1 {
        scope: request.scope.clone(),
        pull_request_id: request.pull_request_id.clone(),
        comment_id: seed.comment_id.clone(),
        body_anchor: anchors.body_anchor.clone(),
        provider_body_digest: seed.body_digest.clone(),
        retained_body_digest: canonical_sha256(&(
            BODY_DIGEST_DOMAIN_V1,
            seed.retained_body.as_str(),
        ))
        .ok()?,
        retained_body: seed.retained_body.clone(),
    };
    valid_stored_body(&body).then_some(body)
}

fn valid_stored_body(body: &StoredGitHubReviewBodyV1) -> bool {
    body.scope.validate().is_ok()
        && body.pull_request_id.validate().is_ok()
        && body.comment_id.validate().is_ok()
        && body.body_anchor.validate().is_ok()
        && body.provider_body_digest.validate().is_ok()
        && body.retained_body_digest.validate().is_ok()
        && !body.retained_body.is_empty()
        && body.retained_body.len() <= super::MAX_GITHUB_REVIEW_BODY_BYTES_V1
        && canonical_sha256(&(BODY_DIGEST_DOMAIN_V1, body.retained_body.as_str()))
            .is_ok_and(|digest| digest == body.retained_body_digest)
        && related_anchor_from_comment(
            "body",
            &body.scope,
            &body.comment_id,
            body.provider_body_digest.as_str(),
        )
        .as_ref()
            == Some(&body.body_anchor)
}

fn same_original_locator(
    left: &GitHubReviewAnchorSeedV1,
    right: &GitHubReviewAnchorSeedV1,
) -> bool {
    left.comment_id == right.comment_id
        && left.path == right.path
        && left.original_commit_id == right.original_commit_id
        && left.original_start_line == right.original_start_line
        && left.original_line == right.original_line
}

fn remap_state(
    original: GitHubReviewImmutableAnchorV1,
    current_scope: FeedbackScopeV1,
    current: Option<GitHubReviewImmutableAnchorV1>,
    stale: bool,
) -> Option<GitHubReviewCurrentBranchRemapV1> {
    let remap = GitHubReviewCurrentBranchRemapV1 {
        original,
        current_scope,
        state: if current.is_some() {
            GitHubReviewRemapStateV1::ExactCurrent
        } else if stale {
            GitHubReviewRemapStateV1::Stale
        } else {
            GitHubReviewRemapStateV1::Unmapped
        },
        current,
    };
    remap.validate().ok()?;
    Some(remap)
}

fn git_historical_blob(
    project_root: &Path,
    scope: &FeedbackScopeV1,
    commit: &CommitId,
    path: &str,
) -> Option<Vec<u8>> {
    let commit = GitOidV1::new(commit.as_str().to_owned()).ok()?;
    let port = NativeGitIntelligence::new(
        project_root.to_path_buf(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
    );
    let blob = GitHistoricalBlobReadPort::historical_blob(
        &port,
        &GitHistoricalBlobRequestV1 {
            commit: commit.clone(),
            path: path.to_owned(),
            max_bytes: MAX_GIT_BLOB_BYTES_V1 as u64,
            include_bytes: true,
        },
    )
    .ok()?;
    if blob.repository != scope.repository_id
        || blob.worktree != scope.worktree_id
        || blob.commit != commit
        || blob.path.as_str() != path
    {
        return None;
    }
    blob.bytes
}

fn content_digest(bytes: &[u8]) -> Option<ContentDigest> {
    let digest = Sha256::digest(bytes);
    ContentDigest::new(format!("sha256:{}", hex::encode(digest))).ok()
}

fn valid_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn digest_suffix(digest: &ManifestDigest) -> Option<&str> {
    digest.as_str().split_once(':').map(|(_, suffix)| suffix)
}

fn anchor_key(anchor_id: &RetrievalAnchorId) -> String {
    format!("{ANCHOR_KEY_PREFIX_V1}{}", anchor_id.as_str())
}

fn body_key(anchor_id: &RetrievalAnchorId) -> String {
    format!("{BODY_KEY_PREFIX_V1}{}", anchor_id.as_str())
}
