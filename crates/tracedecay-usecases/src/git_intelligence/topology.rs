//! Stable native repository topology reads for graph projection.
//!
//! The projection double-reads the ref set around history traversal. A ref
//! change during the capture is stale evidence rather than a mixed snapshot.

use tracedecay_code_index::git_projection::{
    GitTopologyProjectionError, GitTopologyProjectionV1, GitTopologyRefV1,
    git_topology_ref_watermark,
};
use tracedecay_domain::git::{GitCoverageV1, GitHeadStateV1, GitHistoryV1};
use tracedecay_domain::research::{ManifestDigest, RefId};
use tracedecay_runtime_core::git_repository::{
    GitHistoryOptions as RepositoryHistoryOptions, GitReference, GitRepositoryAuthority,
};

use super::{
    GIT_HISTORY_MAX_COUNT_LIMIT, GitIntelligenceError, NativeGitIntelligence, map_repository_error,
};

impl NativeGitIntelligence {
    pub fn topology_ref_watermark(&self) -> Result<ManifestDigest, GitIntelligenceError> {
        let authority =
            GitRepositoryAuthority::discover(&self.repo_root).map_err(map_repository_error)?;
        let (_, _, watermark) = self.topology_ref_snapshot(&authority)?;
        Ok(watermark)
    }

    pub fn topology_projection(
        &self,
        max_count: u32,
    ) -> Result<GitTopologyProjectionV1, GitIntelligenceError> {
        let authority =
            GitRepositoryAuthority::discover(&self.repo_root).map_err(map_repository_error)?;
        let (head, refs, ref_watermark) = self.topology_ref_snapshot(&authority)?;
        let history = authority
            .history(&RepositoryHistoryOptions {
                max_count: max_count.clamp(1, GIT_HISTORY_MAX_COUNT_LIMIT),
                first_parent: false,
                path: None,
                follow_renames: false,
            })
            .map_err(map_repository_error)?;
        let (verified_head, verified_refs, verified_watermark) =
            self.topology_ref_snapshot(&authority)?;
        if head != verified_head || refs != verified_refs || ref_watermark != verified_watermark {
            return Err(GitIntelligenceError::MalformedOutput {
                operation: "topology projection",
                detail: "repository refs changed during topology capture".to_owned(),
            });
        }
        let projection = GitTopologyProjectionV1 {
            repository: self.repository.clone(),
            head,
            refs,
            history: GitHistoryV1 {
                repository: self.repository.clone(),
                commits: history.commits,
                truncated: history.truncated,
                coverage: GitCoverageV1::degraded(history.degradations.into_iter().collect()),
            },
            ref_watermark,
            branch_stacks: Vec::new(),
            worktree_occupancies: Vec::new(),
        };
        projection.validate().map_err(map_topology_error)?;
        Ok(projection)
    }

    fn topology_ref_snapshot(
        &self,
        authority: &GitRepositoryAuthority,
    ) -> Result<(GitHeadStateV1, Vec<GitTopologyRefV1>, ManifestDigest), GitIntelligenceError> {
        let head = authority.head().map_err(map_repository_error)?;
        let refs = authority
            .references()
            .map_err(map_repository_error)?
            .into_iter()
            .filter(|reference| reference.name.starts_with("refs/"))
            .map(topology_ref)
            .collect::<Result<Vec<_>, _>>()?;
        let watermark = git_topology_ref_watermark(&self.repository, &head, &refs)
            .map_err(map_topology_error)?;
        Ok((head, refs, watermark))
    }
}

fn topology_ref(reference: GitReference) -> Result<GitTopologyRefV1, GitIntelligenceError> {
    Ok(GitTopologyRefV1 {
        reference: RefId::new(reference.name)?,
        target: reference.target,
    })
}

fn map_topology_error(error: GitTopologyProjectionError) -> GitIntelligenceError {
    GitIntelligenceError::MalformedOutput {
        operation: "topology projection",
        detail: error.to_string(),
    }
}
