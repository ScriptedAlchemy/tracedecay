//! Type-erased read bridge to a project graph already mounted by the daemon.
//!
//! These definitions used to live in `crate::mcp::server`, which made the
//! dashboard depend on the MCP layer for a seam it only ever consumed as an
//! injected `Option<…>` field on [`crate::DashboardState`]. The dependency is
//! inverted here: this crate owns the contract, and the root crate (MCP server
//! construction, daemon dashboard admission) supplies the implementation when
//! it builds dashboard state. Project selectors must not reconstruct graph
//! ownership from registry paths.

use std::path::PathBuf;
use std::sync::Arc;

use crate::tracedecay::TraceDecay;

pub type RetainedProjectGraphFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = tracedecay_runtime_core::errors::Result<Option<Arc<TraceDecay>>>,
            > + Send
            + 'static,
    >,
>;

#[derive(Clone)]
pub struct RetainedProjectGraphRequest {
    pub owner: Option<crate::global_db::ProjectRegistryContext>,
    pub registered_root: PathBuf,
    pub requested_worktree_root: PathBuf,
    pub requested_git_common_dir: Option<PathBuf>,
    pub requested_branch: Option<String>,
}

impl RetainedProjectGraphRequest {
    pub fn for_registered_project(
        owner: crate::global_db::ProjectRegistryContext,
        requested_worktree_root: PathBuf,
    ) -> Self {
        Self {
            registered_root: PathBuf::from(&owner.project.canonical_root),
            requested_git_common_dir: tracedecay_runtime_core::worktree::git_common_dir(
                &requested_worktree_root,
            ),
            requested_branch: tracedecay_runtime_core::branch::current_branch(
                &requested_worktree_root,
            ),
            requested_worktree_root,
            owner: Some(owner),
        }
    }

    pub fn for_mounted_root(root: PathBuf) -> Self {
        Self {
            requested_git_common_dir: tracedecay_runtime_core::worktree::git_common_dir(&root),
            requested_branch: tracedecay_runtime_core::branch::current_branch(&root),
            registered_root: root.clone(),
            requested_worktree_root: root,
            owner: None,
        }
    }
}

/// Implemented by the root crate (`crate::mcp::server`, `crate::daemon`) and
/// handed to dashboard state at construction.
pub type RetainedProjectGraphResolver =
    Arc<dyn Fn(RetainedProjectGraphRequest) -> RetainedProjectGraphFuture + Send + Sync + 'static>;
