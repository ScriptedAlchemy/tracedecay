//! Native `gix` reader for the narrow historical-blob read port.
//!
//! The port, its request/response values, and the canonical path predicate all
//! live beside this module. Only the concrete `gix` read was left in the root
//! Git adapter, which forced every historical consumer through the root
//! binary. The reader now lives with its port so extracted crates can mount
//! the exact same production read.
//!
//! Read-only is structural: `gix` opens no subprocess, and this module exposes
//! no revision expression, traversal, ref mutation, or object write surface.

use std::path::{Path, PathBuf};

use tracedecay_domain::git::GitOidV1;
use tracedecay_domain::research::{RepositoryId, WorktreeId};

use super::read::{
    GIT_HISTORICAL_BLOB_MAX_BYTES, GitHistoricalBlobReadPort, GitHistoricalBlobRequestV1,
    GitHistoricalBlobV1, GitIntelligenceError, is_canonical_repository_relative_path,
};

/// Fixed read-only historical blob reader for one repository checkout.
pub struct NativeHistoricalBlobReaderV1 {
    repo_root: PathBuf,
    repository: RepositoryId,
    worktree: WorktreeId,
}

impl NativeHistoricalBlobReaderV1 {
    pub fn new(
        repo_root: impl Into<PathBuf>,
        repository: RepositoryId,
        worktree: WorktreeId,
    ) -> Self {
        Self {
            repo_root: repo_root.into(),
            repository,
            worktree,
        }
    }

    /// Read one exact commit/path blob through the mounted Plan 36 authority.
    pub fn read(
        &self,
        request: &GitHistoricalBlobRequestV1,
    ) -> Result<GitHistoricalBlobV1, GitIntelligenceError> {
        if request.max_bytes == 0 || request.max_bytes > GIT_HISTORICAL_BLOB_MAX_BYTES {
            return Err(GitIntelligenceError::HistoricalBlobBoundExceeded {
                bound: GIT_HISTORICAL_BLOB_MAX_BYTES,
                actual: request.max_bytes,
            });
        }
        if !is_canonical_repository_relative_path(&request.path) {
            return Err(GitIntelligenceError::InvalidHistoricalPath(
                request.path.clone(),
            ));
        }
        let repo = gix::open(&self.repo_root).map_err(|error| {
            GitIntelligenceError::NotARepository(format!("{}: {error}", self.repo_root.display()))
        })?;
        let oid =
            gix::hash::ObjectId::from_hex(request.commit.as_str().as_bytes()).map_err(|error| {
                GitIntelligenceError::MalformedOutput {
                    operation: "historical_blob",
                    detail: error.to_string(),
                }
            })?;
        let commit = repo
            .find_object(oid)
            .map_err(|error| GitIntelligenceError::MalformedOutput {
                operation: "historical_blob",
                detail: error.to_string(),
            })?
            .try_into_commit()
            .map_err(|error| GitIntelligenceError::MalformedOutput {
                operation: "historical_blob",
                detail: error.to_string(),
            })?;
        let tree = commit
            .tree()
            .map_err(|error| GitIntelligenceError::MalformedOutput {
                operation: "historical_blob",
                detail: error.to_string(),
            })?;
        let Some(entry) = tree
            .lookup_entry_by_path(Path::new(&request.path))
            .map_err(|error| GitIntelligenceError::MalformedOutput {
                operation: "historical_blob",
                detail: error.to_string(),
            })?
        else {
            return Ok(self.absent(request));
        };
        if !entry.mode().is_blob_or_symlink() {
            return Ok(self.absent(request));
        }
        let size = repo
            .find_header(entry.object_id())
            .map_err(|error| GitIntelligenceError::MalformedOutput {
                operation: "historical_blob",
                detail: error.to_string(),
            })?
            .size();
        if request.include_bytes && size > request.max_bytes {
            return Err(GitIntelligenceError::HistoricalBlobBoundExceeded {
                bound: request.max_bytes,
                actual: size,
            });
        }
        let blob_oid = GitOidV1::new(entry.object_id().to_hex().to_string())?;
        if !request.include_bytes {
            return Ok(GitHistoricalBlobV1 {
                repository: self.repository.clone(),
                worktree: self.worktree.clone(),
                commit: request.commit.clone(),
                path: request.path.clone(),
                blob_oid: Some(blob_oid),
                bytes: None,
            });
        }
        let mut blob = entry
            .object()
            .map_err(|error| GitIntelligenceError::MalformedOutput {
                operation: "historical_blob",
                detail: error.to_string(),
            })?
            .try_into_blob()
            .map_err(|error| GitIntelligenceError::MalformedOutput {
                operation: "historical_blob",
                detail: error.to_string(),
            })?;
        Ok(GitHistoricalBlobV1 {
            repository: self.repository.clone(),
            worktree: self.worktree.clone(),
            commit: request.commit.clone(),
            path: request.path.clone(),
            blob_oid: Some(blob_oid),
            bytes: Some(blob.take_data()),
        })
    }

    /// The path names no readable blob at that commit. Absence is evidence,
    /// never an error.
    fn absent(&self, request: &GitHistoricalBlobRequestV1) -> GitHistoricalBlobV1 {
        GitHistoricalBlobV1 {
            repository: self.repository.clone(),
            worktree: self.worktree.clone(),
            commit: request.commit.clone(),
            path: request.path.clone(),
            blob_oid: None,
            bytes: None,
        }
    }
}

impl GitHistoricalBlobReadPort for NativeHistoricalBlobReaderV1 {
    fn historical_blob(
        &self,
        request: &GitHistoricalBlobRequestV1,
    ) -> Result<GitHistoricalBlobV1, GitIntelligenceError> {
        self.read(request)
    }
}
