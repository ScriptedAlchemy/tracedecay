//! Injectable hook-driven branch-write and detached read-refresh writer
//! boundaries used by daemon-coordinated construction.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::errors::{Result, TraceDecayError};
use crate::mcp::hook_events::{self, HookAgent};
use crate::tracedecay::TraceDecay;

/// Complete hook-driven branch write requested by the MCP server.
///
/// `incremental_sync_agent` is set only for a routed worktree operation. In
/// that case the writer must keep the retained [`TraceDecay`] handle inside the
/// returned future until the conditional incremental sync has completed.
#[derive(Clone)]
pub(crate) struct HookBranchWriteRequest {
    pub(crate) graph: Arc<TraceDecay>,
    pub(crate) root: PathBuf,
    pub(crate) branch: String,
    pub(crate) incremental_sync_agent: Option<HookAgent>,
    /// R4: this write's single live-branch resolution, made once where the
    /// effect root is authorized. The direct writer and the daemon's
    /// coordinated writer both read it instead of re-opening the repository
    /// (a linked worktree would otherwise spawn `git` up to four times for
    /// one hook branch write). Request-scoped — never retained past the write.
    pub(crate) live_branch: crate::branch::BranchMemo,
}

/// Metadata returned after the complete hook branch write has settled. No
/// writable store handle crosses the capability boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HookBranchWriteResult {
    pub(crate) branch_outcome: crate::branch::BranchAddOutcome,
    pub(crate) refresh_file_token_map: bool,
}

/// Injectable ownership boundary for hook-driven branch writes.
///
/// Daemon construction can wrap [`execute_hook_branch_write_direct`] in its
/// store-administration coordinator so the permit covers branch creation and,
/// for routed worktrees, the subsequent open and incremental sync.
pub(crate) type HookBranchWriter = Arc<
    dyn Fn(
            HookBranchWriteRequest,
        ) -> Pin<Box<dyn Future<Output = Result<HookBranchWriteResult>> + Send>>
        + Send
        + Sync
        + 'static,
>;

pub(crate) fn direct_hook_branch_writer() -> HookBranchWriter {
    Arc::new(|request| Box::pin(execute_hook_branch_write_direct(request)))
}

pub(crate) async fn execute_hook_branch_write_direct(
    request: HookBranchWriteRequest,
) -> Result<HookBranchWriteResult> {
    if request.graph.branch_drifted_with(&request.live_branch) {
        return Err(TraceDecayError::Config {
            message: "retained hook branch graph drifted before write".to_string(),
        });
    }
    let branch_outcome = request
        .graph
        .track_worktree_branch(&request.root, &request.branch)
        .await?;
    let mut refresh_file_token_map = false;

    if branch_outcome == crate::branch::BranchAddOutcome::AlreadyTracked
        && let Some(agent) = request.incremental_sync_agent
    {
        let canonical_root = request
            .root
            .canonicalize()
            .unwrap_or_else(|_| request.root.clone());
        let active_branch = request.live_branch.resolve_for(&canonical_root);
        if request.graph.project_root() == canonical_root
            && request.graph.active_branch() == active_branch.as_deref()
        {
            refresh_file_token_map =
                run_hook_incremental_sync_direct(request.graph.as_ref(), agent).await?;
        }
    }

    Ok(HookBranchWriteResult {
        branch_outcome,
        refresh_file_token_map,
    })
}

pub(crate) async fn run_hook_incremental_sync_direct(
    cg: &TraceDecay,
    agent: HookAgent,
) -> Result<bool> {
    let marker = hook_events::sync_marker_path(&cg.store_layout().data_root, agent);
    let now = crate::tracedecay::current_timestamp();
    if !hook_events::should_run_sync(&marker, now, 3) {
        return Ok(false);
    }
    match cg.sync().await {
        Ok(_) | Err(TraceDecayError::SyncLock { .. }) => {
            hook_events::write_sync_marker(&marker, now);
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

/// Complete detached read-refresh write requested by the MCP server.
#[derive(Clone)]
pub(crate) struct BackgroundRefreshRequest {
    pub(crate) graph: Arc<TraceDecay>,
    pub(crate) project_root: PathBuf,
    pub(crate) full_sync_escalation_files: usize,
}

/// Injectable ownership boundary for a detached read refresh. The returned
/// token map is the only state allowed to cross back into the server; the
/// retained [`TraceDecay`] handle remains owned by the callback future.
pub(crate) type BackgroundRefreshWriter = Arc<
    dyn Fn(
            BackgroundRefreshRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Option<HashMap<String, u64>>>> + Send>>
        + Send
        + Sync
        + 'static,
>;

pub(crate) fn direct_background_refresh_writer() -> BackgroundRefreshWriter {
    Arc::new(|request| Box::pin(execute_background_refresh_direct(request)))
}

pub(crate) async fn execute_background_refresh_direct(
    request: BackgroundRefreshRequest,
) -> Result<Option<HashMap<String, u64>>> {
    let canonical_root = request
        .project_root
        .canonicalize()
        .unwrap_or_else(|_| request.project_root.clone());
    let active_branch = crate::branch::current_branch(&canonical_root);
    if request.graph.project_root() != canonical_root
        || request.graph.active_branch() != active_branch.as_deref()
    {
        return Err(TraceDecayError::Config {
            message: "retained background refresh graph is stale".to_string(),
        });
    }
    let cg = request.graph;
    let scoped = match (
        request.full_sync_escalation_files,
        cg.last_synced_commit().await,
    ) {
        (0, _) | (_, None) => None,
        (limit, Some(base)) => cg.stale_files_since_commit(&base, limit),
    };
    let result = if let Some(files) = scoped {
        if files.is_empty() {
            Ok(())
        } else {
            cg.sync_if_stale_silent(&files).await
        }
    } else {
        let stale = cg.find_stale_files().await;
        if stale.is_empty() {
            Ok(())
        } else {
            cg.sync_if_stale_silent(&stale).await
        }
    };
    result?;
    Ok(cg.get_file_token_map().await.ok())
}
