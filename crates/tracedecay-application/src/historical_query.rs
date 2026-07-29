//! Scope-bound historical source queries over the Plan 36 Git read port.
//!
//! This is an additional evidence lane. It does not replace current
//! exact/lexical/graph retrieval, and it never treats labels or expected
//! output as source authorization.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::ContentDigest;
use tracedecay_domain::git::GitOidV1;

use crate::context::ResolvedScope;
use crate::git::{
    GIT_HISTORICAL_BLOB_MAX_BYTES, GitHistoricalBlobReadPort, GitHistoricalBlobRequestV1,
    GitHistoricalBlobV1, GitIntelligenceError, is_canonical_repository_relative_path,
};

const MAX_COMMITS: usize = 256;
const MAX_PATHS: usize = 128;
const MAX_TERMS: usize = 32;
const MAX_TERM_BYTES: usize = 256;
const MAX_RESULTS: usize = 1_024;
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

/// Exact source authority derived from an authenticated source binding.
///
/// The typed project/repository/worktree scope and both allowlists must match
/// the provider mount. Mutable labels are not accepted as authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoricalSourceAuthorizationV1 {
    scope: ResolvedScope,
    commits: BTreeSet<GitOidV1>,
    paths: BTreeSet<String>,
}

impl HistoricalSourceAuthorizationV1 {
    pub fn new(
        scope: ResolvedScope,
        commits: impl IntoIterator<Item = GitOidV1>,
        paths: impl IntoIterator<Item = String>,
    ) -> Result<Self, HistoricalQueryError> {
        scope
            .validate()
            .map_err(|_| HistoricalQueryError::InvalidAuthorization)?;
        let commits = commits.into_iter().collect::<BTreeSet<_>>();
        let paths = paths.into_iter().collect::<BTreeSet<_>>();
        if commits.is_empty() || paths.is_empty() {
            return Err(HistoricalQueryError::MissingAuthorization);
        }
        if commits.len() > MAX_COMMITS || paths.len() > MAX_PATHS {
            return Err(HistoricalQueryError::InvalidBounds);
        }
        for path in &paths {
            validate_path(path)?;
        }
        Ok(Self {
            scope,
            commits,
            paths,
        })
    }

