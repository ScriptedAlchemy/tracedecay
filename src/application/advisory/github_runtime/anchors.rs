use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{FeedbackPortFuture, GitHubReviewReadRequestV1};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubReviewCurrentBranchRemapV1, GitHubReviewImmutableAnchorV1,
    GitHubReviewRemapStateV1,
};
use tracedecay_domain::{
    CommitId, ContentDigest, FileOccurrenceId, ManifestDigest, RetrievalAnchorId, SourceSpan,
    canonical_sha256,
};

use super::{
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCanonicalReviewAnchorsV1,
    GitHubReviewAnchorSeedV1,
};
use crate::application::advisory::{GitHubCurrentBranchRemapper, context_matches_scope};
use crate::db::Database;
use crate::db::engine::params;

const ANCHOR_KEY_PREFIX_V1: &str = "feedback.github-review.anchor.v1.";
const ANCHOR_ID_DOMAIN_V1: &str = "tracedecay.pr13.github.code-anchor.v1";
const FILE_ID_DOMAIN_V1: &str = "tracedecay.pr13.github.file-occurrence.v1";
const RELATED_ANCHOR_DOMAIN_V1: &str = "tracedecay.pr13.github.related-anchor.v1";
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

    async fn resolve_seed(
        &self,
        request: &GitHubReviewReadRequestV1,
        seed: &GitHubReviewAnchorSeedV1,
    ) -> Option<GitHubCanonicalReviewAnchorsV1> {
        if request.scope != self.scope
            || !valid_relative_path(&seed.path)
            || !safe_github_review_url(&seed.safe_url)
        {
            return None;
        }
        let existing_id = original_anchor_id(&self.scope, seed).ok()?;
        if let Some(stored) = self.load(&existing_id).await? {
            return (stored.seed == *seed).then_some(stored.anchors);
        }
        let project_root = Arc::clone(&self.project_root);
        let commit = seed.original_commit_id.clone();
        let path = seed.path.clone();
        let bytes = tokio::task::spawn_blocking(move || git_blob(&project_root, &commit, &path))
            .await
            .ok()??;
        let content_digest = content_digest(&bytes)?;
        let original = immutable_anchor(
            &self.scope,
            &seed.original_commit_id,
            &seed.path,
            &content_digest,
            source_span(&bytes, seed.original_start_line, seed.original_line)?,
            Some(existing_id.clone()),
        )?;
        if original.retrieval_anchor_id != existing_id {
            return None;
        }
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
        let stored = StoredGitHubAnchorV1 {
            seed: seed.clone(),
            anchors: anchors.clone(),
        };
        self.persist(&stored).await.then_some(anchors)
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
        let current_file = self.database.get_file(&stored.seed.path).await.ok()?;
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
            &stored.seed.path,
            &current_digest,
            original.span,
            None,
        )?;
        if let Some(resolver) = self.code_index_identity.as_ref() {
            let identity = resolver.resolve(self.project_root.as_ref().clone()).await?;
            if identity.source_revision() != Some(&current_scope.head_commit_id) {
                return remap_state(original.clone(), current_scope.clone(), None, true);
            }
            let Some((file, indexed_digest)) = identity.file(&stored.seed.path) else {
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

    async fn persist(&self, candidate: &StoredGitHubAnchorV1) -> bool {
        let key = anchor_key(&candidate.anchors.original.retrieval_anchor_id);
        let Ok(encoded) = serde_json::to_string(candidate) else {
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
        if let Some(existing) = existing {
            let matches = serde_json::from_str::<StoredGitHubAnchorV1>(&existing)
                .is_ok_and(|stored| stored == *candidate);
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
}

impl GitHubCanonicalReviewAnchorAuthorityV1 for ProjectGitHubAnchorAuthorityV1 {
    fn resolve<'a>(
        &'a self,
        request: &'a GitHubReviewReadRequestV1,
        seed: &'a GitHubReviewAnchorSeedV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubCanonicalReviewAnchorsV1>> {
        Box::pin(async move { self.resolve_seed(request, seed).await })
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
        .map(|segments| segments.collect::<Vec<_>>())
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
    let digest = canonical_sha256(&(
        RELATED_ANCHOR_DOMAIN_V1,
        role,
        &scope.repository_id,
        &seed.comment_id,
        value,
    ))
    .ok()?;
    RetrievalAnchorId::new(format!("anchor.github-{role}.{}", digest_suffix(&digest)?)).ok()
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

fn source_span(
    bytes: &[u8],
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
    let mut starts = vec![0_usize];
    starts.extend(
        bytes
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1)),
    );
    let start = *starts.get(usize::try_from(start_line - 1).ok()?)?;
    let end = starts
        .get(usize::try_from(end_line).ok()?)
        .copied()
        .unwrap_or(bytes.len());
    let span = SourceSpan {
        start_byte: u64::try_from(start).ok()?,
        end_byte: u64::try_from(end).ok()?,
    };
    span.validate().ok()?;
    Some(Some(span))
}

fn git_blob(project_root: &Path, commit: &CommitId, path: &str) -> Option<Vec<u8>> {
    if !valid_git_oid(commit.as_str()) || !valid_relative_path(path) {
        return None;
    }
    let object = format!("{}:{path}", commit.as_str());
    let size = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["cat-file", "-s"])
        .arg(&object)
        .output()
        .ok()?;
    if !size.status.success() {
        return None;
    }
    let size = String::from_utf8(size.stdout)
        .ok()?
        .trim()
        .parse::<usize>()
        .ok()?;
    if size > MAX_GIT_BLOB_BYTES_V1 {
        return None;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["cat-file", "blob"])
        .arg(object)
        .output()
        .ok()?;
    (output.status.success() && output.stdout.len() == size).then_some(output.stdout)
}

fn content_digest(bytes: &[u8]) -> Option<ContentDigest> {
    let digest = Sha256::digest(bytes);
    ContentDigest::new(format!("sha256:{}", hex::encode(digest))).ok()
}

fn valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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
