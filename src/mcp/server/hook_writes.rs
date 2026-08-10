//! Injectable hook-driven branch-write and detached read-refresh writer
//! boundaries used by daemon-coordinated construction.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

/// Complete hook-driven branch write requested by the MCP server.
///
/// Code-index reconciliation is deliberately outside this writer. The caller
/// submits a bounded scheduler signal only after the branch effect settles, so
/// branch admission never waits for indexing.
#[derive(Clone)]
pub(crate) struct HookBranchWriteRequest {
    pub(crate) graph: Arc<TraceDecay>,
    pub(crate) root: PathBuf,
    pub(crate) branch: String,
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
/// store-administration coordinator so the permit covers branch creation.
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
    Err(TraceDecayError::project_route(
        "code_index_scheduler_unavailable",
        true,
        format!(
            "branch {} at {} requires daemon scheduler admission",
            request.branch,
            request.root.display()
        ),
    ))
}

/// Complete detached reconciliation admission requested by the MCP server.
#[derive(Clone)]
pub(crate) struct BackgroundRefreshRequest {
    pub(crate) graph: Arc<TraceDecay>,
    pub(crate) project_root: PathBuf,
    pub(crate) reconcile_sink: Option<super::CodeIndexReconcileSink>,
}

/// Injectable ownership boundary for detached reconciliation admission.
/// Production returns no token map because scheduler acceptance is distinct
/// from index publication; test writers may return one to verify injection.
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
    let canonical_root =
        request
            .project_root
            .canonicalize()
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "could not resolve retained background refresh root {}: {error}",
                    request.project_root.display()
                ),
            })?;
    let active_branch = crate::branch::current_branch(&canonical_root);
    if request.graph.project_root() != canonical_root
        || request.graph.active_branch() != active_branch.as_deref()
    {
        return Err(TraceDecayError::Config {
            message: "retained background refresh graph is stale".to_string(),
        });
    }
    let Some(reconcile_sink) = request.reconcile_sink else {
        return Err(TraceDecayError::project_route(
            "code_index_scheduler_unavailable",
            true,
            "background refresh requires the daemon code-index scheduler",
        ));
    };
    if !reconcile_sink(canonical_root).await {
        return Err(TraceDecayError::project_route(
            "code_index_scheduler_unavailable",
            true,
            "background refresh was not accepted by the code-index scheduler",
        ));
    }
    Ok(None)
}