    pub fn scope(&self) -> &ResolvedScope {
        &self.scope
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoricalRenameModeV1 {
    ExactPath,
    FollowExactObjectRenames,
}

/// A bounded technical-term query. `commits` are ordered newest-to-oldest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoricalQueryRequestV1 {
    pub commits: Vec<GitOidV1>,
    pub paths: Vec<String>,
    pub terms: Vec<String>,
    pub rename_mode: HistoricalRenameModeV1,
    pub max_results: usize,
    pub max_blob_bytes: u64,
    pub max_total_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoricalRenameCoverageV1 {
    NotRequested,
    Complete {
        renames_followed: u32,
    },
    Unsupported {
        newer_commit: GitOidV1,
        older_commit: GitOidV1,
        path: String,
        reason: HistoricalRenameUnsupportedV1,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoricalRenameUnsupportedV1 {
    NoExactObjectPredecessor,
    AmbiguousExactObjectPredecessor,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoricalQueryCoverageV1 {
    pub commits_requested: u32,
    pub commits_scanned: u32,
    pub paths_requested: u32,
    pub blobs_scanned: u32,
    pub bytes_scanned: u64,
    pub oversized_blobs_skipped: u32,
    pub truncated: bool,
    pub rename: HistoricalRenameCoverageV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoricalTermAnchorV1 {
    pub term: String,
    pub line: u32,
    pub byte_start: u64,
    pub byte_end: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoricalContentEvidenceV1 {
    pub repository_id: tracedecay_domain::RepositoryId,
    pub worktree_id: tracedecay_domain::WorktreeId,
    pub commit: GitOidV1,
    pub path: String,
    pub blob_oid: GitOidV1,
    pub content_digest: ContentDigest,
    pub anchors: Vec<HistoricalTermAnchorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoricalQueryResultV1 {
    pub scope: ResolvedScope,
    pub evidence: Vec<HistoricalContentEvidenceV1>,
    pub coverage: HistoricalQueryCoverageV1,
}

#[derive(Debug, Error)]
pub enum HistoricalQueryError {
    #[error("historical source authorization is required")]
    MissingAuthorization,
    #[error("historical source authorization is invalid")]
    InvalidAuthorization,
    #[error("historical source authorization does not match the mounted scope")]
    ScopeMismatch,
    #[error("historical query bounds are invalid")]
    InvalidBounds,
    #[error("historical query contains an invalid repository-relative path: {0}")]
    InvalidPath(String),
    #[error("historical query contains an invalid technical term")]
    InvalidTerm,
    #[error("historical query commit is outside the authorized scope: {0}")]
    UnauthorizedCommit(GitOidV1),
    #[error("historical query path is outside the authorized scope: {0}")]
    UnauthorizedPath(String),
    #[error("Plan 36 Git read failed: {0}")]
    Git(#[from] GitIntelligenceError),
    #[error("Plan 36 Git provider returned evidence for a different scope")]
    ProviderScopeMismatch,
    #[error("Plan 36 Git provider returned a mismatched commit or path")]
    ProviderAnchorMismatch,
}

/// Code-index join over one already-mounted Plan 36 Git authority.
pub struct HistoricalGitQueryAdapter<'a, P: GitHistoricalBlobReadPort> {
    port: &'a P,
    scope: ResolvedScope,
}

impl<'a, P: GitHistoricalBlobReadPort> HistoricalGitQueryAdapter<'a, P> {
    pub fn new(port: &'a P, scope: ResolvedScope) -> Self {
        Self { port, scope }
    }

    pub fn query(
        &self,
        authorization: Option<&HistoricalSourceAuthorizationV1>,
        request: &HistoricalQueryRequestV1,
    ) -> Result<HistoricalQueryResultV1, HistoricalQueryError> {
        let authorization = authorization.ok_or(HistoricalQueryError::MissingAuthorization)?;
        if authorization.scope != self.scope {
            return Err(HistoricalQueryError::ScopeMismatch);
        }
        validate_request(authorization, request)?;

        let mut coverage = HistoricalQueryCoverageV1 {
            commits_requested: request.commits.len() as u32,
            commits_scanned: 0,
            paths_requested: request.paths.len() as u32,
            blobs_scanned: 0,
            bytes_scanned: 0,
            oversized_blobs_skipped: 0,
            truncated: false,
            rename: match request.rename_mode {
                HistoricalRenameModeV1::ExactPath => HistoricalRenameCoverageV1::NotRequested,
                HistoricalRenameModeV1::FollowExactObjectRenames => {
                    HistoricalRenameCoverageV1::Complete {
                        renames_followed: 0,
                    }
                }
            },
        };
        let mut evidence = Vec::new();
        let mut active_paths = request.paths.clone();

        'commits: for (index, commit) in request.commits.iter().enumerate() {
            coverage.commits_scanned += 1;
            for path in &active_paths {
                let blob = match self.read_blob(commit, path, request.max_blob_bytes, true) {
                    Ok(blob) => blob,
                    Err(HistoricalQueryError::Git(
                        GitIntelligenceError::HistoricalBlobBoundExceeded { .. },
                    )) => {
                        coverage.oversized_blobs_skipped += 1;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let (Some(blob_oid), Some(bytes)) = (blob.blob_oid, blob.bytes) else {
                    continue;
                };
                let size = bytes.len() as u64;
                if coverage.bytes_scanned.saturating_add(size) > request.max_total_bytes {
                    coverage.oversized_blobs_skipped += 1;
                    continue;
                }
                coverage.blobs_scanned += 1;
                coverage.bytes_scanned += size;
                let anchors = term_anchors(&bytes, &request.terms);
                if !anchors.is_empty() {
                    evidence.push(HistoricalContentEvidenceV1 {
                        repository_id: self.scope.repository_id.clone(),
                        worktree_id: self.scope.worktree_id.clone(),
                        commit: commit.clone(),
                        path: path.clone(),
                        blob_oid,
                        content_digest: ContentDigest::of_bytes(&bytes),
                        anchors,
                    });
                    if evidence.len() == request.max_results {
                        coverage.truncated = true;
                        break 'commits;
                    }
                }
            }

            if request.rename_mode == HistoricalRenameModeV1::FollowExactObjectRenames
                && let Some(older_commit) = request.commits.get(index + 1)
            {
                for path in &mut active_paths {
                    if self
                        .read_blob(older_commit, path, request.max_blob_bytes, false)?
                        .blob_oid
                        .is_some()
                    {
                        continue;
                    }
                    match self.exact_rename_predecessor(
                        commit,
                        older_commit,
                        path,
                        &authorization.paths,
                        request.max_blob_bytes,
                    )? {
                        Ok(predecessor) => {
                            *path = predecessor;
                            if let HistoricalRenameCoverageV1::Complete { renames_followed } =
                                &mut coverage.rename
                            {
                                *renames_followed += 1;
                            }
                        }
                        Err(reason) => {
                            coverage.rename = HistoricalRenameCoverageV1::Unsupported {
                                newer_commit: commit.clone(),
                                older_commit: older_commit.clone(),
                                path: path.clone(),
                                reason,
                            };
                        }
                    }
                }
            }
        }

        Ok(HistoricalQueryResultV1 {
            scope: self.scope.clone(),
            evidence,
            coverage,
        })
    }

    fn read_blob(
        &self,
        commit: &GitOidV1,
        path: &str,
        max_bytes: u64,
        include_bytes: bool,
    ) -> Result<GitHistoricalBlobV1, HistoricalQueryError> {
        let blob = self.port.historical_blob(&GitHistoricalBlobRequestV1 {
            commit: commit.clone(),
            path: path.to_owned(),
            max_bytes,
            include_bytes,
        })?;
        if blob.repository != self.scope.repository_id || blob.worktree != self.scope.worktree_id {
            return Err(HistoricalQueryError::ProviderScopeMismatch);
        }
        if blob.commit != *commit || blob.path != path {
            return Err(HistoricalQueryError::ProviderAnchorMismatch);
        }
        Ok(blob)
    }

    fn exact_rename_predecessor(
        &self,
        newer_commit: &GitOidV1,
        older_commit: &GitOidV1,
        path: &str,
        authorized_paths: &BTreeSet<String>,
        max_bytes: u64,
    ) -> Result<Result<String, HistoricalRenameUnsupportedV1>, HistoricalQueryError> {
        let Some(target) = self
            .read_blob(newer_commit, path, max_bytes, false)?
            .blob_oid
        else {
            return Ok(Err(HistoricalRenameUnsupportedV1::NoExactObjectPredecessor));
        };
        let mut candidates = Vec::new();
        for candidate in authorized_paths {
            if candidate == path {
                continue;
            }
            let older = self.read_blob(older_commit, candidate, max_bytes, false)?;
            if older.blob_oid.as_ref() != Some(&target) {
                continue;
            }
            if self
                .read_blob(newer_commit, candidate, max_bytes, false)?
                .blob_oid
                .is_none()
            {
                candidates.push(candidate.clone());
            }
        }
        Ok(match candidates.as_slice() {
            [candidate] => Ok(candidate.clone()),
            [] => Err(HistoricalRenameUnsupportedV1::NoExactObjectPredecessor),
            _ => Err(HistoricalRenameUnsupportedV1::AmbiguousExactObjectPredecessor),
        })
    }
}

fn validate_request(
    authorization: &HistoricalSourceAuthorizationV1,
    request: &HistoricalQueryRequestV1,
) -> Result<(), HistoricalQueryError> {
    if request.commits.is_empty()
        || request.paths.is_empty()
        || request.terms.is_empty()
        || request.commits.len() > MAX_COMMITS
        || request.paths.len() > MAX_PATHS
        || request.terms.len() > MAX_TERMS
        || request.max_results == 0
        || request.max_results > MAX_RESULTS
        || request.max_blob_bytes == 0
        || request.max_blob_bytes > GIT_HISTORICAL_BLOB_MAX_BYTES
        || request.max_total_bytes == 0
        || request.max_total_bytes > MAX_TOTAL_BYTES
    {
        return Err(HistoricalQueryError::InvalidBounds);
    }
    let mut seen_commits = BTreeSet::new();
    for commit in &request.commits {
        commit
            .validate()
            .map_err(|_| HistoricalQueryError::InvalidBounds)?;
        if !seen_commits.insert(commit) {
            return Err(HistoricalQueryError::InvalidBounds);
        }
        if !authorization.commits.contains(commit) {
            return Err(HistoricalQueryError::UnauthorizedCommit(commit.clone()));
        }
    }
    let mut seen_paths = BTreeSet::new();
    for path in &request.paths {
        validate_path(path)?;
        if !seen_paths.insert(path) {
            return Err(HistoricalQueryError::InvalidBounds);
        }
        if !authorization.paths.contains(path) {
            return Err(HistoricalQueryError::UnauthorizedPath(path.clone()));
        }
    }
    let mut seen_terms = BTreeSet::new();
    if request.terms.iter().any(|term| {
        term.is_empty()
            || term.len() > MAX_TERM_BYTES
            || term.chars().any(char::is_control)
            || !seen_terms.insert(term)
    }) {
        return Err(HistoricalQueryError::InvalidTerm);
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), HistoricalQueryError> {
    if !is_canonical_repository_relative_path(path) {
        return Err(HistoricalQueryError::InvalidPath(path.to_owned()));
    }
    Ok(())
}

fn term_anchors(bytes: &[u8], terms: &[String]) -> Vec<HistoricalTermAnchorV1> {
    terms
        .iter()
        .filter_map(|term| {
            let start = bytes
                .windows(term.len())
                .position(|window| window == term.as_bytes())?;
            Some(HistoricalTermAnchorV1 {
                term: term.clone(),
                line: bytes[..start].split(|byte| *byte == b'\n').count() as u32,
                byte_start: start as u64,
                byte_end: (start + term.len()) as u64,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};

    use super::*;

    struct FixtureGitPort {
        scope: ResolvedScope,
        blobs: BTreeMap<(GitOidV1, String), (GitOidV1, Vec<u8>)>,
    }

    impl GitHistoricalBlobReadPort for FixtureGitPort {
        fn historical_blob(
            &self,
            request: &GitHistoricalBlobRequestV1,
        ) -> Result<GitHistoricalBlobV1, GitIntelligenceError> {
            let (blob_oid, bytes) = self
                .blobs
                .get(&(request.commit.clone(), request.path.clone()))
                .map(|(oid, bytes)| {
                    (
                        Some(oid.clone()),
                        request.include_bytes.then(|| bytes.clone()),
                    )
                })
                .unwrap_or((None, None));
            Ok(GitHistoricalBlobV1 {
                repository: self.scope.repository_id.clone(),
                worktree: self.scope.worktree_id.clone(),
                commit: request.commit.clone(),
                path: request.path.clone(),
                blob_oid,
                bytes,
            })
        }
    }

    fn scope() -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new("project.fixture").unwrap(),
            RepositoryId::new("repository.fixture").unwrap(),
            WorktreeId::new("worktree.fixture").unwrap(),
            None,
        )
        .unwrap()
    }

    fn oid(byte: char) -> GitOidV1 {
        GitOidV1::new(byte.to_string().repeat(40)).unwrap()
    }

    fn request(commits: Vec<GitOidV1>) -> HistoricalQueryRequestV1 {
        HistoricalQueryRequestV1 {
            commits,
            paths: vec!["new.rs".to_owned()],
            terms: vec!["technical_adapter".to_owned()],
            rename_mode: HistoricalRenameModeV1::FollowExactObjectRenames,
            max_results: 8,
            max_blob_bytes: 1024,
            max_total_bytes: 4096,
        }
    }

    #[test]
    fn queries_scope_bound_blobs_and_follows_exact_rename() {
        let content = b"fn technical_adapter() {}\n";
        let older = oid('a');
        let newer = oid('b');
        let blob_oid = oid('c');
        let scope = scope();
        let authorization = HistoricalSourceAuthorizationV1::new(
            scope.clone(),
            [newer.clone(), older.clone()],
            ["new.rs".to_owned(), "old.rs".to_owned()],
        )
        .unwrap();
        let port = FixtureGitPort {
            scope: scope.clone(),
            blobs: BTreeMap::from([
                (
                    (newer.clone(), "new.rs".to_owned()),
                    (blob_oid.clone(), content.to_vec()),
                ),
                (
                    (older.clone(), "old.rs".to_owned()),
                    (blob_oid, content.to_vec()),
                ),
            ]),
        };

        let result = HistoricalGitQueryAdapter::new(&port, scope)
            .query(Some(&authorization), &request(vec![newer, older]))
            .unwrap();

        assert_eq!(result.evidence.len(), 2);
        assert_eq!(result.evidence[0].path, "new.rs");
        assert_eq!(result.evidence[1].path, "old.rs");
        assert!(matches!(
            result.coverage.rename,
            HistoricalRenameCoverageV1::Complete {
                renames_followed: 1
            }
        ));
    }

    #[test]
    fn denies_query_without_source_authorization() {
        let scope = scope();
        let port = FixtureGitPort {
            scope: scope.clone(),
            blobs: BTreeMap::new(),
        };
        let request = request(vec![GitOidV1::new("0".repeat(40)).unwrap()]);
        let error = HistoricalGitQueryAdapter::new(&port, scope)
            .query(None, &request)
            .unwrap_err();
        assert!(matches!(error, HistoricalQueryError::MissingAuthorization));
    }
}
